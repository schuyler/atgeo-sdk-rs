//! The sans-I/O core both clients drive: it owns the caches, plans a search, reads every response and
//! assembles the result (contract §4, §9). It never makes a request; each client supplies those and the
//! concurrency.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::SystemTime;

use crate::attribution::{collection_of, collection_prefix, in_collection, merge};
use crate::cache::{CachedTile, Tile, TileCache, TileRecord, TtlCache};
use crate::error::{ArgumentError, DecodingError, Error};
use crate::locations::{in_region, locate, record_key};
use crate::rank::{cmp_utf16, rank};
use crate::region::{center, circle_box, keyword_bbox, query_bboxes, validate};
use crate::text::{Query, normalize, qualifies};
use crate::transport::{BoxError, HttpResponse};
use crate::types::{
    Attribution, Disclosure, FallbackHit, FallbackOptions, FallbackSearchResult, KeywordHit, KeywordOptions,
    KeywordRegionMode, KeywordSearchResult, NearbyOptions, NearbySearchResult, PlaceRecord, Region, ServiceDescription,
    SortMode, TileCounts, TileFailure, TileFailureKind, Tileset,
};
use crate::wire::{self, TileList};
use crate::xrpc;

fn into_tile(payload: wire::TilePayload) -> Arc<Tile> {
    Arc::new(Tile { records: payload.records, attribution: payload.attribution.unwrap_or_default() })
}

pub(crate) const DEFAULT_SERVER: &str = "https://places.atgeo.org";
pub(crate) const DEFAULT_CACHE_BUDGET_BYTES: u64 = 50 * 1024 * 1024;
pub(crate) const MAX_TILES_IN_FLIGHT: usize = 8;

pub(crate) type Clock = Arc<dyn Fn() -> SystemTime + Send + Sync>;

/// Records of a tile a search used, with the generation of the cache entry they came from.
pub(crate) type Used = (u64, Arc<Tile>);

/// What one tile of a search came to: its records, or why it failed.
pub(crate) type Slot = Result<Used, TileFailure>;

/// Parsed tiles by URL, each with the generation of the cache entry it was parsed from.
type ParsedTiles = HashMap<String, Used>;

/// The error of a refused tile URL. It names no URL: the failure carries that, and a URL with credentials
/// should not be repeated wherever the error is printed.
const REFUSED: &str = "tile URL is not an absolute http or https URL without credentials";

/// A cache is an optimization: every critical section leaves it consistent before anything in it can
/// panic, so a poisoned lock is recovered rather than failing every later call.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What one tile list needs: nothing, or a getTiles request whose answer is cached under `key`.
pub(crate) enum ListStep {
    Cached(Arc<TileList>),
    Fetch { key: String, url: String },
}

/// What a nearby search is asked: the options of `search_nearby`, or the nearby half of the fallback, whose
/// text is its keyword.
pub(crate) struct NearbyArgs<'a> {
    pub(crate) dataset: &'a str,
    pub(crate) collection: Option<&'a str>,
    pub(crate) text: Option<&'a str>,
    pub(crate) sort: SortMode,
}

impl<'a> From<&'a NearbyOptions> for NearbyArgs<'a> {
    fn from(o: &'a NearbyOptions) -> Self {
        NearbyArgs { dataset: &o.dataset, collection: o.collection.as_deref(), text: o.text.as_deref(), sort: o.sort }
    }
}

impl<'a> From<&'a FallbackOptions> for NearbyArgs<'a> {
    /// The nearby half of the fallback searches for the keyword.
    fn from(o: &'a FallbackOptions) -> Self {
        NearbyArgs { dataset: &o.dataset, collection: o.collection.as_deref(), text: Some(&o.keyword), sort: o.sort }
    }
}

