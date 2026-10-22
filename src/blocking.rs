//! A blocking client with the same methods and results as the async one, for code without an executor
//! (feature `blocking`), and its default transport over ureq (feature `ureq`).
//!
//! A search runs on scoped threads: the one or two tile lists in parallel, then at most eight tile
//! fetches at a time. It has no cancellation beyond the transport's timeouts.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::SystemTime;

use crate::engine::{Engine, ListStep, MAX_TILES_IN_FLIGHT, Slot, Step, TileStep};
use crate::error::Error;
use crate::transport::{BoxError, HttpResponse};
use crate::types::{PlaceRecord, Region, SearchOptions, SearchResult, ServiceDescription, Tileset};
use crate::wire::TileList;

/// How the blocking client makes requests.
pub trait HttpTransport: Send + Sync {
    /// GET the URL exactly as given. Return a response for every status; fail only when no response
    /// arrived. Must not cache by default. Called from several threads at once.
    fn get(&self, url: &str) -> Result<HttpResponse, BoxError>;
}

impl<T: HttpTransport> HttpTransport for &T {
    fn get(&self, url: &str) -> Result<HttpResponse, BoxError> {
        (**self).get(url)
    }
}

impl<T: HttpTransport> HttpTransport for Arc<T> {
    fn get(&self, url: &str) -> Result<HttpResponse, BoxError> {
        (**self).get(url)
    }
}

/// A blocking client for one atgeo server; see the async [`crate::AtgeoClient`], which this mirrors.
#[cfg(feature = "ureq")]
pub struct AtgeoClient<T = UreqTransport> {
    transport: T,
    engine: Engine,
}

/// A blocking client for one atgeo server; see the async [`crate::AtgeoClient`], which this mirrors.
#[cfg(not(feature = "ureq"))]
pub struct AtgeoClient<T> {
    transport: T,
    engine: Engine,
}

#[cfg(feature = "ureq")]
impl AtgeoClient<UreqTransport> {
    /// A client for the default server, over [`UreqTransport::new`].
    pub fn new() -> Self {
        Self::with_transport(UreqTransport::new())
    }
}

#[cfg(feature = "ureq")]
impl Default for AtgeoClient<UreqTransport> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> fmt::Debug for AtgeoClient<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.engine.fmt(f)
    }
}

