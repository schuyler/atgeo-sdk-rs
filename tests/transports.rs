//! The default transports against a loopback HTTP server (spec §7): statuses, redirects, gzip, the body
//! cap, timeouts, the request headers (contract §12, item 5), no `Referer`, and each transport's own cases.

#![cfg(any(feature = "reqwest", feature = "ureq"))]

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

// ---------------------------------------------------------------------------------------------------------
// The loopback server

/// One request's head, as the server read it. Header names are lowercased.
#[derive(Clone, Debug)]
struct Request {
    line: String,
    headers: Vec<(String, String)>,
}

impl Request {
    fn path(&self) -> &str {
        self.line.split(' ').nth(1).unwrap_or("")
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    fn names(&self) -> BTreeSet<&str> {
        self.headers.iter().map(|(k, _)| k.as_str()).collect()
    }
}

type Handler = dyn Fn(&Request, &mut TcpStream) + Send + Sync;

/// A server on 127.0.0.1 that records every request head and answers it with a handler, one thread per
/// connection. The handler writes the whole response itself, so that it can trickle bytes, write a huge
/// body, or watch the client close; the helpers below answer with `Connection: close`, and the
/// connection's thread reads the next request on the same connection should the client send one.
struct Server {
    port: u16,
    requests: Arc<Mutex<Vec<Request>>>,
}

impl Server {
    fn start(handler: impl Fn(&Request, &mut TcpStream) + Send + Sync + 'static) -> Server {
        Self::serve(vec![TcpListener::bind("127.0.0.1:0").unwrap()], handler)
    }

    /// A server on every address `localhost` resolves to, all on one port, so that a client reaches it
    /// whichever address family it tries first. (Windows resolves `localhost` to `::1` first and takes
    /// seconds to give up on a refused connection.) Families the machine lacks are skipped.
    fn start_localhost(handler: impl Fn(&Request, &mut TcpStream) + Send + Sync + 'static) -> Server {
        use std::net::{SocketAddr, ToSocketAddrs};
        let addrs: Vec<SocketAddr> = "localhost:0".to_socket_addrs().unwrap().collect();
        for _ in 0..20 {
            let mut listeners: Vec<TcpListener> = Vec::new();
            let mut port = 0;
            let mut clash = false;
            for addr in &addrs {
                match TcpListener::bind(SocketAddr::new(addr.ip(), port)) {
                    Ok(l) => {
                        port = l.local_addr().unwrap().port();
                        listeners.push(l);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => clash = true,
                    Err(_) => {} // an address family this machine does not have
                }
            }
            if !clash && !listeners.is_empty() {
                return Self::serve(listeners, handler);
            }
        }
        panic!("no free port on every localhost address");
    }

    fn serve(
        listeners: Vec<TcpListener>,
        handler: impl Fn(&Request, &mut TcpStream) + Send + Sync + 'static,
    ) -> Server {
        let port = listeners[0].local_addr().unwrap().port();
        let requests: Arc<Mutex<Vec<Request>>> = Arc::default();
        let handler: Arc<Handler> = Arc::new(handler);
        for listener in listeners {
            let (handler, log) = (handler.clone(), requests.clone());
            thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { continue };
                    let (handler, log) = (handler.clone(), log.clone());
                    thread::spawn(move || {
                        let _ = stream.set_nodelay(true);
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                        while let Some(request) = read_head(&mut stream) {
                            log.lock().unwrap().push(request.clone());
                            handler(&request, &mut stream);
                        }
                    });
                }
            });
        }
        Server { port, requests }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }
}

/// Reads a request head byte by byte (a GET has no body), or `None` once the client is gone.
fn read_head(stream: &mut TcpStream) -> Option<Request> {
    let mut raw = Vec::new();
    let mut byte = [0u8];
    while !raw.ends_with(b"\r\n\r\n") {
        match stream.read(&mut byte) {
            Ok(1) => raw.push(byte[0]),
            _ => return None,
        }
    }
    let text = String::from_utf8_lossy(&raw).into_owned();
    let mut lines = text.split("\r\n").filter(|l| !l.is_empty());
    let line = lines.next()?.to_owned();
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    Some(Request { line, headers })
}

