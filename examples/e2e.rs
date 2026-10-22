//! An end-to-end run against a live garganorn, through both clients: the steps of the reference's
//! `scripts/e2e.ts`, configured by the same environment variables.
//!
//! ```sh
//! ATGEO_SERVER=http://localhost:8000 cargo run --all-features --example e2e
//! ```

use std::env;
use std::process::ExitCode;

use atgeo::{
    CollectionRef, Error, PlaceRecord, Region, SearchOptions, SearchResult, ServiceDescription, SortMode, Tileset,
};

struct Config {
    server: String,
    dataset: String,
    collection: Option<String>,
    region: Region,
    radius: f64,
}

/// The four calls, so that one run of the steps drives either client.
trait Calls {
    fn describe_service(&self) -> Result<ServiceDescription, Error>;
    fn describe_tileset(&self, slug: &str) -> Result<Tileset, Error>;
    fn search(&self, region: Region, options: SearchOptions) -> Result<SearchResult, Error>;
    fn get_place(&self, origin: &str) -> Result<PlaceRecord, Error>;
}

struct Async(atgeo::AtgeoClient, tokio::runtime::Runtime);

impl Calls for Async {
    fn describe_service(&self) -> Result<ServiceDescription, Error> {
        self.1.block_on(self.0.describe_service())
    }
    fn describe_tileset(&self, slug: &str) -> Result<Tileset, Error> {
        self.1.block_on(self.0.describe_tileset(slug))
    }
    fn search(&self, region: Region, options: SearchOptions) -> Result<SearchResult, Error> {
        self.1.block_on(self.0.search_places(region, options))
    }
    fn get_place(&self, origin: &str) -> Result<PlaceRecord, Error> {
        self.1.block_on(self.0.get_place(origin))
    }
}

struct Blocking(atgeo::blocking::AtgeoClient);

impl Calls for Blocking {
    fn describe_service(&self) -> Result<ServiceDescription, Error> {
        self.0.describe_service()
    }
    fn describe_tileset(&self, slug: &str) -> Result<Tileset, Error> {
        self.0.describe_tileset(slug)
    }
    fn search(&self, region: Region, options: SearchOptions) -> Result<SearchResult, Error> {
        self.0.search_places(region, options)
    }
    fn get_place(&self, origin: &str) -> Result<PlaceRecord, Error> {
        self.0.get_place(origin)
    }
}

fn check(ok: bool, what: &str) -> Result<(), String> {
    if ok { Ok(()) } else { Err(what.to_owned()) }
}

