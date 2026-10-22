//! The async client (spec §5). It has no runtime of its own and spawns nothing: all of a call runs inside
//! the caller's future, and dropping that future cancels every request in it.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::SystemTime;

use futures_util::future::{join_all, try_join_all};

use crate::engine::{Engine, ListStep, MAX_TILES_IN_FLIGHT, Slot, Step, TileStep};
use crate::error::Error;
use crate::transport::HttpTransport;
#[cfg(feature = "reqwest")]
use crate::transport::ReqwestTransport;
use crate::types::{PlaceRecord, Region, SearchOptions, SearchResult, ServiceDescription, Tileset};

/// A client for one atgeo server.
///
/// It holds three in-memory caches (tile lists, tiles, the service description), so make one per server
/// and share it, by reference or in an `Arc`, for the life of the process. It is `Send + Sync` whenever
/// its transport is.
///
/// ```no_run
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> Result<(), atgeo::Error> {
/// use atgeo::{AtgeoClient, Region, SearchOptions};
///
/// let atgeo = AtgeoClient::new();
/// let result = atgeo
///     .search_places(Region::circle(39.9526, -75.1652, 1000.0), SearchOptions::new().text("Starbux"))
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
    /// Any string is taken here; one that is not an absolute http or https URL makes every call fail with
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
    /// load does not fail the search: it is counted in `tiles.failed`, and `complete` is false.
    pub async fn search_places(&self, region: Region, options: SearchOptions) -> Result<SearchResult, Error> {
        let mut plan = self.engine.plan(region, &options)?;
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
        let workers = (0..n.min(MAX_TILES_IN_FLIGHT)).map(|_| async {
            let mut done = Vec::new();
            loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= n {
                    return done;
                }
                done.push((i, self.tile(&urls[i]).await));
            }
        });
        let mut slots: Vec<Slot> = vec![None; n];
        for (i, slot) in join_all(workers).await.into_iter().flatten() {
            slots[i] = slot;
        }
        Ok(self.engine.assemble(plan, urls, overview, slots))
    }

    async fn tile(&self, url: &str) -> Slot {
        match self.engine.tile_step(url) {
            TileStep::Cached(slot) => slot,
            TileStep::Failed => None,
            TileStep::Fetch(prepared) => self.engine.accept_tile(url, self.transport.get(&prepared).await),
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
