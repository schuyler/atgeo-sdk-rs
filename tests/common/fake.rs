//! A fake garganorn, a port of the reference's `test/helpers/fakeServer.ts`, behind both transport traits.
//!
//! There is no Rust validator for atproto lexicons, so it checks what contract §13 asks for and what
//! `fakeServer.test.ts` asserts: a string slug on every tileset, an object value with a string name on every
//! record and an integral importance when present, at construction; known and required parameter names, the
//! bbox pattern and a uri with a scheme, on every request; and the required fields of its own responses.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use atgeo::{BoxError, HttpResponse};
use serde_json::{Map, Value, json};

pub const DEFAULT_SERVER: &str = "https://places.atgeo.org";
const PUBLISHED_AT: &str = "2026-01-01T00:00:00Z";

pub fn place_origin(id: &str) -> String {
    format!("urn:atgeo:overturemaps.org:place:{id}")
}

/// A geo location with six decimals, as the reference's `point()` writes it.
pub fn point(lat: f64, lon: f64) -> Value {
    json!({ "$type": "community.lexicon.location.geo", "latitude": format!("{lat:.6}"), "longitude": format!("{lon:.6}") })
}

pub fn bbox_location(north: f64, south: f64, east: f64, west: f64) -> Value {
    json!({
        "$type": "community.lexicon.location.bbox",
        "north": format!("{north:.6}"), "south": format!("{south:.6}"),
        "east": format!("{east:.6}"), "west": format!("{west:.6}"),
    })
}

/// A place value. `extra` is merged over it; an `origin` of `null` in `extra` removes the origin.
pub fn place(id: &str, name: &str, locations: Vec<Value>, extra: Value) -> Value {
    let mut v = json!({ "$type": "org.atgeo.place", "origin": place_origin(id), "name": name, "locations": locations });
    let o = v.as_object_mut().unwrap();
    if let Value::Object(extra) = extra {
        for (k, x) in extra {
            if x.is_null() && k == "origin" {
                o.remove(&k);
            } else {
                o.insert(k, x);
            }
        }
    }
    v
}

/// A place with no extra fields.
pub fn simple(id: &str, name: &str, locations: Vec<Value>) -> Value {
    place(id, name, locations, json!({}))
}

/// A tile record. Importance and uri are left out when `None`.
pub fn record(value: Value, importance: Option<i64>, uri: Option<&str>) -> Value {
    let mut r = json!({ "value": value });
    if let Some(i) = importance {
        r["importance"] = json!(i);
    }
    if let Some(u) = uri {
        r["uri"] = json!(u);
    }
    r
}

pub fn rec(value: Value) -> Value {
    record(value, None, None)
}

#[derive(Clone, Debug)]
pub struct FakeTile {
    pub url: String,
    /// Tile records, `{ value, uri?, importance? }`.
    pub records: Vec<Value>,
}

pub fn tile(url: &str, records: Vec<Value>) -> FakeTile {
    FakeTile { url: url.to_owned(), records }
}

#[derive(Clone, Debug)]
pub struct FakeTileset {
    pub slug: Option<String>,
    /// Collection descriptors: `{ dataset, collection, … }`.
    pub collections: Vec<Value>,
    pub references: Option<Vec<Value>>,
    pub tiles: Vec<FakeTile>,
}

/// The default tileset, carrying overturemaps.org / place.
pub fn overture(tiles: Vec<FakeTile>) -> FakeTileset {
    FakeTileset {
        slug: Some("overture-place".into()),
        collections: vec![json!({ "dataset": "overturemaps.org", "collection": "place" })],
        references: None,
        tiles,
    }
}

/// What a `tiles_for` override answers getTiles with.
pub enum TilesFor {
    Urls(Vec<String>),
    /// `{ tiles, overview? }`
    Answer {
        tiles: Vec<String>,
        overview: Option<bool>,
    },
    /// A 400 with this error and message.
    Error(&'static str, &'static str),
}

type TilesForFn = dyn Fn(&str) -> TilesFor + Send + Sync;

#[derive(Default)]
struct State {
    requests: Vec<String>,
    offline: bool,
    network_errors: HashSet<String>,
    raw: HashMap<String, (u16, Vec<u8>)>,
    answer_everything: Option<(u16, Vec<u8>)>,
    delays: HashMap<String, Duration>,
    delays_containing: Vec<(String, Duration)>,
}

