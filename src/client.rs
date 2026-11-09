//! The async client (spec §5). It has no runtime of its own and spawns nothing: all of a call runs inside
//! the caller's future, and dropping that future cancels every request in it.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::SystemTime;

use futures_util::future::{join_all, try_join_all};

use crate::autocomplete::KeywordAutocomplete;
use crate::engine::{Engine, ListStep, MAX_TILES_IN_FLIGHT, NearbyArgs, Slot, Step, TileStep, fallback};
use crate::error::Error;
use crate::transport::HttpTransport;
#[cfg(feature = "reqwest")]
use crate::transport::ReqwestTransport;
use crate::types::{
    FallbackOptions, FallbackSearchResult, KeywordOptions, KeywordRegionMode, KeywordSearchResult, NearbyOptions,
    NearbySearchResult, PlaceRecord, Region, ServiceDescription, Tileset,
};

/// A client for one atgeo server.
///
/// It holds three in-memory caches (tile lists, tiles, the service description), so make one per server
/// and share it, by reference or in an `Arc`, for the life of the process. It is `Send + Sync` whenever
/// its transport is.
///
/// ```no_run
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> Result<(), atgeo::Error> {
/// use atgeo::{AtgeoClient, NearbyOptions, Region};
///
/// let atgeo = AtgeoClient::new();
/// let result = atgeo
///     .search_nearby(Region::circle(39.9526, -75.1652, 1000.0), NearbyOptions::new("overturemaps.org").text("Starbux"))
///     .await?;
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "reqwest")]
pub struct AtgeoClient<T = ReqwestTransport> {
    transport: T,
    engine: Engine,
}

/// A client for one atgeo server.
///
/// It holds three in-memory caches (tile lists, tiles, the service description), so make one per server
/// and share it, by reference or in an `Arc`, for the life of the process. It is `Send + Sync` whenever
/// its transport is.
#[cfg(not(feature = "reqwest"))]
pub struct AtgeoClient<T> {
    transport: T,
    engine: Engine,
}

#[cfg(feature = "reqwest")]
impl AtgeoClient<ReqwestTransport> {
    /// A client for the default server, over [`ReqwestTransport::new`].
    ///
    /// # Panics
    ///
    /// If the TLS backend cannot be initialised.
    pub fn new() -> Self {
        Self::with_transport(ReqwestTransport::new())
    }
}

#[cfg(feature = "reqwest")]
impl Default for AtgeoClient<ReqwestTransport> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> fmt::Debug for AtgeoClient<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.engine.fmt(f)
    }
}

impl<T: HttpTransport> AtgeoClient<T> {
    /// A client for the default server, over this transport.
    pub fn with_transport(transport: T) -> Self {
        AtgeoClient { transport, engine: Engine::default() }
    }

    /// The server's base URL; trailing slashes are stripped. Default: `https://places.atgeo.org`.
    ///
    /// Any string is taken here; one that is not an absolute http or https URL without credentials makes every call fail with
    /// `ArgumentError::InvalidServer` before any request.
    pub fn server(self, server: impl Into<String>) -> Self {
        AtgeoClient { transport: self.transport, engine: self.engine.with_server(&server.into()) }
    }

    /// The cap on the tile cache, in bytes of response body. Default: 50 MiB.
    pub fn cache_budget_bytes(self, bytes: u64) -> Self {
        AtgeoClient { transport: self.transport, engine: self.engine.with_budget(bytes) }
    }

    /// The wall clock the tile-list and service-description caches expire by. For tests.
    pub fn clock(self, clock: impl Fn() -> SystemTime + Send + Sync + 'static) -> Self {
        AtgeoClient { transport: self.transport, engine: self.engine.with_clock(Arc::new(clock)) }
    }

    /// Searches a region. The text, and the region's precise coordinates, never leave the device: the
    /// server is asked only for the tiles of a box snapped outward to 0.01°.
    ///
    /// Fails with `Error::InvalidArgument` for an invalid region or a similarity sort without text, before
    /// any request, and with the getTiles call's error if a tile list cannot be had. A tile that fails to
    /// load does not fail the search: it is counted in `tiles.failed` and reported, with why, in
    /// `tile_failures`, and `complete` is false. `disclosed` says what the server learned.
    pub async fn search_nearby(&self, region: Region, options: NearbyOptions) -> Result<NearbySearchResult, Error> {
        self.nearby(region, NearbyArgs::from(&options)).await
    }

