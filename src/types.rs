use std::hash::{Hash, Hasher};
use std::sync::Arc;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::error::Error;
use crate::locations;

/// A place record's value, exactly as served: every key and nested value, unknown ones included.
///
/// It is shared rather than copied; `(*hit.place).clone()` gives an app its own copy.
pub type Place = Arc<serde_json::Map<String, serde_json::Value>>;

/// The area to search.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(tag = "kind", rename_all = "snake_case"))]
pub enum Region {
    /// A circle. Radius in metres.
    Circle { lat: f64, lon: f64, radius: f64 },
    /// A bounding box, in degrees. `min_lon > max_lon` crosses the antimeridian.
    Bbox { min_lon: f64, min_lat: f64, max_lon: f64, max_lat: f64 },
}

impl Region {
    /// A circle around `(lat, lon)` with `radius` in metres.
    pub const fn circle(lat: f64, lon: f64, radius: f64) -> Self {
        Region::Circle { lat, lon, radius }
    }

    /// A bounding box in degrees. `min_lon > max_lon` crosses the antimeridian.
    pub const fn bbox(min_lon: f64, min_lat: f64, max_lon: f64, max_lat: f64) -> Self {
        Region::Bbox { min_lon, min_lat, max_lon, max_lat }
    }
}

/// How a search orders its hits.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
pub enum SortMode {
    /// Text score, then importance, then distance.
    #[default]
    Fused,
    /// Distance, then the size of the place, then importance, then text score.
    Distance,
    /// Text score, then distance. Needs text.
    Similarity,
}

/// What to search for, besides the region.
///
/// ```
/// # use atgeo::{SearchOptions, SortMode};
/// let options = SearchOptions::new().text("Starbux").sort(SortMode::Distance);
/// ```
#[derive(Clone, Debug, Default)]
pub struct SearchOptions {
    pub(crate) text: Option<String>,
    pub(crate) dataset: Option<String>,
    pub(crate) collection: Option<String>,
    pub(crate) sort: SortMode,
}

impl SearchOptions {
    /// No text, the default dataset and collection, fused order.
    pub fn new() -> Self {
        Self::default()
    }

    /// Search text, matched fuzzily against each place's names. It never leaves the device.
    pub fn text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }

    /// The dataset to search, such as `openstreetmap.org`. Defaults to `overturemaps.org`.
    pub fn dataset(mut self, dataset: impl Into<String>) -> Self {
        self.dataset = Some(dataset.into());
        self
    }

    /// One collection of the dataset, such as `way`.
    pub fn collection(mut self, collection: impl Into<String>) -> Self {
        self.collection = Some(collection.into());
        self
    }

    /// The order of the hits.
    pub fn sort(mut self, sort: SortMode) -> Self {
        self.sort = sort;
        self
    }
}

/// What a search found.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct SearchResult {
    pub places: Vec<SearchHit>,
    /// False if any tile failed to load, so that `places` may be missing some; `tile_failures` says which
    /// and why.
    pub complete: bool,
    /// True if the server answered with its coarse overview tiles: the region was too big to search in
    /// full, and only its most important places are here.
    pub overview: bool,
    pub tiles: TileCounts,
    /// Every tile that failed, in the order of their URLs (UTF-16 code units, as the tiles were fetched),
    /// not the order they failed in. Empty when `complete` is true; its length is `tiles.failed`.
    #[cfg_attr(feature = "serde", serde(default))]
    pub tile_failures: Vec<TileFailure>,
}

/// One tile a search could not use (contract §4.4).
///
/// Two failures are equal, and hash alike, when their `url`, `kind` and `status` agree: `error` is left
/// out, since its type and message are this crate's and its transport's, not the contract's. Under the
/// `serde` feature `error` is not serialized either.
///
/// It carries the tile URL and what fetching it produced, never the search text. The tile URL names a
/// tile the server chose for the snapped box, so it reveals the coarse area searched, though not the region.
#[non_exhaustive]
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct TileFailure {
    /// The tile URL as the server's tile list gave it, before the client prepared it for a request.
    pub url: String,
    /// Where the failure arose.
    pub kind: TileFailureKind,
    /// The HTTP status, for [`TileFailureKind::Status`]; `None` for every other kind.
    pub status: Option<u16>,
    /// The error the tile failed with:
    ///
    /// - [`Refused`](TileFailureKind::Refused): an [`Error::Transport`] whose source is a message
    ///   saying the URL was refused. No transport was called.
    /// - [`Status`](TileFailureKind::Status): an [`Error::Atgeo`] read from the response, as any other
    ///   call's non-2xx is.
    /// - [`Decode`](TileFailureKind::Decode): an [`Error::Decoding`].
    /// - [`Transport`](TileFailureKind::Transport): an [`Error::Transport`] holding the transport's own
    ///   error, which `downcast_ref` reaches.
    ///
    /// It is `Some` for every failure a search reports. It is `None` only on a failure deserialized
    /// under the `serde` feature, which skips it.
    #[cfg_attr(feature = "serde", serde(skip))]
    pub error: Option<Arc<Error>>,
}