struct Inner {
    server: String,
    tilesets: Vec<FakeTileset>,
    tiles_for: Option<Box<TilesForFn>>,
    fail_tiles: HashSet<String>,
    state: Mutex<State>,
    in_flight: AtomicUsize,
    peak: AtomicUsize,
    dropped: AtomicUsize,
}

/// A cheap handle: clones share one server, so that one fake can serve two clients and be inspected after.
#[derive(Clone)]
pub struct FakeServer(Arc<Inner>);

#[derive(Default)]
pub struct Builder {
    server: Option<String>,
    tilesets: Option<Vec<FakeTileset>>,
    tiles_for: Option<Box<TilesForFn>>,
    fail_tiles: Vec<String>,
}

impl Builder {
    pub fn server(mut self, server: &str) -> Self {
        self.server = Some(server.to_owned());
        self
    }
    pub fn tilesets(mut self, tilesets: Vec<FakeTileset>) -> Self {
        self.tilesets = Some(tilesets);
        self
    }
    /// Replaces the whole getTiles answer once dataset and collection selection has passed.
    pub fn tiles_for(mut self, f: impl Fn(&str) -> TilesFor + Send + Sync + 'static) -> Self {
        self.tiles_for = Some(Box::new(f));
        self
    }
    /// Each of these tiles answers 500.
    pub fn fail_tiles(mut self, urls: &[&str]) -> Self {
        self.fail_tiles = urls.iter().map(|u| u.to_string()).collect();
        self
    }

    pub fn try_build(self) -> Result<FakeServer, String> {
        let tilesets = self.tilesets.unwrap_or_else(|| vec![overture(vec![])]);
        for t in &tilesets {
            if t.slug.is_none() {
                return Err("tileset must have property \"slug\"".into());
            }
            for tile in &t.tiles {
                for r in &tile.records {
                    let r = r.as_object().ok_or("tile record must be an object")?;
                    let Some(value) = r.get("value") else {
                        return Err("tile record must have property \"value\"".into());
                    };
                    let value = value.as_object().ok_or("record value must be an object")?;
                    if !value.get("name").is_some_and(Value::is_string) {
                        return Err("place record must have property \"name\" (a string)".into());
                    }
                    if let Some(i) = r.get("importance") {
                        if !(i.is_i64() || i.is_u64()) {
                            return Err("importance must be an integer".into());
                        }
                    }
                }
            }
        }
        Ok(FakeServer(Arc::new(Inner {
            server: self.server.unwrap_or_else(|| DEFAULT_SERVER.to_owned()),
            tilesets,
            tiles_for: self.tiles_for,
            fail_tiles: self.fail_tiles.into_iter().collect(),
            state: Mutex::new(State::default()),
            in_flight: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            dropped: AtomicUsize::new(0),
        })))
    }

    pub fn build(self) -> FakeServer {
        self.try_build().unwrap()
    }
}

/// Percent-decodes a query component, `+` as a space, as `URLSearchParams` does.
pub fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The query parameters of a URL, decoded, in order.
pub fn params(url: &str) -> Vec<(String, String)> {
    let Some((_, q)) = url.split_once('?') else { return vec![] };
    let q = q.split('#').next().unwrap_or("");
    q.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (decode(k), decode(v))
        })
        .collect()
}

pub fn param(url: &str, name: &str) -> Option<String> {
    params(url).into_iter().find(|(k, _)| k == name).map(|(_, v)| v)
}

/// The URL without its query: origin and path.
pub fn path_of(url: &str) -> &str {
    url.split(['?', '#']).next().unwrap_or(url)
}

/// The last path segment, the NSID of an XRPC call.
pub fn nsid(url: &str) -> &str {
    path_of(url).rsplit('/').next().unwrap_or("")
}

fn json_response(status: u16, body: &Value) -> HttpResponse {
    HttpResponse::new(status, serde_json::to_vec(body).unwrap())
}

fn xrpc_error(error: &str, message: &str, status: u16) -> HttpResponse {
    json_response(status, &json!({ "error": error, "message": message }))
}

/// `^-?\d+\.\d\d,-?\d+\.\d\d,-?\d+\.\d\d,-?\d+\.\d\d$`, by hand.
pub fn bbox_pattern(s: &str) -> bool {
    let parts: Vec<&str> = s.split(',').collect();
    parts.len() == 4
        && parts.iter().all(|p| {
            let p = p.strip_prefix('-').unwrap_or(p);
            let Some((int, frac)) = p.split_once('.') else { return false };
            !int.is_empty()
                && int.bytes().all(|b| b.is_ascii_digit())
                && frac.len() == 2
                && frac.bytes().all(|b| b.is_ascii_digit())
        })
}