    async fn nearby(&self, region: Region, args: NearbyArgs<'_>) -> Result<NearbySearchResult, Error> {
        let mut plan = self.engine.plan(region, &args)?;
        let steps = std::mem::take(&mut plan.lists);
        // The first error returns at once and drops the other request (contract §14).
        let lists = try_join_all(steps.into_iter().map(|step| async move {
            match step {
                ListStep::Cached(list) => Ok(list),
                ListStep::Fetch { key, url } => self.engine.accept_tile_list(&key, self.transport.get(&url).await),
            }
        }))
        .await?;
        let (urls, overview) = Engine::tile_urls(&lists);

        // The reference's mapLimit: min(8, n) workers, each claiming the next index.
        let n = urls.len();
        let next = AtomicUsize::new(0);
        let attempted = AtomicBool::new(false);
        let workers = (0..n.min(MAX_TILES_IN_FLIGHT)).map(|_| async {
            let mut done = Vec::new();
            loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= n {
                    return done;
                }
                done.push((i, self.tile(&urls[i], &attempted).await));
            }
        });
        let slots = Engine::in_url_order(join_all(workers).await.into_iter().flatten());
        Ok(self.engine.assemble(plan, urls, overview, slots, attempted.into_inner()))
    }

    /// Asks the server for places whose names match the keyword's words (contract §16). Unlike every
    /// other search, **this sends the keyword to the server**, and with a region, a box snapped outward to
    /// whole degrees (a circle is not accepted); `disclosed` on the result says so. The server answers at
    /// most 20 places, most notable first, which this returns in that order with no ranking, region filter
    /// or dedupe of its own. Nothing is cached.
    ///
    /// The privacy trade is this. A nearby search tells the server only a coarse area (0.01°) and never
    /// what was searched for; a keyword search tells it the words, and a region only as coarse as a
    /// degree, if one is given. The server keeps no log of it (garganorn's privacy design), but a hostile
    /// operator can record what it receives, the keyword with the time and the address the request came
    /// from. Whether that is acceptable is the app's decision; a nearby search is
    /// [`search_nearby`](Self::search_nearby).
    ///
    /// Fails with `Error::InvalidArgument` for a malformed region, before any request. A keyword the server
    /// cannot use (no word of two characters among its first twelve distinct words) is not an argument
    /// error: it goes out and the server answers `Error::Atgeo` with code `KeywordRequired`. A keyword
    /// over 256 bytes of UTF-8 is answered `InvalidRequest`.
    pub async fn search_by_keyword(
        &self,
        keyword: &str,
        options: KeywordOptions,
    ) -> Result<KeywordSearchResult, Error> {
        let url = self.engine.keyword_step(keyword, &options)?;
        self.engine.accept_keyword(self.transport.get(&url).await, options.region.is_some())
    }

    /// A nearby search for the keyword on the device, and, only if that finds no solid hit, a keyword
    /// request (contract §18). A hit whose name matches the keyword word for word (match tier 1, whatever its
    /// score) is solid. When it is not, and the keyword could be answered, **the keyword goes to the server**
    /// as for [`search_by_keyword`](Self::search_by_keyword), with the nearby region snapped to whole degrees
    /// when `keyword_region` is [`Degree`](KeywordRegionMode::Degree) and without one for
    /// [`None`](crate::KeywordRegionMode::None). The result's `disclosed` says which happened.
    ///
    /// The keyword hits come first, then the nearby hits the answer did not already hold.
    ///
    /// A failure after the nearby half, such as the keyword request failing, fails the call, and the nearby
    /// results are lost; the keyword may have reached the server even though there is no result to say so.
    /// A keyword the server refuses with `KeywordRequired` is not a failure: the nearby result stands.
    pub async fn search_nearby_then_by_keyword(
        &self,
        region: Region,
        options: FallbackOptions,
    ) -> Result<FallbackSearchResult, Error> {
        let nearby = self.nearby(region, NearbyArgs::from(&options)).await?;
        let Some(url) = self.engine.fallback_step(&nearby, &region, &options) else {
            return Ok(fallback(nearby));
        };
        let found = self
            .engine
            .accept_keyword(self.transport.get(&url).await, options.keyword_region == KeywordRegionMode::Degree);
        Engine::finish_fallback(nearby, found, options.keyword_region)
    }

    /// An autocompleter for one text field (contract §19): its `search_by_keyword` sends **every keyword
    /// that qualifies to the server as typed, prefixes included**, and a call supersedes the previous call on
    /// the same autocompleter. The dataset needs no checking, and a malformed region fails here, with
    /// `Error::InvalidArgument`.
    pub fn autocomplete_by_keyword(&self, options: KeywordOptions) -> Result<KeywordAutocomplete<'_, T>, Error> {
        KeywordAutocomplete::new(self, options)
    }

    async fn tile(&self, url: &str, attempted: &AtomicBool) -> Slot {
        match self.engine.tile_step(url) {
            TileStep::Done(slot) => slot,
            TileStep::Fetch(prepared) => {
                attempted.store(true, Ordering::Relaxed);
                // Let the other workers look up their URLs before this one can store a tile, as the reference's
                // do: a transport that answers on its first poll would otherwise store, and evict, before the
                // first wave's lookups, and change which tiles a tight budget keeps.
                YieldOnce(false).await;
                self.engine.accept_tile(url, self.transport.get(&prepared).await)
            }
        }
    }

    /// One place by its origin: from a tile an earlier search fetched when one holds it, without a
    /// request, else by asking the server, which sends it the origin and nothing else.
    pub async fn get_place(&self, origin: &str) -> Result<PlaceRecord, Error> {
        match self.engine.place_step(origin)? {
            Step::Done(record) => Ok(record),
            Step::Fetch(url) => self.engine.accept_place(origin, self.transport.get(&url).await),
        }
    }

    /// The server's identity and the tilesets it serves. Cached for an hour.
    pub async fn describe_service(&self) -> Result<ServiceDescription, Error> {
        match self.engine.service_step()? {
            Step::Done(service) => Ok(service),
            Step::Fetch(url) => self.engine.accept_service(self.transport.get(&url).await),
        }
    }

    /// A tileset's descriptor. Never cached.
    pub async fn describe_tileset(&self, slug: &str) -> Result<Tileset, Error> {
        let url = self.engine.tileset_url(slug)?;
        self.engine.accept_tileset(self.transport.get(&url).await)
    }
}

/// Pending once, waking itself straight away: one turn of whatever executor polls it.
struct YieldOnce(bool);

impl Future for YieldOnce {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.0 {
            Poll::Ready(())
        } else {
            self.0 = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}
