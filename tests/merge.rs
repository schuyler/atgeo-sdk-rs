//! A port of the reference's `test/merge.test.ts`: dedupe, ordering ties, region filtering and determinism.

#[macro_use]
mod common;

use std::time::Duration;

use atgeo::{Region, SearchOptions};
use common::*;

fn search(kind: Kind, records: Vec<Value>, region: Region) -> Vec<String> {
    let fake = FakeServer::with_tilesets(vec![overture(vec![tile(&url("t"), records.into_iter().map(rec).collect())])]);
    ids_sorted(&kind.client(&fake).ok(region, SearchOptions::new()))
}

fn divisions(tiles: Vec<FakeTile>) -> FakeTileset {
    FakeTileset {
        slug: Some("overture-division".into()),
        collections: vec![json!({ "dataset": "overturemaps.org", "collection": "division" })],
        references: None,
        tiles,
    }
}

fn division(id: &str, name: &str) -> Value {
    place(
        id,
        name,
        vec![point(37.775, -122.415)],
        json!({ "origin": format!("urn:atgeo:overturemaps.org:division:{id}") }),
    )
}

fn names(r: &atgeo::SearchResult) -> Vec<String> {
    r.places.iter().map(|h| h.name().unwrap().to_owned()).collect()
}

// describe("deduplication")
mod deduplication {
    use super::*;

    fn first() -> Value {
        simple("dup", "First copy", vec![point(37.775, -122.415)])
    }
    fn second() -> Value {
        simple("dup", "Second copy", vec![point(37.775, -122.415)])
    }

    both! {
        fn collapses_a_duplicate_origin_across_tiles_to_one_result(kind: Kind) {
            let fake = FakeServer::with_tilesets(vec![overture(vec![
                tile(&url("a"), vec![rec(first()), rec(simple("a-only", "A", vec![point(37.772, -122.412)]))]),
                tile(&url("b"), vec![rec(second()), rec(simple("b-only", "B", vec![point(37.773, -122.413)]))]),
            ])]);
            assert_eq!(ids_sorted(&kind.client(&fake).ok(REGION, SearchOptions::new())), ["a-only", "b-only", "dup"]);
        }
    }

    both! {
        fn keeps_the_first_tiles_copy_in_the_default_order_too(kind: Kind) {
            let fake = FakeServer::with_tilesets(vec![overture(vec![tile(&url("a"), vec![rec(first())]), tile(&url("b"), vec![rec(second())])])]);
            assert_eq!(names(&kind.client(&fake).ok(REGION, SearchOptions::new())), ["First copy"]);
        }
    }

    both! {
        fn merges_a_division_record_held_by_two_tiles_into_one_hit(kind: Kind) {
            let fake = FakeServer::with_tilesets(vec![divisions(vec![
                tile(&url("a"), vec![rec(division("d1", "Springfield"))]),
                tile(&url("b"), vec![rec(division("d1", "Springfield"))]),
            ])]);
            let result = kind.client(&fake).ok(REGION, SearchOptions::new().dataset("overturemaps.org").collection("division"));
            assert_eq!(origins(&result), [Some("urn:atgeo:overturemaps.org:division:d1".to_owned())]);
        }
    }

    both! {
        fn keeps_the_lowest_url_copy_across_an_antimeridian_split(kind: Kind) {
            for (east_name, west_name, winner) in [("a", "z", "East copy"), ("z", "a", "West copy")] {
                let (e, w) = (url(east_name), url(west_name));
                let fake = FakeServer::builder()
                    .tilesets(vec![overture(vec![
                        tile(&e, vec![rec(simple("x", "East copy", vec![point(15.0, 175.0)]))]),
                        tile(&w, vec![rec(simple("x", "West copy", vec![point(15.0, -175.0)]))]),
                    ])])
                    .tiles_for(move |bbox| TilesFor::Urls(vec![if bbox.starts_with("170") { e.clone() } else { w.clone() }]))
                    .build();
                let result = kind.client(&fake).ok(Region::bbox(170.0, 10.0, -170.0, 20.0), SearchOptions::new());
                assert_eq!(names(&result), [winner]);
            }
        }
    }

    both! {
        fn keeps_two_records_that_have_neither_origin_nor_uri(kind: Kind) {
            let anonymous = |name: &str| place("x", name, vec![point(37.775, -122.415)], json!({ "origin": null }));
            let fake = FakeServer::with_tilesets(vec![overture(vec![tile(&url("a"), vec![rec(anonymous("One")), rec(anonymous("Two"))])])]);
            let result = kind.client(&fake).ok(REGION, SearchOptions::new().dataset("overturemaps.org"));
            let mut got = names(&result);
            got.sort();
            assert_eq!(got, ["One", "Two"]);
            assert!(result.places.iter().all(|h| h.origin.is_none()));
        }
    }

    both! {
        fn identifies_an_origin_less_record_by_its_uri(kind: Kind) {
            let value = place("x", "By uri", vec![point(37.775, -122.415)], json!({ "origin": null }));
            let fake = FakeServer::with_tilesets(vec![overture(vec![tile(&url("a"), vec![record(value, None, Some("https://example.org/r/1"))])])]);
            let result = kind.client(&fake).ok(REGION, SearchOptions::new().dataset("overturemaps.org"));
            assert_eq!(origins(&result), [Some("https://example.org/r/1".to_owned())]);
        }
    }
}

