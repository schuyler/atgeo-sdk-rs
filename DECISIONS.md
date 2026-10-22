# Decisions

This port was built to the Rust spec, [`docs/rust-sdk-spec.md`](https://github.com/schuyler/atgeo-sdk-js/blob/main/docs/rust-sdk-spec.md) in atgeo-sdk-js (still in schuyler/atgeo-sdk-js#21 when this was written), against the port contract on that repository's `main`. This file records what the spec left to the builder, and where the build departs from it and why. `CONFORMANCE.md` covers where the port's behaviour differs from the reference.

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

- **Fixtures** are vendored from atgeo-sdk-js `main` at `7b8a400`, the newest commit when the port was built. They are identical to the `a14961f` fixtures the Kotlin and Swift ports pinned.
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
