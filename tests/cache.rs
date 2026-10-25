//! A port of the reference's `test/cache.test.ts`: warm regions, expiry and the getPlace cache scan. The LRU
//! and the budget, which the reference does not test, are covered by the unit tests in `src/cache.rs` and by
//! the budget tests at the end of this file.

#[macro_use]
mod common;

use atgeo::Region;
use common::*;

const OTHER_ORIGIN: &str = "urn:atgeo:example.org:thing:shared";

fn setup() -> FakeServer {
    FakeServer::with_tilesets(vec![
        overture(vec![tile(
            &url("a"),
            vec![
                record(simple("a", "Alpha", vec![point(37.772, -122.412)]), Some(3), None),
                record(simple("shared", "Shared in A", vec![point(37.773, -122.413)]), Some(2), None),
            ],
        )]),
        FakeTileset {
            slug: Some("example-thing".into()),
            collections: vec![json!({ "dataset": "example.org", "collection": "thing" })],
            references: None,
            overview: None,
            tiles: vec![tile(
                &url("o"),
                vec![rec(place(
                    "shared",
                    "Shared in other",
                    vec![point(10.0, 10.0)],
                    json!({ "origin": OTHER_ORIGIN }),
                ))],
            )],
        },
    ])
}

fn tile_list_count(fake: &FakeServer) -> usize {
    fake.tile_list_requests().len()
}

// describe("warm regions")
mod warm_regions {
    use super::*;

    both! {
        fn answers_an_identical_second_search_offline(kind: Kind) {
            let fake = setup();
            let client = kind.client(&fake);
            let first = client.ok(REGION, overture_options().text("Alpha"));
            fake.go_offline();
            let second = client.ok(REGION, overture_options().text("Alpha"));
            assert_eq!(second, first);
            assert!(!first.places.is_empty());
        }
    }

    both! {
        fn reuses_tiles_for_a_different_text_over_the_same_region(kind: Kind) {
            let fake = setup();
            let client = kind.client(&fake);
            client.ok(REGION, overture_options());
            let before = fake.request_count();
            client.ok(REGION, overture_options().text("Alpha"));
            assert_eq!(fake.request_count(), before);
        }
    }

    both! {
        fn makes_no_tile_request_for_a_repeated_search(kind: Kind) {
            let fake = setup();
            let client = kind.client(&fake);
            client.ok(REGION, overture_options());
            client.ok(REGION, overture_options());
            assert_eq!(fake.count_of(&url("a")), 1);
        }
    }
}

// describe("tile-list expiry")
mod tile_list_expiry {
    use super::*;

    both! {
        fn expires_after_3600_s_and_then_requires_network(kind: Kind) {
            let fake = setup();
            let clock = TestClock::new();
            let client = kind.with_clock(&fake, &clock);
            client.ok(REGION, overture_options());
            let base = fake.request_count();

            clock.set(59 * 60 + 59);
            client.ok(REGION, overture_options());
            assert_eq!(fake.request_count(), base);

            clock.set(3601);
            client.ok(REGION, overture_options());
            assert_eq!(tile_list_count(&fake), 2);
            assert_eq!(fake.count_of(&url("a")), 1);
        }
    }

    both! {
        fn fails_offline_once_the_tile_list_has_expired(kind: Kind) {
            let fake = setup();
            let clock = TestClock::new();
            let client = kind.with_clock(&fake, &clock);
            client.ok(REGION, overture_options());
            fake.go_offline();
            clock.set(3601);
            assert!(client.search(REGION, overture_options()).is_err());
        }
    }

    both! {
        fn caches_describe_service_for_3600_s(kind: Kind) {
            let fake = setup();
            let clock = TestClock::new();
            let client = kind.with_clock(&fake, &clock);
            client.describe_service().unwrap();
            clock.set(59 * 60 + 59);
            client.describe_service().unwrap();
            assert_eq!(fake.request_count(), 1);
            clock.set(3601);
            client.describe_service().unwrap();
            assert_eq!(fake.request_count(), 2);
        }
    }

    both! {
        fn is_fresh_until_exactly_3600_s(kind: Kind) {
            let fake = setup();
            let clock = TestClock::new();
            let client = kind.with_clock(&fake, &clock);
            client.ok(REGION, overture_options());
            clock.set(3599);
            client.ok(REGION, overture_options());
            assert_eq!(tile_list_count(&fake), 1);
            clock.set(3600);
            client.ok(REGION, overture_options());
            assert_eq!(tile_list_count(&fake), 2);
        }
    }
}

