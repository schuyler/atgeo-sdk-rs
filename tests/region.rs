//! A port of the reference's `test/region.test.ts`, plus the contract's §3 table through the client.

#[macro_use]
mod common;

use atgeo::{ArgumentError, Error, Region, SearchOptions, SortMode};
use common::*;

/// The bbox of each getTiles request a search for `region` makes.
fn emitted(kind: Kind, region: Region) -> Vec<String> {
    let fake = FakeServer::new();
    kind.client(&fake).ok(region, SearchOptions::new());
    let bboxes: Vec<String> = fake.tile_list_requests().iter().map(|r| param(r, "bbox").unwrap_or_default()).collect();
    in_segment_order(kind, bboxes)
}

/// The blocking client sends the two halves of a crossing region from two threads at once, so either may
/// reach the server first (CONFORMANCE.md). This puts them back in segment order: the second half is the
/// one starting at -180. The async client starts them in order, and is held to it.
#[cfg(feature = "blocking")]
fn in_segment_order(kind: Kind, mut bboxes: Vec<String>) -> Vec<String> {
    if kind == Kind::Blocking {
        bboxes.sort_by_key(|b| b.starts_with("-180.00,"));
    }
    bboxes
}

#[cfg(not(feature = "blocking"))]
fn in_segment_order(_: Kind, bboxes: Vec<String>) -> Vec<String> {
    bboxes
}

fn parse(bbox: &str) -> Vec<f64> {
    bbox.split(',').map(|v| v.parse().unwrap()).collect()
}

/// `^-?\d{1,3}\.\d{2}$`
fn is_coord(c: &str) -> bool {
    let c = c.strip_prefix('-').unwrap_or(c);
    let Some((int, frac)) = c.split_once('.') else { return false };
    (1..=3).contains(&int.len())
        && int.bytes().all(|b| b.is_ascii_digit())
        && frac.len() == 2
        && frac.bytes().all(|b| b.is_ascii_digit())
}

// describe("coordinate serialization")
mod coordinate_serialization {
    use super::*;

    both! {
        fn emits_only_two_decimal_coordinates(kind: Kind) {
            let adversarial = [
                ("full GPS precision circle", Region::circle(37.7749295, -122.4194155, 250.123456)),
                ("full GPS precision bbox", Region::bbox(-122.4194155, 37.7749295, -122.4094155, 37.7849295)),
                ("trailing zeros", Region::bbox(-122.4, 37.7, -122.3, 37.8)),
                ("scientific notation tiny", Region::bbox(1e-7, 1e-7, 2e-7, 2e-7)),
                ("scientific notation", Region::bbox(3.7749e1, 3.7749e1, 3.775e1, 3.776e1)),
                ("negative scientific notation", Region::bbox(-1e-7, -2e-7, 1e-7, 2e-7)),
                ("tiny radius", Region::circle(12.3456789, 98.7654321, 1e-9)),
                ("radius zero", Region::circle(12.3456789, 98.7654321, 0.0)),
                ("zero-area bbox", Region::bbox(10.123456, 20.123456, 10.123456, 20.123456)),
                ("negative zero bbox", Region::bbox(-0.0, -0.0, 0.0, 0.0)),
                ("negative zero circle", Region::circle(-0.0, -0.0, 0.0)),
                ("tiny negative", Region::bbox(-0.0001, -0.0001, -0.00001, -0.00001)),
                ("circle at pole", Region::circle(90.0, 0.0, 1000.0)),
                ("circle at antimeridian", Region::circle(0.0, 180.0, 1000.0)),
                ("crossing bbox", Region::bbox(179.123456, 1.987654, -179.123456, 2.987654)),
            ];
            for (name, region) in adversarial {
                let bboxes = emitted(kind, region);
                assert!(!bboxes.is_empty(), "{name}");
                for bbox in bboxes {
                    let parts: Vec<&str> = bbox.split(',').collect();
                    assert_eq!(parts.len(), 4, "{name}: {bbox}");
                    assert!(parts.iter().all(|c| is_coord(c)), "{name}: {bbox}");
                }
            }
        }
    }