/// Re-raises a worker thread's panic in the caller.
fn joined<R>(handle: thread::ScopedJoinHandle<'_, R>) -> R {
    handle.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

impl<T: HttpTransport> AtgeoClient<T> {
    /// A client for the default server, over this transport.
    pub fn with_transport(transport: T) -> Self {
        AtgeoClient { transport, engine: Engine::default() }
    }

    /// The server's base URL; trailing slashes are stripped. Default: `https://places.atgeo.org`.
    pub fn server(self, server: impl Into<String>) -> Self {
        AtgeoClient { transport: self.transport, engine: self.engine.with_server(&server.into()) }
    }

    /// The cap on the tile cache, in bytes of response body. Default: 50 MiB.
    pub fn cache_budget_bytes(self, bytes: u64) -> Self {
        AtgeoClient { transport: self.transport, engine: self.engine.with_budget(bytes) }
    }

    /// The wall clock the tile-list and service-description caches expire by. For tests.
    pub fn clock(self, clock: impl Fn() -> SystemTime + Send + Sync + 'static) -> Self {
        AtgeoClient { transport: self.transport, engine: self.engine.with_clock(Arc::new(clock)) }
    }

    fn list(&self, step: ListStep) -> Result<Arc<TileList>, Error> {
        match step {
            ListStep::Cached(list) => Ok(list),
            ListStep::Fetch { key, url } => self.engine.accept_tile_list(&key, self.transport.get(&url)),
        }
    }

    fn tile(&self, url: &str) -> Slot {
        match self.engine.tile_step(url) {
            TileStep::Cached(slot) => slot,
            TileStep::Failed => None,
            TileStep::Fetch(prepared) => self.engine.accept_tile(url, self.transport.get(&prepared)),
        }
    }

    /// Searches a region; see the async [`crate::AtgeoClient::search_places`].
    pub fn search_places(&self, region: Region, options: SearchOptions) -> Result<SearchResult, Error> {
        let mut plan = self.engine.plan(region, &options)?;
        let steps = std::mem::take(&mut plan.lists);
        let fetches = steps.iter().filter(|s| matches!(s, ListStep::Fetch { .. })).count();
        // Both halves are joined before returning, since a blocking request cannot be cancelled; collecting
        // into a Result keeps the error of the first in call order.
        let lists = if fetches > 1 {
            thread::scope(|s| {
                let handles: Vec<_> = steps.into_iter().map(|step| s.spawn(move || self.list(step))).collect();
                handles.into_iter().map(joined).collect::<Vec<_>>()
            })
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?
        } else {
            steps.into_iter().map(|step| self.list(step)).collect::<Result<Vec<_>, _>>()?
        };
        let (urls, overview) = Engine::tile_urls(&lists);

        let n = urls.len();
        let next = AtomicUsize::new(0);
        let worker = || {
            let mut done = Vec::new();
            loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= n {
                    return done;
                }
                done.push((i, self.tile(&urls[i])));
            }
        };
        let finished: Vec<Vec<(usize, Slot)>> = thread::scope(|s| {
            let handles: Vec<_> = (0..n.min(MAX_TILES_IN_FLIGHT)).map(|_| s.spawn(worker)).collect();
            handles.into_iter().map(joined).collect()
        });
        let mut slots: Vec<Slot> = vec![None; n];
        for (i, slot) in finished.into_iter().flatten() {
            slots[i] = slot;
        }
        Ok(self.engine.assemble(plan, urls, overview, slots))
    }

    /// One place by its origin; see the async [`crate::AtgeoClient::get_place`].
    pub fn get_place(&self, origin: &str) -> Result<PlaceRecord, Error> {
        match self.engine.place_step(origin)? {
            Step::Done(record) => Ok(record),
            Step::Fetch(url) => self.engine.accept_place(origin, self.transport.get(&url)),
        }
    }

    /// The server's identity and the tilesets it serves. Cached for an hour.
    pub fn describe_service(&self) -> Result<ServiceDescription, Error> {
        match self.engine.service_step()? {
            Step::Done(service) => Ok(service),
            Step::Fetch(url) => self.engine.accept_service(self.transport.get(&url)),
        }
    }

    /// A tileset's descriptor. Never cached.
    pub fn describe_tileset(&self, slug: &str) -> Result<Tileset, Error> {
        let url = self.engine.tileset_url(slug)?;
        self.engine.accept_tileset(self.transport.get(&url))
    }
}

#[cfg(feature = "ureq")]
pub use self::ureq_transport::UreqTransport;

#[cfg(feature = "ureq")]
mod ureq_transport {
    use std::io::{BufRead, BufReader, Read};
    use std::time::{Duration, Instant};

    use ureq::Agent;
    use ureq::config::ConfigBuilder;
    use ureq::tls::{RootCerts, TlsConfig};
    use ureq::typestate::AgentScope;

    use super::HttpTransport;
    use crate::transport::{BoxError, DEFAULT_MAX_BODY_BYTES, HttpResponse, USER_AGENT};
    use crate::xrpc::{is_http_url, prepare_url, resolve};

    const MAX_REDIRECTS: usize = 10;
    const BUFFER: usize = 64 * 1024;

    /// The blocking client's default transport, over ureq.
    ///
    /// It sends `Accept: application/json`, `Accept-Encoding: gzip` and `User-Agent: atgeo-client-rust`,
    /// decodes gzip itself so that `max_body_bytes` (32 MiB by default) counts decoded bytes, and follows
    /// up to ten redirects itself without reading their bodies.
    #[derive(Clone, Debug)]
    pub struct UreqTransport {
        agent: Agent,
        max_body_bytes: usize,
    }

    impl UreqTransport {
        /// A transport over an agent built from [`config_builder`](Self::config_builder).
        pub fn new() -> Self {
            Self::with_agent(Self::config_builder().build().into())
        }