/// A validated search, ready to run.
pub(crate) struct Plan {
    region: Region,
    query: Option<Query>,
    sort: SortMode,
    /// `urn:atgeo:{dataset}:{collection}:` when the search has a collection.
    prefix: Option<String>,
    pub(crate) lists: Vec<ListStep>,
    /// Some tile list is not cached, so a getTiles request will go out (contract §4.11).
    lists_requested: bool,
}

/// What one tile needs.
pub(crate) enum TileStep {
    /// Nothing: a tile from the cache, or a URL refused without a request (not an absolute http or https
    /// URL without credentials).
    Done(Slot),
    /// A request to this (prepared) URL.
    Fetch(String),
}

/// What a call that can be answered from cache needs.
pub(crate) enum Step<T> {
    Done(T),
    Fetch(String),
}

pub(crate) struct Engine {
    server: String,
    budget: u64,
    clock: Clock,
    tiles: Mutex<TileCache>,
    /// The parsed records of the latest search's tiles, kept outside the budget (contract §10).
    parsed: Mutex<ParsedTiles>,
    tile_lists: Mutex<TtlCache<Arc<TileList>>>,
    service: Mutex<TtlCache<ServiceDescription>>,
}

impl fmt::Debug for Engine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AtgeoClient").field("server", &self.server).field("cache_budget_bytes", &self.budget).finish()
    }
}

impl Default for Engine {
    fn default() -> Self {
        Engine::new(DEFAULT_SERVER, DEFAULT_CACHE_BUDGET_BYTES, Arc::new(SystemTime::now))
    }
}

impl Engine {
    pub(crate) fn new(server: &str, budget: u64, clock: Clock) -> Self {
        Engine {
            server: xrpc::strip_server(server),
            budget,
            clock,
            tiles: Mutex::new(TileCache::new(budget)),
            parsed: Mutex::new(HashMap::new()),
            tile_lists: Mutex::new(TtlCache::new()),
            service: Mutex::new(TtlCache::new()),
        }
    }

    /// The same settings with one changed, and empty caches.
    pub(crate) fn with_server(self, server: &str) -> Self {
        Engine::new(server, self.budget, self.clock)
    }

    pub(crate) fn with_budget(self, budget: u64) -> Self {
        Engine::new(&self.server, budget, self.clock)
    }

    pub(crate) fn with_clock(self, clock: Clock) -> Self {
        Engine::new(&self.server, self.budget, clock)
    }

    fn now(&self) -> SystemTime {
        (self.clock)()
    }

    fn check_server(&self) -> Result<(), Error> {
        if xrpc::is_http_url(&xrpc::prepare_url(&self.server)) {
            Ok(())
        } else {
            Err(ArgumentError::InvalidServer.into())
        }
    }

    fn xrpc_url(&self, nsid: &str, params: &[(&str, &str)]) -> String {
        xrpc::prepare_url(&xrpc::url(&self.server, nsid, params))
    }

    /// The body of a 2xx response; anything else is the server's `AtgeoError`.
    fn checked(response: HttpResponse) -> Result<Vec<u8>, Error> {
        if (200..300).contains(&response.status) {
            Ok(response.body)
        } else {
            Err(Error::Atgeo(wire::error_from(response.status, &response.body)))
        }
    }

    // --- searchNearby -------------------------------------------------------------------------

    /// Validates in the contract's order (§3) and plans the tile lists, before any request. The dataset is
    /// required by the type, so the order starts with the region.
    pub(crate) fn plan(&self, region: Region, args: &NearbyArgs<'_>) -> Result<Plan, Error> {
        validate(&region)?;
        let query = Query::new(&normalize(args.text.unwrap_or("")));
        if args.sort == SortMode::Similarity && query.is_none() {
            return Err(ArgumentError::SimilarityRequiresText.into());
        }
        self.check_server()?;

        let (dataset, collection) = (args.dataset, args.collection);
        let now = self.now();
        let lists: Vec<ListStep> = query_bboxes(&region)
            .into_iter()
            .map(|bbox| {
                let key = format!("{dataset}|{}|{bbox}", collection.unwrap_or(""));
                if let Some(list) = lock(&self.tile_lists).get(&key, now) {
                    return ListStep::Cached(list);
                }
                let url = match collection {
                    Some(c) => {
                        self.xrpc_url("org.atgeo.getTiles", &[("dataset", dataset), ("collection", c), ("bbox", &bbox)])
                    }
                    None => self.xrpc_url("org.atgeo.getTiles", &[("dataset", dataset), ("bbox", &bbox)]),
                };
                ListStep::Fetch { key, url }
            })
            .collect();
        let lists_requested = lists.iter().any(|l| matches!(l, ListStep::Fetch { .. }));
        Ok(Plan {
            region,
            query,
            sort: args.sort,
            prefix: collection.map(|c| collection_prefix(dataset, c)),
            lists,
            lists_requested,
        })
    }

