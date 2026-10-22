//! The sans-I/O core both clients drive: it owns the caches, plans a search, reads every response and
//! assembles the result (contract §4, §9). It never makes a request; each client supplies those and the
//! concurrency.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::SystemTime;

use crate::cache::{CachedTile, TileCache, TileRecord, TtlCache};
use crate::error::{ArgumentError, DecodingError, Error};
use crate::locations::{in_region, locate, record_key};
use crate::rank::{cmp_utf16, rank};
use crate::region::{center, query_bboxes, validate};
use crate::text::{Query, normalize};
use crate::transport::{BoxError, HttpResponse};
use crate::types::{
    PlaceRecord, Region, SearchOptions, SearchResult, ServiceDescription, SortMode, TileCounts, TileFailure,
    TileFailureKind, Tileset,
};
use crate::wire::{self, TileList};
use crate::xrpc;

pub(crate) const DEFAULT_SERVER: &str = "https://places.atgeo.org";
pub(crate) const DEFAULT_CACHE_BUDGET_BYTES: u64 = 50 * 1024 * 1024;
const DEFAULT_DATASET: &str = "overturemaps.org";
const DEFAULT_COLLECTION: &str = "place";
pub(crate) const MAX_TILES_IN_FLIGHT: usize = 8;

pub(crate) type Clock = Arc<dyn Fn() -> SystemTime + Send + Sync>;

/// Records of a tile a search used, with the generation of the cache entry they came from.
pub(crate) type Used = (u64, Arc<[TileRecord]>);

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

/// A validated search, ready to run.
pub(crate) struct Plan {
    region: Region,
    query: Option<Query>,
    sort: SortMode,
    /// `urn:atgeo:{dataset}:{collection}:` when the search has a collection.
    prefix: Option<String>,
    pub(crate) lists: Vec<ListStep>,
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

    // --- searchPlaces -------------------------------------------------------------------------

    /// Validates in the contract's order (§3) and plans the tile lists, before any request.
    pub(crate) fn plan(&self, region: Region, options: &SearchOptions) -> Result<Plan, Error> {
        validate(&region)?;
        let query = Query::new(&normalize(options.text.as_deref().unwrap_or("")));
        if options.sort == SortMode::Similarity && query.is_none() {
            return Err(ArgumentError::SimilarityRequiresText.into());
        }
        self.check_server()?;

        let dataset = options.dataset.as_deref().unwrap_or(DEFAULT_DATASET);
        let collection = match (&options.collection, &options.dataset) {
            (Some(c), _) => Some(c.as_str()),
            (None, None) => Some(DEFAULT_COLLECTION),
            (None, Some(_)) => None,
        };
        let now = self.now();
        let lists = query_bboxes(&region)
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
        Ok(Plan {
            region,
            query,
            sort: options.sort,
            prefix: collection.map(|c| format!("urn:atgeo:{dataset}:{c}:")),
            lists,
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
    fn records_of(&self, tile: &CachedTile) -> Result<Arc<[TileRecord]>, DecodingError> {
        if let Some((generation, records)) = lock(&self.parsed).get(&tile.url) {
            if *generation == tile.generation {
                return Ok(records.clone());
            }
        }
        wire::tile_records(&tile.body).map(Arc::from)
    }

    /// Called when a worker reaches the URL, as the reference consults the cache, so that recency is
    /// touched in the same order.
    pub(crate) fn tile_step(&self, url: &str) -> TileStep {
        let cached = lock(&self.tiles).get(url);
        if let Some(tile) = cached {
            return TileStep::Done(
                self.records_of(&tile)
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
        let records: Arc<[TileRecord]> =
            wire::tile_records(&response.body).map_err(|e| fail(TileFailureKind::Decode, None, e.into()))?.into();
        let keys = records.iter().filter_map(|r| record_key(r).map(str::to_owned)).collect();
        let generation = lock(&self.tiles).insert(url, response.body.into(), keys);
        Ok((generation, records))
    }

    /// The slots the workers finished, by index, in URL order. Every index is there once.
    pub(crate) fn in_url_order(done: impl IntoIterator<Item = (usize, Slot)>) -> Vec<Slot> {
        let mut done: Vec<(usize, Slot)> = done.into_iter().collect();
        done.sort_unstable_by_key(|(i, _)| *i);
        debug_assert!(done.iter().enumerate().all(|(n, (i, _))| n == *i));
        done.into_iter().map(|(_, slot)| slot).collect()
    }

    /// Concatenates, dedupes, filters and ranks (§4.5 to §4.9), and keeps this search's parsed tiles.
    pub(crate) fn assemble(&self, plan: Plan, urls: Vec<String>, overview: bool, slots: Vec<Slot>) -> SearchResult {
        debug_assert_eq!(urls.len(), slots.len());
        let mut used: Vec<(String, Used)> = Vec::with_capacity(slots.len());
        let mut tile_failures = Vec::new();
        for (url, slot) in urls.into_iter().zip(slots) {
            match slot {
                Ok(tile) => used.push((url, tile)),
                Err(failure) => tile_failures.push(failure),
            }
        }
        let (fetched, failed) = (used.len(), tile_failures.len());

        let mut seen: HashSet<&str> = HashSet::new();
        let mut kept: Vec<&TileRecord> = Vec::new();
        for (_, (_, records)) in &used {
            for r in records.iter() {
                // dedupe picks a winner before the region filter runs (§4.6).
                let key = record_key(r);
                if key.is_some_and(|k| !seen.insert(k)) {
                    continue;
                }
                if let Some(prefix) = &plan.prefix {
                    let origin = r.value.get("origin").and_then(|o| o.as_str());
                    if !origin.is_some_and(|o| o.starts_with(prefix.as_str())) {
                        continue;
                    }
                }
                if in_region(&locate(&r.value), &plan.region) {
                    kept.push(r);
                }
            }
        }
        let places = rank(kept, center(&plan.region), plan.query.as_ref(), plan.sort);

        *lock(&self.parsed) = used.into_iter().collect();

        SearchResult { places, complete: failed == 0, overview, tiles: TileCounts { fetched, failed }, tile_failures }
    }

    // --- getPlace and the describe calls --------------------------------------------------------

    /// From the cached tile with the lowest URL that holds the origin, or a getPlace request.
    pub(crate) fn place_step(&self, origin: &str) -> Result<Step<PlaceRecord>, Error> {
        self.check_server()?;
        let found = lock(&self.tiles).find(origin);
        if let Some(tile) = found {
            if let Ok(records) = self.records_of(&tile) {
                if let Some(r) = records.iter().find(|r| record_key(r) == Some(origin)) {
                    return Ok(Step::Done(PlaceRecord {
                        origin: origin.to_owned(),
                        place: r.value.clone(),
                        importance: r.importance,
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
        let (record, importance) = wire::place_view(&body)?;
        Ok(PlaceRecord { origin: origin.to_owned(), place: Arc::new(record), importance })
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