// describe("getPlace cache scan")
mod get_place_cache_scan {
    use super::*;

    both! {
        fn answers_from_an_already_fetched_tile_without_a_request(kind: Kind) {
            let fake = setup();
            let client = kind.client(&fake);
            client.ok(REGION, overture_options());
            let before = fake.request_count();
            let found = client.get_place(&place_origin("a")).unwrap();
            assert_eq!(fake.request_count(), before);
            assert_eq!(found.origin, place_origin("a"));
            assert_eq!(*found.place, map(&simple("a", "Alpha", vec![point(37.772, -122.412)])));
            assert_eq!(found.importance, Some(3));
            assert_eq!(attribution_json(&found.attribution), overture_attribution());
        }
    }

    both! {
        fn answers_offline_for_a_cached_origin(kind: Kind) {
            let fake = setup();
            let client = kind.client(&fake);
            client.ok(REGION, overture_options());
            fake.go_offline();
            assert_eq!(client.get_place(&place_origin("shared")).unwrap().name(), Some("Shared in A"));
        }
    }

    both! {
        fn matches_the_whole_origin_not_a_shared_id(kind: Kind) {
            let fake = setup();
            let client = kind.client(&fake);
            client.ok(REGION, overture_options());
            let before = fake.request_count();
            let found = client.get_place(OTHER_ORIGIN).unwrap();
            let get_places: Vec<String> =
                fake.requests()[before..].iter().filter(|r| nsid(r) == "org.atgeo.getPlace").cloned().collect();
            assert_eq!(get_places.len(), 1);
            assert_eq!(param(&get_places[0], "uri").as_deref(), Some(OTHER_ORIGIN));
            assert_eq!(found.name(), Some("Shared in other"));
        }
    }

    both! {
        fn returns_the_copy_from_the_lowest_tile_url_although_a_higher_one_was_cached_first(kind: Kind) {
            let copy = |name: &str| rec(simple("x", name, vec![point(37.775, -122.415)]));
            let (a, b) = (url("a"), url("b"));
            let fake = FakeServer::builder()
                .tilesets(vec![overture(vec![tile(&a, vec![copy("Copy A")]), tile(&b, vec![copy("Copy B")])])])
                .tiles_for(move |bbox| TilesFor::Urls(vec![if bbox.starts_with("-122") { a.clone() } else { b.clone() }]))
                .build();
            let client = kind.client(&fake);
            client.ok(Region::bbox(-100.5, 10.5, -100.4, 10.6), overture_options());
            assert_eq!(client.get_place(&place_origin("x")).unwrap().name(), Some("Copy B"));
            client.ok(REGION, overture_options());
            fake.go_offline();
            assert_eq!(client.get_place(&place_origin("x")).unwrap().name(), Some("Copy A"));
        }
    }
}

// Contract §10: the budget, which the reference does not test.
mod budget {
    use super::*;

    fn three_tiles() -> FakeServer {
        FakeServer::with_tilesets(vec![overture(
            ["a", "b", "c"]
                .iter()
                .map(|n| tile(&url(n), vec![rec(simple(n, n, vec![point(37.775, -122.415)]))]))
                .collect(),
        )])
    }

    both! {
        fn a_budget_smaller_than_one_tile_caches_nothing_but_the_search_still_uses_every_tile(kind: Kind) {
            let fake = three_tiles();
            let client = kind.configured(&fake, Config { budget: Some(10), ..Config::default() });
            let first = client.ok(REGION, overture_options());
            assert_eq!(first.tiles.fetched, 3);
            assert_eq!(first.places.len(), 3);
            let second = client.ok(REGION, overture_options());
            assert_eq!(second, first);
            assert_eq!(fake.count_of(&url("a")), 2, "evicted tiles are fetched again");
            fake.go_offline();
            assert!(client.get_place(&place_origin("a")).is_err(), "nothing cached to answer from");
        }
    }

    both! {
        fn a_budget_of_two_tiles_keeps_two(kind: Kind) {
            let fake = three_tiles();
            // Every tile body here is the same length, so the size of one tells the budget.
            let one = fake.handle(&url("a")).body.len() as u64;
            let client = kind.configured(&fake, Config { budget: Some(2 * one), ..Config::default() });
            client.ok(REGION, overture_options());
            // Which one goes depends on the order the tiles are stored in, which the blocking client's threads
            // do not fix (CONFORMANCE.md); the async test below pins it.
            fake.go_offline();
            let cached = ["a", "b", "c"].iter().filter(|n| client.get_place(&place_origin(n)).is_ok()).count();
            assert_eq!(cached, 2);
        }
    }