    /// Reads a getTiles answer and caches it for an hour; a failure caches nothing.
    pub(crate) fn accept_tile_list(
        &self,
        key: &str,
        response: Result<HttpResponse, BoxError>,
    ) -> Result<Arc<TileList>, Error> {
        let body = Self::checked(response.map_err(Error::Transport)?)?;
        let list = Arc::new(wire::tile_list(&body)?);
        lock(&self.tile_lists).set(key, list.clone(), self.now());
        Ok(list)
    }

    /// Every list's tiles, deduplicated and sorted by UTF-16 code units, and whether any list is overview.
    pub(crate) fn tile_urls(lists: &[Arc<TileList>]) -> (Vec<String>, bool) {
        let mut seen = HashSet::new();
        let mut urls: Vec<String> =
            lists.iter().flat_map(|l| &l.tiles).filter(|u| seen.insert(u.as_str())).cloned().collect();
        urls.sort_by(|a, b| cmp_utf16(a, b));
        (urls, lists.iter().any(|l| l.overview))
    }

    /// A cached tile's records. Only a body that was parsed once can be here, so an error is one a
    /// deterministic parser cannot give; it is reported as a `decode` failure all the same (§4.4).
    fn tile_of(&self, tile: &CachedTile) -> Result<Arc<Tile>, DecodingError> {
        if let Some((generation, records)) = lock(&self.parsed).get(&tile.url) {
            if *generation == tile.generation {
                return Ok(records.clone());
            }
        }
        wire::tile_payload(&tile.body).map(into_tile)
    }

    /// Called when a worker reaches the URL, as the reference consults the cache, so that recency is
    /// touched in the same order.
    pub(crate) fn tile_step(&self, url: &str) -> TileStep {
        let cached = lock(&self.tiles).get(url);
        if let Some(tile) = cached {
            return TileStep::Done(
                self.tile_of(&tile)
                    .map(|r| (tile.generation, r))
                    .map_err(|e| TileFailure::new(url, TileFailureKind::Decode, None, e.into())),
            );
        }
        let prepared = xrpc::prepare_url(url);
        if xrpc::is_http_url(&prepared) {
            TileStep::Fetch(prepared)
        } else {
            TileStep::Done(Err(TileFailure::new(url, TileFailureKind::Refused, None, Error::Transport(REFUSED.into()))))
        }
    }

    /// Reads a tile and caches it. What the transport, the status check and decoding fail with is a failed
    /// tile, never an error; the cache insert after them is not guarded, and cannot fail (§4.4).
    pub(crate) fn accept_tile(&self, url: &str, response: Result<HttpResponse, BoxError>) -> Slot {
        let fail = |kind, status, error| TileFailure::new(url, kind, status, error);
        let response = response.map_err(|e| fail(TileFailureKind::Transport, None, Error::Transport(e)))?;
        if !(200..300).contains(&response.status) {
            let error = Error::Atgeo(wire::error_from(response.status, &response.body));
            return Err(fail(TileFailureKind::Status, Some(response.status), error));
        }
        // A tile whose body does not decode fails here, before anything is stored, so its attribution is
        // never read into a result (§4.10).
        let tile =
            into_tile(wire::tile_payload(&response.body).map_err(|e| fail(TileFailureKind::Decode, None, e.into()))?);
        let keys = tile.records.iter().filter_map(|r| record_key(r).map(str::to_owned)).collect();
        let generation = lock(&self.tiles).insert(url, response.body.into(), keys);
        Ok((generation, tile))
    }

