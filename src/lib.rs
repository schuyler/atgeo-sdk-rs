#![cfg_attr(all(feature = "reqwest", feature = "ureq"), doc = include_str!("../README.md"))]
#![cfg_attr(
    not(all(feature = "reqwest", feature = "ureq")),
    doc = "A client for the atgeo gazetteer that searches places without telling the server what you searched for. See the README."
)]

mod cache;
mod client;
mod engine;
mod error;
mod locations;
mod rank;
mod region;
mod text;
mod transport;
mod types;
mod wire;
mod xrpc;

#[cfg(feature = "blocking")]
pub mod blocking;

pub use client::AtgeoClient;
pub use error::{ArgumentError, AtgeoError, DecodingError, Error};
#[cfg(feature = "reqwest")]
pub use transport::ReqwestTransport;
pub use transport::{BoxError, HttpResponse, HttpTransport};
pub use types::{
    CategoryCount, CollectionAttribution, CollectionInfo, CollectionRef, Extent, Place, PlaceBox, PlaceLocation,
    PlacePoint, PlaceRecord, Region, SearchHit, SearchOptions, SearchResult, ServedTileset, ServiceDescription,
    SortMode, TileCounts, Tileset,
};

/// The server a client talks to unless told otherwise.
pub const DEFAULT_SERVER: &str = engine::DEFAULT_SERVER;

/// The tile cache's default cap: 50 MiB of response body.
pub const DEFAULT_CACHE_BUDGET_BYTES: u64 = engine::DEFAULT_CACHE_BUDGET_BYTES;