/// A string with a scheme, a colon and no spaces, which is what a lexicon `uri` format asks.
fn is_uri(s: &str) -> bool {
    let Some((scheme, _)) = s.split_once(':') else { return false };
    !scheme.is_empty()
        && scheme.as_bytes()[0].is_ascii_alphabetic()
        && scheme.bytes().all(|b| b.is_ascii_alphanumeric() || b"+-.".contains(&b))
        && !s.chars().any(char::is_whitespace)
}

struct InFlight<'a> {
    inner: &'a Inner,
    finished: bool,
}

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.inner.in_flight.fetch_sub(1, Ordering::SeqCst);
        if !self.finished {
            self.inner.dropped.fetch_add(1, Ordering::SeqCst);
        }
    }
}

impl FakeServer {
    pub fn builder() -> Builder {
        Builder::default()
    }

    /// The default: one tileset, "overture-place", with no tiles.
    pub fn new() -> FakeServer {
        Builder::default().build()
    }

    pub fn with_tilesets(tilesets: Vec<FakeTileset>) -> FakeServer {
        Builder::default().tilesets(tilesets).build()
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Every request URL, in the order the requests were made.
    pub fn requests(&self) -> Vec<String> {
        self.state().requests.clone()
    }

    pub fn request_count(&self) -> usize {
        self.state().requests.len()
    }

    /// The getTiles requests.
    pub fn tile_list_requests(&self) -> Vec<String> {
        self.requests().into_iter().filter(|r| r.contains("org.atgeo.getTiles")).collect()
    }

    pub fn count_of(&self, url: &str) -> usize {
        self.requests().iter().filter(|r| *r == url).count()
    }

    /// After this, every request fails as a network failure.
    pub fn go_offline(&self) {
        self.state().offline = true;
    }

    /// Requests for this URL fail as a network failure, like the reference's rejecting fetch.
    pub fn network_error(&self, url: &str) {
        self.state().network_errors.insert(url.to_owned());
    }

    /// Answers this URL with exactly this status and body, bypassing routing and validation.
    pub fn raw(&self, url: &str, status: u16, body: impl Into<Vec<u8>>) {
        self.state().raw.insert(url.to_owned(), (status, body.into()));
    }

    /// Answers every request with this status and body.
    pub fn answer_everything(&self, status: u16, body: impl Into<Vec<u8>>) {
        self.state().answer_everything = Some((status, body.into()));
    }

    /// Delays the answer to this URL.
    pub fn delay(&self, url: &str, d: Duration) {
        self.state().delays.insert(url.to_owned(), d);
    }

    /// Delays every request whose URL contains `part` and has no delay of its own, such as one half of a
    /// crossing region by `bbox=170.00`, whatever the client's encoding of the rest.
    pub fn delay_containing(&self, part: &str, d: Duration) {
        self.state().delays_containing.push((part.to_owned(), d));
    }

    /// Delays every tile URL that has no delay of its own.
    pub fn delay_all(&self, d: Duration) {
        self.state().delays.insert(String::new(), d);
    }

    pub fn peak_in_flight(&self) -> usize {
        self.0.peak.load(Ordering::SeqCst)
    }

    pub fn in_flight(&self) -> usize {
        self.0.in_flight.load(Ordering::SeqCst)
    }

    /// Requests whose future was dropped before it finished.
    pub fn dropped(&self) -> usize {
        self.0.dropped.load(Ordering::SeqCst)
    }

    /// Records the request and fails it if the network is down; otherwise counts it in flight.
    fn begin(&self, url: &str) -> Result<(InFlight<'_>, Option<Duration>), BoxError> {
        let delay = {
            let mut s = self.state();
            s.requests.push(url.to_owned());
            if s.offline || s.network_errors.contains(url) {
                return Err(Box::new(std::io::Error::other("network disabled")));
            }
            s.delays
                .get(url)
                .or_else(|| s.delays_containing.iter().find(|(part, _)| url.contains(part.as_str())).map(|(_, d)| d))
                .or_else(|| if url.contains("/xrpc/") { None } else { s.delays.get("") })
                .copied()
        };
        let now = self.0.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.0.peak.fetch_max(now, Ordering::SeqCst);
        Ok((InFlight { inner: &self.0, finished: false }, delay))
    }

    fn attribution_of(c: &Value) -> Value {
        let keys = ["dataset", "collection", "release", "source", "licenses", "attribution"];
        Value::Object(keys.iter().filter_map(|k| c.get(*k).map(|v| (k.to_string(), v.clone()))).collect())
    }

    fn tile_body(tileset: &FakeTileset, records: &[Value]) -> Value {
        let mut attribution: Vec<Value> = tileset.collections.iter().map(Self::attribution_of).collect();
        attribution.extend(tileset.references.clone().unwrap_or_default());
        json!({ "publishedAt": PUBLISHED_AT, "attribution": attribution, "records": records })
    }

    fn descriptor_of(t: &FakeTileset) -> Value {
        let mut d = json!({ "slug": t.slug, "collections": t.collections });
        if let Some(r) = &t.references {
            d["references"] = json!(r);
        }
        d
    }

    fn did(&self) -> String {
        let rest = self.0.server.split_once("://").map_or(self.0.server.as_str(), |(_, r)| r);
        let host = rest.split(['/', ':']).next().unwrap_or("");
        format!("did:web:{host}")
    }

    fn get_tiles(&self, p: &HashMap<String, String>) -> HttpResponse {
        let dataset = &p["dataset"];
        let collection = p.get("collection");
        let has = |c: &Value, ds: &str| c.get("dataset").and_then(Value::as_str) == Some(ds);
        let with_dataset: Vec<&FakeTileset> =
            self.0.tilesets.iter().filter(|t| t.collections.iter().any(|c| has(c, dataset))).collect();
        if with_dataset.is_empty() {
            return xrpc_error("DatasetNotFound", &format!("Dataset {dataset} not found"), 400);
        }
        let selected: Vec<&&FakeTileset> = with_dataset
            .iter()
            .filter(|t| {
                t.collections.iter().any(|c| {
                    has(c, dataset)
                        && collection.is_none_or(|want| c.get("collection").and_then(Value::as_str) == Some(want))
                })
            })
            .collect();
        if selected.is_empty() {
            let c = collection.map_or("undefined", String::as_str);
            return xrpc_error("CollectionNotFound", &format!("Collection {c} not found"), 400);
        }
        let mut urls: Vec<String> = selected.iter().flat_map(|t| t.tiles.iter().map(|tile| tile.url.clone())).collect();
        urls.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
        let answer = match &self.0.tiles_for {
            None => json!({ "tiles": urls }),
            Some(f) => match f(&p["bbox"]) {
                TilesFor::Urls(tiles) => json!({ "tiles": tiles }),
                TilesFor::Answer { tiles, overview: None } => json!({ "tiles": tiles }),
                TilesFor::Answer { tiles, overview: Some(o) } => json!({ "tiles": tiles, "overview": o }),
                TilesFor::Error(code, message) => return xrpc_error(code, message, 400),
            },
        };
        assert!(answer["tiles"].is_array(), "getTiles output must have tiles");
        json_response(200, &answer)
    }

    fn get_place(&self, origin: &str) -> HttpResponse {
        let parts: Vec<&str> = origin.split(':').collect();
        let (dataset, collection) = (parts.get(2).copied(), parts.get(3).copied());
        let rest = parts.get(4..).map(|r| r.join(":")).unwrap_or_default();
        for t in &self.0.tilesets {
            let carries = t.collections.iter().any(|c| {
                c.get("dataset").and_then(Value::as_str) == dataset
                    && c.get("collection").and_then(Value::as_str) == collection
            });
            if !carries {
                continue;
            }
            for tile in &t.tiles {
                if let Some(found) = tile.records.iter().find(|r| r["value"]["origin"].as_str() == Some(origin)) {
                    let mut view = json!({
                        "uri": format!("{}/{}/{}/{rest}", self.0.server, dataset.unwrap(), collection.unwrap()),
                        "record": found["value"],
                    });
                    if let Some(i) = found.get("importance") {
                        view["importance"] = i.clone();
                    }
                    return json_response(200, &view);
                }
            }
        }
        xrpc_error("PlaceNotFound", &format!("Place {origin} not found"), 400)
    }

    fn xrpc(&self, url: &str) -> HttpResponse {
        let nsid = nsid(url);
        let (allowed, required): (&[&str], &[&str]) = match nsid {
            "org.atgeo.getTiles" => (&["dataset", "collection", "bbox"], &["dataset", "bbox"]),
            "org.atgeo.getPlace" => (&["uri"], &["uri"]),
            "org.atgeo.describeService" => (&[], &[]),
            "org.atgeo.describeTileset" => (&["slug"], &["slug"]),
            _ => return xrpc_error("MethodNotImplemented", &format!("Method {nsid} not implemented"), 501),
        };
        let ps = params(url);
        let mut p = HashMap::new();
        for (k, v) in ps {
            if !allowed.contains(&k.as_str()) {
                return xrpc_error("InvalidRequest", &format!("Unknown parameter {k}"), 400);
            }
            if p.insert(k.clone(), v).is_some() {
                return xrpc_error("InvalidRequest", &format!("Repeated parameter {k}"), 400);
            }
        }
        if let Some(missing) = required.iter().find(|r| !p.contains_key(**r)) {
            return xrpc_error("InvalidRequest", &format!("Params must have the property \"{missing}\""), 400);
        }
        let too_long = |k: &str, max: usize| p.get(k).is_some_and(|v: &String| v.chars().count() > max);
        if too_long("dataset", 253) || too_long("collection", 64) || too_long("bbox", 128) || too_long("slug", 64) {
            return xrpc_error("InvalidRequest", "Parameter too long", 400);
        }
        if p.get("bbox").is_some_and(|b| !bbox_pattern(b)) {
            return xrpc_error("InvalidRequest", "bbox must match the two-decimal pattern", 400);
        }
        if p.get("uri").is_some_and(|u| !is_uri(u)) {
            return xrpc_error("InvalidRequest", "uri must be a uri", 400);
        }
        match nsid {
            "org.atgeo.getTiles" => self.get_tiles(&p),
            "org.atgeo.getPlace" => self.get_place(&p["uri"]),
            "org.atgeo.describeService" => {
                let tilesets: Vec<Value> = self
                    .0
                    .tilesets
                    .iter()
                    .map(|t| {
                        let cs: Vec<Value> = t
                            .collections
                            .iter()
                            .map(|c| json!({ "dataset": c["dataset"], "collection": c["collection"] }))
                            .collect();
                        json!({ "slug": t.slug, "collections": cs })
                    })
                    .collect();
                json_response(200, &json!({ "did": self.did(), "tilesets": tilesets }))
            }
            _ => match self.0.tilesets.iter().find(|t| t.slug.as_deref() == Some(p["slug"].as_str())) {
                Some(t) => json_response(200, &Self::descriptor_of(t)),
                None => xrpc_error("TilesetNotFound", &format!("Tileset {} not found", p["slug"]), 400),
            },
        }
    }

    /// Routes a request the way the reference's fake does.
    pub fn handle(&self, url: &str) -> HttpResponse {
        {
            let s = self.state();
            if let Some((status, body)) = s.raw.get(url).or(s.answer_everything.as_ref()) {
                return HttpResponse::new(*status, body.clone());
            }
        }
        if url.starts_with(&format!("{}/xrpc/", self.0.server)) {
            return self.xrpc(url);
        }
        for t in &self.0.tilesets {
            if let Some(tile) = t.tiles.iter().find(|tile| tile.url == url) {
                if self.0.fail_tiles.contains(url) {
                    return HttpResponse::new(500, b"upstream error".to_vec());
                }
                return json_response(200, &Self::tile_body(t, &tile.records));
            }
        }
        HttpResponse::new(404, b"Not found".to_vec())
    }
}

impl std::fmt::Debug for FakeServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeServer").field("server", &self.0.server).finish_non_exhaustive()
    }
}

impl Default for FakeServer {
    fn default() -> Self {
        FakeServer::new()
    }
}

impl atgeo::HttpTransport for FakeServer {
    async fn get(&self, url: &str) -> Result<HttpResponse, BoxError> {
        let (mut guard, delay) = self.begin(url)?;
        if let Some(d) = delay {
            tokio::time::sleep(d).await;
        }
        let response = self.handle(url);
        guard.finished = true;
        Ok(response)
    }
}

#[cfg(feature = "blocking")]
impl atgeo::blocking::HttpTransport for FakeServer {
    fn get(&self, url: &str) -> Result<HttpResponse, BoxError> {
        let (mut guard, delay) = self.begin(url)?;
        if let Some(d) = delay {
            std::thread::sleep(d);
        }
        let response = self.handle(url);
        guard.finished = true;
        Ok(response)
    }
}

/// A place value as a `Map`, for comparing with a hit's `place`.
pub fn map(v: &Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}