    /// The slots the workers finished, by index, in URL order.
    ///
    /// # Panics
    ///
    /// Unless every index from 0 is there exactly once, in release builds too: a slot attributed to the
    /// wrong URL would report the wrong tile as failed.
    pub(crate) fn in_url_order(done: impl IntoIterator<Item = (usize, Slot)>) -> Vec<Slot> {
        let mut done: Vec<(usize, Slot)> = done.into_iter().collect();
        done.sort_unstable_by_key(|(i, _)| *i);
        assert!(done.iter().enumerate().all(|(n, (i, _))| n == *i), "every tile index exactly once");
        done.into_iter().map(|(_, slot)| slot).collect()
    }

    /// Concatenates, dedupes, filters and ranks (§4.5 to §4.9), and keeps this search's parsed tiles.
    /// `tiles_requested` is whether a tile fetch was attempted: a URL refused without a request is not.
    pub(crate) fn assemble(
        &self,
        plan: Plan,
        urls: Vec<String>,
        overview: bool,
        slots: Vec<Slot>,
        tiles_requested: bool,
    ) -> NearbySearchResult {
        assert_eq!(urls.len(), slots.len(), "one slot per tile URL");
        let mut used: Vec<(String, Used)> = Vec::with_capacity(slots.len());
        let mut tile_failures = Vec::new();
        for (url, slot) in urls.into_iter().zip(slots) {
            match slot {
                Ok(tile) => used.push((url, tile)),
                Err(failure) => tile_failures.push(failure),
            }
        }
        let (fetched, failed) = (used.len(), tile_failures.len());

        // Every used tile counts, whether or not any of its records survives below (§4.10).
        let attribution = merge(used.iter().map(|(_, (_, tile))| tile.attribution.as_slice()));

        let mut seen: HashSet<&str> = HashSet::new();
        let mut kept: Vec<(&TileRecord, &[Attribution])> = Vec::new();
        for (_, (_, tile)) in &used {
            for r in &tile.records {
                // dedupe picks a winner before the region filter runs (§4.6).
                let key = record_key(r);
                if key.is_some_and(|k| !seen.insert(k)) {
                    continue;
                }
                if plan.prefix.as_deref().is_some_and(|prefix| !in_collection(&r.value, prefix)) {
                    continue;
                }
                if in_region(&locate(&r.value), &plan.region) {
                    kept.push((r, tile.attribution.as_slice()));
                }
            }
        }
        let places = rank(kept, center(&plan.region), plan.query.as_ref(), plan.sort, overview);

        *lock(&self.parsed) = used.into_iter().collect();

        // Contract §4.11: the server learned the 0.01° grid if the call asked for a tile list or tried a tile;
        // a call answered entirely from cache told it nothing.
        let region = (plan.lists_requested || tiles_requested).then_some(0.01);
        NearbySearchResult {
            places,
            complete: failed == 0,
            overview,
            tiles: TileCounts { fetched, failed },
            tile_failures,
            attribution,
            disclosed: Disclosure::new(false, region),
        }
    }

    // --- searchByKeyword and the fallback ---------------------------------------------------------

    /// The searchPlaces URL: `dataset`, `collection` only if given, `q` exactly as passed, `bbox` only if
    /// given (contract §16).
    fn keyword_url(&self, dataset: &str, collection: Option<&str>, keyword: &str, bbox: Option<&str>) -> String {
        let mut params = vec![("dataset", dataset)];
        params.extend(collection.map(|c| ("collection", c)));
        params.push(("q", keyword));
        params.extend(bbox.map(|b| ("bbox", b)));
        self.xrpc_url("org.atgeo.searchPlaces", &params)
    }