// describe("ordering ties")
mod ordering_ties {
    use super::*;

    both! {
        fn orders_records_equal_on_score_importance_and_distance_by_origin_null_last(kind: Kind) {
            let same = |id: &str, extra: Value| record(place(id, "Same", vec![point(37.775, -122.415)], extra), Some(5), None);
            let fake = FakeServer::with_tilesets(vec![overture(vec![tile(
                &url("a"),
                vec![same("x", json!({ "origin": null })), same("c", json!({})), same("a", json!({})), same("b", json!({}))],
            )])]);
            let result = kind.client(&fake).ok(REGION, SearchOptions::new().dataset("overturemaps.org"));
            assert_eq!(origins(&result), [Some(place_origin("a")), Some(place_origin("b")), Some(place_origin("c")), None]);
        }
    }
}

// describe("region filtering: bbox")
mod region_filtering_bbox {
    use super::*;

    const REGION: Region = Region::bbox(-122.4194, 37.7749, -122.415, 37.7799);

    both! {
        fn drops_a_point_inside_the_snapped_box_but_outside_the_region(kind: Kind) {
            let kept = search(kind, vec![
                simple("in", "In", vec![point(37.777, -122.417)]),
                simple("south", "South", vec![point(37.771, -122.417)]),
                simple("east", "East", vec![point(37.777, -122.411)]),
                simple("far", "Far", vec![point(40.0, -100.0)]),
            ], REGION);
            assert_eq!(kept, ["in"]);
        }
    }

    both! {
        fn keeps_a_point_on_the_region_boundary(kind: Kind) {
            assert_eq!(search(kind, vec![simple("edge", "Edge", vec![point(37.7749, -122.4194)])], REGION), ["edge"]);
        }
    }

    both! {
        fn passes_bbox_only_records_by_intersection(kind: Kind) {
            let kept = search(kind, vec![
                simple("overlap", "Overlap", vec![bbox_location(37.776, 37.7, -122.416, -122.5)]),
                simple("encloses", "Encloses", vec![bbox_location(38.0, 37.0, -122.0, -123.0)]),
                simple("inside", "Inside", vec![bbox_location(37.777, 37.776, -122.417, -122.418)]),
                simple("north", "North", vec![bbox_location(38.0, 37.9, -122.416, -122.418)]),
                simple("west", "West", vec![bbox_location(37.777, 37.776, -122.5, -122.6)]),
            ], REGION);
            assert_eq!(kept, ["encloses", "inside", "overlap"]);
        }
    }

    both! {
        fn tests_a_record_with_both_point_and_bbox_on_the_point(kind: Kind) {
            let kept = search(kind, vec![
                simple("point-out", "Point out", vec![point(40.0, -100.0), bbox_location(38.0, 37.0, -122.0, -123.0)]),
                simple("point-in", "Point in", vec![bbox_location(10.0, 9.0, 10.0, 9.0), point(37.777, -122.417)]),
            ], REGION);
            assert_eq!(kept, ["point-in"]);
        }
    }

    both! {
        fn drops_records_with_no_parseable_location(kind: Kind) {
            let mut garbage = point(0.0, 0.0);
            garbage["latitude"] = json!("north-ish");
            let kept = search(kind, vec![
                simple("ok", "Ok", vec![point(37.777, -122.417)]),
                simple("none", "None", vec![]),
                simple("address", "Address", vec![json!({ "$type": "community.lexicon.location.address", "country": "US", "street": "1 Main St" })]),
                simple("garbage", "Garbage", vec![garbage]),
                json!({ "$type": "org.atgeo.place", "origin": place_origin("missing"), "name": "Missing" }),
            ], REGION);
            assert_eq!(kept, ["ok"]);
        }
    }

    both! {
        fn handles_a_crossing_region_against_points_and_crossing_bboxes(kind: Kind) {
            let kept = search(kind, vec![
                simple("east-side", "E", vec![point(0.0, 175.0)]),
                simple("west-side", "W", vec![point(0.0, -175.0)]),
                simple("prime", "P", vec![point(0.0, 0.0)]),
                simple("crossing-box", "X", vec![bbox_location(1.0, -1.0, -175.0, 178.0)]),
                simple("far-box", "F", vec![bbox_location(1.0, -1.0, 100.0, 90.0)]),
            ], Region::bbox(170.0, -10.0, -170.0, 10.0));
            assert_eq!(kept, ["crossing-box", "east-side", "west-side"]);
        }
    }
}

// describe("region filtering: circle")
mod region_filtering_circle {
    use super::*;

    both! {
        fn uses_the_true_radius_not_the_enclosing_square(kind: Kind) {
            let (lat, lon) = (37.7749, -122.4194);
            let kept = search(kind, vec![
                simple("near", "Near", vec![point(lat + 0.003, lon)]),
                simple("corner", "Corner", vec![point(lat + 0.004, lon + 0.005)]),
                simple("beyond", "Beyond", vec![point(lat + 0.005, lon)]),
            ], Region::circle(lat, lon, 500.0));
            assert_eq!(kept, ["near"]);
        }
    }