fn run(label: &str, c: &Config, client: &dyn Calls, fresh: &dyn Calls) -> Result<(), String> {
    let log = |step: &str, detail: String| println!("ok  [{label}] {step}: {detail}");
    // The error and each of its sources, as anyhow's `{:#}` prints them.
    let fail = |e: Error| {
        let mut message = e.to_string();
        let mut source = std::error::Error::source(&e);
        while let Some(s) = source {
            message = format!("{message}: {s}");
            source = s.source();
        }
        message
    };
    // With a collection set, the tileset must serve that collection, not just the dataset.
    let wanted = |dataset: Option<&str>, collection: Option<&str>| {
        dataset == Some(c.dataset.as_str()) && c.collection.as_deref().is_none_or(|want| collection == Some(want))
    };
    let wanted_ref = |r: &CollectionRef| wanted(r.dataset.as_deref(), r.collection.as_deref());

    let service = client.describe_service().map_err(fail)?;
    let served = service.tilesets.iter().find(|t| t.collections.iter().any(wanted_ref));
    let served = served.ok_or(format!(
        "{}{} not served",
        c.dataset,
        c.collection.as_ref().map_or(String::new(), |c| format!("/{c}"))
    ))?;
    log("describeService", format!("{}, {} tileset(s)", service.did.as_deref().unwrap_or("?"), service.tilesets.len()));

    let slug = served.slug.clone().ok_or("served tileset has no slug")?;
    let tileset = client.describe_tileset(&slug).map_err(fail)?;
    check(
        tileset.collections.iter().any(|i| wanted(i.dataset.as_deref(), i.collection.as_deref())),
        "describeTileset does not list the dataset and collection",
    )?;
    log("describeTileset", format!("{slug}, {} collection(s)", tileset.collections.len()));

    let options = || {
        let o = SearchOptions::new().dataset(c.dataset.clone());
        match &c.collection {
            Some(col) => o.collection(col.clone()),
            None => o,
        }
    };
    let nearby = client.search(c.region, options().sort(SortMode::Distance)).map_err(fail)?;
    check(nearby.complete, &format!("{} tile(s) failed: {:?}", nearby.tiles.failed, nearby.tile_failures))?;
    check(!nearby.places.is_empty(), "no places in region")?;
    let distances: Vec<f64> = nearby.places.iter().map(|h| h.distance).collect();
    check(
        distances.iter().all(|d| *d <= c.radius) && distances.windows(2).all(|w| w[1] >= w[0]),
        "not distance-ordered within radius",
    )?;
    log("searchPlaces (no text)", format!("{} places from {} tile(s)", nearby.places.len(), nearby.tiles.fetched));

    let named = nearby.places.iter().find(|h| h.origin.is_some() && h.name().is_some_and(|n| !n.is_empty()));
    let named = named.ok_or("no named place in region")?;
    let name = named.name().unwrap().to_owned();
    let matched = client.search(c.region, options().text(name.clone())).map_err(fail)?;
    check(
        matched.places.iter().any(|h| h.origin == named.origin && h.score.is_some()),
        "named place missing from text search",
    )?;
    check(matched.places[0].score.is_some_and(|s| s > 0.0), "top text hit has no score")?;
    log(
        "searchPlaces (text)",
        format!("\"{name}\" -> top hit {}, {} places", matched.places[0].name().unwrap_or("?"), matched.places.len()),
    );

    let origin = named.origin.clone().unwrap();
    let record = fresh.get_place(&origin).map_err(fail)?;
    check(record.name() == Some(name.as_str()), "getPlace returned a different name")?;
    log("getPlace", format!("{origin} -> {}", record.name().unwrap_or("?")));
    Ok(())
}

fn main() -> ExitCode {
    let server = env::var("ATGEO_SERVER").unwrap_or_else(|_| "http://localhost:8000".into());
    // "lat,lon,radius" in metres; the default is Manchester, NH, inside the local OSM New Hampshire tileset.
    let region = env::var("ATGEO_REGION").unwrap_or_else(|_| "43.0,-71.45,3000".into());
    let parts: Vec<f64> =
        region.split(',').map(|p| p.trim().parse().expect("ATGEO_REGION is lat,lon,radius")).collect();
    let [lat, lon, radius] = parts[..] else { panic!("ATGEO_REGION is lat,lon,radius") };
    let c = Config {
        server: server.clone(),
        dataset: env::var("ATGEO_DATASET").unwrap_or_else(|_| "openstreetmap.org".into()),
        collection: env::var("ATGEO_COLLECTION").ok().filter(|s| !s.is_empty()),
        region: Region::circle(lat, lon, radius),
        radius,
    };

    let runtime = || tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let async_client = || Async(atgeo::AtgeoClient::new().server(c.server.clone()), runtime());
    let blocking_client = || Blocking(atgeo::blocking::AtgeoClient::new().server(c.server.clone()));

    let results = [
        ("async", run("async", &c, &async_client(), &async_client())),
        ("blocking", run("blocking", &c, &blocking_client(), &blocking_client())),
    ];
    let mut code = ExitCode::SUCCESS;
    for (label, result) in results {
        if let Err(e) = result {
            eprintln!("FAIL [{label}] {e}");
            code = ExitCode::FAILURE;
        }
    }
    code
}
