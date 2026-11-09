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
    /// A bounding box, in degrees. `west > east` crosses the antimeridian.
    Bbox { west: f64, south: f64, east: f64, north: f64 },
}

impl Region {
    /// A circle around `(lat, lon)` with `radius` in metres.
    pub const fn circle(lat: f64, lon: f64, radius: f64) -> Self {
        Region::Circle { lat, lon, radius }
    }

    /// A bounding box in degrees, in the order of the `getTiles` bbox string. `west > east` crosses the
    /// antimeridian.
    pub const fn bbox(west: f64, south: f64, east: f64, north: f64) -> Self {
        Region::Bbox { west, south, east, north }
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

/// What to search for, besides the region, for [`search_nearby`](crate::AtgeoClient::search_nearby).
///
/// A search needs a dataset (contract §4.1), so there is no way to build the options without one: `new`
/// takes it, and there is no `Default`. A missing dataset, and a collection named without one, are
/// compile errors, and so is a dataset that is not a string.
///
/// ```
/// # use atgeo::{NearbyOptions, SortMode};
/// let options = NearbyOptions::new("overturemaps.org").text("Starbux").sort(SortMode::Distance);
/// ```
///
/// ```compile_fail
/// # use atgeo::NearbyOptions;
/// let options = NearbyOptions::new(); // the dataset is required
/// ```
///
/// ```compile_fail
/// # use atgeo::NearbyOptions;
/// let options = NearbyOptions::default(); // and has no default
/// ```
///
/// ```compile_fail
/// # use atgeo::NearbyOptions;
/// let options = NearbyOptions::new(5); // and is a string
/// ```
#[derive(Clone, Debug)]
pub struct NearbyOptions {
    pub(crate) dataset: String,
    pub(crate) text: Option<String>,
    pub(crate) collection: Option<String>,
    pub(crate) sort: SortMode,
}

impl NearbyOptions {
    /// Options for a search of `dataset`, such as `overturemaps.org` or `openstreetmap.org`: no text, no
    /// collection (every collection of the dataset together), fused order. The dataset is required and has
    /// no default. An empty string is sent as given, and the server answers for itself.
    pub fn new(dataset: impl Into<String>) -> Self {
        NearbyOptions { dataset: dataset.into(), text: None, collection: None, sort: SortMode::default() }
    }

    /// Search text, matched fuzzily against each place's names. It never leaves the device: only the
    /// methods whose names contain `keyword` send text to the server.
    pub fn text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
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

/// What a nearby search found.
///
/// It is `Send` and `Sync`, but not `UnwindSafe` or `RefUnwindSafe`: a [`TileFailure`]'s error can hold a
/// transport's boxed error, which promises neither. Wrap it in `std::panic::AssertUnwindSafe` to move it
/// into `catch_unwind`.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct NearbySearchResult {
    pub places: Vec<NearbyHit>,
    /// False if any tile failed to load, so that `places` may be missing some; `tile_failures` says which
    /// and why.
    pub complete: bool,
    /// True if the server answered with its coarse overview tiles: the region was too big to search in
    /// full, and only its most important places are here.
    pub overview: bool,
    pub tiles: TileCounts,
    /// Every tile that failed, in the order of their URLs (UTF-16 code units, as the tiles were fetched),
    /// not the order they failed in. Empty when `complete` is true; its length is `tiles.failed`, except
    /// in a result deserialized from one serialized before this field existed, which reads as empty.
    #[cfg_attr(feature = "serde", serde(default))]
    pub tile_failures: Vec<TileFailure>,
    /// The credit due to every collection whose data the search's tiles hold (contract §4.10): the union
    /// of the attribution of every tile the search used, one entry per `(dataset, collection)`, the lowest
    /// tile URL's first, sorted by dataset and then collection. Always a list. The app's own copy.
    #[cfg_attr(feature = "serde", serde(default))]
    pub attribution: Vec<Attribution>,
    /// What the server learned during this call (contract §4.11): never the keyword, and the 0.01° grid
    /// unless the call was answered entirely from cache.
    #[cfg_attr(feature = "serde", serde(default))]
    pub disclosed: Disclosure,
}

/// What the server learned during one call (contract §4.11).
///
/// It is about the call, not about the area: a warm search that reuses tiles an earlier search fetched
/// reports `region: None`, though the earlier call told the server about the area. A call that fails
/// returns no result and so reports nothing.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Disclosure {
    /// The search text went to the server. True exactly when a `searchPlaces` request was made.
    pub keyword: bool,
    /// The grid, in degrees, of the finest region sent during the call: `0.01` (the tiles of a nearby
    /// search), `1` (a keyword search's region, snapped to whole degrees), or `None` if none was sent.
    pub region: Option<f64>,
}

impl Disclosure {
    pub(crate) fn new(keyword: bool, region: Option<f64>) -> Self {
        Disclosure { keyword, region }
    }
}

/// The collection a hit belongs to (contract §8): the `dataset` and `collection` of the attribution entry
/// its origin names.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct CollectionKey {
    pub dataset: String,
    pub collection: String,
}

