//! Test support shared by the integration tests: the fake server, and a wrapper that runs one test body
//! against both clients.
//!
//! `both! { fn name(kind: Kind) { … } }` becomes a module `name` with the tests `async_client` and, with the
//! `blocking` feature, `blocking_client`. The async client runs on a current-thread Tokio runtime with its
//! clock paused, so that the fake's delays cost no wall time.

#![allow(dead_code, unused_imports, unused_macros)]

pub mod fake;

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use atgeo::{
    Attribution, CollectionKey, Error, PlaceRecord, Region, SearchOptions, SearchResult, ServiceDescription, Tileset,
};

pub use fake::*;
pub use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Async,
    #[cfg(feature = "blocking")]
    Blocking,
}

/// A wall clock a test sets by hand.
#[derive(Clone)]
pub struct TestClock(Arc<Mutex<SystemTime>>);

impl TestClock {
    /// Starts at 2026-01-01T00:00:00Z, as the reference's expiry tests do.
    pub fn new() -> Self {
        TestClock(Arc::new(Mutex::new(SystemTime::UNIX_EPOCH + Duration::from_secs(1_767_225_600))))
    }

    /// Sets the clock to that start plus `secs`.
    pub fn set(&self, secs: u64) {
        *self.0.lock().unwrap() = SystemTime::UNIX_EPOCH + Duration::from_secs(1_767_225_600 + secs);
    }

    pub fn now(&self) -> SystemTime {
        *self.0.lock().unwrap()
    }
}

#[derive(Default, Clone)]
pub struct Config {
    pub server: Option<String>,
    pub budget: Option<u64>,
    pub clock: Option<TestClock>,
}

enum Inner {
    Async(atgeo::AtgeoClient<FakeServer>, tokio::runtime::Runtime),
    #[cfg(feature = "blocking")]
    Blocking(atgeo::blocking::AtgeoClient<FakeServer>),
}

/// Either client, behind methods that block.
pub struct Client(Inner);

pub fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_time().start_paused(true).build().unwrap()
}

macro_rules! configure {
    ($client:expr, $config:expr) => {{
        let mut c = $client;
        if let Some(s) = &$config.server {
            c = c.server(s.clone());
        }
        if let Some(b) = $config.budget {
            c = c.cache_budget_bytes(b);
        }
        if let Some(clock) = $config.clock.clone() {
            c = c.clock(move || clock.now());
        }
        c
    }};
}

impl Kind {
    pub fn client(self, fake: &FakeServer) -> Client {
        self.configured(fake, Config::default())
    }

    pub fn configured(self, fake: &FakeServer, config: Config) -> Client {
        match self {
            Kind::Async => {
                Client(Inner::Async(configure!(atgeo::AtgeoClient::with_transport(fake.clone()), config), runtime()))
            }
            #[cfg(feature = "blocking")]
            Kind::Blocking => {
                Client(Inner::Blocking(configure!(atgeo::blocking::AtgeoClient::with_transport(fake.clone()), config)))
            }
        }
    }

    pub fn with_clock(self, fake: &FakeServer, clock: &TestClock) -> Client {
        self.configured(fake, Config { clock: Some(clock.clone()), ..Config::default() })
    }

    pub fn with_server(self, fake: &FakeServer, server: &str) -> Client {
        self.configured(fake, Config { server: Some(server.to_owned()), ..Config::default() })
    }
}

impl Client {
    pub fn search(&self, region: Region, options: SearchOptions) -> Result<SearchResult, Error> {
        match &self.0 {
            Inner::Async(c, rt) => rt.block_on(c.search_places(region, options)),
            #[cfg(feature = "blocking")]
            Inner::Blocking(c) => c.search_places(region, options),
        }
    }

    /// A search that must succeed.
    pub fn ok(&self, region: Region, options: SearchOptions) -> SearchResult {
        self.search(region, options).unwrap()
    }

    pub fn get_place(&self, origin: &str) -> Result<PlaceRecord, Error> {
        match &self.0 {
            Inner::Async(c, rt) => rt.block_on(c.get_place(origin)),
            #[cfg(feature = "blocking")]
            Inner::Blocking(c) => c.get_place(origin),
        }
    }

