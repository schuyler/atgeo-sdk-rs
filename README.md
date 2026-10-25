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
            SearchOptions::new("overturemaps.org").text("Starbux"),
        )
        .await?;

    for hit in &result.places {
        println!("{} {:.0} {:?} {:?}", hit.name().unwrap_or("-"), hit.distance, hit.score, hit.origin);
    }
    Ok(())
}
```

Misspellings are fine; matching is fuzzy, and that finds the Starbucks around Philadelphia's City Hall. The public server at places.atgeo.org has a limited dataset for now, mostly the east coast, so a search elsewhere may come back empty. If `result.complete` is `false`, some tiles failed to load and the results are partial; `result.tile_failures` says which and why. If `result.overview` is `true`, the region was too big to search in full and you got only its most important places; see [Overview answers](#overview-answers).

The async example needs `tokio` (with its `macros` and `rt` features) in your own dependencies for `#[tokio::main]`; the crate itself doesn't depend on Tokio, though its default transport needs a Tokio runtime to run on. The blocking client, behind the `ureq` feature (below), is the same without the `.await`:

```rust,no_run
use atgeo::{Region, SearchOptions, blocking::AtgeoClient};

fn main() -> Result<(), atgeo::Error> {
    let atgeo = AtgeoClient::new();
    let result = atgeo.search_places(Region::circle(39.9526, -75.1652, 1000.0), SearchOptions::new("overturemaps.org").text("Starbux"))?;
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

It needs Rust 1.85 or later (edition 2024). Through reqwest → url → idna, a current lockfile picks `icu_*` crates that need a newer Rust (2.2 needs 1.86, 2.4 needs 1.88), and on 1.85 the build fails. Cargo's MSRV-aware resolver picks versions that build instead. Edition 2024 turns it on; an edition-2021 app on 1.85 to 1.87 needs `resolver = "3"` in its `Cargo.toml` (under `[package]`, or `[workspace]` in a workspace), then a fresh `cargo update`. CI builds such an app on 1.85.

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

`SearchOptions::new(dataset)` and its setters. The dataset is the one required piece, and there's no default: `SearchOptions::new("overturemaps.org")`. It's a string, so a missing or non-string dataset (and a collection without one) won't compile. An empty string is sent as given, and the server answers for itself.

| Setter | Meaning |
|---|---|
| `.text(…)` | Optional search text. Matched fuzzily against each place's `name` and its `variants[].name`, after lowercasing and stripping accents and extra whitespace. Each name is scored against the whole query and against the query's first 12 words one at a time; words past the 12th count only in the whole-query comparison. |
| `.collection(…)` | Optional. Restricts the search to one collection of the dataset, such as `way`. |
| `.sort(…)` | `SortMode::Fused` (default), `SortMode::Distance`, or `SortMode::Similarity`. |

Regions:

```rust,ignore
Region::circle(39.9526, -75.1652, 1000.0)          // lat, lon, radius in meters
Region::bbox(-75.18, 39.94, -75.14, 39.96)          // west, south, east, north, in degrees
```

A `Region` isn't checked when you build it; `search_places` rejects an invalid one before making any request.

Datasets and collections work like this:

- With only a dataset, you search the whole dataset, every collection together. `SearchOptions::new("openstreetmap.org")` returns nodes, ways and relations in one list.
- Add `.collection("way")` to get only ways.

A collection-filtered search keeps only records whose `origin` starts with `urn:atgeo:{dataset}:{collection}:`. A record with no `origin` matches no collection filter, so such a search never returns one. A dataset-only search has no filter and returns those records too.

A bbox with `west > east` crosses the antimeridian. A place with a point location is kept if the point falls inside the region; a place with only a bounding box is kept if that box touches the region.

The result:

```rust,ignore
pub struct SearchResult {
    pub places: Vec<SearchHit>,
    pub complete: bool,        // false if any tile failed to load
    pub overview: bool,        // true if the server answered with its coarse overview; see below
    pub tiles: TileCounts,     // .fetched and .failed
    pub tile_failures: Vec<TileFailure>, // each failed tile, in URL order; empty when complete
    pub attribution: Vec<Attribution>,   // who to credit for the data in the tiles the search used
}

