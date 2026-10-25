//! Reading a place's location (contract §5), distance and containment (§6), and record keys and
//! dedupe (§4.6).

use serde_json::{Map, Value};

use crate::cache::TileRecord;
use crate::region::{Segments, haversine, lon_in_segments, lon_segments, lon_segments_overlap};
use crate::types::{PlaceBox, PlaceLocation, PlacePoint, Region};

const GEO: &str = "community.lexicon.location.geo";
const BBOX: &str = "community.lexicon.location.bbox";

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct BoxLocation {
    pub(crate) south: f64,
    pub(crate) north: f64,
    pub(crate) west: f64,
    pub(crate) east: f64,
}

impl BoxLocation {
    pub(crate) fn segments(&self) -> Segments {
        lon_segments(self.west, self.east)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Locations {
    /// `(lat, lon)`
    pub(crate) point: Option<(f64, f64)>,
    pub(crate) bbox: Option<BoxLocation>,
}

impl Locations {
    pub(crate) fn public(&self) -> PlaceLocation {
        PlaceLocation {
            point: self.point.map(|(lat, lon)| PlacePoint { lat, lon }),
            bbox: self.bbox.map(|b| PlaceBox { south: b.south, west: b.west, north: b.north, east: b.east }),
        }
    }
}

/// A coordinate from a JSON string holding a decimal number, or a JSON number; `None` for anything
/// else and for a non-finite result. `str::parse` is stricter than `parseFloat` (CONFORMANCE.md).
pub(crate) fn parse_coord(v: Option<&Value>) -> Option<f64> {
    let d = match v? {
        Value::Number(n) => n.as_f64()?,
        Value::String(s) => s.parse::<f64>().ok()?,
        _ => return None,
    };
    d.is_finite().then_some(d)
}

/// The first usable point and the first usable box in `value.locations`.
pub(crate) fn locate(value: &Map<String, Value>) -> Locations {
    let mut result = Locations::default();
    let Some(Value::Array(locations)) = value.get("locations") else {
        return result;
    };
    for loc in locations {
        let Some(loc) = loc.as_object() else { continue };
        let num = |key: &str| parse_coord(loc.get(key));
        match loc.get("$type").and_then(Value::as_str) {
            Some(GEO) if result.point.is_none() => {
                if let (Some(lat), Some(lon)) = (num("latitude"), num("longitude")) {
                    result.point = Some((lat, lon));
                }
            }
            Some(BBOX) if result.bbox.is_none() => {
                if let (Some(south), Some(north), Some(west), Some(east)) =
                    (num("south"), num("north"), num("west"), num("east"))
                {
                    result.bbox = Some(BoxLocation { south, north, west, east });
                }
            }
            _ => {}
        }
        if result.point.is_some() && result.bbox.is_some() {
            break;
        }
    }
    result
}

/// `value.origin` if it is a string, else the record's `uri`.
pub(crate) fn record_key(r: &TileRecord) -> Option<&str> {
    match r.value.get("origin") {
        Some(Value::String(s)) => Some(s),
        _ => r.uri.as_deref(),
    }
}

/// `Math.min` over three, which returns NaN if any operand is NaN (f64::min would drop it).
fn js_min3(a: f64, b: f64, c: f64) -> f64 {
    if a.is_nan() || b.is_nan() || c.is_nan() { f64::NAN } else { a.min(b).min(c) }
}

fn angular_gap(a: f64, b: f64) -> f64 {
    let d = (a - b).abs() % 360.0;
    d.min(360.0 - d)
}

fn nearest_lon(lon: f64, segments: &[(f64, f64)]) -> f64 {
    if lon_in_segments(lon, segments) {
        return lon;
    }
    let mut ends = segments.iter().flat_map(|&(lo, hi)| [lo, hi]);
    let first = ends.next().expect("at least one segment");
    // Strictly closer replaces, so on a tie the first endpoint in segment order wins.
    ends.fold(first, |best, e| if angular_gap(e, lon) < angular_gap(best, lon) { e } else { best })
}

/// ±180 are one meridian, and every meridian meets at a pole.
fn covers(south: f64, north: f64, segments: &[(f64, f64)], lat: f64, lon: f64) -> bool {
    if lat < south || lat > north {
        return false;
    }
    lat.abs() == 90.0 || lon_in_segments(lon, segments) || (lon.abs() == 180.0 && lon_in_segments(-lon, segments))
}

/// The box-to-box form of `covers`: boxes touching ±180 meet at the seam, and boxes reaching a pole meet
/// there.
fn meets(bbox: &BoxLocation, south: f64, north: f64, segments: &[(f64, f64)]) -> bool {
    // Math.max and Math.min, which these are as long as nothing is NaN; parse_coord saw to that.
    let lo = bbox.south.max(south);
    let hi = bbox.north.min(north);
    if lo > hi {
        return false;
    }
    if hi == 90.0 || lo == -90.0 {
        return true;
    }
    let mut twins = Vec::with_capacity(6);
    for &(a, b) in bbox.segments().as_slice() {
        twins.push((a, b));
        if b == 180.0 {
            twins.push((-180.0, -180.0));
        }
        if a == -180.0 {
            twins.push((180.0, 180.0));
        }
    }
    lon_segments_overlap(&twins, segments)
}

/// Metres from `(lat, lon)` to the nearest point of the place; `None` without a location. Containment is
/// tested first so that it yields exactly 0 rather than trigonometric noise.
pub(crate) fn distance_to(locations: &Locations, lat: f64, lon: f64) -> Option<f64> {
    let contained = match (locations.point, &locations.bbox) {
        (Some((plat, plon)), _) => covers(plat, plat, Segments::one(plon, plon).as_slice(), lat, lon),
        (None, Some(b)) => covers(b.south, b.north, b.segments().as_slice(), lat, lon),
        (None, None) => false,
    };
    if contained {
        return Some(0.0);
    }
    if let Some((plat, plon)) = locations.point {
        return Some(haversine(lat, lon, plat, plon));
    }
    let b = locations.bbox?;
    // Along the nearest meridian, cos(d) = R cos(phi - phiC); phiC is where that meridian comes closest.
    let lon_star = nearest_lon(lon, b.segments().as_slice());
    let rad = std::f64::consts::PI / 180.0;
    let phi0 = lat * rad;
    let d_lon = angular_gap(lon_star, lon) * rad;
    let phi_c = (phi0.sin()).atan2(phi0.cos() * d_lon.cos()) / rad;
    // Math.min(Math.max(phiC, south), north): never f64::clamp, which panics on a box with south > north.
    let clamped = phi_c.max(b.south).min(b.north);
    Some(js_min3(
        haversine(lat, lon, clamped, lon_star),
        haversine(lat, lon, b.south, lon_star),
        haversine(lat, lon, b.north, lon_star),
    ))
}

pub(crate) fn in_region(locations: &Locations, region: &Region) -> bool {
    match *region {
        Region::Bbox { west: min_lon, south: min_lat, east: max_lon, north: max_lat } => {
            let segments = lon_segments(min_lon, max_lon);
            if let Some((lat, lon)) = locations.point {
                return covers(min_lat, max_lat, segments.as_slice(), lat, lon);
            }
            locations.bbox.as_ref().is_some_and(|b| meets(b, min_lat, max_lat, segments.as_slice()))
        }
        Region::Circle { lat, lon, radius } => distance_to(locations, lat, lon).is_some_and(|d| d <= radius),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn box_at(south: f64, north: f64, west: f64, east: f64) -> Locations {
        Locations { point: None, bbox: Some(BoxLocation { south, north, west, east }) }
    }

    fn point_at(lat: f64, lon: f64) -> Locations {
        Locations { point: Some((lat, lon)), bbox: None }
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() <= 1e-6 * b.abs().max(1.0)
    }

    // Minimum haversine distance from the centre to a dense sampling of the box boundary.
    fn sampled_distance(lat: f64, lon: f64, south: f64, north: f64, west: f64, east: f64) -> f64 {
        let steps = 100_000;
        let spans: Vec<(f64, f64)> =
            if west <= east { vec![(west, east)] } else { vec![(west, 180.0), (-180.0, east)] };
        let mut best = f64::INFINITY;
        for (lo, hi) in spans {
            for i in 0..=steps {
                let la = south + (north - south) * i as f64 / steps as f64;
                let lo2 = lo + (hi - lo) * i as f64 / steps as f64;
                for (a, b) in [(la, lo), (la, hi), (south, lo2), (north, lo2)] {
                    best = best.min(haversine(lat, lon, a, b));
                }
            }
        }
        best
    }

    // The contract's §6 table.
    #[test]
    fn distance_matches_the_contract_table() {
        let b = |north, south, east, west| box_at(south, north, west, east);
        let table = [
            (point_at(0.001, 0.0), (0.0, 0.0), 111.19508023353292),
            (b(46.0, 44.0, 11.0, 9.0), (45.0, 10.0), 0.0),
            (b(46.0, 44.0, 11.0, 9.0), (45.0, 8.0), 78624.79923702011),
            (b(46.0, 44.0, 11.0, 9.0), (47.0, 10.0), 111195.0802335329),
            (b(46.0, 44.0, 11.0, 9.0), (47.0, 8.0), 134989.80127268212),
            (b(80.0, 70.0, 30.0, 10.0), (75.0, 60.0), 826787.9307589367),
            (b(50.0, 40.0, -170.0, -180.0), (45.0, 180.0), 0.0),
            (b(10.0, -10.0, -170.0, 170.0), (0.0, 160.0), 1111950.8023353291),
        ];
        for (loc, (lat, lon), expected) in table {
            let d = distance_to(&loc, lat, lon).unwrap();
            if expected == 0.0 {
                assert_eq!(d, 0.0, "{loc:?}");
            } else {
                assert!(close(d, expected), "{loc:?} from ({lat}, {lon}): {d}, expected {expected}");
            }
        }
        assert_eq!(distance_to(&Locations::default(), 1.0, 2.0), None);
    }

    // distance.test.ts: containment is exactly zero
    #[test]
    fn containment_is_exactly_zero() {
        assert_eq!(distance_to(&box_at(46.0, 47.0, 10.0, 11.0), 46.0986, 10.5), Some(0.0));
        assert_eq!(distance_to(&box_at(40.0, 50.0, -180.0, -170.0), 45.0, 180.0), Some(0.0));
        assert_eq!(distance_to(&box_at(40.0, 50.0, 170.0, 180.0), 45.0, -180.0), Some(0.0));
        assert_eq!(distance_to(&box_at(-90.0, -80.0, 10.0, 20.0), -90.0, 0.0), Some(0.0));
        assert_eq!(distance_to(&point_at(46.0986, 10.5), 46.0986, 10.5), Some(0.0));
        assert_eq!(distance_to(&point_at(10.0, 180.0), 10.0, -180.0), Some(0.0));
        assert_eq!(distance_to(&point_at(10.0, -180.0), 10.0, 180.0), Some(0.0));
    }

    // distance.test.ts: outside a box
    #[test]
    fn matches_the_sampled_boundary_distance() {
        let cases = [
            ("due south within the longitude range", 0.0, 5.0, [10.0, 20.0, 0.0, 10.0]),
            ("east of a high-latitude box", 65.0, 10.0, [60.0, 70.0, 20.0, 30.0]),
            ("more than 90 degrees of longitude away", 50.0, 0.0, [40.0, 60.0, 120.0, 140.0]),
            ("across the antimeridian from a box", 60.0, -179.0, [55.0, 65.0, 175.0, 179.0]),
            ("from an antimeridian-crossing box", 45.0, -160.0, [40.0, 50.0, 170.0, -170.0]),
            ("west of an antimeridian-crossing box", 45.0, 150.0, [40.0, 50.0, 170.0, -170.0]),
        ];
        for (name, lat, lon, [south, north, west, east]) in cases {
            let expected = sampled_distance(lat, lon, south, north, west, east);
            let d = distance_to(&box_at(south, north, west, east), lat, lon).unwrap();
            // toBeCloseTo(expected, 1): within 0.05
            assert!((d - expected).abs() < 0.05, "{name}: {d}, sampled {expected}");
        }
    }

    #[test]
    fn is_strictly_less_than_the_latitude_clamped_distance_for_a_high_latitude_box_to_the_east() {
        let clamped = haversine(65.0, 10.0, 65.0, 20.0);
        let actual = distance_to(&box_at(60.0, 70.0, 20.0, 30.0), 65.0, 10.0).unwrap();
        assert!(actual < clamped - 1000.0);
    }

    #[test]
    fn a_reversed_box_does_not_panic_and_matches_the_reference() {
        // South above north; the expected values are the reference's own.
        let d = distance_to(&box_at(10.0, 5.0, 0.0, 0.0), 0.0, 0.0).unwrap();
        assert!(close(d, 555975.4011676646), "{d}");
        let d = distance_to(&box_at(10.0, 5.0, -1.0, 1.0), 0.0, 3.0).unwrap();
        assert!(close(d, 598698.9307667436), "{d}");
    }

    // distance.test.ts: inRegion: bbox region seams
    #[test]
    fn keeps_across_bbox_region_seams() {
        let r = Region::bbox;
        let keep = [
            ("a point at lon 180 in a region with west -180", point_at(45.0, 180.0), r(-180.0, 40.0, -170.0, 50.0)),
            ("a point at lon -180 in a region with east 180", point_at(45.0, -180.0), r(170.0, 40.0, 180.0, 50.0)),
            (
                "a box ending at 180 and a region starting at -180",
                box_at(40.0, 50.0, 170.0, 180.0),
                r(-180.0, 40.0, -170.0, 50.0),
            ),
            (
                "a box starting at -180 and a region ending at 180",
                box_at(40.0, 50.0, -180.0, -170.0),
                r(170.0, 40.0, 180.0, 50.0),
            ),
            ("a point at the pole whatever its longitude", point_at(90.0, 0.0), r(10.0, 80.0, 20.0, 90.0)),
            (
                "a box reaching the pole at other longitudes",
                box_at(85.0, 90.0, 100.0, 110.0),
                r(10.0, 80.0, 20.0, 90.0),
            ),
        ];
        for (name, loc, region) in keep {
            assert!(in_region(&loc, &region), "keeps {name}");
        }
        let drop = [
            ("a point just short of the seam", point_at(45.0, 179.99), r(-180.0, 40.0, -170.0, 50.0)),
            ("a box just short of the seam", box_at(40.0, 50.0, 160.0, 179.99), r(-180.0, 40.0, -170.0, 50.0)),
            (
                "a polar box when the region stops short of the pole",
                box_at(85.0, 90.0, 100.0, 110.0),
                r(10.0, 80.0, 20.0, 89.99),
            ),
        ];
        for (name, loc, region) in drop {
            assert!(!in_region(&loc, &region), "drops {name}");
        }
    }

    #[test]
    fn circle_uses_the_true_radius() {
        let loc = point_at(0.001, 0.0);
        assert!(in_region(&loc, &Region::circle(0.0, 0.0, 111.2)));
        assert!(!in_region(&loc, &Region::circle(0.0, 0.0, 111.1)));
    }

    #[test]
    fn parses_coordinates_strictly() {
        let p = |v: Value| parse_coord(Some(&v));
        for (s, want) in [("+1.5", 1.5), (".5", 0.5), ("1.", 1.0), ("-0", -0.0), ("1e3", 1000.0), ("1e+5", 1e5)] {
            assert_eq!(p(json!(s)), Some(want), "{s}");
        }
        for s in [" 1.5", "1.5abc", "1e", "1_000", "0x10", "Infinity", "NaN", "inf", "-infinity", "", "."] {
            assert_eq!(p(json!(s)), None, "{s:?}");
        }
        assert_eq!(p(json!(2.5)), Some(2.5));
        assert_eq!(p(json!(7)), Some(7.0));
        assert_eq!(p(json!([0.5])), None);
        assert_eq!(p(json!(["0.5"])), None);
        assert_eq!(p(json!(null)), None);
        assert_eq!(p(json!(true)), None);
        assert_eq!(parse_coord(None), None);
    }

    #[test]
    fn locates_the_first_usable_point_and_box() {
        let v = json!({ "locations": [
            null, 3, "x", [],
            { "$type": GEO, "latitude": "north-ish", "longitude": "1" },
            { "$type": "community.lexicon.location.address", "country": "US" },
            { "$type": BBOX, "south": "1", "north": "2", "west": "3", "east": "nope" },
            { "$type": GEO, "latitude": "1.5", "longitude": 2 },
            { "$type": BBOX, "south": "1", "north": "2", "west": "170", "east": "-170" },
            { "$type": GEO, "latitude": "9", "longitude": "9" },
        ]});
        let loc = locate(v.as_object().unwrap());
        assert_eq!(loc.point, Some((1.5, 2.0)));
        assert_eq!(loc.bbox, Some(BoxLocation { south: 1.0, north: 2.0, west: 170.0, east: -170.0 }));
        assert_eq!(locate(json!({ "locations": {} }).as_object().unwrap()), Locations::default());
    }

    #[test]
    fn keeps_out_of_range_coordinates_as_parsed() {
        let v = json!({ "locations": [{ "$type": GEO, "latitude": "95", "longitude": "200" }] });
        assert_eq!(locate(v.as_object().unwrap()).point, Some((95.0, 200.0)));
    }
}
