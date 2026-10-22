//! The sans-I/O core both clients drive: it owns the caches, plans a search, reads every response and
//! assembles the result (contract §4, §9). It never makes a request; each client supplies those and the
//! concurrency.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::SystemTime;

use crate::cache::{CachedTile, TileCache, TileRecord, TtlCache};
use crate::error::{ArgumentError, Error};
use crate::locations::{in_region, locate, record_key};
use crate::rank::{cmp_utf16, rank};
use crate::region::{center, query_bboxes, validate};
use crate::text::{Query, normalize};
use crate::transport::{BoxError, HttpResponse};
use crate::types::{
    PlaceRecord, Region, SearchOptions, SearchResult, ServiceDescription, SortMode, TileCounts, Tileset,
};
use crate::wire::{self, TileList};
use crate::xrpc;

pub(crate) const DEFAULT_SERVER: &str = "https://places.atgeo.org";
pub(crate) const DEFAULT_CACHE_BUDGET_BYTES: u64 = 50 * 1024 * 1024;
const DEFAULT_DATASET: &str = "overturemaps.org";
const DEFAULT_COLLECTION: &str = "place";
pub(crate) const MAX_TILES_IN_FLIGHT: usize = 8;

pub(crate) type Clock = Arc<dyn Fn() -> SystemTime + Send + Sync>;

/// Records of a tile a search used, with the generation of the cache entry they came from; `None` for a
/// failed tile.
pub(crate) type Slot = Option<(u64, Arc<[TileRecord]>)>;

/// Parsed tiles by URL, each with the generation of the cache entry it was parsed from.
type ParsedTiles = HashMap<String, (u64, Arc<[TileRecord]>)>;

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
    /// From the cache, without a request.
    Cached(Slot),
    /// A request to this (prepared) URL.
    Fetch(String),
    /// Not an absolute http or https URL without credentials: a failed tile, without a request.
    Failed,
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

    fn records_of(&self, tile: &CachedTile) -> Option<Arc<[TileRecord]>> {
        if let Some((generation, records)) = lock(&self.parsed).get(&tile.url) {
            if *generation == tile.generation {
                return Some(records.clone());
            }
        }
        wire::tile_records(&tile.body).map(Arc::from)
    }

    /// Called when a worker reaches the URL, as the reference consults the cache, so that recency is
    /// touched in the same order.
    pub(crate) fn tile_step(&self, url: &str) -> TileStep {
        let cached = lock(&self.tiles).get(url);
        if let Some(tile) = cached {
            return TileStep::Cached(self.records_of(&tile).map(|r| (tile.generation, r)));
        }
        let prepared = xrpc::prepare_url(url);
        if xrpc::is_http_url(&prepared) { TileStep::Fetch(prepared) } else { TileStep::Failed }
    }

    /// Reads a tile and caches it; any failure is a failed tile, never an error.
    pub(crate) fn accept_tile(&self, url: &str, response: Result<HttpResponse, BoxError>) -> Slot {
        let response = response.ok()?;
        if !(200..300).contains(&response.status) {
            return None;
        }
        let records: Arc<[TileRecord]> = wire::tile_records(&response.body)?.into();
        let keys = records.iter().filter_map(|r| record_key(r).map(str::to_owned)).collect();
        let generation = lock(&self.tiles).insert(url, response.body.into(), keys);
        Some((generation, records))
    }

    /// Concatenates, dedupes, filters and ranks (§4.5 to §4.9), and keeps this search's parsed tiles.
    pub(crate) fn assemble(&self, plan: Plan, urls: Vec<String>, overview: bool, slots: Vec<Slot>) -> SearchResult {
        let fetched = slots.iter().filter(|s| s.is_some()).count();
        let failed = slots.len() - fetched;

        let mut seen: HashSet<&str> = HashSet::new();
        let mut kept: Vec<&TileRecord> = Vec::new();
        for (_, records) in slots.iter().flatten() {
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

        *lock(&self.parsed) = urls.into_iter().zip(slots).filter_map(|(url, slot)| slot.map(|s| (url, s))).collect();

        SearchResult { places, complete: failed == 0, overview, tiles: TileCounts { fetched, failed } }
    }

    // --- getPlace and the describe calls --------------------------------------------------------

    /// From the cached tile with the lowest URL that holds the origin, or a getPlace request.
    pub(crate) fn place_step(&self, origin: &str) -> Result<Step<PlaceRecord>, Error> {
        self.check_server()?;
        let found = lock(&self.tiles).find(origin);
        if let Some(tile) = found {
            if let Some(records) = self.records_of(&tile) {
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
        let TileStep::Cached(Some((g, again))) = e.tile_step("https://t/a") else { panic!() };
        assert_eq!(g, g1);
        assert!(Arc::ptr_eq(&first, &again), "the memo answers while the generation matches");

        let (g2, _) = e.accept_tile("https://t/a", ok(TILE)).unwrap();
        assert_ne!(g1, g2);
        let TileStep::Cached(Some((g, reparsed))) = e.tile_step("https://t/a") else { panic!() };
        assert_eq!(g, g2);
        assert!(!Arc::ptr_eq(&first, &reparsed), "a replaced entry is parsed again");
        assert_eq!(first, reparsed);
    }

    #[test]
    fn a_tile_url_that_is_not_http_fails_without_a_request() {
        let e = Engine::default();
        for url in ["tiles/a.json", "file:///etc/passwd", "ftp://h/a", ""] {
            assert!(matches!(e.tile_step(url), TileStep::Failed), "{url:?}");
        }
        let TileStep::Fetch(u) = e.tile_step("https://h/a b.json") else { panic!() };
        assert_eq!(u, "https://h/a%20b.json");
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
