//! Regions: validation, centre, the circle's enclosing box, snapping and the getTiles bbox strings
//! (contract §3), and the longitude segments every geometric test works on.

use crate::error::ArgumentError;
use crate::types::Region;

const DEG_TO_M: f64 = 111194.927;
pub(crate) const EARTH_RADIUS_M: f64 = 6371008.8;
const EPSILON: f64 = 1e-9;

/// One or two longitude ranges, lo <= hi each: a box crossing the antimeridian is two.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Segments {
    segs: [(f64, f64); 2],
    len: usize,
}

impl Segments {
    pub(crate) fn one(lo: f64, hi: f64) -> Self {
        Segments { segs: [(lo, hi), (0.0, 0.0)], len: 1 }
    }

    pub(crate) fn as_slice(&self) -> &[(f64, f64)] {
        &self.segs[..self.len]
    }
}

/// `[[min, max]]`, or `[[min, 180], [-180, max]]` when `min > max`.
pub(crate) fn lon_segments(min_lon: f64, max_lon: f64) -> Segments {
    if min_lon <= max_lon {
        Segments::one(min_lon, max_lon)
    } else {
        Segments { segs: [(min_lon, 180.0), (-180.0, max_lon)], len: 2 }
    }
}

pub(crate) fn lon_in_segments(lon: f64, segments: &[(f64, f64)]) -> bool {
    segments.iter().any(|&(lo, hi)| lon >= lo && lon <= hi)
}

pub(crate) fn lon_segments_overlap(a: &[(f64, f64)], b: &[(f64, f64)]) -> bool {
    a.iter().any(|&(alo, ahi)| b.iter().any(|&(blo, bhi)| alo <= bhi && blo <= ahi))
}

fn ok(n: f64, limit: f64) -> bool {
    n.is_finite() && n.abs() <= limit
}

pub(crate) fn validate(region: &Region) -> Result<(), ArgumentError> {
    match *region {
        Region::Bbox { west: min_lon, south: min_lat, east: max_lon, north: max_lat } => {
            if !(ok(min_lon, 180.0) && ok(max_lon, 180.0) && ok(min_lat, 90.0) && ok(max_lat, 90.0))
                || min_lat > max_lat
            {
                return Err(ArgumentError::InvalidRegion("invalid bbox"));
            }
        }
        Region::Circle { lat, lon, radius } => {
            if !ok(lat, 90.0) || !ok(lon, 180.0) || !radius.is_finite() || radius < 0.0 {
                return Err(ArgumentError::InvalidRegion("invalid circle"));
            }
        }
    }
    Ok(())
}

/// The centre distances are measured from, as `(lat, lon)`.
pub(crate) fn center(region: &Region) -> (f64, f64) {
    match *region {
        Region::Circle { lat, lon, .. } => (lat, lon),
        Region::Bbox { west: min_lon, south: min_lat, east: max_lon, north: max_lat } => {
            let mut lon =
                if min_lon <= max_lon { (min_lon + max_lon) / 2.0 } else { (min_lon + max_lon + 360.0) / 2.0 };
            if lon > 180.0 {
                lon -= 360.0;
            }
            ((min_lat + max_lat) / 2.0, lon)
        }
    }
}

pub(crate) fn haversine(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let rad = std::f64::consts::PI / 180.0;
    let a = (((lat2 - lat1) * rad) / 2.0).sin().powi(2)
        + (lat1 * rad).cos() * (lat2 * rad).cos() * (((lon2 - lon1) * rad) / 2.0).sin().powi(2);
    // f64::min returns the other operand for a NaN, where Math.min returns NaN; keep the NaN.
    let s = a.sqrt();
    let s = if s.is_nan() { s } else { s.min(1.0) };
    2.0 * EARTH_RADIUS_M * s.asin()
}

/// The circle's enclosing box, `(min_lon, min_lat, max_lon, max_lat)`; `min_lon > max_lon` when it wraps.
fn circle_box(lat: f64, lon: f64, radius: f64) -> (f64, f64, f64, f64) {
    let half = radius / DEG_TO_M;
    let min_lat = (lat - half).max(-90.0);
    let max_lat = (lat + half).min(90.0);
    // Half-width of a spherical cap: asin(sin r / cos lat). At or past 1 the cap holds a pole.
    let sin_half_lon = (half * std::f64::consts::PI / 180.0).sin() / (lat * std::f64::consts::PI / 180.0).cos();
    #[allow(clippy::neg_cmp_op_on_partial_ord)] // NaN must take the pole branch too
    if min_lat == -90.0 || max_lat == 90.0 || !(sin_half_lon < 1.0) {
        return (-180.0, min_lat, 180.0, max_lat);
    }
    let half_lon = sin_half_lon.asin() * 180.0 / std::f64::consts::PI;
    let wrap = |x: f64| {
        if x > 180.0 {
            x - 360.0
        } else if x < -180.0 {
            x + 360.0
        } else {
            x
        }
    };
    (wrap(lon - half_lon), min_lat, wrap(lon + half_lon), max_lat)
}