    /// Validates in the contract's order (§3, §16), and builds the request.
    pub(crate) fn keyword_step(&self, keyword: &str, options: &KeywordOptions) -> Result<String, Error> {
        if let Some(region) = &options.region {
            validate(&region.as_region())?;
        }
        self.check_server()?;
        let bbox = options.region.map(|r| keyword_bbox(r.west, r.south, r.east, r.north));
        Ok(self.keyword_url(&options.dataset, options.collection.as_deref(), keyword, bbox.as_deref()))
    }

    /// Reads a searchPlaces answer. Nothing from it enters any cache (contract §10). `region_sent` is whether
    /// the request carried a region.
    pub(crate) fn accept_keyword(
        &self,
        response: Result<HttpResponse, BoxError>,
        region_sent: bool,
    ) -> Result<KeywordSearchResult, Error> {
        let body = Self::checked(response.map_err(Error::Transport)?)?;
        let found = wire::search_places(&body)?;
        let attribution = merge(found.iter().map(|p| p.attribution.as_slice()));
        let places = found
            .into_iter()
            .map(|p| KeywordHit {
                collection: collection_of(&p.value, &p.attribution),
                origin: p.origin,
                place: p.value,
                importance: p.importance,
            })
            .collect();
        Ok(KeywordSearchResult { places, attribution, disclosed: Disclosure::new(true, region_sent.then_some(1.0)) })
    }

    /// The keyword request of the fallback (contract §18), or `None` when the nearby result stands: a hit
    /// of match tier 1 is solid, and a keyword the server would refuse is not sent.
    pub(crate) fn fallback_step(
        &self,
        nearby: &NearbySearchResult,
        region: &Region,
        options: &FallbackOptions,
    ) -> Option<String> {
        // A keyword that normalizes to nothing has no tokens, so every hit is tier 1, as in the reference.
        let tier_one = match Query::new(&normalize(&options.keyword)) {
            Some(query) => nearby.places.iter().any(|hit| query.tier(&hit.place) == 1),
            None => !nearby.places.is_empty(),
        };
        if tier_one || !qualifies(&options.keyword) {
            return None;
        }
        let bbox = match (options.keyword_region, *region) {
            (KeywordRegionMode::Degree, Region::Bbox { west, south, east, north }) => {
                Some(keyword_bbox(west, south, east, north))
            }
            (KeywordRegionMode::Degree, Region::Circle { lat, lon, radius }) => {
                let (west, south, east, north) = circle_box(lat, lon, radius);
                Some(keyword_bbox(west, south, east, north))
            }
            _ => None,
        };
        Some(self.keyword_url(&options.dataset, options.collection.as_deref(), &options.keyword, bbox.as_deref()))
    }

    /// The fallback's result once its keyword request has been answered (contract §18, steps 6 to 8).
    pub(crate) fn finish_fallback(
        nearby: NearbySearchResult,
        found: Result<KeywordSearchResult, Error>,
        mode: KeywordRegionMode,
    ) -> Result<FallbackSearchResult, Error> {
        let sent_region = match mode {
            KeywordRegionMode::Degree => Some(1.0),
            _ => None,
        };
        match found {
            Ok(found) => {
                let mut nearby = nearby;
                let origins: HashSet<&str> = found.places.iter().filter_map(|h| h.origin.as_deref()).collect();
                // A hit with no origin is never left out, even when a keyword hit has none either.
                let kept: Vec<FallbackHit> = std::mem::take(&mut nearby.places)
                    .into_iter()
                    .filter(|h| h.origin.as_deref().is_none_or(|o| !origins.contains(o)))
                    .map(FallbackHit::Nearby)
                    .collect();
                // The nearby entry wins a tie, so it is listed first.
                let attribution = merge([nearby.attribution.as_slice(), found.attribution.as_slice()]);
                let region = nearby.disclosed.region.or(found.disclosed.region);
                let places = found.places.into_iter().map(FallbackHit::Keyword).chain(kept).collect();
                Ok(FallbackSearchResult {
                    attribution,
                    disclosed: Disclosure::new(true, region),
                    ..fallback_with(nearby, places)
                })
            }
            // The server refused a keyword the qualifying check let through: the nearby result stands, though
            // the keyword did go out.
            Err(Error::Atgeo(e)) if e.code == "KeywordRequired" => {
                let region = nearby.disclosed.region.or(sent_region);
                Ok(FallbackSearchResult { disclosed: Disclosure::new(true, region), ..fallback(nearby) })
            }
            Err(e) => Err(e),
        }
    }

