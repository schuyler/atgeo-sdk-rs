# Decisions

This port was built to the Rust spec, [`docs/rust-sdk-spec.md`](https://github.com/schuyler/atgeo-sdk-js/blob/main/docs/rust-sdk-spec.md) in atgeo-sdk-js (added in schuyler/atgeo-sdk-js#21 and brought up to date with this port in #24), against the port contract on that repository's `main`. This file records what the spec left to the builder, and where the build departs from it and why. `CONFORMANCE.md` covers where the port's behaviour differs from the reference.

## Where the build departs from the spec

- **Repository name.** The spec assumed `schuyler/atgeo-sdk-rust`; the repository is `schuyler/atgeo-sdk-rs`, and the README's dependency line and links say so.
- **`prepare_url` encodes more.** The spec's recipe percent-encodes spaces, controls and non-ASCII. The port also encodes `" < > ` { }`, as WHATWG parsing does in a path, and trims leading and trailing controls and spaces, as WHATWG does. Without the first, ureq's `http::Uri` rejected `<`, `>` and `` ` `` and failed the tile where reqwest and `fetch` fetch it. Without the second, a tile URL with a stray leading space was a failed tile where `fetch` loads it.
- **The async driver yields once before each tile request.** The spec's worker loop calls the transport as soon as a worker misses the cache. With a transport that answers on its first poll, worker 0 then stored a tile, and evicted, before workers 1 to 7 had looked up their URLs. The reference's first lookups all happen before any store. A search's results were the same, but with a tight budget which tiles stayed cached was not: six equal tiles and room for two, and the warm search fetched six where the reference fetches four. A runtime-agnostic one-poll yield (no `tokio::task::yield_now`) between the miss and the request restores the reference's order; `tests/cache.rs` checks the counts against the reference's own. Cached tiles don't yield, so a warm search still completes on its first poll.
- **`UreqTransport` does more than the spec's list.** It removes dot segments when it resolves a redirect, as reqwest and `fetch` do; decodes multi-member gzip (`MultiGzDecoder`) and `x-gzip`, as `fetch` does; and applies the agent's total timeout to the whole chain of redirects, since a deadline per hop would let eleven hops take 22 minutes.
- **`ReqwestTransport` fails a redirect it won't follow.** reqwest hands back the 3xx when a `Location` names another scheme (`file:`). The async client then reported `Error::Atgeo { code: "HttpError", status: 302 }` where the blocking client and `fetch` report a network failure. The transport now returns an error for a 301, 302, 303, 307 or 308 that still carries a `Location`.
- **`TtlCache::set` checks its addition.** `SystemTime + Duration` panics on overflow, so a test clock near the end of time caches nothing instead of panicking.
- **The blocking client fetches a lone tile list on the calling thread.** The spec runs "the one or two tile lists on their own scoped threads"; with only one to fetch, a thread buys nothing.
- **The `both!` test macro makes a module, not two names.** The spec's `<name>_async` and `<name>_blocking` would need `paste` or a proc-macro to build the identifiers; `both! { fn name(kind: Kind) { … } }` makes `mod name` with `async_client` and `blocking_client` tests instead, and needs no dependency.

## What the spec left open

- **Fixtures** are vendored from atgeo-sdk-js at `2710a14`, the port contract revision of atgeo-sdk-js#43 (attribution, hit collections, a required dataset, the `getPlace` value shape). They were `7b8a400` when the port was built, and are now at `fc56331` (see the revision of atgeo-sdk-js#63 below).
- **The base branch.** The repository had no commits, so its `main` starts with the shared `LICENSE` alone, giving the port's pull request something to target.
- **`rustfmt.toml`** sets `max_width = 120`, which keeps the contract's comparator chains and test tables on one line each.
- **`publish = false`** in `Cargo.toml` until the beta ends (spec §2, distribution), so nothing reaches crates.io by accident.
- **The README is the crate's documentation** only when both `reqwest` and `ureq` are on, since its examples use both clients; other builds get a one-line summary. docs.rs builds with every feature.
- **The fake server** checks parameter lengths against the lexicons' `maxLength` as well as the contract's minimum, and records a request whose future was dropped, so that `tests/concurrency.rs` can count them.
- **`examples/e2e.rs`** runs the reference's steps once through each client, and exits non-zero if either fails.
- **The blocking client's request order** across its threads is not fixed: the two halves of a crossing region, and the tiles, start in whatever order the threads reach them. Results are unaffected; `CONFORMANCE.md` lists it. Forcing the order would mean starting requests one at a time.

## The spec's open questions, as built

These stay open for the maintainer (spec §8); the port does the conservative thing in each.

1. `wasm32`: not supported. The transport trait requires `Send` futures.
2. Cancelling the blocking client: none beyond the transport's timeouts.
3. Reserving `atgeo` on crates.io: not done; it's the maintainer's call.
4. The parsed-tile memo: unbounded, holding the latest search's tiles.
5. An offload hook for the async client's CPU work: none. The README points to `spawn_blocking` with the blocking client.
6. Non-BMP Jaro-Winkler cases: the port's unit tests carry the three the spec proposes (`😀😁`/`😀😂`, `pizza`/`pizza 🍕`, `a😀`/`a`); the shared fixture doesn't have them yet.
7. `1e400`: accepted as a margin and recorded in `CONFORMANCE.md`. A tile holding one fails.
8. Reporting failed tiles (contract §4.4, as amended at atgeo-sdk-js `a6cb4f0`): built as the spec's item 8 says, with these choices.
   - `NearbySearchResult::tile_failures: Vec<TileFailure>`, in URL order; `TileCounts` stays `Copy`. The field is `serde(default)`, so a result serialized before it existed still deserializes, with an empty list (and then its length need not be `tiles.failed`).
   - `TileFailure { url, kind, status, error: Option<Arc<Error>> }`. `PartialEq`, `Eq` and `Hash` are written by hand over `url`, `kind` and `status`. Under `serde`, `error` is skipped (so `None` only after deserializing), `status` is left out when `None`, and `TileFailureKind` reads and writes `refused`, `status`, `decode`, `transport`.
   - The errors: `Error::Atgeo` for `Status`, read from the body as any other call's non-2xx is; `Error::Decoding` for `Decode`; `Error::Transport` with the transport's own error for `Transport`. For `Refused`, where no transport ran, an `Error::Transport` whose source is a fixed message that does not repeat the URL, since it may carry credentials. No variant was added to `Error`.
   - Not additive in one respect: `NearbySearchResult` and `TileFailure` are no longer `UnwindSafe` or `RefUnwindSafe`, since the boxed transport error promises neither. They stay `Send + Sync`. Before 1.0, and with `AssertUnwindSafe` as the way round, that seemed better than hiding the error behind a wrapper.
   - Nothing is guarded beyond the four steps of §4.4, and nothing needs narrowing: the cache insert cannot fail and a panic propagates. serde_json's recursion limit makes deeply nested JSON a parse error, so a `Decode` tile. A slot is matched to its URL by index with a release-mode `assert!`, so a bug there panics rather than blaming the wrong tile.
   - No logging: no `log` or `tracing` dependency. The result carries everything a log line would.

## The port contract revision (atgeo-sdk-js#43)

The spec says `NearbyOptions::new(dataset)`, without `Default` or a `.dataset()` setter, and is silent on the rest. The build chose:

- **A required dataset is a type, not a check.** `new` takes `impl Into<String>`, so a missing or non-string dataset, and a collection named without one, don't compile; no `ArgumentError` variant was added. Three `compile_fail` doctests on `NearbyOptions` keep it that way. An empty string is sent as given.
- **`Attribution` is its own type.** A search entry is kept only if its `dataset` and `collection` are strings, so those are `String` in `Attribution`; the describe types' `CollectionAttribution` has every field optional and stays as it was. A hit's `collection` is a `CollectionKey { dataset, collection }`, not the entry, so it can't carry a `release` or `licenses`.
- **Owned values for the copy rule.** Contract §9 lets a port hand out immutable values or copies. Entries are plain owned data, cloned out of the cache for every answer, so there is nothing to freeze. The cost is a clone of a short list per search and per cached `get_place`.
- **The cache still holds bodies.** A parsed tile is now its records and its attribution, and a tile read again from its body reads the attribution again (contract §10). The tile's size is still its body length.
- **No constructor for the new types.** They are `#[non_exhaustive]`, and `Attribution` derives `Default`, so a test can build one by assigning fields. Nobody has asked for more.
- **The names of the new describe fields** follow the existing ones: `ServiceDescription::max_tiles` and `Tileset::overview` (`TilesetOverview { records }`).
- **Bbox fields** are `west`, `south`, `east`, `north` (garganorn#16); `Region::bbox` still takes them in that order.
- **`examples/e2e.rs`** is unchanged in what it checks; it can't pass until the server work in garganorn#27 is deployed, and it isn't skipped.

## The port contract revision of atgeo-sdk-js#63 (keyword search)

The Rust spec says what the keyword methods look like; the build chose, where it did not:

- **`search_places` is `search_nearby`**, `SearchOptions`, `SearchResult` and `SearchHit` are `NearbyOptions`, `NearbySearchResult` and `NearbyHit`, as the spec names them. There is no deprecated alias: the crate is not published.
- **`via` is a method.** The spec writes `FallbackHit::via()` and gives the hit structs no `via` field; `NearbyHit` and `KeywordHit` have the method too (`"tiles"`, `"keyword"`), so that code over any of them reads alike.
- **The result types carry what the reference's do.** The spec's sketch of `NearbySearchResult` and `FallbackSearchResult` lacked `tile_failures` and `attribution`, which the port already had (item 8 above, and the attribution revision); both have them, and `KeywordSearchResult` has `attribution`. Hits carry `collection`. The data types derive `serde` with `serde(default)` on the fields added after a persisted shape could exist (`tile_failures`, `attribution`, `disclosed`).
- **`Disclosure::region` is an `Option<f64>`**, since the grid is `0.01` or `1`; `Disclosure::default()` is `keyword: false, region: None`.
- **`autocompleter.search_by_keyword` is not an `async fn`.** It returns `impl Future + Send`, and supersedes the previous call when it is *called*. An `async fn` body runs at its first poll, so two calls polled in the opposite order would supersede the wrong one. It borrows nothing from the autocompleter but the client, so a call can outlive the borrow of `&self`. A call whose keyword cannot qualify is not wrapped in an `Abortable`: it has already resolved, in the reference's terms, and a later call must not turn it into `Error::Superseded`.
- **The fallback decides on the nearby result as built.** It re-computes match tier 1 for each hit from the hit's place with the query the nearby search used, rather than carrying a tier out of ranking, so the result types do not grow a hidden field. A keyword that normalizes to nothing counts every hit as tier 1, as `matchTier` does in the reference (the call then returns the nearby result either way, since such a keyword cannot qualify).
- **`disclosed.region` for a nearby call** comes from an `AtomicBool` the tile loop sets when a tile fetch is attempted and from the plan's list of uncached tile lists. A tile refused before any request, and a tile found in the cache, set nothing.
- **The fake server** checks a `searchPlaces` request with the server's own rules (`KeywordRequired` from the server's `name_tokens`, not the client's predicate; precision, then `InvalidBbox`, then dataset and collection, then the keyword) and answers `BboxTooPrecise` to a `getTiles` coordinate finer than two decimals. A fixture whose bodies need not validate is served through `raw_method`, which answers a method for any query.
- **The vendored lexicons** are garganorn `59d21b1`, as the task for this revision pinned them. `scripts/check-fixtures.sh` holds them to that pin, which is also the one the reference's own `test/lexicon/` names.