/// A credit for one collection (`org.atgeo.defs#collectionAttribution`), as a search or `get_place` returns
/// it (contract §1). An entry the client cannot credit to a collection, one without a string `dataset` and
/// `collection`, is dropped when it is read, so those two are not optional here. Every other field is read
/// on its own and is `None` when the server omits it or sends the wrong type. It is the app's own copy:
/// changing it, or its `licenses`, changes nothing the client holds.
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Attribution {
    pub dataset: String,
    pub collection: String,
    pub release: Option<String>,
    pub source: Option<String>,
    pub attribution: Option<String>,
    pub licenses: Option<Vec<String>>,
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
    /// The HTTP status, for [`TileFailureKind::Status`]; `None` for every other kind, and then left out
    /// when serialized.
    #[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "Option::is_none"))]
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

/// One place a nearby search found.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct NearbyHit {
    /// The place's identity: its `origin`, else the tile record's `uri`, else `None`.
    pub origin: Option<String>,
    /// The collection the hit belongs to, matched against its tile's attribution (contract §8); `None`
    /// when no entry matches, as for a hit keyed by its `uri`.
    pub collection: Option<CollectionKey>,
    /// The record value as served.
    pub place: Place,
    /// The record's importance, relative to its tileset; larger is more important.
    pub importance: Option<i64>,
    /// Metres from the region's centre to the nearest point of the place; 0 if the place contains it.
    pub distance: f64,
    /// Text similarity, 0 to 1; `None` without text.
    pub score: Option<f64>,
}

impl NearbyHit {
    /// Where the hit came from: always `"tiles"`.
    pub const fn via(&self) -> &'static str {
        VIA_TILES
    }

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

const VIA_TILES: &str = "tiles";
const VIA_KEYWORD: &str = "keyword";

/// The options of a keyword search: a dataset, and optionally a collection and a region.
///
/// Every method that takes them sends the keyword to the server (contract §16): see
/// [`search_by_keyword`](crate::AtgeoClient::search_by_keyword).
///
/// ```
/// # use atgeo::{KeywordOptions, KeywordRegion};
/// let options = KeywordOptions::new("overturemaps.org")
///     .collection("place")
///     .region(KeywordRegion::new(-123.0, 37.0, -122.0, 38.0));
/// ```
#[derive(Clone, Debug)]
pub struct KeywordOptions {
    pub(crate) dataset: String,
    pub(crate) collection: Option<String>,
    pub(crate) region: Option<KeywordRegion>,
}

impl KeywordOptions {
    /// Options for a keyword search of `dataset`, with no collection and no region. The dataset is required
    /// and has no default.
    pub fn new(dataset: impl Into<String>) -> Self {
        KeywordOptions { dataset: dataset.into(), collection: None, region: None }
    }