impl TileFailure {
    pub(crate) fn new(url: &str, kind: TileFailureKind, status: Option<u16>, error: Error) -> Self {
        TileFailure { url: url.to_owned(), kind, status, error: Some(Arc::new(error)) }
    }
}

impl PartialEq for TileFailure {
    fn eq(&self, other: &Self) -> bool {
        (&self.url, self.kind, self.status) == (&other.url, other.kind, other.status)
    }
}

impl Eq for TileFailure {}

impl Hash for TileFailure {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (&self.url, self.kind, self.status).hash(state);
    }
}

/// Where a tile's failure arose (contract §4.4).
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
pub enum TileFailureKind {
    /// The URL is not an absolute `http` or `https` URL, or it carries credentials
    /// (`https://user:pw@host/…`), so no request was made.
    Refused,
    /// The server answered with a non-2xx status.
    Status,
    /// A 2xx whose body is not a usable tile: not JSON, not an object, or without a `records` array.
    Decode,
    /// The transport failed: no response arrived, or the transport gave up on it (a timeout, a body
    /// over its size cap, anything an app's transport returns as an error).
    Transport,
}

/// How many tiles a search used and how many failed.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct TileCounts {
    pub fetched: usize,
    pub failed: usize,
}

/// One place a search found.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct SearchHit {
    /// The place's identity: its `origin`, else the tile record's `uri`, else `None`.
    pub origin: Option<String>,
    /// The record value as served.
    pub place: Place,
    /// The record's importance, relative to its tileset; larger is more important.
    pub importance: Option<i64>,
    /// Metres from the region's centre to the nearest point of the place; 0 if the place contains it.
    pub distance: f64,
    /// Text similarity, 0 to 1; `None` without text.
    pub score: Option<f64>,
}

impl SearchHit {
    /// The place's `name`, when that is a string.
    pub fn name(&self) -> Option<&str> {
        name_of(&self.place)
    }

    /// The point and box the client read from the place's `locations`, the same ones `distance` was
    /// measured to.
    pub fn location(&self) -> PlaceLocation {
        locations::locate(&self.place).public()
    }
}

/// A place fetched by its origin.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct PlaceRecord {
    /// The origin asked for.
    pub origin: String,
    /// The record value as served.
    pub place: Place,
    pub importance: Option<i64>,
}

impl PlaceRecord {
    /// The place's `name`, when that is a string.
    pub fn name(&self) -> Option<&str> {
        name_of(&self.place)
    }

    /// The point and box the client reads from the place's `locations`.
    pub fn location(&self) -> PlaceLocation {
        locations::locate(&self.place).public()
    }
}

fn name_of(place: &Place) -> Option<&str> {
    place.get("name").and_then(|v| v.as_str())
}

/// Where a place is, as read from its `locations`: the first usable point and the first usable box.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct PlaceLocation {
    pub point: Option<PlacePoint>,
    pub bbox: Option<PlaceBox>,
}

/// A point in degrees.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct PlacePoint {
    pub lat: f64,
    pub lon: f64,
}

/// A box in degrees. `west > east` crosses the antimeridian.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct PlaceBox {
    pub south: f64,
    pub west: f64,
    pub north: f64,
    pub east: f64,
}

/// What `describe_service` returns: the server's identity and the tilesets it serves.
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct ServiceDescription {
    pub did: Option<String>,
    pub tilesets: Vec<ServedTileset>,
}

/// One tileset a server serves, with the collections it carries.
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct ServedTileset {
    pub slug: Option<String>,
    pub collections: Vec<CollectionRef>,
}

/// One collection of one dataset.
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct CollectionRef {
    pub dataset: Option<String>,
    pub collection: Option<String>,
}

