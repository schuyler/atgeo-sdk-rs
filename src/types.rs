use std::sync::Arc;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

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
    /// False if any tile failed to load, so that `places` may be missing some.
    pub complete: bool,
    /// True if the server answered with its coarse overview tiles: the region was too big to search in
    /// full, and only its most important places are here.
    pub overview: bool,
    pub tiles: TileCounts,
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