    /// One collection of the dataset, such as `way`.
    pub fn collection(mut self, collection: impl Into<String>) -> Self {
        self.collection = Some(collection.into());
        self
    }

    /// Only places in or meeting this box. It goes to the server snapped outward to whole degrees.
    pub fn region(mut self, region: KeywordRegion) -> Self {
        self.region = Some(region);
        self
    }
}

/// A bounding box in degrees, for a keyword search: a box only, never a circle, because a circle API invites
/// the device's own position as its centre. It is sent snapped outward to whole degrees (contract §16).
///
/// Like [`Region`], it is not validated on construction: a search with a malformed one fails with
/// `ArgumentError::InvalidRegion` before any request.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct KeywordRegion {
    pub west: f64,
    pub south: f64,
    pub east: f64,
    pub north: f64,
}

impl KeywordRegion {
    /// A bounding box in degrees, in the order of the `getTiles` bbox string. `west > east` crosses the
    /// antimeridian.
    pub const fn new(west: f64, south: f64, east: f64, north: f64) -> Self {
        KeywordRegion { west, south, east, north }
    }

    pub(crate) const fn as_region(&self) -> Region {
        Region::Bbox { west: self.west, south: self.south, east: self.east, north: self.north }
    }
}

/// Whether [`search_nearby_then_by_keyword`](crate::AtgeoClient::search_nearby_then_by_keyword)'s keyword
/// request carries the nearby region, snapped to whole degrees.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
pub enum KeywordRegionMode {
    /// The keyword request carries no region.
    None,
    /// The keyword request carries the nearby region as a box (a circle as its enclosing box), snapped
    /// outward to whole degrees.
    Degree,
}

/// The options of [`search_nearby_then_by_keyword`](crate::AtgeoClient::search_nearby_then_by_keyword).
///
/// The keyword and `keyword_region` are required: the fallback needs text, and what goes to the server with
/// it is the caller's decision every time.
#[derive(Clone, Debug)]
pub struct FallbackOptions {
    pub(crate) dataset: String,
    pub(crate) keyword: String,
    pub(crate) keyword_region: KeywordRegionMode,
    pub(crate) collection: Option<String>,
    pub(crate) sort: SortMode,
}

impl FallbackOptions {
    /// Options for a search of `dataset` for `keyword`: no collection, fused order. The keyword is matched
    /// on the device first, and goes to the server only when that finds nothing solid.
    pub fn new(dataset: impl Into<String>, keyword: impl Into<String>, keyword_region: KeywordRegionMode) -> Self {
        FallbackOptions {
            dataset: dataset.into(),
            keyword: keyword.into(),
            keyword_region,
            collection: None,
            sort: SortMode::default(),
        }
    }

    /// One collection of the dataset, such as `way`.
    pub fn collection(mut self, collection: impl Into<String>) -> Self {
        self.collection = Some(collection.into());
        self
    }

    /// The order of the nearby hits.
    pub fn sort(mut self, sort: SortMode) -> Self {
        self.sort = sort;
        self
    }
}

/// What a keyword search found (contract §16).
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct KeywordSearchResult {
    /// The server's matches, in the server's order: no dedupe, no region filter, no ranking.
    pub places: Vec<KeywordHit>,
    /// The union of the attribution of every place, one entry per `(dataset, collection)`, sorted by dataset
    /// and then collection (contract §4.10). Always a list.
    pub attribution: Vec<Attribution>,
    /// Always `keyword: true`; `region` is `Some(1.0)` when a region was given.
    pub disclosed: Disclosure,
}

impl KeywordSearchResult {
    /// No places and no attribution, with this disclosure.
    pub(crate) fn empty(disclosed: Disclosure) -> Self {
        KeywordSearchResult { places: Vec::new(), attribution: Vec::new(), disclosed }
    }
}