    pub fn describe_service(&self) -> Result<ServiceDescription, Error> {
        match &self.0 {
            Inner::Async(c, rt) => rt.block_on(c.describe_service()),
            #[cfg(feature = "blocking")]
            Inner::Blocking(c) => c.describe_service(),
        }
    }

    pub fn describe_tileset(&self, slug: &str) -> Result<Tileset, Error> {
        match &self.0 {
            Inner::Async(c, rt) => rt.block_on(c.describe_tileset(slug)),
            #[cfg(feature = "blocking")]
            Inner::Blocking(c) => c.describe_tileset(slug),
        }
    }
}

/// The server's error code, or a panic naming what came back instead.
pub fn code_of(e: &Error) -> &str {
    match e {
        Error::Atgeo(a) => &a.code,
        other => panic!("expected an AtgeoError, got {other:?}"),
    }
}

/// `{ bbox: [-122.42, 37.77, -122.41, 37.78] }`, the region most reference tests use.
pub const REGION: Region = Region::bbox(-122.42, 37.77, -122.41, 37.78);

/// An attribution entry as the wire spells it: a field that is `None` is a missing key, as the reference's
/// `toStrictEqual` sees it, so a test compares a list with `json!([...])`.
pub fn attribution_json(list: &[Attribution]) -> Value {
    Value::Array(
        list.iter()
            .map(|a| {
                let mut o = serde_json::Map::new();
                o.insert("dataset".into(), json!(a.dataset));
                o.insert("collection".into(), json!(a.collection));
                for (k, v) in [("release", &a.release), ("source", &a.source), ("attribution", &a.attribution)] {
                    if let Some(v) = v {
                        o.insert(k.into(), json!(v));
                    }
                }
                if let Some(l) = &a.licenses {
                    o.insert("licenses".into(), json!(l));
                }
                Value::Object(o)
            })
            .collect(),
    )
}

/// A hit's collection as `{ dataset, collection }` or `null`.
pub fn collection_json(c: &Option<CollectionKey>) -> Value {
    match c {
        Some(k) => json!({ "dataset": k.dataset, "collection": k.collection }),
        None => Value::Null,
    }
}

/// `[{ dataset: "overturemaps.org", collection: "place" }]`, what the default tileset of the fake serves.
pub fn overture_attribution() -> Value {
    json!([{ "dataset": "overturemaps.org", "collection": "place" }])
}

/// A fixture's region: `{ lat, lon, radius }` or `{ bbox: [west, south, east, north] }`.
pub fn region_from_json(v: &Value) -> Region {
    match v.get("bbox") {
        Some(b) => {
            let b: Vec<f64> = b.as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
            Region::bbox(b[0], b[1], b[2], b[3])
        }
        None => Region::circle(v["lat"].as_f64().unwrap(), v["lon"].as_f64().unwrap(), v["radius"].as_f64().unwrap()),
    }
}

/// The dataset and collection the reference's tests spread into every call (`OVERTURE`): searches have no
/// defaults (contract §4.1).
pub fn overture_options() -> SearchOptions {
    SearchOptions::new("overturemaps.org").collection("place")
}

pub fn url(n: &str) -> String {
    format!("https://tiles.example/{n}.json")
}

/// The hits' origins, in order.
pub fn origins(r: &SearchResult) -> Vec<Option<String>> {
    r.places.iter().map(|h| h.origin.clone()).collect()
}

/// The part of each origin after `urn:atgeo:overturemaps.org:place:`, sorted.
pub fn ids_sorted(r: &SearchResult) -> Vec<String> {
    let prefix = place_origin("");
    let mut ids: Vec<String> = r
        .places
        .iter()
        .map(|h| h.origin.clone().unwrap_or_else(|| "null".into()).trim_start_matches(&prefix).to_owned())
        .collect();
    ids.sort();
    ids
}

macro_rules! both {
    ($(#[$meta:meta])* fn $name:ident($kind:ident: Kind) $body:block) => {
        $(#[$meta])*
        mod $name {
            #[allow(unused_imports)]
            use super::*;

            fn body($kind: crate::common::Kind) $body

            #[test]
            fn async_client() {
                body(crate::common::Kind::Async)
            }

            #[cfg(feature = "blocking")]
            #[test]
            fn blocking_client() {
                body(crate::common::Kind::Blocking)
            }
        }
    };
}