    // --- getPlace and the describe calls --------------------------------------------------------

    /// From the cached tile with the lowest URL that holds the origin, or a getPlace request.
    pub(crate) fn place_step(&self, origin: &str) -> Result<Step<PlaceRecord>, Error> {
        self.check_server()?;
        let found = lock(&self.tiles).find(origin);
        if let Some(tile) = found {
            if let Ok(tile) = self.tile_of(&tile) {
                if let Some(r) = tile.records.iter().find(|r| record_key(r) == Some(origin)) {
                    return Ok(Step::Done(PlaceRecord {
                        origin: origin.to_owned(),
                        place: r.value.clone(),
                        importance: r.importance,
                        attribution: tile.attribution.clone(),
                    }));
                }
            }
        }
        Ok(Step::Fetch(self.xrpc_url("org.atgeo.getPlace", &[("uri", origin)])))
    }

    pub(crate) fn accept_place(
        &self,
        origin: &str,
        response: Result<HttpResponse, BoxError>,
    ) -> Result<PlaceRecord, Error> {
        let body = Self::checked(response.map_err(Error::Transport)?)?;
        let view = wire::place_view(&body)?;
        Ok(PlaceRecord {
            origin: origin.to_owned(),
            place: Arc::new(view.value),
            importance: view.importance,
            attribution: view.attribution,
        })
    }

    pub(crate) fn service_step(&self) -> Result<Step<ServiceDescription>, Error> {
        self.check_server()?;
        match lock(&self.service).get("", self.now()) {
            Some(service) => Ok(Step::Done(service)),
            None => Ok(Step::Fetch(self.xrpc_url("org.atgeo.describeService", &[]))),
        }
    }

    pub(crate) fn accept_service(&self, response: Result<HttpResponse, BoxError>) -> Result<ServiceDescription, Error> {
        let body = Self::checked(response.map_err(Error::Transport)?)?;
        let service = wire::service_description(&body)?;
        lock(&self.service).set("", service.clone(), self.now());
        Ok(service)
    }

    pub(crate) fn tileset_url(&self, slug: &str) -> Result<String, Error> {
        self.check_server()?;
        Ok(self.xrpc_url("org.atgeo.describeTileset", &[("slug", slug)]))
    }

    pub(crate) fn accept_tileset(&self, response: Result<HttpResponse, BoxError>) -> Result<Tileset, Error> {
        let body = Self::checked(response.map_err(Error::Transport)?)?;
        Ok(wire::tileset(&body)?)
    }
}

/// The nearby result as a fallback result, with its own `disclosed`.
pub(crate) fn fallback(mut nearby: NearbySearchResult) -> FallbackSearchResult {
    let places = std::mem::take(&mut nearby.places).into_iter().map(FallbackHit::Nearby).collect();
    fallback_with(nearby, places)
}