    #[test]
    fn a_budget_of_two_tiles_keeps_the_two_most_recently_used() {
        let fake = three_tiles();
        let one = fake.handle(&url("a")).body.len() as u64;
        let client = Kind::Async.configured(&fake, Config { budget: Some(2 * one), ..Config::default() });
        client.ok(REGION, overture_options());
        // Stored in URL order a, b, c: inserting c evicted a.
        fake.go_offline();
        assert!(client.get_place(&place_origin("a")).is_err());
        assert_eq!(client.get_place(&place_origin("b")).unwrap().name(), Some("b"));
        assert_eq!(client.get_place(&place_origin("c")).unwrap().name(), Some("c"));
    }
}

// Contract §4.4, §10: the reference's workers all look up their first URL before any of them stores a tile.
// Six equal tiles and room for two: the cold search leaves e and f cached, and the warm search must find
// them there, fetching only a to d. Async only: the blocking client's threads race, so which tiles a tight
// budget keeps is not fixed there (CONFORMANCE.md).
#[test]
fn the_first_wave_of_lookups_happens_before_any_tile_is_stored() {
    let names = ["a", "b", "c", "d", "e", "f"];
    let fake = FakeServer::with_tilesets(vec![overture(
        names.iter().map(|n| tile(&url(n), vec![rec(simple(n, n, vec![point(37.775, -122.415)]))])).collect(),
    )]);
    let one = fake.handle(&url("a")).body.len() as u64;
    let client = Kind::Async.configured(&fake, Config { budget: Some(2 * one), ..Config::default() });
    client.ok(REGION, overture_options());
    client.ok(REGION, overture_options());
    let counts: Vec<usize> = names.iter().map(|n| fake.count_of(&url(n))).collect();
    assert_eq!(counts, [2, 2, 2, 2, 1, 1]);
}

// ---- contract §4.10, §9, §10: the attribution a tile carries stays with it in the cache ----------------------------

/// A copy of the default tileset under another slug, with these references.
fn overture_with(slug: &str, references: Vec<Value>, tiles: Vec<FakeTile>) -> FakeTileset {
    let mut t = overture(tiles);
    t.slug = Some(slug.into());
    t.references = Some(references);
    t
}

const REGION_B: Region = Region::bbox(-100.5, 10.5, -100.4, 10.6);
const REGION_C: Region = Region::bbox(-90.5, 10.5, -90.4, 10.6);

// describe("attribution in the tile cache")
mod attribution_in_the_tile_cache {
    use super::*;

    both! {
        fn gives_a_warm_offline_search_the_same_non_empty_attribution_and_hit_collections(kind: Kind) {
            let fake = setup();
            let client = kind.client(&fake);
            let first = client.ok(REGION, overture_options());
            assert!(!first.attribution.is_empty());
            assert!(!first.places.is_empty());
            for hit in &first.places {
                assert_eq!(collection_json(&hit.collection), json!({ "dataset": "overturemaps.org", "collection": "place" }));
            }
            fake.go_offline();
            let second = client.ok(REGION, overture_options());
            assert_eq!(second.attribution, first.attribution);
            let collections = |r: &atgeo::SearchResult| r.places.iter().map(|h| h.collection.clone()).collect::<Vec<_>>();
            assert_eq!(collections(&second), collections(&first));
        }
    }

    both! {
        fn builds_a_searchs_attribution_only_from_its_own_tiles_though_another_tile_is_still_cached(kind: Kind) {
            let entry_a = json!({ "dataset": "ref-a.example", "collection": "c" });
            let entry_b = json!({ "dataset": "ref-b.example", "collection": "c" });
            let (ua, ub) = (url("a"), url("b"));
            let tiles_a = vec![tile(&ua, vec![rec(simple("pa", "A", vec![point(37.775, -122.415)]))])];
            let tiles_b = vec![tile(&ub, vec![rec(simple("pb", "B", vec![point(10.55, -100.45)]))])];
            let fake = FakeServer::builder()
                .tilesets(vec![
                    overture_with("overture-place", vec![entry_a.clone()], tiles_a),
                    overture_with("overture-place-b", vec![entry_b.clone()], tiles_b),
                ])
                .tiles_for(move |bbox| TilesFor::Urls(vec![if bbox.starts_with("-122") { ua.clone() } else { ub.clone() }]))
                .build();
            let client = kind.client(&fake);
            let place_ = json!({ "dataset": "overturemaps.org", "collection": "place" });
            let a = client.ok(REGION, overture_options());
            let b = client.ok(REGION_B, overture_options());
            assert_eq!(attribution_json(&a.attribution), json!([place_, entry_a]));
            assert_eq!(attribution_json(&b.attribution), json!([place_, entry_b]));
        }
    }
}