pub struct TileFailure {
    pub url: String,             // the tile URL as the server's tile list gave it
    pub kind: TileFailureKind,   // Refused, Status, Decode or Transport
    pub status: Option<u16>,     // the HTTP status, for Status only
    pub error: Option<Arc<atgeo::Error>>, // what it failed with; None only when deserialized
}

pub struct SearchHit {
    pub origin: Option<String>,  // the place's identity: place.origin, else the tile record's uri, else None
    pub collection: Option<CollectionKey>, // the collection its origin names, if the tile's attribution has it
    pub place: Place,            // the record value, exactly as served
    pub importance: Option<i64>, // an integer relative to its tileset; None if it has none
    pub distance: f64,           // meters from the region's center to the nearest point of the place; 0 if the place contains the center
    pub score: Option<f64>,      // 0 to 1 text similarity; None if no text was given
}
```

`Place` is `Arc<serde_json::Map<String, serde_json::Value>>`: the gazetteer's value untouched, every key as served, and nothing decoded into a Rust type. It's shared rather than copied, because the client keeps the latest search's tiles parsed and a hit points into them; `(*hit.place).clone()` gives you a copy of your own. `hit.name()` reads `name` when it's a string, and `hit.location()` returns the point and box the client read from `locations`, the same ones `distance` was measured to.

#### Attribution

`result.attribution` credits every collection whose data the search's tiles hold. If your app shows a hit, show this too: it includes the collections a tileset only references, since a hit's `within` relations can carry a containing division's name.

```rust,ignore
pub struct Attribution {
    pub dataset: String,
    pub collection: String,
    pub release: Option<String>,
    pub source: Option<String>,
    pub attribution: Option<String>,
    pub licenses: Option<Vec<String>>,
}
```

It's built from every tile the search used, from the network or the cache, whether or not any of its places made it into the results. A tile that failed adds nothing. There is one entry per `(dataset, collection)`; when tiles disagree, the lowest tile URL wins, and fields aren't merged. The list is sorted by dataset and then collection, in UTF-16 code unit order like everything else the client sorts. It's a list even when it's empty (a tile with no attribution counts as an empty list), and an entry the server sends without a string `dataset` and `collection` is dropped.

`hit.collection` is a `CollectionKey { dataset, collection }`: the first entry in the hit's own tile's attribution whose `urn:atgeo:{dataset}:{collection}:` starts the hit's `origin`. It's `None` when none does, including for a hit identified only by its `uri`. It never holds the `release` or `licenses`; look those up in `result.attribution`.

You own what you're given. `Attribution` is plain owned data, so changing an entry or its `licenses` changes nothing the client holds.

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

Returns a `PlaceRecord` with `origin`, `place`, `importance` and `attribution`, as in a hit, and the same `name()` and `location()`. `attribution` is a list of `Attribution`, in the order the source served it, duplicates included and unsorted: from the cached tile that held the place (the lowest URL, if several did), or from the server's answer. It's empty if there is none. Pass the `origin` of a hit. If a cached tile holds the place, no request is made; otherwise the client asks the server (`org.atgeo.getPlace`) directly.

### `describe_service() -> ServiceDescription`

Returns the server's `did`, its `max_tiles` (its tile budget, for information: tile zooms adapt, so you can't tell from it whether a box will fit) and its `tilesets`, each a `slug` and its `collections` (`dataset`, `collection`). Cached for an hour.

### `describe_tileset(slug) -> Tileset`

Returns the tileset's descriptor: `slug`, `collections` (each with `dataset`, `collection` and optional `release`, `source`, `licenses`, `attribution`, `record_count`, `location_formats`, `containment_types`, `categories` and `attributes`), and optional `published_at`, `extent`, `references` and `overview` (`records`: how many records the tileset's overview band holds; absent if it has none). Not cached.

Every field of the describe types is an `Option` except the lists the lexicon requires, which are `Vec`s. A field the server drops or retypes comes back `None` (or empty) rather than failing the call.

### Errors

`atgeo::Error` is one enum:

- `Error::Atgeo(AtgeoError)`: the server answered with a non-2xx status. `AtgeoError` has a `code`, a `message` and the HTTP `status`, which tells a request the server refused (4xx) from one it failed to answer (5xx) without matching on codes. The code and message come from the server's JSON error body; if there isn't one, the code is `HttpError`. Apart from `HttpError`, every code comes from the server: for example `PlaceNotFound`, `TilesetNotFound`, `DatasetNotFound`, `CollectionNotFound`, `BboxTooLarge`, or `InvalidRequest`. The list is open, so report codes rather than matching on them.
- `Error::Decoding(DecodingError)`: a 2xx body that isn't JSON, or has nothing usable in it (no `tiles` list, no `value` object in a `getPlace` answer, a describe body that isn't an object).
- `Error::Transport(BoxError)`: no response arrived. It holds the transport's own error, so `downcast_ref::<reqwest::Error>()` reaches it.
- `Error::InvalidArgument(ArgumentError)`: an invalid region (non-finite or out-of-range coordinates, `south > north`, a negative radius), `Similarity` without text, or a server that isn't an http or https URL, or that carries credentials (`user:password@`). All of these are found before any request.

An error's `Display` says what went wrong at its own level and leaves the cause to `source()`, as the standard library's errors do: a transport failure displays as `transport error`, and a body that isn't JSON as `response is not JSON`, with the transport's or the parser's error as the source. To log the whole story, walk `source()`, or print it with anyhow's `{:#}`.

A failed `getTiles` request fails the whole search. A failed tile does not: that tile is skipped, `tiles.failed` counts it, `complete` is `false`, and `tile_failures` reports it, in the order of the tile URLs rather than the order they failed in. Its `kind` says where it failed:

- `Refused`: the tile URL isn't an absolute http or https URL, or it carries credentials, so no request was made. `error` is an `Error::Transport` saying so.
- `Status`: the tile answered with a non-2xx `status`. `error` is the `Error::Atgeo` any other call would have failed with.
- `Decode`: a 2xx whose body isn't a usable tile (not JSON, not an object, no `records` array). `error` is an `Error::Decoding`.
- `Transport`: the transport failed, a body over `max_body_bytes` included. `error` is the `Error::Transport` holding the transport's own error.

So a run of `Transport` failures points at the network or your transport, `Refused` or `Decode` at the server, and `Status` at whatever its `status` says. Two failures are equal when their `url`, `kind` and `status` are; `error` is ignored there, and under the `serde` feature it isn't serialized, so a deserialized failure has `None`. A failure carries the tile URL and what fetching it produced, never your search text, and nothing about your region beyond what the tile URL says: the server chose that tile for the snapped box, so it does reveal the coarse area searched. Nothing is logged, and nothing is sent anywhere: the result is the only place a failure goes.

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

`ReqwestTransport` needs Tokio: reqwest spawns a task per connection. On another runtime, supply a transport built on your own HTTP stack. For HTTP/2, a proxy or your own TLS settings, build a `reqwest::Client` from `ReqwestTransport::client_builder()` and pass it to `ReqwestTransport::with_client`. Starting from `reqwest::Client::builder()` instead would bring back the `Referer` header on redirects, which carries the snapped bbox to whatever host the redirect names, and would follow a redirect to a URL that carries credentials, which `fetch` and the default transports refuse. `UreqTransport::config_builder()` does the same for a `ureq::Agent`.

Both default transports cap a response body at 32 MiB, decoded, so a small gzip body can't decode to gigabytes (`max_body_bytes` changes it). They time out a connection after 10 s and a stalled read after 30 s. `UreqTransport` also stops a whole request, redirects included, after 120 s, because a blocking call can't be cancelled any other way. Both honour the `HTTPS_PROXY` family of environment variables, though not identically (ureq uses the first one set for every scheme).

## Concurrency and cancellation

The async client spawns nothing and has no runtime of its own: a whole search runs inside the future you poll, at most eight tile requests in flight. Drop the future to cancel it, and every request in it goes too. A call answered entirely from cache completes on its first poll.

Parsing and ranking run on whichever thread polls the future. On a big area that is real work: the spec measured about 12 ms to parse a 2 MB tile and 40 to 100 ms to score the 15,849 places of 42 Philadelphia tiles. One poll can parse several tiles that arrived together. If your executor can't afford that, call the blocking client from `tokio::task::spawn_blocking`, which moves the requests, the parsing and the ranking off it.

The blocking client runs a search on scoped threads, eight at most for the tiles, and has no cancellation beyond the transport's timeouts.

## Privacy

Your search text never leaves the device. Neither does your precise location.

To search, the client asks the server which tiles cover an area, downloads those tiles, and does the matching, filtering, and ranking locally. The only location a search sends is a bounding box (two across the antimeridian), snapped outward to a 0.01 degree grid (roughly 1 km of latitude) before it goes out; alongside it go the search's dataset and, if it has one, its collection. Your exact region is used only on the device, to filter and to compute distances.

What does go on the wire:

- `getTiles` with the snapped bbox, the search's dataset, and its collection if it has one. A region that crosses the antimeridian sends two.
- Requests for the tile URLs the server returned, as served (with only the percent-encoding a browser would add; see `CONFORMANCE.md`), minus any already in the cache. So the server can tell which of those tiles this client fetched before (that you searched near here recently) but nothing finer than the tiles themselves.
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

`tests/fixtures/` holds the reference client's language-neutral fixtures and lexicons, vendored at the atgeo-sdk-js commit named in `tests/fixtures/SOURCE`. The unit tests in `src/` run the normalization, Jaro-Winkler and lenient-reader fixtures and the contract's worked tables. `tests/attribution.rs` runs the attribution scenarios, with a fresh client each, and again with the tiles arriving in reverse URL order. The tests in `tests/` port the reference's tests against a fake server that checks its own traffic, running nearly every one against both clients; the few whose results depend on the order the blocking client's threads finish in run against the async client only. `tests/transports.rs` runs the two real transports against a loopback server.

`CONFORMANCE.md` lists where this port differs from the reference at the edges the [port contract](https://github.com/schuyler/atgeo-sdk-js/blob/main/docs/port-contract.md) leaves open, mostly in how malformed JSON and odd URLs are handled. None of it comes up with a garganorn server. [`rust-sdk-spec.md`](https://github.com/schuyler/atgeo-sdk-js/blob/main/docs/rust-sdk-spec.md) in atgeo-sdk-js is the design this port was built to, and `DECISIONS.md` records where the build departed from it.

There's also an end-to-end run against a live server, through both clients, which isn't part of `cargo test`:

```sh
ATGEO_SERVER=http://localhost:8000 cargo run --all-features --example e2e
```

It needs the next garganorn release, tracked in [garganorn#27](https://github.com/schuyler/garganorn/issues/27): it breaks the lexicons (the new `getPlace` shape, `.json` tile URLs, `maxTiles` and `overview`), so the server has to be rebuilt and deployed first. Until then it can't pass against an older garganorn or places.atgeo.org. It takes the same environment variables as the reference's `scripts/e2e.ts`: `ATGEO_SERVER` (default `http://localhost:8000`), `ATGEO_DATASET` (default `openstreetmap.org`), `ATGEO_COLLECTION` (default none), and `ATGEO_REGION` as `lat,lon,radius` in meters (default `43.0,-71.45,3000`, Manchester, NH). It prints a line per step and exits non-zero if either client fails. Against places.atgeo.org, once that release is deployed there: `ATGEO_SERVER=https://places.atgeo.org ATGEO_DATASET=overturemaps.org ATGEO_REGION=39.9526,-75.1652,1000`.

## Server requirements

The client needs a garganorn that serves `org.atgeo.getTiles`, `getPlace`, `describeService` and `describeTileset`, and it follows the tile URLs `getTiles` returns as-is.

## License

BSD 3-Clause. See [LICENSE](https://github.com/schuyler/atgeo-sdk-rs/blob/main/LICENSE).
