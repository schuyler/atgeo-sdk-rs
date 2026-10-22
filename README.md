# atgeo

A Rust client for the atgeo gazetteer served by [garganorn](https://github.com/schuyler/garganorn), which searches places without telling the server what you searched for.

It's a port of [@atgeo/client](https://github.com/schuyler/atgeo-sdk-js), the TypeScript reference, and behaves the same way: same requests on the wire, same filtering, scoring and ranking. There's an async client that runs on any executor, and a blocking one for code without an executor. Both have the same methods and return the same results.

## Synopsis

```rust,no_run
use atgeo::{AtgeoClient, Region, SearchOptions};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), atgeo::Error> {
    let atgeo = AtgeoClient::new();

    let result = atgeo
        .search_places(
            Region::circle(39.9526, -75.1652, 1000.0), // radius in meters
            SearchOptions::new().text("Starbux"),
        )
        .await?;

    for hit in &result.places {
        println!("{} {:.0} {:?} {:?}", hit.name().unwrap_or("-"), hit.distance, hit.score, hit.origin);
    }
    Ok(())
}
```

Misspellings are fine; matching is fuzzy, and that finds the Starbucks around Philadelphia's City Hall. The public server at places.atgeo.org has a limited dataset for now, mostly the east coast, so a search elsewhere may come back empty. If `result.complete` is `false`, some tiles failed to load and the results are partial. If `result.overview` is `true`, the region was too big to search in full and you got only its most important places; see [Overview answers](#overview-answers).

The blocking client is the same without the `.await`:

```rust,no_run
use atgeo::{Region, SearchOptions, blocking::AtgeoClient};

fn main() -> Result<(), atgeo::Error> {
    let atgeo = AtgeoClient::new();
    let result = atgeo.search_places(Region::circle(39.9526, -75.1652, 1000.0), SearchOptions::new().text("Starbux"))?;
    println!("{} places", result.places.len());
    Ok(())
}
```

## Installation

This isn't on crates.io yet; it stays unpublished until beta testing ends. Depend on the repository, pinned to a commit:

```toml
[dependencies]
atgeo = { git = "https://github.com/schuyler/atgeo-sdk-rs", rev = "<commit-sha>" }
```

Pin a full commit SHA rather than a branch: the API is still changing, and breaking changes land on `main`.

Features:

| Feature | Default | What it adds |
|---|---|---|
| `reqwest` | yes | `ReqwestTransport`, the async client's default transport. It needs a Tokio runtime. |
| `blocking` | no | `blocking::AtgeoClient` and `blocking::HttpTransport`, for a transport of your own |
| `ureq` | no | `blocking::UreqTransport`, the blocking client's default transport; turns on `blocking` |
| `serde` | no | `Serialize` and `Deserialize` on the data types, for apps that persist what they fetched |

So `atgeo = { git = "…", rev = "…", default-features = false, features = ["ureq"] }` is the blocking client with no async stack at all. With no features, the core is 13 crates: serde_json, futures-util and two Unicode tables, plus their dependencies. TLS is rustls in both transports, with the operating system's root certificates; nothing pulls in OpenSSL.

It needs Rust 1.85 or later (edition 2024). On 1.85 to 1.87, a current lockfile can pick an `icu_*` 2.4 crate (through reqwest → url → idna), which needs 1.88. Cargo's MSRV-aware resolver avoids it; an edition-2021 app gets that with `resolver = "3"` in its `Cargo.toml`, or else `cargo update -p icu_provider --precise 2.1.1`.

## Constructing a client

```rust,no_run
use atgeo::{AtgeoClient, ReqwestTransport};

let atgeo = AtgeoClient::with_transport(ReqwestTransport::new()) // default: ReqwestTransport::new()
    .server("http://localhost:8000")                           // default: https://places.atgeo.org
    .cache_budget_bytes(10 * 1024 * 1024);                     // default: 50 MiB
```

Point `server` at your own garganorn to run against your own data; see [Server requirements](#server-requirements). The budget caps the in-memory tile cache (see [Caching](#caching)). Configuration is by value, on a client nobody shares yet; there's also `clock`, which replaces the wall clock the caches expire by, for tests.

A client is `Send + Sync` whenever its transport is. Make one per server and share it, by reference or in an `Arc`, for the life of the process: every call shares its caches.

## API

The async client's methods are `async`; the blocking client's are the same without it. All return `Result<_, atgeo::Error>`.

### `search_places(region, options) -> SearchResult`

`SearchOptions::new()` and its setters:

| Setter | Meaning |
|---|---|
| `.text(…)` | Optional search text. Matched fuzzily against each place's `name` and its `variants[].name`, after lowercasing and stripping accents and extra whitespace. Each name is scored against the whole query and against the query's first 12 words one at a time; words past the 12th count only in the whole-query comparison. |
| `.dataset(…)` | Optional. The dataset to search, such as `openstreetmap.org`. Defaults to `overturemaps.org`. |
| `.collection(…)` | Optional. Restricts the search to one collection of the dataset, such as `way`. See below for the defaults. |
| `.sort(…)` | `SortMode::Fused` (default), `SortMode::Distance`, or `SortMode::Similarity`. |

Regions:

```rust,ignore
Region::circle(39.9526, -75.1652, 1000.0)          // lat, lon, radius in meters
Region::bbox(-75.18, 39.94, -75.14, 39.96)          // min_lon, min_lat, max_lon, max_lat, in degrees
```

A `Region` isn't checked when you build it; `search_places` rejects an invalid one before making any request.

Datasets and collections work like this:

- With neither, you search `overturemaps.org` / `place`.
- With only `collection`, you search that collection of `overturemaps.org`.
- With only `dataset`, you search the whole dataset, every collection together. `.dataset("openstreetmap.org")` returns nodes, ways and relations in one list. Add `.collection("way")` to get only ways.
- With both, you search that collection.

A collection-filtered search, including the default `overturemaps.org` / `place`, keeps only records whose `origin` starts with `urn:atgeo:{dataset}:{collection}:`. A record with no `origin` matches no collection filter, so such a search never returns one. A dataset-only search has no filter and returns those records too.

A bbox with `min_lon > max_lon` crosses the antimeridian. A place with a point location is kept if the point falls inside the region; a place with only a bounding box is kept if that box touches the region.

The result:

```rust,ignore
pub struct SearchResult {
    pub places: Vec<SearchHit>,
    pub complete: bool,        // false if any tile failed to load
    pub overview: bool,        // true if the server answered with its coarse overview; see below
    pub tiles: TileCounts,     // .fetched and .failed
}

pub struct SearchHit {
    pub origin: Option<String>,  // the place's identity: place.origin, else the tile record's uri, else None
    pub place: Place,            // the record value, exactly as served
    pub importance: Option<i64>, // an integer relative to its tileset; None if it has none
    pub distance: f64,           // meters from the region's center to the nearest point of the place; 0 if the place contains the center
    pub score: Option<f64>,      // 0 to 1 text similarity; None if no text was given
}
```

`Place` is `Arc<serde_json::Map<String, serde_json::Value>>`: the gazetteer's value untouched, every key as served, and nothing decoded into a Rust type. It's shared rather than copied, because the client keeps the latest search's tiles parsed and a hit points into them; `(*hit.place).clone()` gives you a copy of your own. `hit.name()` reads `name` when it's a string, and `hit.location()` returns the point and box the client read from `locations`, the same ones `distance` was measured to.

The result types are `#[non_exhaustive]`, so fields can be added without a major release. The price is that you can't build one with a struct literal.

When there's text, places scoring below 0.6 are dropped.

Sort modes:

- `Fused` orders by text similarity (best first), breaking near-ties by the hit's `importance`, then by distance. Without text, that means importance, then distance. This is the one you usually want.
- `Distance` orders nearest first, then smaller place first (so the most specific of the places containing the center leads), then by importance, then similarity. Text still filters out poor matches.
- `Similarity` orders by text similarity, then nearest first. It's an error without text.

Similarity is compared to three decimal places, so scores that differ only beyond that count as ties. Remaining ties break on `origin` (a hit with no `origin` sorts last), so ordering is deterministic.

#### Overview answers

When a region is too large for the server's regular tile budget and the collection has an overview, the server answers with that instead: a coarse tileset holding only the most important places. The search still runs normally over those tiles, and `overview` is `true`. The list is not everything in the region.

`overview` and `complete` are independent: `complete` says whether every tile the server named loaded, and `overview` says whether the server named a thin set of tiles to begin with. If `overview` is `true`, don't treat the results as exhaustive. Show something like "zoom in for more" and search a smaller region to get the full set.

#### Places that appear in more than one tile

A place can be stored in several tiles. The client keeps one hit per origin (or per `uri`, for a record with no origin); when the copies differ, the copy from the lowest tile URL wins. Garganorn's copies are byte-identical, so this only matters for other producers' tilesets. The [JS README](https://github.com/schuyler/atgeo-sdk-js#places-that-appear-in-more-than-one-tile) has the long version.

### `get_place(origin) -> PlaceRecord`

Returns a `PlaceRecord` with `origin`, `place` and `importance`, as in a hit, and the same `name()` and `location()`. Pass the `origin` of a hit. If a cached tile holds the place, no request is made; otherwise the client asks the server (`org.atgeo.getPlace`) directly.

### `describe_service() -> ServiceDescription`

Returns the server's `did` and its `tilesets`, each a `slug` and its `collections` (`dataset`, `collection`). Cached for an hour.

### `describe_tileset(slug) -> Tileset`

Returns the tileset's descriptor: `slug`, `collections` (each with `dataset`, `collection` and optional `release`, `source`, `licenses`, `attribution`, `record_count`, `location_formats`, `containment_types`, `categories` and `attributes`), and optional `published_at`, `extent` and `references`. Not cached.

Every field of the describe types is an `Option` except the lists the lexicon requires, which are `Vec`s. A field the server drops or retypes comes back `None` (or empty) rather than failing the call.

### Errors

`atgeo::Error` is one enum:

- `Error::Atgeo(AtgeoError)`: the server answered with a non-2xx status. `AtgeoError` has a `code`, a `message` and the HTTP `status`, which tells a request the server refused (4xx) from one it failed to answer (5xx) without matching on codes. The code and message come from the server's JSON error body; if there isn't one, the code is `HttpError`. Apart from `HttpError`, every code comes from the server: for example `PlaceNotFound`, `TilesetNotFound`, `DatasetNotFound`, `CollectionNotFound`, `BboxTooLarge`, or `InvalidRequest`. The list is open, so report codes rather than matching on them.
- `Error::Decoding(DecodingError)`: a 2xx body that isn't JSON, or has nothing usable in it (no `tiles` list, no `record`, a describe body that isn't an object).
- `Error::Transport(BoxError)`: no response arrived. It holds the transport's own error, so `downcast_ref::<reqwest::Error>()` reaches it.
- `Error::InvalidArgument(ArgumentError)`: an invalid region (non-finite or out-of-range coordinates, `min_lat > max_lat`, a negative radius), `Similarity` without text, or a server that isn't an http or https URL. All of these are found before any request.

A failed `getTiles` request fails the whole search. A failed tile does not: that tile is skipped, `tiles.failed` counts it, and `complete` is `false`.

## Transports

The async client is generic over `atgeo::HttpTransport`, a trait with one method:

```rust,no_run
use atgeo::{BoxError, HttpResponse, HttpTransport};

struct MyTransport;

impl HttpTransport for MyTransport {
    async fn get(&self, url: &str) -> Result<HttpResponse, BoxError> {
        // GET the URL exactly as given. Return a response for every status, with the body decoded;
        // fail only when no response arrived. Don't cache, and don't send a Referer.
        Ok(HttpResponse::new(200, b"{}".to_vec()))
    }
}

let atgeo = atgeo::AtgeoClient::with_transport(MyTransport);
```

The future must be `Send`, which the compiler checks at your `impl`, so the client's futures can be spawned on a multi-threaded runtime. The blocking client takes `atgeo::blocking::HttpTransport`, the same method without `async`; it's called from several threads at once. Both traits are implemented for `&T` and `Arc<T>`, so one transport can serve two clients.

`ReqwestTransport` needs Tokio: reqwest spawns a task per connection. On another runtime, supply a transport built on your own HTTP stack. For HTTP/2, a proxy or your own TLS settings, build a `reqwest::Client` from `ReqwestTransport::client_builder()` and pass it to `ReqwestTransport::with_client`. Starting from `reqwest::Client::builder()` instead would bring back the `Referer` header on redirects, which carries the snapped bbox to whatever host the redirect names. `UreqTransport::config_builder()` does the same for a `ureq::Agent`.

Both default transports cap a response body at 32 MiB, decoded, so a small gzip body can't decode to gigabytes (`max_body_bytes` changes it). They time out a connection after 10 s and a stalled read after 30 s. `UreqTransport` also stops a whole request, redirects included, after 120 s, because a blocking call can't be cancelled any other way. Both honour the `HTTPS_PROXY` family of environment variables, though not identically (ureq uses the first one set for every scheme).

## Concurrency and cancellation

The async client spawns nothing and has no runtime of its own: a whole search runs inside the future you poll, at most eight tile requests in flight. Drop the future to cancel it, and every request in it goes too. A call answered entirely from cache completes on its first poll.

Parsing and ranking run on whichever thread polls the future. On a big area that is real work: the spec measured about 12 ms to parse a 2 MB tile and 40 to 100 ms to score the 15,849 places of 42 Philadelphia tiles. One poll can parse several tiles that arrived together. If your executor can't afford that, call the blocking client from `tokio::task::spawn_blocking`, which moves the requests, the parsing and the ranking off it.

The blocking client runs a search on scoped threads, eight at most for the tiles, and has no cancellation beyond the transport's timeouts.

## Privacy

Your search text never leaves the device. Neither does your precise location.

To search, the client asks the server which tiles cover an area, downloads those tiles, and does the matching, filtering, and ranking locally. The only location a search sends is a bounding box (two across the antimeridian), snapped outward to a 0.01 degree grid (roughly 1 km of latitude) before it goes out; alongside it go the search's dataset and, if it has one, its collection (including the default `place`). Your exact region is used only on the device, to filter and to compute distances.

What does go on the wire:

- `getTiles` with the snapped bbox, the search's dataset, and its collection if it has one. A region that crosses the antimeridian sends two.
- Requests for the tile URLs the server returned, exactly as served, minus any already in the cache. So the server can tell which of those tiles this client fetched before (that you searched near here recently) but nothing finer than the tiles themselves.
- `describeService` and `describeTileset`, only if you call them.
- `get_place` on a cache miss sends the origin of the place you asked for.

The server also sees your IP address and whatever headers your transport sends. Both default transports send `Accept: application/json`, `Accept-Encoding: gzip` and `User-Agent: atgeo-client-rust`, the same everywhere: no version, no `Accept-Language`, no `Referer`. A transport you supply sends whatever its stack does. Snapping hides where in a ~1 km cell you are; it doesn't hide the cell.

## Caching

`getTiles` responses and the service description are cached for an hour, by the wall clock, so a laptop that sleeps through the hour doesn't keep serving a stale tile list. Tiles are cached in memory, least recently used out first, up to `cache_budget_bytes` (measured as the size of each tile's response body). Searching a region again while its tile list and tiles are warm makes no requests at all. `get_place` checks cached tiles before asking the server.

The cache holds each tile's body as it arrived, so the budget is close to what it costs in memory. On top of that, the client keeps the latest search's tiles parsed, outside the budget, so that repeating a search, or changing only its text or sort, doesn't parse them again. A parsed tile takes about six times its body: the spec measured 211 MB for a 35 MB, 42-tile search of Center City Philadelphia. That memory is released by the next search.

There is no persistence; a new client starts cold. Neither default transport has a cookie jar or a response cache, so nothing is cached behind the client's back either.

## Development

```sh
cargo test --all-features
```

`tests/fixtures/` holds the reference client's language-neutral fixtures and lexicons, vendored at the atgeo-sdk-js commit named in `tests/fixtures/SOURCE`. The unit tests in `src/` run the normalization and Jaro-Winkler fixtures and the contract's worked tables. The tests in `tests/` port the reference's tests, running each against both clients and a fake server that checks its own traffic. `tests/transports.rs` runs the two real transports against a loopback server.

`CONFORMANCE.md` lists where this port differs from the reference at the edges the [port contract](https://github.com/schuyler/atgeo-sdk-js/blob/main/docs/port-contract.md) leaves open, mostly in how malformed JSON and odd URLs are handled. None of it comes up with a garganorn server. [`rust-sdk-spec.md`](https://github.com/schuyler/atgeo-sdk-js/blob/main/docs/rust-sdk-spec.md) in atgeo-sdk-js is the design this port was built to, and `DECISIONS.md` records where the build departed from it.

There's also an end-to-end run against a live server, through both clients, which isn't part of `cargo test`:

```sh
ATGEO_SERVER=http://localhost:8000 cargo run --all-features --example e2e
```

It takes the same environment variables as the reference's `scripts/e2e.ts`: `ATGEO_SERVER` (default `http://localhost:8000`), `ATGEO_DATASET` (default `openstreetmap.org`), `ATGEO_COLLECTION` (default none), and `ATGEO_REGION` as `lat,lon,radius` in meters (default `43.0,-71.45,3000`, Manchester, NH). It prints a line per step and exits non-zero if either client fails. Against places.atgeo.org: `ATGEO_SERVER=https://places.atgeo.org ATGEO_DATASET=overturemaps.org ATGEO_REGION=39.9526,-75.1652,1000`.

## Server requirements

The client needs a garganorn that serves `org.atgeo.getTiles`, `getPlace`, `describeService` and `describeTileset`, and it follows the tile URLs `getTiles` returns as-is.

## License

BSD 3-Clause. See [LICENSE](https://github.com/schuyler/atgeo-sdk-rs/blob/main/LICENSE).