// describe("getPlace cache scan"), continued
mod get_place_attribution {
    use super::*;

    both! {
        fn takes_the_attribution_of_the_lowest_url_tile_as_served_though_higher_ones_were_cached_before_and_after_it(kind: Kind) {
            let references = vec![
                json!({ "dataset": "z.example", "collection": "c" }),
                json!({ "dataset": "a.example", "collection": "c", "release": "1" }),
                json!({ "dataset": "a.example", "collection": "c", "release": "2" }),
            ];
            let copy = || rec(simple("x", "Copy", vec![point(37.775, -122.415)]));
            let (ua, ub, uc) = (url("a"), url("b"), url("c"));
            let fake = FakeServer::builder()
                .tilesets(vec![
                    overture_with("overture-place", references.clone(), vec![tile(&ua, vec![copy()])]),
                    overture_with("overture-place-b", vec![json!({ "dataset": "b.example", "collection": "c" })], vec![tile(&ub, vec![copy()])]),
                    overture_with("overture-place-c", vec![json!({ "dataset": "c.example", "collection": "c" })], vec![tile(&uc, vec![copy()])]),
                ])
                .tiles_for(move |bbox| {
                    TilesFor::Urls(vec![if bbox.starts_with("-122") {
                        ua.clone()
                    } else if bbox.starts_with("-100") {
                        ub.clone()
                    } else {
                        uc.clone()
                    }])
                })
                .build();
            let client = kind.client(&fake);
            // b, then a, then c: the lowest URL is neither the first nor the last cached.
            client.ok(REGION_B, overture_options());
            client.ok(REGION, overture_options());
            client.ok(REGION_C, overture_options());
            fake.go_offline();
            let answer = client.get_place(&place_origin("x")).unwrap();
            let mut expected = vec![json!({ "dataset": "overturemaps.org", "collection": "place" })];
            expected.extend(references);
            assert_eq!(attribution_json(&answer.attribution), Value::Array(expected));
        }
    }

    // Each raw tile body below carries the attribution (or lack of it) named, and the record `x`.
    both! {
        fn gives_an_empty_attribution_from_a_tile_without_a_usable_one(kind: Kind) {
            let cases: [(&str, Option<Value>); 4] = [
                ("no attribution", None),
                ("an attribution that is an object", Some(json!({}))),
                ("an attribution that is null", Some(Value::Null)),
                ("an attribution that is a string", Some(json!("x"))),
            ];
            for (name, attribution) in cases {
                let value = simple("x", "Plain", vec![point(37.775, -122.415)]);
                let mut body = json!({ "records": [{ "value": value, "importance": 3 }] });
                if let Some(a) = attribution {
                    body["attribution"] = a;
                }
                let raw = url("raw");
                let fake = FakeServer::builder()
                    .tilesets(vec![overture(vec![])])
                    .tiles_for({
                        let raw = raw.clone();
                        move |_| TilesFor::Urls(vec![raw.clone()])
                    })
                    .build();
                fake.raw(&raw, 200, body.to_string());
                let client = kind.client(&fake);
                let result = client.ok(REGION, overture_options());
                assert_eq!(result.places.len(), 1, "{name}");
                assert_eq!(result.places[0].collection, None, "{name}");
                assert_eq!(result.attribution, vec![], "{name}");
                let before = fake.request_count();
                let found = client.get_place(&place_origin("x")).unwrap();
                assert_eq!(found.origin, place_origin("x"), "{name}");
                assert_eq!(*found.place, map(&value), "{name}");
                assert_eq!(found.importance, Some(3), "{name}");
                assert_eq!(found.attribution, vec![], "{name}");
                assert_eq!(fake.request_count(), before, "{name}");
            }
        }
    }
}