    both! {
        fn never_emits_a_coordinate_outside_the_valid_range(kind: Kind) {
            let regions = [
                Region::bbox(-180.0, -90.0, 180.0, 90.0),
                Region::bbox(180.0, 90.0, 180.0, 90.0),
                Region::bbox(-180.0, -90.0, -180.0, -90.0),
                Region::circle(90.0, 180.0, 5.0),
                Region::circle(-90.0, -180.0, 5.0),
                Region::circle(89.999, 179.999, 100000.0),
            ];
            for region in regions {
                for bbox in emitted(kind, region) {
                    let b = parse(&bbox);
                    assert!(b[0] >= -180.0 && b[2] <= 180.0 && b[1] >= -90.0 && b[3] <= 90.0, "{region:?}: {bbox}");
                    assert!(b[0] < b[2] && b[1] < b[3], "{region:?}: {bbox}");
                }
            }
        }
    }
}

// describe("outward snapping")
mod outward_snapping {
    use super::*;

    both! {
        fn floors_mins_and_ceils_maxes(kind: Kind) {
            assert_eq!(emitted(kind, Region::bbox(-122.4194155, 37.7749295, -122.4094155, 37.7849295)), ["-122.42,37.77,-122.40,37.79"]);
        }
    }

    both! {
        fn contains_the_callers_region_for_arbitrary_precision_input(kind: Kind) {
            let inputs = [
                [-122.419415, 37.774929, -122.418001, 37.775001],
                [0.001, 0.002, 0.003, 0.004],
                [-0.004, -0.003, -0.002, -0.001],
                [151.2093, -33.8688, 151.2193, -33.8588],
                [-73.9857, 40.7484, -73.9757, 40.7584],
            ];
            for [min_lon, min_lat, max_lon, max_lat] in inputs {
                let bbox = &emitted(kind, Region::bbox(min_lon, min_lat, max_lon, max_lat))[0];
                let b = parse(bbox);
                assert!(b[0] <= min_lon && b[1] <= min_lat && b[2] >= max_lon && b[3] >= max_lat, "{bbox}");
                assert!(b[2] - b[0] < max_lon - min_lon + 0.0201, "{bbox}");
                assert!(b[3] - b[1] < max_lat - min_lat + 0.0201, "{bbox}");
            }
        }
    }

    both! {
        fn does_not_widen_a_grid_aligned_bbox_through_float_error(kind: Kind) {
            assert_eq!(emitted(kind, REGION), ["-122.42,37.77,-122.41,37.78"]);
        }
    }

    both! {
        fn does_not_widen_grid_aligned_values_anywhere_on_the_grid(kind: Kind) {
            let mut k = 0;
            while k < 9000 {
                let lat = [(k + 1) as f64 / 100.0, (k + 6) as f64 / 100.0];
                let lon = [-((k + 8) as f64) / 100.0, -((k + 5) as f64) / 100.0];
                let expected = format!("{:.2},{:.2},{:.2},{:.2}", lon[0], lat[0], lon[1], lat[1]);
                assert_eq!(emitted(kind, Region::bbox(lon[0], lat[0], lon[1], lat[1])), [expected]);
                k += 97;
            }
        }
    }
}

// describe("degenerate spans")
mod degenerate_spans {
    use super::*;

    both! {
        fn widens_a_grid_aligned_point_by_one_step(kind: Kind) {
            assert_eq!(emitted(kind, Region::bbox(10.0, 20.0, 10.0, 20.0)), ["10.00,20.00,10.01,20.01"]);
        }
    }

    both! {
        fn widens_a_zero_radius_circle_on_the_grid(kind: Kind) {
            assert_eq!(emitted(kind, Region::circle(20.0, 10.0, 0.0)), ["10.00,20.00,10.01,20.01"]);
        }
    }

    both! {
        fn widens_negative_zero(kind: Kind) {
            assert_eq!(emitted(kind, Region::bbox(-0.0, -0.0, 0.0, 0.0)), ["0.00,0.00,0.01,0.01"]);
        }
    }

    both! {
        fn widens_a_line(kind: Kind) {
            assert_eq!(emitted(kind, Region::bbox(10.0, 20.0, 10.5, 20.0)), ["10.00,20.00,10.50,20.01"]);
        }
    }

    both! {
        fn steps_min_down_at_latitude_90_and_longitude_180(kind: Kind) {
            assert_eq!(emitted(kind, Region::bbox(0.0, 90.0, 0.0, 90.0)), ["0.00,89.99,0.01,90.00"]);
            assert_eq!(emitted(kind, Region::bbox(180.0, 0.0, 180.0, 0.0)), ["179.99,0.00,180.00,0.01"]);
        }
    }