/// Writes a response head declaring `length` body bytes.
fn head(s: &mut TcpStream, status: u16, headers: &[(&str, &str)], length: u64) -> bool {
    let mut text = format!("HTTP/1.1 {status} Status\r\nContent-Length: {length}\r\nConnection: close\r\n");
    for (k, v) in headers {
        text.push_str(&format!("{k}: {v}\r\n"));
    }
    text.push_str("\r\n");
    s.write_all(text.as_bytes()).is_ok()
}

fn respond(s: &mut TcpStream, status: u16, headers: &[(&str, &str)], body: &[u8]) {
    if head(s, status, headers, body.len() as u64) {
        let _ = s.write_all(body);
    }
}

/// Sends `body` a byte at a time, sleeping `interval` before each byte after the first.
fn trickle(s: &mut TcpStream, body: &[u8], interval: Duration) {
    if !head(s, 200, &[], body.len() as u64) {
        return;
    }
    for (i, b) in body.iter().enumerate() {
        if i > 0 {
            thread::sleep(interval);
        }
        if s.write_all(&[*b]).is_err() {
            return;
        }
    }
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

const ERROR_JSON: &[u8] = br#"{"error":"NotFound","message":"no such tile"}"#;
const TILE_JSON: &[u8] = r#"{"records":[{"value":{"name":"Café"}}]}"#.as_bytes();
const CAP: usize = 64 * 1024;
const MIB: u64 = 1024 * 1024;

/// The routes most cases share; the number after a path's last `/` parameterises it.
fn routes(r: &Request, s: &mut TcpStream) {
    let path = r.path().split('?').next().unwrap_or("").to_owned();
    let n = |i: usize| path.split('/').nth(i).and_then(|p| p.parse::<usize>().ok()).unwrap_or(0);
    let json = [("Content-Type", "application/json")];
    let gz = [("Content-Encoding", "gzip")];
    match path.split('/').nth(1).unwrap_or("") {
        "json404" => respond(s, 404, &json, ERROR_JSON),
        "empty400" => respond(s, 400, &[], b""),
        "redirect" => respond(s, 302, &[("Location", "/target")], b""),
        "target" => respond(s, 200, &[], b"landed"),
        "gzip" => respond(s, 200, &gz, &gzip(TILE_JSON)),
        "gzip-empty" => respond(s, 200, &gz, b""),
        "bytes" => respond(s, 200, &[], &vec![b'a'; n(2)]),
        "zeros-gz" => respond(s, 200, &gz, &gzip(&vec![0; n(2)])),
        "trickle" => trickle(s, b"0123456789", Duration::from_millis(n(2) as u64)),
        // /hop/{hops}/{i}: a chain of `hops` redirects ending at a 200.
        "hop" if n(3) < n(2) => respond(s, 302, &[("Location", &format!("/hop/{}/{}", n(2), n(3) + 1))], b""),
        "hop" => respond(s, 200, &[], b"end of the chain"),
        "to-file" => respond(s, 302, &[("Location", "file:///etc/passwd")], b""),
        // Back to this server, so that only the refusal (not a failed lookup) keeps the second request away.
        "to-credentials" => {
            let location = format!("http://user:pw@{}/target", r.header("host").unwrap_or(""));
            respond(s, 302, &[("Location", &location)], b"")
        }
        // Two gzip members, as `cat a.gz b.gz` makes; the body is their concatenation.
        "gzip-multi" => {
            let mut body = gzip(&TILE_JSON[..10]);
            body.extend(gzip(&TILE_JSON[10..]));
            respond(s, 200, &gz, &body);
        }
        "x-gzip" => respond(s, 200, &[("Content-Encoding", "x-gzip")], &gzip(TILE_JSON)),
        "dots" => respond(s, 302, &[("Location", "sub/../target")], b""),
        // /slowhop/{hops}/{i}: like /hop, but each answer waits 200 ms first.
        "slowhop" => {
            thread::sleep(Duration::from_millis(200));
            if n(3) < n(2) {
                respond(s, 302, &[("Location", &format!("/slowhop/{}/{}", n(2), n(3) + 1))], b"");
            } else {
                respond(s, 200, &[], b"end of the chain");
            }
        }
        "away" => {
            let location = format!("http://localhost:{}/landing", n(2));
            respond(s, 302, &[("Location", &location)], b"");
        }
        // A tile list naming a tile URL with a space and a tab in it, on this same server.
        "xrpc" => {
            let host = r.header("host").unwrap_or("");
            if r.path().contains("dataset=userinfo") {
                // A backslash before an `@`: WHATWG ends the authority at the backslash, so the host is `h`;
                // a parser that does not would read userinfo `h\%FF` and send it to this server. The client
                // refuses both.
                let tiles = format!(r#"["http://h\\%FF@{host}/t/a.json","http://h\\@{host}/t/b.json"]"#);
                respond(s, 200, &json, format!(r#"{{"tiles":{tiles}}}"#).as_bytes());
            } else if r.path().contains("dataset=failing") {
                // One tile over CAP and one 404.
                let tiles = format!(r#"["http://{host}/bytes/{}","http://{host}/json404"]"#, CAP + 1);
                respond(s, 200, &json, format!(r#"{{"tiles":{tiles}}}"#).as_bytes());
            } else if r.path().contains("dataset=margins") {
                // A tile URL with a `..` segment and a backslash: the margins CONFORMANCE.md lists.
                respond(s, 200, &json, format!(r#"{{"tiles":["http://{host}/t/x/../a\\b.json"]}}"#).as_bytes());
            } else {
                respond(s, 200, &json, format!(r#"{{"tiles":["http://{host}/t/a b\tc.json"]}}"#).as_bytes());
            }
        }
        "t" => respond(s, 200, &json, br#"{"records":[]}"#),
        _ => respond(s, 200, &json, b"{}"),
    }
}

/// The header set both transports must send, the same on every request (contract §12, item 5).
fn assert_request_headers(requests: &[Request]) {
    assert_eq!(requests.len(), 2, "{requests:?}");
    let (a, b) = (&requests[0], &requests[1]);
    assert_ne!(a.path(), b.path());
    assert_eq!(a.names(), b.names(), "the same header names on both requests");
    let sorted = |r: &Request| {
        let mut h = r.headers.clone();
        h.sort();
        h
    };
    assert_eq!(sorted(a), sorted(b), "the same header values on both requests");
    assert_eq!(a.names(), BTreeSet::from(["accept", "accept-encoding", "host", "user-agent"]));
    assert_eq!(a.header("user-agent"), Some("atgeo-client-rust"));
    assert_eq!(a.header("accept"), Some("application/json"));
    assert_eq!(a.header("accept-encoding"), Some("gzip"));
    assert_eq!(a.header("accept-language"), None);
    assert_eq!(a.header("referer"), None);
}

/// A search whose tile URLs put a backslash before an `@` refuses both tiles without a request, so nothing
/// reaches the host after the `@`, and no `Authorization` header either.
fn assert_userinfo_behind_a_backslash_is_not_sent(server: &Server, result: &atgeo::SearchResult) {
    assert_eq!((result.tiles.fetched, result.tiles.failed), (0, 2), "{:?}", result.tile_failures);
    for f in &result.tile_failures {
        assert_eq!((f.kind, f.status), (atgeo::TileFailureKind::Refused, None), "{f:?}");
    }
    let requests = server.requests();
    assert!(requests.iter().all(|r| r.header("authorization").is_none()), "{requests:?}");
    assert_eq!(requests.len(), 1, "only the tile list: {requests:?}");
}

/// A search over a tile past the cap and a 404: a `Transport` failure and a `Status` one, in URL order.
fn assert_cap_and_404_reported(server: &Server, result: &atgeo::SearchResult) {
    use atgeo::{Error, TileFailureKind};
    let failures: Vec<_> = result.tile_failures.iter().map(|f| (f.url.clone(), f.kind, f.status)).collect();
    assert_eq!(
        failures,
        [
            (server.url(&format!("/bytes/{}", CAP + 1)), TileFailureKind::Transport, None),
            (server.url("/json404"), TileFailureKind::Status, Some(404)),
        ]
    );
    match result.tile_failures[0].error.as_deref() {
        Some(Error::Transport(e)) => assert!(e.to_string().contains("exceeds max_body_bytes"), "{e}"),
        other => panic!("{other:?}"),
    }
    match result.tile_failures[1].error.as_deref() {
        Some(Error::Atgeo(e)) => assert_eq!((e.status, e.code.as_str()), (404, "NotFound")),
        other => panic!("{other:?}"),
    }
}

/// The tile request a search sent for `http://…/t/a b\tc.json`: the space as `%20`, the tab dropped.
fn assert_space_and_tab_prepared(server: &Server) {
    let lines: Vec<String> = server.requests().into_iter().map(|r| r.line).collect();
    assert!(lines.iter().any(|l| l == "GET /t/a%20bc.json HTTP/1.1"), "{lines:?}");
}

// ---------------------------------------------------------------------------------------------------------

#[cfg(feature = "reqwest")]
mod reqwest_cases {
    use std::sync::mpsc;
    use std::time::Instant;

    use atgeo::{HttpTransport, Region, ReqwestTransport, SearchOptions};

    use super::*;

    fn transport_with(read_timeout: Duration) -> ReqwestTransport {
        let client = ReqwestTransport::client_builder()
            .connect_timeout(Duration::from_secs(2))
            .read_timeout(read_timeout)
            .no_proxy()
            .build()
            .unwrap();
        ReqwestTransport::with_client(client)
    }

    fn transport() -> ReqwestTransport {
        transport_with(Duration::from_millis(300))
    }

    #[tokio::test]
    async fn a_4xx_is_ok_with_its_status_and_body() {
        let server = Server::start(routes);
        let r = transport().get(&server.url("/json404")).await.unwrap();
        assert_eq!((r.status, r.body.as_slice()), (404, ERROR_JSON));
        let r = transport().get(&server.url("/empty400")).await.unwrap();
        assert_eq!((r.status, r.body.as_slice()), (400, &b""[..]));
    }

    #[tokio::test]
    async fn follows_a_same_scheme_redirect() {
        let server = Server::start(routes);
        let r = transport().get(&server.url("/redirect")).await.unwrap();
        assert_eq!((r.status, r.body.as_slice()), (200, &b"landed"[..]));
        let paths: Vec<String> = server.requests().iter().map(|r| r.path().to_owned()).collect();
        assert_eq!(paths, ["/redirect", "/target"]);
    }

    #[tokio::test]
    async fn decodes_gzip_and_reads_an_empty_gzip_body_as_empty() {
        let server = Server::start(routes);
        let r = transport().get(&server.url("/gzip")).await.unwrap();
        assert_eq!((r.status, r.body.as_slice()), (200, TILE_JSON));
        let r = transport().get(&server.url("/gzip-empty")).await.unwrap();
        assert_eq!((r.status, r.body.len()), (200, 0));
    }

    #[tokio::test]
    async fn caps_the_decoded_body() {
        let server = Server::start(routes);
        // A long read timeout, so that only the cap can fail these, however slow the runner.
        let t = transport_with(Duration::from_secs(10)).max_body_bytes(CAP);
        let over_cap = |e: atgeo::BoxError| assert!(e.to_string().contains("exceeds max_body_bytes"), "{e}");
        assert_eq!(t.get(&server.url(&format!("/bytes/{CAP}"))).await.unwrap().body.len(), CAP);
        over_cap(t.get(&server.url(&format!("/bytes/{}", CAP + 1))).await.unwrap_err());
        assert_eq!(t.get(&server.url(&format!("/zeros-gz/{CAP}"))).await.unwrap().body.len(), CAP);
        assert!(gzip(&vec![0; 4 << 20]).len() < 16 * 1024, "a few kilobytes on the wire");
        over_cap(t.get(&server.url(&format!("/zeros-gz/{}", 4 << 20))).await.unwrap_err());
    }

    #[tokio::test]
    async fn a_trickle_slower_than_the_read_timeout_fails_and_a_faster_one_succeeds() {
        let server = Server::start(routes);
        let start = Instant::now();
        assert!(transport().get(&server.url("/trickle/1500")).await.is_err());
        assert!(start.elapsed() < Duration::from_millis(1200), "{:?}", start.elapsed());
        // 10 bytes 50 ms apart: 450 ms in all, longer than the 300 ms timer, which is per read.
        let r = transport().get(&server.url("/trickle/50")).await.unwrap();
        assert_eq!(r.body, b"0123456789");
    }

    #[tokio::test]
    async fn sends_the_same_fixed_headers_on_every_request() {
        let server = Server::start(routes);
        let t = transport();
        t.get(&server.url("/first")).await.unwrap();
        t.get(&server.url("/json404?q=1")).await.unwrap();
        assert_request_headers(&server.requests());
    }

    #[tokio::test]
    async fn sends_no_referer_after_a_redirect_to_another_host() {
        let away = Server::start_localhost(routes);
        let server = Server::start(routes);
        let url = server.url(&format!("/away/{}?bbox=1.00,2.00,3.00,4.00", away.port));
        assert_eq!(transport().get(&url).await.unwrap().status, 200);
        let landed = away.requests();
        assert_eq!(landed.len(), 1);
        assert_eq!(landed[0].header("host"), Some(format!("localhost:{}", away.port).as_str()));
        assert_eq!(landed[0].header("referer"), None);

        // The control: a client not built from client_builder() does send it, the bbox and all.
        let plain = ReqwestTransport::with_client(reqwest::Client::builder().no_proxy().build().unwrap());
        plain.get(&url).await.unwrap();
        assert_eq!(away.requests()[1].header("referer"), Some(url.as_str()));
    }

    #[tokio::test]
    async fn a_url_with_a_space_and_a_tab_arrives_prepared() {
        let server = Server::start(routes);
        let client = atgeo::AtgeoClient::with_transport(transport()).server(server.url(""));
        let result = client
            .search_places(
                Region::bbox(0.0, 0.0, 0.01, 0.01),
                SearchOptions::new("overturemaps.org").collection("place"),
            )
            .await
            .unwrap();
        assert_eq!((result.tiles.fetched, result.tiles.failed), (1, 0));
        assert_space_and_tab_prepared(&server);
    }

    // CONFORMANCE.md: reqwest reads a tile URL's backslash and dot segments as WHATWG does.
    #[tokio::test]
    async fn reads_a_backslash_and_dot_segments_in_a_tile_url_as_whatwg_does() {
        let server = Server::start(routes);
        let client = atgeo::AtgeoClient::with_transport(transport()).server(server.url(""));
        let options = SearchOptions::new("margins");
        let result = client.search_places(Region::bbox(0.0, 0.0, 0.01, 0.01), options).await.unwrap();
        assert_eq!((result.tiles.fetched, result.tiles.failed), (1, 0));
        assert_eq!(server.requests()[1].path(), "/t/a/b.json");
    }

    #[tokio::test]
    async fn sends_no_userinfo_that_follows_a_backslash() {
        let server = Server::start(routes);
        let client = atgeo::AtgeoClient::with_transport(transport()).server(server.url(""));
        let options = SearchOptions::new("userinfo");
        let result = client.search_places(Region::bbox(0.0, 0.0, 0.01, 0.01), options).await.unwrap();
        assert_userinfo_behind_a_backslash_is_not_sent(&server, &result);
    }

    #[tokio::test]
    async fn reports_a_tile_over_the_cap_and_a_404() {
        let server = Server::start(routes);
        let t = transport_with(Duration::from_secs(10)).max_body_bytes(CAP);
        let client = atgeo::AtgeoClient::with_transport(t).server(server.url(""));
        let options = SearchOptions::new("failing");
        let result = client.search_places(Region::bbox(0.0, 0.0, 0.01, 0.01), options).await.unwrap();
        assert_cap_and_404_reported(&server, &result);
    }

    #[tokio::test]
    async fn refuses_a_redirect_to_file() {
        let server = Server::start(routes);
        let error = transport().get(&server.url("/to-file")).await.unwrap_err();
        assert!(error.to_string().contains("redirect not followed"), "{error}");
        assert_eq!(server.requests().len(), 1);
    }

    // client_builder()'s redirect policy replaces reqwest's own, so its limit is tested here.
    #[tokio::test]
    async fn follows_ten_redirects_and_refuses_an_eleventh() {
        let server = Server::start(routes);
        let r = transport().get(&server.url("/hop/10/0")).await.unwrap();
        assert_eq!((r.status, r.body.as_slice()), (200, &b"end of the chain"[..]));
        assert_eq!(server.requests().len(), 11);

        let server = Server::start(routes);
        let error = transport().get(&server.url("/hop/11/0")).await.unwrap_err();
        let source = error.source().map(|s| s.to_string()).unwrap_or_default();
        assert!(source.contains("more than 10 redirects"), "{error}: {source}");
        assert_eq!(server.requests().len(), 11, "the eleventh redirect's target is never requested");
    }

    #[tokio::test]
    async fn refuses_a_redirect_to_a_url_with_credentials() {
        let server = Server::start(routes);
        let error = transport().get(&server.url("/to-credentials")).await.unwrap_err();
        assert!(!format!("{error:?}").contains("pw"), "{error:?}");
        let mut chain = error.to_string();
        let mut source = error.source();
        while let Some(s) = source {
            chain = format!("{chain}: {s}");
            source = s.source();
        }
        assert!(chain.contains("carries credentials") && !chain.contains("pw"), "{chain}");
        assert_eq!(server.requests().len(), 1);
    }

    #[tokio::test]
    async fn removes_dot_segments_from_a_redirect() {
        let server = Server::start(routes);
        let r = transport().get(&server.url("/dots")).await.unwrap();
        assert_eq!((r.status, r.body.as_slice()), (200, &b"landed"[..]));
    }

    // Multi-threaded: the socket belongs to a task reqwest spawned, which a current-thread runtime would
    // not run again until the test next awaits (spec §4).
    #[tokio::test(flavor = "multi_thread")]
    async fn dropping_the_future_mid_body_closes_the_connection() {
        let (tx, rx) = mpsc::channel::<Option<Instant>>();
        let tx = Mutex::new(tx);
        let server = Server::start(move |_, s| {
            head(s, 200, &[], MIB);
            let _ = s.write_all(&[b'x'; 100]);
            // Nothing more is sent; wait for the client to close (EOF or a reset), for up to 3 s.
            let _ = s.set_read_timeout(Some(Duration::from_secs(3)));
            let closed = match s.read(&mut [0; 16]) {
                Ok(0) => true,
                Ok(_) => false,
                Err(e) => !matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut),
            };
            let _ = tx.lock().unwrap().send(closed.then(Instant::now));
        });
        // A long read timeout, so that only the drop can close the connection.
        let t = transport_with(Duration::from_secs(10));
        let pending = tokio::time::timeout(Duration::from_secs(1), t.get(&server.url("/body"))).await;
        assert!(pending.is_err(), "the body never completes, so the request is still pending");
        let dropped = Instant::now();
        let observed = tokio::task::spawn_blocking(move || rx.recv_timeout(Duration::from_secs(5)))
            .await
            .unwrap()
            .expect("the server reports");
        let closed_at = observed.expect("the server saw the connection close");
        assert!(closed_at.saturating_duration_since(dropped) < Duration::from_secs(2));
    }
}

// ---------------------------------------------------------------------------------------------------------

#[cfg(feature = "ureq")]
mod ureq_cases {
    use std::sync::mpsc;
    use std::time::Instant;

    use atgeo::blocking::{HttpTransport, UreqTransport};
    use atgeo::{Region, SearchOptions};

    use super::*;

    fn transport_with(per_read: Duration, global: Duration) -> UreqTransport {
        let config = UreqTransport::config_builder()
            .timeout_connect(Some(Duration::from_secs(2)))
            .timeout_per_read(Some(per_read))
            .timeout_global(Some(global))
            .proxy(None)
            .build();
        UreqTransport::with_agent(config.into())
    }

    fn transport() -> UreqTransport {
        transport_with(Duration::from_millis(300), Duration::from_secs(5))
    }

    #[test]
    fn a_4xx_is_ok_with_its_status_and_body() {
        let server = Server::start(routes);
        let r = transport().get(&server.url("/json404")).unwrap();
        assert_eq!((r.status, r.body.as_slice()), (404, ERROR_JSON));
        let r = transport().get(&server.url("/empty400")).unwrap();
        assert_eq!((r.status, r.body.as_slice()), (400, &b""[..]));
    }

    #[test]
    fn follows_a_same_scheme_redirect() {
        let server = Server::start(routes);
        let r = transport().get(&server.url("/redirect")).unwrap();
        assert_eq!((r.status, r.body.as_slice()), (200, &b"landed"[..]));
        let paths: Vec<String> = server.requests().iter().map(|r| r.path().to_owned()).collect();
        assert_eq!(paths, ["/redirect", "/target"]);
    }

    #[test]
    fn decodes_gzip_and_reads_an_empty_gzip_body_as_empty() {
        let server = Server::start(routes);
        let r = transport().get(&server.url("/gzip")).unwrap();
        assert_eq!((r.status, r.body.as_slice()), (200, TILE_JSON));
        let r = transport().get(&server.url("/gzip-empty")).unwrap();
        assert_eq!((r.status, r.body.len()), (200, 0));
    }

    #[test]
    fn caps_the_decoded_body() {
        let server = Server::start(routes);
        // Long timeouts, so that only the cap can fail these, however slow the runner.
        let t = transport_with(Duration::from_secs(10), Duration::from_secs(30)).max_body_bytes(CAP);
        let over_cap = |e: atgeo::BoxError| assert!(e.to_string().contains("exceeds max_body_bytes"), "{e}");
        assert_eq!(t.get(&server.url(&format!("/bytes/{CAP}"))).unwrap().body.len(), CAP);
        over_cap(t.get(&server.url(&format!("/bytes/{}", CAP + 1))).unwrap_err());
        assert_eq!(t.get(&server.url(&format!("/zeros-gz/{CAP}"))).unwrap().body.len(), CAP);
        assert!(gzip(&vec![0; 4 << 20]).len() < 16 * 1024, "a few kilobytes on the wire");
        over_cap(t.get(&server.url(&format!("/zeros-gz/{}", 4 << 20))).unwrap_err());
    }

    #[test]
    fn a_trickle_slower_than_the_read_timeout_fails_and_a_faster_one_succeeds() {
        let server = Server::start(routes);
        let start = Instant::now();
        assert!(transport().get(&server.url("/trickle/1500")).is_err());
        assert!(start.elapsed() < Duration::from_millis(1200), "{:?}", start.elapsed());
        let r = transport().get(&server.url("/trickle/50")).unwrap();
        assert_eq!(r.body, b"0123456789");
    }

    #[test]
    fn the_total_timeout_fires_against_a_steady_trickle() {
        let server = Server::start(|_, s| trickle(s, &[b'x'; 60], Duration::from_millis(50)));
        // A byte every 50 ms never trips the 300 ms idle timer; the 800 ms total stops it.
        let t = transport_with(Duration::from_millis(300), Duration::from_millis(800));
        let start = Instant::now();
        assert!(t.get(&server.url("/slow")).is_err());
        let elapsed = start.elapsed();
        assert!(elapsed >= Duration::from_millis(700) && elapsed < Duration::from_millis(2000), "{elapsed:?}");
    }

    #[test]
    fn sends_the_same_fixed_headers_on_every_request() {
        let server = Server::start(routes);
        let t = transport();
        t.get(&server.url("/first")).unwrap();
        t.get(&server.url("/json404?q=1")).unwrap();
        assert_request_headers(&server.requests());
    }

    #[test]
    fn sends_no_referer_after_a_redirect_to_another_host() {
        let away = Server::start_localhost(routes);
        let server = Server::start(routes);
        let url = server.url(&format!("/away/{}?bbox=1.00,2.00,3.00,4.00", away.port));
        assert_eq!(transport().get(&url).unwrap().status, 200);
        let landed = away.requests();
        assert_eq!(landed.len(), 1);
        assert_eq!(landed[0].header("host"), Some(format!("localhost:{}", away.port).as_str()));
        assert_eq!(landed[0].header("referer"), None);
    }

    #[test]
    fn follows_ten_redirects_and_refuses_an_eleventh() {
        let server = Server::start(routes);
        let r = transport().get(&server.url("/hop/10/0")).unwrap();
        assert_eq!((r.status, r.body.as_slice()), (200, &b"end of the chain"[..]));
        assert_eq!(server.requests().len(), 11);

        let server = Server::start(routes);
        let error = transport().get(&server.url("/hop/11/0")).unwrap_err();
        assert!(error.to_string().contains("more than 10 redirects"), "{error}");
        let paths: Vec<String> = server.requests().iter().map(|r| r.path().to_owned()).collect();
        assert_eq!(paths.len(), 11, "the eleventh redirect's target is never requested: {paths:?}");
        assert_eq!(paths.last().map(String::as_str), Some("/hop/11/10"));
    }

    #[test]
    fn decodes_a_multi_member_gzip_body_and_x_gzip() {
        let server = Server::start(routes);
        let r = transport().get(&server.url("/gzip-multi")).unwrap();
        assert_eq!((r.status, r.body.as_slice()), (200, TILE_JSON));
        let r = transport().get(&server.url("/x-gzip")).unwrap();
        assert_eq!((r.status, r.body.as_slice()), (200, TILE_JSON));
    }

    #[test]
    fn removes_dot_segments_from_a_redirect() {
        let server = Server::start(routes);
        let r = transport().get(&server.url("/dots")).unwrap();
        assert_eq!((r.status, r.body.as_slice()), (200, &b"landed"[..]));
        let paths: Vec<String> = server.requests().iter().map(|r| r.path().to_owned()).collect();
        assert_eq!(paths, ["/dots", "/target"]);
    }

    #[test]
    fn the_total_timeout_covers_the_whole_chain_of_redirects() {
        // Each hop takes 200 ms, far within the 1.5 s total, but eight of them do not fit in it.
        let t = transport_with(Duration::from_secs(1), Duration::from_millis(1500));
        let server = Server::start(routes);
        assert!(t.get(&server.url("/slowhop/1/0")).is_ok());
        let server = Server::start(routes);
        let started = Instant::now();
        assert!(t.get(&server.url("/slowhop/7/0")).is_err());
        assert!(started.elapsed() < Duration::from_millis(3000), "{:?}", started.elapsed());
    }

    // CONFORMANCE.md: ureq sends a tile URL's backslash and dot segments as given.
    #[test]
    fn sends_a_backslash_and_dot_segments_in_a_tile_url_as_given() {
        let server = Server::start(routes);
        let client = atgeo::blocking::AtgeoClient::with_transport(transport()).server(server.url(""));
        let options = SearchOptions::new("margins");
        let result = client.search_places(Region::bbox(0.0, 0.0, 0.01, 0.01), options).unwrap();
        assert_eq!((result.tiles.fetched, result.tiles.failed), (1, 0));
        assert_eq!(server.requests()[1].path(), "/t/x/../a\\b.json");
    }

    #[test]
    fn sends_no_userinfo_that_follows_a_backslash() {
        let server = Server::start(routes);
        let client = atgeo::blocking::AtgeoClient::with_transport(transport()).server(server.url(""));
        let options = SearchOptions::new("userinfo");
        let result = client.search_places(Region::bbox(0.0, 0.0, 0.01, 0.01), options).unwrap();
        assert_userinfo_behind_a_backslash_is_not_sent(&server, &result);
    }

    #[test]
    fn reports_a_tile_over_the_cap_and_a_404() {
        let server = Server::start(routes);
        let t = transport_with(Duration::from_secs(10), Duration::from_secs(20)).max_body_bytes(CAP);
        let client = atgeo::blocking::AtgeoClient::with_transport(t).server(server.url(""));
        let options = SearchOptions::new("failing");
        let result = client.search_places(Region::bbox(0.0, 0.0, 0.01, 0.01), options).unwrap();
        assert_cap_and_404_reported(&server, &result);
    }

    #[test]
    fn refuses_a_redirect_to_file() {
        let server = Server::start(routes);
        let error = transport().get(&server.url("/to-file")).unwrap_err();
        assert!(error.to_string().contains("not http or https"), "{error}");
        assert_eq!(server.requests().len(), 1);
    }

    #[test]
    fn refuses_a_redirect_to_a_url_with_credentials() {
        let server = Server::start(routes);
        let error = transport().get(&server.url("/to-credentials")).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("without credentials") && message.ends_with("/target"), "{message}");
        assert!(!message.contains("user") && !message.contains("pw"), "{message}");
        assert_eq!(server.requests().len(), 1);
    }

    #[test]
    fn leaves_a_redirects_huge_body_unread() {
        const DECLARED: u64 = 300 * MIB;
        let (tx, rx) = mpsc::channel::<u64>();
        let tx = Mutex::new(tx);
        let server = Server::start(move |r, s| {
            if r.path() != "/big" {
                return respond(s, 200, &[], b"small");
            }
            if !head(s, 302, &[("Location", "/small")], DECLARED) {
                return;
            }
            let chunk = vec![0u8; 64 * 1024];
            let mut written = 0;
            while written < DECLARED {
                match s.write(&chunk[..chunk.len().min((DECLARED - written) as usize)]) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => written += n as u64,
                }
            }
            let _ = tx.lock().unwrap().send(written);
        });
        let r = transport().get(&server.url("/big")).unwrap();
        assert_eq!((r.status, r.body.as_slice()), (200, &b"small"[..]));
        let written = rx.recv_timeout(Duration::from_secs(10)).expect("the server stops writing");
        // Whatever fit in the sockets' buffers before the client closed; nothing like 300 MiB.
        assert!(written < DECLARED / 20, "the server wrote {written} of {DECLARED} bytes");
    }

    #[test]
    fn a_url_with_a_space_and_a_tab_arrives_prepared() {
        let server = Server::start(routes);
        let client = atgeo::blocking::AtgeoClient::with_transport(transport()).server(server.url(""));
        let result = client
            .search_places(
                Region::bbox(0.0, 0.0, 0.01, 0.01),
                SearchOptions::new("overturemaps.org").collection("place"),
            )
            .unwrap();
        assert_eq!((result.tiles.fetched, result.tiles.failed), (1, 0));
        assert_space_and_tab_prepared(&server);
    }
}
