# Conformance

Where this port's behaviour differs from the TypeScript reference at the margins the [port contract](https://github.com/schuyler/atgeo-sdk-js/blob/main/docs/port-contract.md) leaves open (§5, §14), and the Unicode skew its §7 asks ports to record. None of these cases arises from a compliant garganorn server.

The fixtures are vendored at the commit named in `tests/fixtures/SOURCE`. Unless a row says otherwise, the entries hold for both clients, and the JSON entries assume serde_json's default features. Cargo unifies features across a build, so another crate in your app can turn one on; the client is correct under any of them, but these margins move (see [JSON](#json)).

## JSON

Bodies are decoded as `JSON.parse(await response.text())` decodes them: invalid UTF-8 becomes U+FFFD and a leading byte-order mark is skipped (`src/wire.rs`). Then serde_json parses them.

| Input | This port | Reference (`JSON.parse`) |
|---|---|---|
| A lone surrogate escape (`"\uD800"`) | U+FFFD. A Rust `String` cannot hold a lone surrogate, so a body that fails to parse is parsed again with each such escape rewritten. | the lone code unit, kept |
| A number literal that overflows a double (`1e400`) | parse error: a failed tile, or `Error::Decoding` for an XRPC call. serde_json cannot hold infinity without `arbitrary_precision`, which a library must not impose on its app. | Infinity |
| Nesting deeper than 127 arrays or objects | parse error, as above (serde_json's recursion limit) | accepted |
| Integers between 2^53 and 2^64 inside a place | kept exactly in the `serde_json::Value` (`9007199254740993` stays itself) | rounded to the nearest double. `importance` is read through a double in both, as contract §1 requires. |
| Decimal literals of 17 or more significant digits | may round to a neighbouring double (serde_json without `float_roundtrip`) | correctly rounded. Coordinates in garganorn tiles are strings, which both parse exactly. |
| Key order of a place | sorted, since `serde_json::Map` is a `BTreeMap` without `preserve_order` | as served. The contract does not require order, and the Swift port does not keep it either. |

Where the two agree, at margins a port might get wrong: a literal that underflows (`1e-400`) is zero; `-0` stays negative zero; the last of duplicate keys wins; integers beyond 64 bits become the nearest double.

## Numbers

Coordinates given as strings are parsed with `str::parse::<f64>`, where the reference uses `parseFloat`. Non-finite results are rejected in both.

| Input | This port | Reference (`parseFloat`) |
|---|---|---|
| ` 1.5`, `1.5 ` (surrounding whitespace) | no location | 1.5 |
| `1.5abc`, `1e`, `1_000`, `0x10` | no location | 1.5, 1, 1, 0 |
| `+1.5`, `.5`, `1.`, `-0`, `1e3`, `1e+5` | same in both | same in both |
| `Infinity`, `inf`, `NaN` in any case | non-finite, skipped | non-finite, skipped |
| `["0.5"]`, a coordinate in a one-element array | no location (contract §16) | 0.5 |

A NaN distance, which only out-of-range coordinates in a place can produce, sorts after every number; `-0` ties with `0`, as subtraction does. The reference compares by subtracting, so a NaN ties with everything and its order is the engine's. Rust's `sort_by` may panic on a comparator that is not a total order, so this port does not reproduce that. The Kotlin port made the same choice.

## URLs

The client checks and prepares every URL before a transport sees one (`src/xrpc.rs`): leading and trailing controls and spaces are trimmed; tab, LF and CR are dropped; spaces, other controls, non-ASCII bytes and `" < > ` { }` are percent-encoded; anything else is left alone. A URL that is not an absolute http or https URL with a host, or that carries credentials (`https://user:pw@host/…`, which the Fetch standard's `Request` refuses), is a failed tile, or `ArgumentError::InvalidServer` for the server. That makes both transports send the bytes the reference's WHATWG URL parsing would, except:

| Input | This port | Reference (`fetch`) |
|---|---|---|
| `` ` ``, `{` or `}` in a query | percent-encoded (a server decodes it to the same value) | sent raw |
| `'` in a query | `ReqwestTransport` percent-encodes it, as `fetch` does; `UreqTransport` sends it raw | `%27` |
| A backslash, or a `.` or `..` segment, in a tile URL | `ReqwestTransport` reads them as WHATWG does (`/`, resolved); `UreqTransport` sends them as given | `/`, resolved |
| A non-ASCII host (`café.example`) | `ReqwestTransport` sends it in punycode, as `fetch` does; `UreqTransport` fails the request, since the host arrives percent-encoded rather than in punycode | punycode |
| An http(s) URL without its slashes (`https:h/x`) | not an http URL: a failed tile | `https://h/x` |
| A redirect to `https:/elsewhere` (a scheme and a path, no `//`) | `ReqwestTransport` resolves it as `fetch` does; `UreqTransport` fails the request | `https://<current host>/elsewhere` when the scheme is the current URL's |

A redirect to anything but http or https (`file:`, say) is a transport error in both transports, as it is in `fetch`, and so is a redirect to a URL that carries credentials, as in the Python port. `UreqTransport` follows redirects itself, removing dot segments as it resolves them.

## Responses

| Input | This port | Reference (`fetch`) |
|---|---|---|
| A gzip body of several members (`cat a.gz b.gz`) | `UreqTransport` decodes every member; `ReqwestTransport` returns only the first, so the tile fails to parse | every member |
| `Content-Encoding: x-gzip` | `UreqTransport` decodes it; `ReqwestTransport` returns the gzip bytes, so the tile fails to parse | decoded |
| A body that decodes past 32 MiB | a transport error (`max_body_bytes`), so a failed tile (contract §14) | read in full |
| An empty body labelled gzip | empty, in both transports. If another crate in your app turns on ureq's `gzip` feature, ureq decodes it itself and fails it instead. | empty |

## Concurrency

| Behaviour | This port | Reference |
|---|---|---|
| The order the two getTiles requests of a region crossing the antimeridian start in | async client: the half from the region's western edge to 180 first, then the half from -180, as the reference does. Blocking client: either, since they go out from two threads at once. | the half ending at 180 first |
| The order tile requests start in, and so the order tiles are touched in the cache | async client: the reference's, index order with every worker's first lookup before any store. Blocking client: whichever of its eight threads gets there first. So with a budget too small for a search's tiles, which tiles stay cached afterwards can differ from run to run; results do not. | index order |
| Which error a search returns when both getTiles halves fail | async client: the first to arrive, and the other request is dropped (§14). Blocking client: the first in call order, after both finish, since a blocking request cannot be cancelled. | the first to arrive |
| Cancellation | async client: drop the future. A call answered entirely from cache completes on its first poll. Blocking client: none beyond the transport's timeouts. | `AbortSignal` |

## Unicode skew

Cases of `tests/fixtures/normalize.json` that fail because of Unicode version differences between the tables this port uses (the standard library's case mapping, `unicode-normalization` and `unicode-properties`, at Unicode 17.0, 18.0 and 17.0 respectively, with Rust 1.97) and V8's: none. All 41 cases pass.

`str::to_lowercase` applies Final_Sigma as V8 does. It agrees with V8 on the fixture's cases and on `Σ1Σ` and `Σ𝚺` + U+0345, where the JDK, and so the Kotlin port, differs.