    both! {
        fn does_not_widen_a_span_that_snapping_already_opened(kind: Kind) {
            assert_eq!(emitted(kind, Region::bbox(10.005, 20.005, 10.005, 20.005)), ["10.00,20.00,10.01,20.01"]);
        }
    }
}

// describe("circle to bbox")
mod circle_to_bbox {
    use super::*;

    both! {
        fn uses_111194_927_m_per_degree_of_latitude(kind: Kind) {
            assert_eq!(emitted(kind, Region::circle(0.0, 0.0, 111194.927)), ["-1.00,-1.00,1.00,1.00"]);
        }
    }

    both! {
        fn widens_longitude_to_the_spherical_caps_half_width(kind: Kind) {
            let b = parse(&emitted(kind, Region::circle(60.0, 10.0, 111194.927))[0]);
            assert!((b[1] - 59.0).abs() < 0.05 && (b[3] - 61.0).abs() < 0.05, "{b:?}");
            assert!((7.99..=8.0).contains(&b[0]), "{b:?}");
            assert!((12.0..=12.01).contains(&b[2]), "{b:?}");
        }
    }

    // asin(sin 20° / cos 60°) = 43.16°; the flat 20° / cos 60° is 40°.
    both! {
        fn reaches_the_caps_true_east_and_west_edges_at_high_latitude(kind: Kind) {
            assert_eq!(emitted(kind, Region::circle(60.0, 0.0, 20.0 * 111194.927)), ["-43.17,40.00,43.17,80.00"]);
        }
    }

    both! {
        fn covers_all_longitudes_for_a_polar_circle(kind: Kind) {
            let north = emitted(kind, Region::circle(89.9, 12.34, 50000.0));
            assert_eq!(north.len(), 1);
            let n = parse(&north[0]);
            assert_eq!([n[0], n[2], n[3]], [-180.0, 180.0, 90.0]);
            assert!((89.44..=89.46).contains(&n[1]), "{n:?}");

            let south = emitted(kind, Region::circle(-89.9, -45.0, 50000.0));
            assert_eq!(south.len(), 1);
            let s = parse(&south[0]);
            assert_eq!([s[0], s[2], s[1]], [-180.0, 180.0, -90.0]);
            assert!((-89.46..=-89.44).contains(&s[3]), "{s:?}");
        }
    }
}

// describe("antimeridian")
mod antimeridian {
    use super::*;

    both! {
        fn splits_a_crossing_bbox_into_two_get_tiles_calls(kind: Kind) {
            assert_eq!(emitted(kind, Region::bbox(170.0, 10.0, -170.0, 20.0)), ["170.00,10.00,180.00,20.00", "-180.00,10.00,-170.00,20.00"]);
        }
    }

    both! {
        fn splits_a_circle_that_wraps_past_180(kind: Kind) {
            assert_eq!(emitted(kind, Region::circle(0.0, 179.5, 111194.927)), ["178.50,-1.00,180.00,1.00", "-180.00,-1.00,-179.50,1.00"]);
        }
    }

    both! {
        fn splits_a_circle_that_wraps_past_minus_180(kind: Kind) {
            assert_eq!(emitted(kind, Region::circle(0.0, -179.5, 111194.927)), ["179.50,-1.00,180.00,1.00", "-180.00,-1.00,-178.50,1.00"]);
        }
    }

    both! {
        fn issues_a_single_call_for_a_non_crossing_bbox(kind: Kind) {
            assert_eq!(emitted(kind, Region::bbox(-10.0, 10.0, 10.0, 20.0)).len(), 1);
        }
    }

    both! {
        fn snaps_each_half_independently(kind: Kind) {
            assert_eq!(emitted(kind, Region::bbox(170.123, 10.123, -170.123, 20.123)), ["170.12,10.12,180.00,20.13", "-180.00,10.12,-170.12,20.13"]);
        }
    }

