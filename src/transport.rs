//! The seam between the client and HTTP (spec §4), and the async client's default transport.

use std::future::Future;
use std::sync::Arc;

/// Whatever a transport fails with.
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// The user agent the default transports send: it names the port and nothing else, no version and
/// nothing about the device (contract §12, item 5).
#[cfg(any(feature = "reqwest", feature = "ureq"))]
pub(crate) const USER_AGENT: &str = "atgeo-client-rust";

/// A response with its body decoded: no `Content-Encoding` left on it.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn new(status: u16, body: Vec<u8>) -> Self {
        HttpResponse { status, body }
    }
}

/// How the async client makes requests.
///
/// Implement it with an `async fn`:
///
/// ```
/// use atgeo::{BoxError, HttpResponse, HttpTransport};
///
/// struct Offline;
///
/// impl HttpTransport for Offline {
///     async fn get(&self, _url: &str) -> Result<HttpResponse, BoxError> {
///         Err("offline".into())
///     }
/// }
/// ```
///
/// The future must be `Send`, which the compiler checks at the `impl`, so that the client's own futures
/// can run on a multi-threaded executor.
pub trait HttpTransport: Send + Sync {
    /// GET the URL exactly as given. Return a response for every status; fail only when no response
    /// arrived. Must not cache by default. Dropping the returned future must abandon the request.
    fn get(&self, url: &str) -> impl Future<Output = Result<HttpResponse, BoxError>> + Send;
}

impl<T: HttpTransport> HttpTransport for &T {
    fn get(&self, url: &str) -> impl Future<Output = Result<HttpResponse, BoxError>> + Send {
        (**self).get(url)
    }
}

impl<T: HttpTransport> HttpTransport for Arc<T> {
    fn get(&self, url: &str) -> impl Future<Output = Result<HttpResponse, BoxError>> + Send {
        (**self).get(url)
    }
}

/// The default cap on a decoded response body.
#[cfg(any(feature = "reqwest", feature = "ureq"))]
pub(crate) const DEFAULT_MAX_BODY_BYTES: usize = 32 * 1024 * 1024;

#[cfg(feature = "reqwest")]
pub use self::reqwest_transport::ReqwestTransport;

#[cfg(feature = "reqwest")]
mod reqwest_transport {
    use std::time::Duration;

    use reqwest::header::{ACCEPT, HeaderValue, USER_AGENT};

    use super::{BoxError, DEFAULT_MAX_BODY_BYTES, HttpResponse, HttpTransport};

    /// The async client's default transport, over reqwest. It needs a Tokio runtime.
    ///
    /// It sends `Accept: application/json` and `User-Agent: atgeo-client-rust`, no `Referer`, and stops a
    /// body once it decodes past `max_body_bytes` (32 MiB by default).
    #[derive(Clone, Debug)]
    pub struct ReqwestTransport {
        client: reqwest::Client,
        max_body_bytes: usize,
    }

    impl ReqwestTransport {
        /// A transport over a client built from [`client_builder`](Self::client_builder).
        ///
        /// # Panics
        ///
        /// If the TLS backend cannot be initialised, as `reqwest::Client::new` does.
        pub fn new() -> Self {
            Self::with_client(Self::client_builder().build().expect("the TLS backend initialises"))
        }

        /// The settings the default client uses: no `Referer` on a redirect, a 10 s connect timeout and a
        /// 30 s idle timeout per read. Start here for a client of your own (HTTP/2, a proxy, your own TLS),
        /// so that it keeps them.
        pub fn client_builder() -> reqwest::ClientBuilder {
            reqwest::Client::builder()
                .referer(false)
                .connect_timeout(Duration::from_secs(10))
                .read_timeout(Duration::from_secs(30))
        }

        /// A transport over an app's own client. Build it from [`client_builder`](Self::client_builder):
        /// a client from `reqwest::Client::builder()` sends the previous URL as `Referer` on a redirect.
        pub fn with_client(client: reqwest::Client) -> Self {
            ReqwestTransport { client, max_body_bytes: DEFAULT_MAX_BODY_BYTES }
        }

        /// The largest decoded body accepted; a larger one fails the request.
        pub fn max_body_bytes(mut self, bytes: usize) -> Self {
            self.max_body_bytes = bytes;
            self
        }
    }

    impl Default for ReqwestTransport {
        fn default() -> Self {
            Self::new()
        }
    }

    impl HttpTransport for ReqwestTransport {
        async fn get(&self, url: &str) -> Result<HttpResponse, BoxError> {
            let mut response = self
                .client
                .get(url)
                .header(ACCEPT, HeaderValue::from_static("application/json"))
                .header(USER_AGENT, HeaderValue::from_static(super::USER_AGENT))
                .send()
                .await?;
            let status = response.status().as_u16();
            // Count decoded bytes as they arrive; Response::bytes() would hold the whole body first.
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await? {
                if chunk.len() > self.max_body_bytes - body.len() {
                    return Err("response body exceeds max_body_bytes".into());
                }
                body.extend_from_slice(&chunk);
            }
            Ok(HttpResponse::new(status, body))
        }
    }
}