fn snap_down(x: f64) -> i64 {
    let r = (x * 100.0).round();
    if (x * 100.0 - r).abs() < EPSILON { r as i64 } else { (x * 100.0).floor() as i64 }
}

fn snap_up(x: f64) -> i64 {
    let r = (x * 100.0).round();
    if (x * 100.0 - r).abs() < EPSILON { r as i64 } else { (x * 100.0).ceil() as i64 }
}

/// Outward to the 0.01° grid, in hundredths; a point or a line is widened by one step.
fn snap_span(min: f64, max: f64, limit: i64) -> (i64, i64) {
    let (mut lo, mut hi) = (snap_down(min), snap_up(max));
    if lo == hi {
        if hi < limit * 100 {
            hi += 1;
        } else {
            lo -= 1;
        }
    }
    (lo, hi)
}

/// Hundredths of a degree as `-122.42`; negative zero cannot arise from an integer.
fn fixed(units: i64) -> String {
    let a = units.unsigned_abs();
    format!("{}{}.{:02}", if units < 0 { "-" } else { "" }, a / 100, a % 100)
}

/// The snapped, serialized getTiles bbox strings, one per antimeridian half, in call order.
pub(crate) fn query_bboxes(region: &Region) -> Vec<String> {
    let (min_lon, min_lat, max_lon, max_lat) = match *region {
        Region::Bbox { west: min_lon, south: min_lat, east: max_lon, north: max_lat } => {
            (min_lon, min_lat, max_lon, max_lat)
        }
        Region::Circle { lat, lon, radius } => circle_box(lat, lon, radius),
    };
    let (lat0, lat1) = snap_span(min_lat, max_lat, 90);
    lon_segments(min_lon, max_lon)
        .as_slice()
        .iter()
        .map(|&(lo, hi)| {
            let (lon0, lon1) = snap_span(lo, hi, 180);
            format!("{},{},{},{}", fixed(lon0), fixed(lat0), fixed(lon1), fixed(lat1))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // The contract's §3 table.
    #[test]
    fn bbox_strings_match_the_contract_table() {
        let table: &[(Region, &[&str])] = &[
            (Region::circle(37.7598, -122.4194, 1000.0), &["-122.44,37.75,-122.40,37.77"]),
            (Region::circle(10.0, 20.0, 0.0), &["20.00,10.00,20.01,10.01"]),
            (Region::circle(0.0, 179.999, 5000.0), &["179.95,-0.05,180.00,0.05", "-180.00,-0.05,-179.95,0.05"]),
            (Region::circle(0.0, -179.999, 5000.0), &["179.95,-0.05,180.00,0.05", "-180.00,-0.05,-179.95,0.05"]),
            (Region::circle(89.9, 0.0, 50000.0), &["-180.00,89.45,180.00,90.00"]),
            (Region::circle(80.0, 0.0, 100000.0), &["-5.19,79.10,5.19,80.90"]),
            (Region::circle(-0.0, -0.0, 0.0), &["0.00,0.00,0.01,0.01"]),
            (Region::bbox(-122.42, 37.77, -122.41, 37.78), &["-122.42,37.77,-122.41,37.78"]),
            (Region::bbox(-122.4194155, 37.7749295, -122.4094155, 37.7849295), &["-122.42,37.77,-122.40,37.79"]),
            (Region::bbox(170.0, 10.0, -170.0, 20.0), &["170.00,10.00,180.00,20.00", "-180.00,10.00,-170.00,20.00"]),
            (Region::bbox(180.0, 90.0, 180.0, 90.0), &["179.99,89.99,180.00,90.00"]),
            (Region::bbox(10.0, 10.0, 10.5, 10.0), &["10.00,10.00,10.50,10.01"]),
        ];
        for (region, expected) in table {
            assert_eq!(query_bboxes(region), *expected, "{region:?}");
        }
    }

    #[test]
    fn centre_of_a_crossing_box() {
        assert_eq!(center(&Region::bbox(170.0, 10.0, -170.0, 20.0)), (15.0, 180.0));
        assert_eq!(center(&Region::bbox(100.0, 0.0, -100.0, 0.0)), (0.0, 180.0));
    }

    #[test]
    fn fixed_formats_hundredths() {
        assert_eq!(fixed(-12242), "-122.42");
        assert_eq!(fixed(18000), "180.00");
        assert_eq!(fixed(0), "0.00");
        assert_eq!(fixed(-1), "-0.01");
        assert_eq!(fixed(5), "0.05");
    }

    #[test]
    fn haversine_keeps_a_nan() {
        assert!(haversine(-89.0, 0.0, 269.0, 180.0).is_nan());
        assert!((haversine(0.0, 0.0, 1.0, 0.0) - 111195.0802335329).abs() < 1e-6);
        assert!((haversine(37.7749, -122.4194, 34.0522, -118.2437) - 559121.3493517868).abs() < 1e-6);
    }
}