    both! {
        fn passes_bbox_only_records_by_nearest_point_distance(kind: Kind) {
            let kept = search(kind, vec![
                simple("close-box", "Close", vec![bbox_location(0.4, 0.3, 0.4, 0.3)]),
                simple("corner-box", "Corner", vec![bbox_location(0.5, 0.4, 0.5, 0.4)]),
                simple("surrounding", "Surrounding", vec![bbox_location(1.0, -1.0, 1.0, -1.0)]),
            ], Region::circle(0.0, 0.0, 55597.0));
            assert_eq!(kept, ["close-box", "surrounding"]);
        }
    }

    both! {
        fn keeps_a_high_latitude_bbox_only_record_that_clips_the_circle_with_distance_within_the_radius(kind: Kind) {
            let (lat, radius) = (85.0, 370000.0);
            // Measured to the box edge along the centre's parallel: 379.9 km. The true nearest point is 357.1 km.
            let fake = FakeServer::with_tilesets(vec![overture(vec![tile(&url("t"), vec![rec(simple("clipping", "Clipping", vec![bbox_location(89.0, 80.0, 50.0, 40.0)]))])])]);
            let result = kind.client(&fake).ok(Region::circle(lat, 0.0, radius), SearchOptions::new());
            assert_eq!(ids_sorted(&result), ["clipping"]);
            assert!(result.places[0].distance <= radius);
        }
    }

    both! {
        fn finds_the_nearest_longitude_around_the_antimeridian(kind: Kind) {
            let kept = search(kind, vec![
                simple("near-edge", "Near", vec![bbox_location(1.0, -1.0, 180.0, 170.0)]),
                simple("far-edge", "Far", vec![bbox_location(1.0, -1.0, 170.0, 160.0)]),
                simple("crossing", "Crossing", vec![bbox_location(1.0, -1.0, -170.0, 170.0)]),
            ], Region::circle(0.0, -175.0, 667170.0));
            assert_eq!(kept, ["crossing", "near-edge"]);
        }
    }

    both! {
        fn measures_point_distance_across_the_antimeridian(kind: Kind) {
            let kept = search(kind, vec![
                simple("p-wrap", "Wrap", vec![point(0.0, 179.5)]),
                simple("p-far", "Far", vec![point(0.0, 178.0)]),
                simple("p-same", "Same", vec![point(0.0, -176.0)]),
            ], Region::circle(0.0, -175.0, 667170.0));
            assert_eq!(kept, ["p-same", "p-wrap"]);
        }
    }
}

// describe("determinism")
mod determinism {
    use super::*;

    fn tiles() -> Vec<FakeTile> {
        let r =
            |id: &str, name: &str, lat: f64, lon: f64| record(simple(id, name, vec![point(lat, lon)]), Some(5), None);
        vec![
            tile(
                &url("a"),
                vec![
                    r("dup", "Copy A", 37.775, -122.415),
                    r("a1", "Alpha", 37.772, -122.412),
                    r("a2", "Alpha", 37.772, -122.412),
                ],
            ),
            tile(&url("b"), vec![r("dup", "Copy B", 37.775, -122.415), r("b1", "Alpha", 37.772, -122.412)]),
            tile(&url("c"), vec![r("dup", "Copy C", 37.775, -122.415), r("c1", "Alpha", 37.772, -122.412)]),
        ]
    }

    fn permutations(xs: &[&'static str]) -> Vec<Vec<&'static str>> {
        if xs.len() <= 1 {
            return vec![xs.to_vec()];
        }
        (0..xs.len())
            .flat_map(|i| {
                let mut rest = xs.to_vec();
                let x = rest.remove(i);
                permutations(&rest).into_iter().map(move |mut p| {
                    p.insert(0, x);
                    p
                })
            })
            .collect()
    }

    both! {
        fn gives_identical_output_for_every_tile_arrival_order(kind: Kind) {
            for text in [None, Some("Alpha")] {
                let mut outputs = Vec::new();
                for order in permutations(&["a", "b", "c"]) {
                    let fake = FakeServer::with_tilesets(vec![overture(tiles())]);
                    for (rank, n) in order.iter().enumerate() {
                        fake.delay(&url(n), Duration::from_millis(rank as u64 * 15));
                    }
                    let options = match text {
                        Some(t) => SearchOptions::new().text(t),
                        None => SearchOptions::new(),
                    };
                    outputs.push(kind.client(&fake).ok(REGION, options));
                }
                assert!(outputs.windows(2).all(|w| w[0] == w[1]), "{text:?}");
                assert_eq!(ids_sorted(&outputs[0]), ["a1", "a2", "b1", "c1", "dup"]);
                let dup = outputs[0].places.iter().find(|h| h.origin == Some(place_origin("dup"))).unwrap();
                assert_eq!(dup.name(), Some("Copy A"));
            }
        }
    }
}