/// The nearby result's other fields around these places.
fn fallback_with(nearby: NearbySearchResult, places: Vec<FallbackHit>) -> FallbackSearchResult {
    FallbackSearchResult {
        places,
        complete: nearby.complete,
        overview: nearby.overview,
        tiles: nearby.tiles,
        tile_failures: nearby.tile_failures,
        attribution: nearby.attribution,
        disclosed: nearby.disclosed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(body: &str) -> Result<HttpResponse, BoxError> {
        Ok(HttpResponse::new(200, body.as_bytes().to_vec()))
    }

    const TILE: &str = r#"{"records":[{"value":{"origin":"o1","name":"A","locations":[
        {"$type":"community.lexicon.location.geo","latitude":"0","longitude":"0"}]}}]}"#;

    #[test]
    fn a_memoized_tile_is_used_only_while_the_cache_holds_the_same_entry() {
        let e = Engine::default();
        let (g1, first) = e.accept_tile("https://t/a", ok(TILE)).unwrap();
        *lock(&e.parsed) = HashMap::from([("https://t/a".to_owned(), (g1, first.clone()))]);
        let TileStep::Done(Ok((g, again))) = e.tile_step("https://t/a") else { panic!() };
        assert_eq!(g, g1);
        assert!(Arc::ptr_eq(&first, &again), "the memo answers while the generation matches");

        let (g2, _) = e.accept_tile("https://t/a", ok(TILE)).unwrap();
        assert_ne!(g1, g2);
        let TileStep::Done(Ok((g, reparsed))) = e.tile_step("https://t/a") else { panic!() };
        assert_eq!(g, g2);
        assert!(!Arc::ptr_eq(&first, &reparsed), "a replaced entry is parsed again");
        assert_eq!(first, reparsed);
    }

    #[test]
    fn a_tile_url_that_is_not_http_fails_without_a_request() {
        let e = Engine::default();
        for url in ["tiles/a.json", "file:///etc/passwd", "ftp://h/a", ""] {
            let TileStep::Done(Err(f)) = e.tile_step(url) else { panic!("{url:?}") };
            assert_eq!((f.url.as_str(), f.kind, f.status), (url, TileFailureKind::Refused, None));
            assert!(matches!(f.error.as_deref(), Some(Error::Transport(_))));
        }
        let TileStep::Fetch(u) = e.tile_step("https://h/a b.json") else { panic!() };
        assert_eq!(u, "https://h/a%20b.json");
    }

    #[test]
    fn a_cached_tile_that_fails_to_parse_again_is_a_decode_failure() {
        let e = Engine::default();
        lock(&e.tiles).insert("https://t/a", b"not json".to_vec().into(), HashSet::new());
        let TileStep::Done(Err(f)) = e.tile_step("https://t/a") else { panic!() };
        assert_eq!((f.url.as_str(), f.kind, f.status), ("https://t/a", TileFailureKind::Decode, None));
        assert!(matches!(f.error.as_deref(), Some(Error::Decoding(_))));
    }

    #[test]
    fn puts_finished_slots_in_url_order() {
        let failure =
            |u: &str| Err(TileFailure::new(u, TileFailureKind::Transport, None, Error::Transport("x".into())));
        let slots = Engine::in_url_order([(2, failure("c")), (0, failure("a")), (1, failure("b"))]);
        let urls: Vec<_> = slots.iter().map(|s| s.as_ref().unwrap_err().url.as_str()).collect();
        assert_eq!(urls, ["a", "b", "c"]);
    }

    #[test]
    #[should_panic(expected = "every tile index exactly once")]
    fn a_missing_slot_panics_rather_than_shifting_the_rest() {
        let failure =
            |u: &str| Err(TileFailure::new(u, TileFailureKind::Transport, None, Error::Transport("x".into())));
        Engine::in_url_order([(0, failure("a")), (2, failure("c"))]);
    }

    #[test]
    fn sorts_and_dedupes_tile_urls_across_lists() {
        let l = |tiles: &[&str], overview| {
            Arc::new(TileList { tiles: tiles.iter().map(|s| s.to_string()).collect(), overview })
        };
        let (urls, overview) =
            Engine::tile_urls(&[l(&["https://b", "https://a/9"], false), l(&["https://a/10", "https://b"], true)]);
        assert_eq!(urls, ["https://a/10", "https://a/9", "https://b"]);
        assert!(overview);
    }
}