    both! {
        fn merges_results_from_both_halves(kind: Kind) {
            let (west, east) = ("https://tiles.example/west.json", "https://tiles.example/east.json");
            let fake = FakeServer::builder()
                .tilesets(vec![overture(vec![
                    tile(west, vec![rec(simple("w", "West", vec![point(15.0, 175.0)]))]),
                    tile(east, vec![rec(simple("e", "East", vec![point(15.0, -175.0)]))]),
                ])])
                .tiles_for(move |bbox| TilesFor::Urls(vec![if bbox.starts_with("170") { west.into() } else { east.into() }]))
                .build();
            let result = kind.client(&fake).ok(Region::bbox(170.0, 10.0, -170.0, 20.0), SearchOptions::new());
            assert_eq!(ids_sorted(&result), ["e", "w"]);
            assert!(result.complete);
        }
    }
}

// describe("invalid regions")
mod invalid_regions {
    use super::*;

    both! {
        fn rejects_with_invalid_argument_before_any_request(kind: Kind) {
            let nan = f64::NAN;
            let inf = f64::INFINITY;
            let bad = [
                ("NaN lat", Region::circle(nan, 0.0, 10.0)),
                ("NaN lon", Region::circle(0.0, nan, 10.0)),
                ("Infinity lon", Region::circle(0.0, inf, 10.0)),
                ("NaN radius", Region::circle(0.0, 0.0, nan)),
                ("Infinity radius", Region::circle(0.0, 0.0, inf)),
                ("negative radius", Region::circle(0.0, 0.0, -1.0)),
                ("lat above 90", Region::circle(90.01, 0.0, 10.0)),
                ("lat below -90", Region::circle(-91.0, 0.0, 10.0)),
                ("lon above 180", Region::circle(0.0, 180.5, 10.0)),
                ("lon below -180", Region::circle(0.0, -181.0, 10.0)),
                ("bbox NaN", Region::bbox(0.0, 0.0, nan, 1.0)),
                ("bbox Infinity", Region::bbox(0.0, 0.0, 1.0, inf)),
                ("bbox -Infinity", Region::bbox(-inf, 0.0, 1.0, 1.0)),
                ("bbox minLat > maxLat", Region::bbox(0.0, 10.0, 1.0, 5.0)),
                ("bbox lat above 90", Region::bbox(0.0, 0.0, 1.0, 91.0)),
                ("bbox lat below -90", Region::bbox(0.0, -91.0, 1.0, 0.0)),
                ("bbox lon above 180", Region::bbox(0.0, 0.0, 181.0, 1.0)),
                ("bbox lon below -180", Region::bbox(-181.0, 0.0, 1.0, 1.0)),
            ];
            for (name, region) in bad {
                let fake = FakeServer::new();
                let e = kind.client(&fake).search(region, SearchOptions::new()).unwrap_err();
                assert!(matches!(e, Error::InvalidArgument(ArgumentError::InvalidRegion(_))), "{name}: {e:?}");
                assert_eq!(fake.request_count(), 0, "{name}");
            }
        }
    }

    both! {
        fn accepts_min_lon_greater_than_max_lon(kind: Kind) {
            assert!(kind.client(&FakeServer::new()).search(Region::bbox(170.0, 0.0, -170.0, 1.0), SearchOptions::new()).is_ok());
        }
    }

    both! {
        fn rejects_similarity_sort_without_text_before_any_request(kind: Kind) {
            for options in [SearchOptions::new().sort(SortMode::Similarity), SearchOptions::new().text(" \u{3000}").sort(SortMode::Similarity)] {
                let fake = FakeServer::new();
                let e = kind.client(&fake).search(Region::bbox(0.0, 0.0, 1.0, 1.0), options).unwrap_err();
                assert!(matches!(e, Error::InvalidArgument(ArgumentError::SimilarityRequiresText)), "{e:?}");
                assert_eq!(fake.request_count(), 0);
            }
        }
    }

    both! {
        fn reports_the_region_before_the_sort_when_both_are_wrong(kind: Kind) {
            let e = kind
                .client(&FakeServer::new())
                .search(Region::circle(0.0, 0.0, -1.0), SearchOptions::new().sort(SortMode::Similarity))
                .unwrap_err();
            assert!(matches!(e, Error::InvalidArgument(ArgumentError::InvalidRegion("invalid circle"))), "{e:?}");
        }
    }
}

// The contract's §3 table, through the client.
both! {
    fn sends_the_contracts_worked_bbox_strings(kind: Kind) {
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
            assert_eq!(emitted(kind, *region), *expected, "{region:?}");
        }
    }
}