/// One place a keyword search found. It has no distance and no score: the server ranks by neither, and an
/// app can compute a distance from [`location`](KeywordHit::location).
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct KeywordHit {
    /// The place's identity: its `origin`, else the element's `uri`, else `None`.
    pub origin: Option<String>,
    /// The collection the hit belongs to, matched against the element's own attribution (contract §8).
    pub collection: Option<CollectionKey>,
    /// The record value as served.
    pub place: Place,
    pub importance: Option<i64>,
}

impl KeywordHit {
    /// Where the hit came from: always `"keyword"`.
    pub const fn via(&self) -> &'static str {
        VIA_KEYWORD
    }

    /// The place's `name`, when that is a string.
    pub fn name(&self) -> Option<&str> {
        name_of(&self.place)
    }

    /// The point and box the client reads from the place's `locations`.
    pub fn location(&self) -> PlaceLocation {
        locations::locate(&self.place).public()
    }
}

/// What [`search_nearby_then_by_keyword`](crate::AtgeoClient::search_nearby_then_by_keyword) found: the
/// nearby search's result, with `places` holding both kinds of hit.
///
/// When the keyword was sent, the keyword hits come first, then the nearby hits the keyword answer did not
/// already hold. `complete`, `overview`, `tiles` and `tile_failures` are the nearby search's.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct FallbackSearchResult {
    pub places: Vec<FallbackHit>,
    pub complete: bool,
    pub overview: bool,
    pub tiles: TileCounts,
    #[cfg_attr(feature = "serde", serde(default))]
    pub tile_failures: Vec<TileFailure>,
    /// The union of the nearby and keyword attribution, the nearby entry winning a tie.
    #[cfg_attr(feature = "serde", serde(default))]
    pub attribution: Vec<Attribution>,
    /// What the server learned during the call: `keyword` is true when the keyword request went out, and
    /// `region` is the finest region sent (`0.01` if the nearby search sent anything, else `1` for a
    /// [`KeywordRegionMode::Degree`] request, else `None`).
    #[cfg_attr(feature = "serde", serde(default))]
    pub disclosed: Disclosure,
}

/// One place of a [`FallbackSearchResult`]: from the device's tiles, or from the server's keyword search.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum FallbackHit {
    Nearby(NearbyHit),
    Keyword(KeywordHit),
}

impl FallbackHit {
    /// `"tiles"` for a nearby hit, `"keyword"` for a keyword hit.
    pub const fn via(&self) -> &'static str {
        match self {
            FallbackHit::Nearby(h) => h.via(),
            FallbackHit::Keyword(h) => h.via(),
        }
    }

    /// The place's identity: its `origin`, else its `uri`, else `None`.
    pub fn origin(&self) -> Option<&str> {
        match self {
            FallbackHit::Nearby(h) => h.origin.as_deref(),
            FallbackHit::Keyword(h) => h.origin.as_deref(),
        }
    }

    /// The collection the hit belongs to, as for [`NearbyHit`] and [`KeywordHit`].
    pub fn collection(&self) -> Option<&CollectionKey> {
        match self {
            FallbackHit::Nearby(h) => h.collection.as_ref(),
            FallbackHit::Keyword(h) => h.collection.as_ref(),
        }
    }

    /// The record value as served.
    pub fn place(&self) -> &Place {
        match self {
            FallbackHit::Nearby(h) => &h.place,
            FallbackHit::Keyword(h) => &h.place,
        }
    }

    pub fn importance(&self) -> Option<i64> {
        match self {
            FallbackHit::Nearby(h) => h.importance,
            FallbackHit::Keyword(h) => h.importance,
        }
    }

    /// The place's `name`, when that is a string.
    pub fn name(&self) -> Option<&str> {
        name_of(self.place())
    }

    /// The point and box the client reads from the place's `locations`.
    pub fn location(&self) -> PlaceLocation {
        locations::locate(self.place()).public()
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
    /// The collections the place's data comes from, as served and in served order, duplicates included
    /// and not sorted (contract §9); empty when the source has none. The app's own copy.
    #[cfg_attr(feature = "serde", serde(default))]
    pub attribution: Vec<Attribution>,
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
    /// The server's tile budget. Informational: tile zooms are adaptive.
    pub max_tiles: Option<i64>,
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
    /// The tileset's overview band, when it has one.
    pub overview: Option<TilesetOverview>,
}

