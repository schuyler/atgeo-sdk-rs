use std::fmt;

use crate::transport::BoxError;

/// Everything a call can fail with.
#[non_exhaustive]
#[derive(Debug)]
pub enum Error {
    /// A non-2xx answer from the server (contract §1).
    Atgeo(AtgeoError),
    /// A 2xx answer the client could not use: not JSON, or without what the call needs (contract §1).
    Decoding(DecodingError),
    /// No answer arrived: whatever the transport returned, boxed. `downcast_ref` reaches the
    /// transport's own error, such as a `reqwest::Error`.
    Transport(BoxError),
    /// A bad argument, found before any request (contract §3).
    InvalidArgument(ArgumentError),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Atgeo(e) => e.fmt(f),
            Error::Decoding(e) => e.fmt(f),
            Error::Transport(e) => write!(f, "transport error: {e}"),
            Error::InvalidArgument(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Atgeo(e) => Some(e),
            Error::Decoding(e) => Some(e),
            Error::Transport(e) => Some(e.as_ref()),
            Error::InvalidArgument(e) => Some(e),
        }
    }
}

impl From<AtgeoError> for Error {
    fn from(e: AtgeoError) -> Self {
        Error::Atgeo(e)
    }
}

impl From<DecodingError> for Error {
    fn from(e: DecodingError) -> Self {
        Error::Decoding(e)
    }
}

impl From<ArgumentError> for Error {
    fn from(e: ArgumentError) -> Self {
        Error::InvalidArgument(e)
    }
}

/// The server refused a call or failed: a non-2xx response.
///
/// `code` is the server's error name (`BboxTooLarge`, `PlaceNotFound`, …), or `HttpError` when the body
/// named none. The list of codes is open; report them rather than matching on them. `status` tells a
/// refused request (4xx) from a server failure (5xx).
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AtgeoError {
    pub code: String,
    pub message: String,
    pub status: u16,
}

impl AtgeoError {
    pub(crate) fn new(code: String, message: String, status: u16) -> Self {
        AtgeoError { code, message, status }
    }
}

impl fmt::Display for AtgeoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

impl std::error::Error for AtgeoError {}

/// A 2xx response the client could not use: a body that is not JSON, or one without what the call needs.
#[derive(Debug)]
pub struct DecodingError {
    message: String,
    source: Option<serde_json::Error>,
}

impl DecodingError {
    pub(crate) fn json(source: serde_json::Error) -> Self {
        DecodingError { message: format!("response is not JSON: {source}"), source: Some(source) }
    }

    pub(crate) fn shape(message: &str) -> Self {
        DecodingError { message: message.to_owned(), source: None }
    }

    /// What was wrong with the body.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for DecodingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for DecodingError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_ref().map(|e| e as _)
    }
}

/// An argument the client rejected before making any request.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArgumentError {
    /// A region with a non-finite or out-of-range coordinate, a south edge above its north edge, or a
    /// negative or non-finite radius. The string says which kind of region it was.
    InvalidRegion(&'static str),
    /// `SortMode::Similarity` with no text, or text that normalizes to nothing.
    SimilarityRequiresText,
    /// The configured server is not an absolute http or https URL with a host and no credentials.
    InvalidServer,
}

impl fmt::Display for ArgumentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ArgumentError::InvalidRegion(what) => f.write_str(what),
            ArgumentError::SimilarityRequiresText => f.write_str("sort 'similarity' requires text"),
            ArgumentError::InvalidServer => {
                f.write_str("server is not an absolute http or https URL without credentials")
            }
        }
    }
}

impl std::error::Error for ArgumentError {}