        /// The settings the default agent uses: every status returned as a response, redirects handed back
        /// for the transport to follow, a 10 s connect timeout, a 30 s idle timeout per read, 120 s for the
        /// whole request, eight idle connections per host, and the operating system's root certificates.
        /// Start here for an agent of your own, and keep `http_status_as_error(false)` and
        /// `max_redirects(0)`.
        pub fn config_builder() -> ConfigBuilder<AgentScope> {
            Agent::config_builder()
                .http_status_as_error(false)
                .timeout_connect(Some(Duration::from_secs(10)))
                .timeout_per_read(Some(Duration::from_secs(30)))
                .timeout_global(Some(Duration::from_secs(120)))
                .max_idle_connections_per_host(8)
                .max_redirects(0)
                .max_redirects_will_error(false)
                .tls_config(TlsConfig::builder().root_certs(RootCerts::PlatformVerifier).build())
        }

        /// A transport over an app's own agent.
        pub fn with_agent(agent: Agent) -> Self {
            UreqTransport { agent, max_body_bytes: DEFAULT_MAX_BODY_BYTES }
        }

        /// The largest decoded body accepted; a larger one fails the request.
        pub fn max_body_bytes(mut self, bytes: usize) -> Self {
            self.max_body_bytes = bytes;
            self
        }

        fn read(&self, response: ureq::http::Response<ureq::Body>) -> Result<HttpResponse, BoxError> {
            let status = response.status().as_u16();
            // ureq decodes gzip itself, and removes the header, only if another crate turned its `gzip`
            // feature on; then this reads the decoded stream through the same capped loop.
            let gzip = response
                .headers()
                .get("content-encoding")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.trim().eq_ignore_ascii_case("gzip") || v.trim().eq_ignore_ascii_case("x-gzip"));
            let mut reader = BufReader::with_capacity(BUFFER, response.into_body().into_reader());
            let body = if gzip && !reader.fill_buf()?.is_empty() {
                read_capped(flate2::bufread::MultiGzDecoder::new(reader), self.max_body_bytes)?
            } else {
                read_capped(reader, self.max_body_bytes)?
            };
            Ok(HttpResponse::new(status, body))
        }
    }

    /// Reads to the end through a fixed buffer, failing once the total passes `max`.
    fn read_capped(mut reader: impl Read, max: usize) -> Result<Vec<u8>, BoxError> {
        let mut body = Vec::new();
        let mut buffer = vec![0; BUFFER];
        loop {
            let n = match reader.read(&mut buffer) {
                Ok(0) => return Ok(body),
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            };
            if n > max - body.len() {
                return Err("response body exceeds max_body_bytes".into());
            }
            body.extend_from_slice(&buffer[..n]);
        }
    }

    impl Default for UreqTransport {
        fn default() -> Self {
            Self::new()
        }
    }

    impl HttpTransport for UreqTransport {
        fn get(&self, url: &str) -> Result<HttpResponse, BoxError> {
            let mut current = url.to_owned();
            // The agent's total timeout covers the whole chain of redirects, not each hop.
            let deadline = self.agent.config().timeouts().global.map(|total| Instant::now() + total);
            for _ in 0..=MAX_REDIRECTS {
                let request = self
                    .agent
                    .get(&current)
                    .header("Accept", "application/json")
                    .header("Accept-Encoding", "gzip")
                    .header("User-Agent", USER_AGENT);
                let response = match deadline {
                    Some(deadline) => {
                        let left = deadline.saturating_duration_since(Instant::now());
                        if left.is_zero() {
                            return Err("timed out following redirects".into());
                        }
                        request.config().timeout_global(Some(left)).build().call()?
                    }
                    None => request.call()?,
                };
                let redirect = matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308);
                let location = response.headers().get("location").and_then(|v| v.to_str().ok());
                match location {
                    Some(location) if redirect => {
                        let next = prepare_url(&resolve(&current, location));
                        if !is_http_url(&next) {
                            return Err(format!("redirect to a URL that is not http or https: {next}").into());
                        }
                        // Dropped unread: a redirect's body is never downloaded.
                        drop(response);
                        current = next;
                    }
                    _ => return self.read(response),
                }
            }
            Err(format!("more than {MAX_REDIRECTS} redirects").into())
        }
    }
}