/// Where a collection's data comes from, and the terms it is under.
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct CollectionAttribution {
    pub dataset: Option<String>,
    pub collection: Option<String>,
    pub release: Option<String>,
    pub source: Option<String>,
    pub attribution: Option<String>,
    pub licenses: Option<Vec<String>>,
}

/// A collection a tileset carries, with its attribution and what it holds.
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct CollectionInfo {
    pub dataset: Option<String>,
    pub collection: Option<String>,
    pub release: Option<String>,
    pub source: Option<String>,
    pub attribution: Option<String>,
    pub licenses: Option<Vec<String>>,
    pub record_count: Option<i64>,
    pub location_formats: Option<Vec<String>>,
    pub containment_types: Option<Vec<String>>,
    pub attributes: Option<Vec<String>>,
    pub categories: Option<Vec<CategoryCount>>,
}

/// How many records of a collection have one category.
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct CategoryCount {
    pub value: Option<String>,
    pub count: Option<i64>,
}

/// What `describe_tileset` returns: a tileset's descriptor.
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct Tileset {
    pub slug: Option<String>,
    pub published_at: Option<String>,
    pub extent: Option<Extent>,
    pub collections: Vec<CollectionInfo>,
    pub references: Option<Vec<CollectionAttribution>>,
}

/// A tileset's extent, its edges as served (decimal degrees, as strings).
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct Extent {
    pub north: Option<String>,
    pub south: Option<String>,
    pub east: Option<String>,
    pub west: Option<String>,
}

#[cfg(test)]
mod tests {
    use std::collections::hash_map::DefaultHasher;

    use super::*;
    use crate::error::{AtgeoError, DecodingError};

    fn hash_of(f: &TileFailure) -> u64 {
        let mut h = DefaultHasher::new();
        f.hash(&mut h);
        h.finish()
    }

    #[test]
    fn a_failure_is_compared_and_hashed_without_its_error() {
        let a = TileFailure::new(
            "https://t/a",
            TileFailureKind::Status,
            Some(503),
            AtgeoError::new("HttpError".into(), "x".into(), 503).into(),
        );
        let mut b = a.clone();
        b.error = Some(Arc::new(DecodingError::shape("something else").into()));
        assert_eq!(a, b);
        assert_eq!(hash_of(&a), hash_of(&b));
        b.error = None;
        assert_eq!(a, b);

        for differ in [
            TileFailure { url: "https://t/b".into(), ..a.clone() },
            TileFailure { kind: TileFailureKind::Transport, ..a.clone() },
            TileFailure { status: Some(500), ..a.clone() },
            TileFailure { status: None, ..a.clone() },
        ] {
            assert_ne!(a, differ);
        }
    }

    #[cfg(feature = "serde")]
    #[test]
    fn serializes_a_failure_without_its_error_and_reads_it_back_as_none() {
        let failure = TileFailure::new(
            "https://t/a",
            TileFailureKind::Status,
            Some(503),
            AtgeoError::new("HttpError".into(), "x".into(), 503).into(),
        );
        let json = serde_json::to_value(&failure).unwrap();
        assert_eq!(json, serde_json::json!({ "url": "https://t/a", "kind": "status", "status": 503 }));
        let back: TileFailure = serde_json::from_value(json).unwrap();
        assert_eq!(back, failure);
        assert!(back.error.is_none());

        for (kind, name) in [
            (TileFailureKind::Refused, "refused"),
            (TileFailureKind::Status, "status"),
            (TileFailureKind::Decode, "decode"),
            (TileFailureKind::Transport, "transport"),
        ] {
            assert_eq!(serde_json::to_value(kind).unwrap(), name);
            assert_eq!(serde_json::from_value::<TileFailureKind>(name.into()).unwrap(), kind);
        }

        let result = SearchResult {
            places: Vec::new(),
            complete: false,
            overview: false,
            tiles: TileCounts { fetched: 0, failed: 1 },
            tile_failures: vec![failure],
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains(r#""tile_failures":[{"url":"https://t/a","kind":"status","status":503}]"#), "{json}");
        assert_eq!(serde_json::from_str::<SearchResult>(&json).unwrap(), result);

        // A result serialized before tile_failures existed still reads, with none.
        let old = r#"{"places":[],"complete":true,"overview":false,"tiles":{"fetched":1,"failed":0}}"#;
        assert!(serde_json::from_str::<SearchResult>(old).unwrap().tile_failures.is_empty());
    }
}