/// A tileset's overview band.
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct TilesetOverview {
    /// The number of records in the band.
    pub records: Option<i64>,
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

    fn record_of(place: serde_json::Value) -> PlaceRecord {
        let serde_json::Value::Object(map) = place else { unreachable!() };
        PlaceRecord { origin: "urn:x:1".into(), place: Arc::new(map), importance: None, attribution: Vec::new() }
    }

    // Contract §8/§9: `name` is the place's name only when it is a string; `location` is what §5 reads.
    #[test]
    fn place_record_name_is_none_unless_a_string() {
        assert_eq!(record_of(serde_json::json!({ "name": "Cafe" })).name(), Some("Cafe"));
        assert_eq!(record_of(serde_json::json!({})).name(), None);
        assert_eq!(record_of(serde_json::json!({ "name": 5 })).name(), None);
        assert_eq!(record_of(serde_json::json!({ "name": null })).name(), None);
        assert_eq!(record_of(serde_json::json!({ "name": ["Cafe"] })).name(), None);
    }

    #[test]
    fn place_record_location_is_the_first_usable_point_and_box() {
        let record = record_of(serde_json::json!({ "locations": [
            { "$type": "community.lexicon.location.geo", "latitude": "x", "longitude": "1" },
            { "$type": "community.lexicon.location.geo", "latitude": "40.5", "longitude": -74.25 },
            { "$type": "community.lexicon.location.bbox", "south": "1", "west": "170", "north": "2", "east": "-170" },
            { "$type": "community.lexicon.location.geo", "latitude": "1", "longitude": "1" }
        ] }));
        let location = record.location();
        assert_eq!(location.point.map(|p| (p.lat, p.lon)), Some((40.5, -74.25)));
        let b = location.bbox.unwrap();
        assert_eq!((b.south, b.west, b.north, b.east), (1.0, 170.0, 2.0, -170.0));
        let empty = record_of(serde_json::json!({ "locations": "no" })).location();
        assert_eq!((empty.point, empty.bbox), (None, None));
    }

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

        // A kind without a status leaves it out, and reads back as None.
        let refused = TileFailure::new("ftp://t/a", TileFailureKind::Refused, None, Error::Transport("x".into()));
        let json = serde_json::to_value(&refused).unwrap();
        assert_eq!(json, serde_json::json!({ "url": "ftp://t/a", "kind": "refused" }));
        let back: TileFailure = serde_json::from_value(json).unwrap();
        assert_eq!((back.status, back.error.is_none()), (None, true));
        assert_eq!(back, refused);

        let result = NearbySearchResult {
            places: Vec::new(),
            complete: false,
            overview: false,
            tiles: TileCounts { fetched: 0, failed: 1 },
            tile_failures: vec![failure],
            attribution: Vec::new(),
            disclosed: Disclosure::new(false, Some(0.01)),
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains(r#""tile_failures":[{"url":"https://t/a","kind":"status","status":503}]"#), "{json}");
        assert_eq!(serde_json::from_str::<NearbySearchResult>(&json).unwrap(), result);

        // A result serialized before tile_failures, attribution and disclosed existed still reads, with none.
        let old = r#"{"places":[],"complete":true,"overview":false,"tiles":{"fetched":1,"failed":0}}"#;
        let old = serde_json::from_str::<NearbySearchResult>(old).unwrap();
        assert!(old.tile_failures.is_empty());
        assert_eq!(old.disclosed, Disclosure::new(false, None));
    }
}
