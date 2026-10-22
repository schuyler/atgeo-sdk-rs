//! A port of the reference's `test/client.test.ts`, plus the clients' own URL checks (spec §4, URLs).

#[macro_use]
mod common;

use atgeo::{ArgumentError, Error, Region, SearchOptions, SortMode};
use common::*;

// describe("getPlace")
mod get_place {
    use super::*;

    pub fn value() -> Value {
        place(
            "r1",
            "Ferry Building",
            vec![point(37.7955, -122.3937)],
            json!({
                "attributes": { "kind": "landmark" },
                "futureKey": { "nested": [1, 2] },
            }),
        )
    }

    pub fn setup() -> FakeServer {
        let no_importance = simple("r2", "No Importance", vec![point(37.7955, -122.3937)]);
        FakeServer::with_tilesets(vec![overture(vec![tile(
            &url("a"),
            vec![record(value(), Some(80), None), rec(no_importance)],
        )])])
    }

    pub const NEAR_FERRY: Region = Region::bbox(-122.40, 37.79, -122.39, 37.80);

    both! {
        fn sends_one_get_place_request_with_the_origin_as_uri_on_a_cold_client_and_no_describe_service(kind: Kind) {
            let fake = setup();
            kind.client(&fake).get_place(&place_origin("r1")).unwrap();
            let requests = fake.requests();
            assert_eq!(requests.len(), 1);
            assert_eq!(path_of(&requests[0]), format!("{DEFAULT_SERVER}/xrpc/org.atgeo.getPlace"));
            assert_eq!(param(&requests[0], "uri"), Some(place_origin("r1")));
            assert!(!requests.iter().any(|r| nsid(r) == "org.atgeo.describeService"));
        }
    }

    both! {
        fn returns_the_value_unmodified_with_importance_beside_it(kind: Kind) {
            let found = kind.client(&setup()).get_place(&place_origin("r1")).unwrap();
            assert_eq!(found.origin, place_origin("r1"));
            assert_eq!(*found.place, map(&value()));
            assert_eq!(found.importance, Some(80));
        }
    }

    both! {
        fn returns_a_null_importance_when_the_server_gives_none(kind: Kind) {
            assert_eq!(kind.client(&setup()).get_place(&place_origin("r2")).unwrap().importance, None);
        }
    }

    both! {
        fn returns_the_same_answer_from_the_cache_and_the_network(kind: Kind) {
            let fake = setup();
            let network = kind.client(&fake).get_place(&place_origin("r1")).unwrap();
            let fresh = kind.client(&fake);
            fresh.ok(NEAR_FERRY, SearchOptions::new());
            let before = fake.request_count();
            assert_eq!(fresh.get_place(&place_origin("r1")).unwrap(), network);
            assert_eq!(fake.request_count(), before);
        }
    }

    both! {
        fn makes_no_request_for_the_origin_of_a_hit_from_an_earlier_search(kind: Kind) {
            let fake = setup();
            let client = kind.client(&fake);
            let result = client.ok(NEAR_FERRY, SearchOptions::new());
            let before = fake.request_count();
            client.get_place(result.places[0].origin.as_deref().unwrap()).unwrap();
            assert_eq!(fake.request_count(), before);
        }
    }

    both! {
        fn resolves_a_hit_identified_by_a_record_uri_from_the_cache(kind: Kind) {
            let uri_only = place("u", "By uri", vec![point(37.775, -122.415)], json!({ "origin": null }));
            let fake = FakeServer::with_tilesets(vec![overture(vec![tile(
                &url("a"),
                vec![record(uri_only.clone(), Some(4), Some("https://example.org/r/u"))],
            )])]);
            let client = kind.client(&fake);
            let result = client.ok(REGION, SearchOptions::new().dataset("overturemaps.org"));
            assert_eq!(result.places[0].origin.as_deref(), Some("https://example.org/r/u"));
            fake.go_offline();
            let found = client.get_place("https://example.org/r/u").unwrap();
            assert_eq!(found.origin, "https://example.org/r/u");
            assert_eq!(*found.place, map(&uri_only));
            assert_eq!(found.importance, Some(4));
        }
    }

    both! {
        fn rejects_unknown_origins_with_place_not_found(kind: Kind) {
            for unknown in [place_origin("nope"), "https://example.org/not-an-origin".to_owned()] {
                let e = kind.client(&setup()).get_place(&unknown).unwrap_err();
                assert_eq!(code_of(&e), "PlaceNotFound", "{unknown}");
            }
        }
    }
}

// describe("describeService and describeTileset")
mod describe {
    use super::*;

    fn osm() -> FakeTileset {
        FakeTileset {
            slug: Some("osm".into()),
            collections: vec![json!({
                "dataset": "openstreetmap.org", "collection": "way",
                "recordCount": 3, "categories": [{ "value": "cafe", "count": 2 }],
            })],
            references: Some(vec![json!({
                "dataset": "openstreetmap.org", "collection": "node",
                "licenses": ["https://opendatacommons.org/licenses/odbl/"],
            })]),
            tiles: vec![],
        }
    }

    fn fake() -> FakeServer {
        FakeServer::with_tilesets(vec![overture(vec![]), osm()])
    }

    both! {
        fn describe_service_returns_the_did_and_the_configured_tilesets(kind: Kind) {
            let service = kind.client(&fake()).describe_service().unwrap();
            assert_eq!(service.did.as_deref(), Some("did:web:places.atgeo.org"));
            let slugs: Vec<_> = service.tilesets.iter().map(|t| t.slug.as_deref().unwrap()).collect();
            assert_eq!(slugs, ["overture-place", "osm"]);
            let collections: Vec<Vec<(Option<&str>, Option<&str>)>> = service
                .tilesets
                .iter()
                .map(|t| t.collections.iter().map(|c| (c.dataset.as_deref(), c.collection.as_deref())).collect())
                .collect();
            assert_eq!(collections, [
                vec![(Some("overturemaps.org"), Some("place"))],
                vec![(Some("openstreetmap.org"), Some("way"))],
            ]);
        }
    }

    both! {
        fn describe_tileset_returns_the_descriptor(kind: Kind) {
            let t = kind.client(&fake()).describe_tileset("osm").unwrap();
            assert_eq!(t.slug.as_deref(), Some("osm"));
            assert_eq!(t.collections.len(), 1);
            let c = &t.collections[0];
            assert_eq!((c.dataset.as_deref(), c.collection.as_deref()), (Some("openstreetmap.org"), Some("way")));
            assert_eq!(c.record_count, Some(3));
            let categories = c.categories.as_ref().unwrap();
            assert_eq!((categories[0].value.as_deref(), categories[0].count), (Some("cafe"), Some(2)));
            let refs = t.references.as_ref().unwrap();
            assert_eq!(refs.len(), 1);
            assert_eq!(refs[0].dataset.as_deref(), Some("openstreetmap.org"));
            assert_eq!(refs[0].collection.as_deref(), Some("node"));
            assert_eq!(refs[0].licenses, Some(vec!["https://opendatacommons.org/licenses/odbl/".to_owned()]));
            assert_eq!(t.published_at, None);
            assert_eq!(t.extent, None);
        }
    }

    both! {
        fn describe_tileset_asks_the_server_each_time(kind: Kind) {
            let fake = fake();
            let client = kind.client(&fake);
            client.describe_tileset("osm").unwrap();
            client.describe_tileset("osm").unwrap();
            assert_eq!(fake.request_count(), 2);
        }
    }

    both! {
        fn describe_tileset_rejects_an_unknown_slug_with_tileset_not_found(kind: Kind) {
            let e = kind.client(&fake()).describe_tileset("nope").unwrap_err();
            assert_eq!(code_of(&e), "TilesetNotFound");
        }
    }
}

// describe("searchPlaces results")
mod search_places_results {
    use super::*;

    fn records() -> Vec<Value> {
        vec![
            record(
                place(
                    "cafe",
                    "Zanzibar Cafe",
                    vec![point(0.001, 0.0)],
                    json!({ "futureKey": { "a": 1 }, "variants": [{ "name": "ZC" }] }),
                ),
                Some(2),
                None,
            ),
            record(simple("shop", "Corner Shop", vec![point(0.002, 0.0)]), Some(9), None),
            record(simple("division", "Some Division", vec![bbox_location(0.01, -0.01, 0.01, -0.01)]), Some(1), None),
        ]
    }

    const CIRCLE: Region = Region::circle(0.0, 0.0, 2000.0);

    fn setup(kind: Kind) -> Client {
        kind.client(&FakeServer::with_tilesets(vec![overture(vec![tile(&url("a"), records())])]))
    }

    both! {
        fn returns_origin_the_wire_place_importance_meters_distance_and_null_score_without_text(kind: Kind) {
            let result = setup(kind).ok(CIRCLE, SearchOptions::new());
            let cafe = result.places.iter().find(|h| h.origin == Some(place_origin("cafe"))).unwrap();
            assert_eq!(*cafe.place, map(&records()[0]["value"]));
            assert!(!cafe.place.contains_key("importance"));
            assert_eq!(cafe.importance, Some(2));
            assert!((cafe.distance - 111.195).abs() < 0.05, "{}", cafe.distance);
            assert!(result.places.iter().all(|h| h.score.is_none()));
            assert_eq!(cafe.name(), Some("Zanzibar Cafe"));
            let location = cafe.location();
            assert_eq!(location.point.map(|p| (p.lat, p.lon)), Some((0.001, 0.0)));
            assert_eq!(location.bbox, None);
        }
    }

    both! {
        fn gives_distance_0_to_a_bbox_containing_the_center(kind: Kind) {
            let result = setup(kind).ok(CIRCLE, SearchOptions::new());
            let division = result.places.iter().find(|h| h.origin == Some(place_origin("division"))).unwrap();
            assert_eq!(division.distance, 0.0);
            let b = division.location().bbox.unwrap();
            assert_eq!((b.south, b.west, b.north, b.east), (-0.01, -0.01, 0.01, 0.01));
        }
    }

    both! {
        fn returns_a_numeric_score_with_text(kind: Kind) {
            let result = setup(kind).ok(CIRCLE, SearchOptions::new().text("Zanzibar Cafe"));
            let cafe = result.places.iter().find(|h| h.origin == Some(place_origin("cafe"))).unwrap();
            assert!((cafe.score.unwrap() - 1.0).abs() < 0.005);
            assert!(result.places.iter().all(|h| h.score.is_some()));
        }
    }

    both! {
        fn treats_text_that_normalizes_to_empty_as_no_text(kind: Kind) {
            let client = setup(kind);
            let none = client.ok(CIRCLE, SearchOptions::new());
            let blank = client.ok(CIRCLE, SearchOptions::new().text("   "));
            assert!(!blank.places.is_empty());
            assert_eq!(blank, none);
            assert!(blank.places.iter().all(|h| h.score.is_none()));
        }
    }
}

// describe("ordering among places containing the center")
mod ordering_among_places_containing_the_center {
    use super::*;

    fn ids(kind: Kind, values: Vec<(Value, i64)>, region: Region, options: SearchOptions) -> Vec<String> {
        let records = values.into_iter().map(|(v, i)| record(v, Some(i), None)).collect();
        let fake = FakeServer::with_tilesets(vec![overture(vec![tile(&url("a"), records)])]);
        let prefix = place_origin("");
        kind.client(&fake)
            .ok(region, options)
            .places
            .iter()
            .map(|h| h.origin.clone().unwrap().trim_start_matches(&prefix).to_owned())
            .collect()
    }

    both! {
        fn puts_the_smaller_box_first_when_the_center_sits_on_the_antimeridian_seam(kind: Kind) {
            // Center is exactly (45, 180). The small box reaches it as lon -180 / 180.
            let small = simple("small", "Small", vec![bbox_location(50.0, 40.0, -170.0, -180.0)]);
            let large = simple("large", "Large", vec![bbox_location(55.0, 35.0, -160.0, 160.0)]);
            let order = ids(kind, vec![(small, 1), (large, 9)], Region::bbox(170.0, 40.0, -170.0, 50.0), SearchOptions::new().sort(SortMode::Distance));
            assert_eq!(order, ["small", "large"]);
        }
    }

    both! {
        fn leaves_ties_to_the_origin_under_fused_and_similarity_sort_whatever_the_sizes(kind: Kind) {
            for sort in [SortMode::Fused, SortMode::Similarity] {
                let large = simple("a-large", "Same", vec![bbox_location(50.0, 40.0, 15.0, 5.0)]);
                let small = simple("b-small", "Same", vec![bbox_location(46.0, 44.0, 11.0, 9.0)]);
                let order = ids(kind, vec![(small, 5), (large, 5)], Region::circle(45.0, 10.0, 1000.0), SearchOptions::new().text("Same").sort(sort));
                assert_eq!(order, ["a-large", "b-small"], "{sort:?}");
            }
        }
    }

    both! {
        fn puts_a_point_at_the_center_before_a_more_important_containing_box(kind: Kind) {
            let order = ids(
                kind,
                vec![
                    (simple("box", "Box", vec![bbox_location(46.0, 44.0, 11.0, 9.0)]), 9),
                    (simple("pin", "Pin", vec![point(45.0, 10.0)]), 1),
                ],
                Region::circle(45.0, 10.0, 1000.0),
                SearchOptions::new().sort(SortMode::Distance),
            );
            assert_eq!(order, ["pin", "box"]);
        }
    }
}

// describe("datasets and collections")
mod datasets_and_collections {
    use super::*;

    fn osm_place(kind: &str, id: &str, lat: f64) -> Value {
        place(
            id,
            &format!("{kind} {id}"),
            vec![point(lat, -122.415)],
            json!({ "origin": format!("urn:atgeo:openstreetmap.org:{kind}:{id}") }),
        )
    }

    fn setup() -> FakeServer {
        let osm = FakeTileset {
            slug: Some("osm".into()),
            collections: ["node", "way", "relation"]
                .iter()
                .map(|c| json!({ "dataset": "openstreetmap.org", "collection": c }))
                .collect(),
            references: None,
            tiles: vec![tile(
                &url("osm"),
                vec![
                    rec(osm_place("node", "1", 37.771)),
                    rec(osm_place("way", "2", 37.772)),
                    rec(osm_place("relation", "3", 37.773)),
                    record(
                        place("u", "By uri", vec![point(37.774, -122.415)], json!({ "origin": null })),
                        None,
                        Some("https://example.org/r/u"),
                    ),
                ],
            )],
        };
        FakeServer::with_tilesets(vec![overture(vec![]), osm])
    }

    fn first(fake: &FakeServer) -> String {
        fake.requests()[0].clone()
    }

    both! {
        fn keys_the_tile_list_on_dataset_and_collection(kind: Kind) {
            let fake = setup();
            let client = kind.client(&fake);
            client.ok(REGION, SearchOptions::new().dataset("openstreetmap.org").collection("way"));
            client.ok(REGION, SearchOptions::new().dataset("openstreetmap.org"));
            client.ok(REGION, SearchOptions::new().dataset("overturemaps.org"));
            let keys: Vec<String> = fake
                .tile_list_requests()
                .iter()
                .map(|r| format!("{}|{}", param(r, "dataset").unwrap(), param(r, "collection").unwrap_or("null".into())))
                .collect();
            assert_eq!(keys, ["openstreetmap.org|way", "openstreetmap.org|null", "overturemaps.org|null"]);
        }
    }

    both! {
        fn matches_the_collection_not_a_collection_sharing_its_prefix(kind: Kind) {
            let fake = FakeServer::with_tilesets(vec![FakeTileset {
                slug: Some("osm".into()),
                collections: ["way", "wayside"].iter().map(|c| json!({ "dataset": "openstreetmap.org", "collection": c })).collect(),
                references: None,
                tiles: vec![tile(&url("osm"), vec![rec(osm_place("way", "2", 37.772)), rec(osm_place("wayside", "9", 37.773))])],
            }]);
            let result = kind.client(&fake).ok(REGION, SearchOptions::new().dataset("openstreetmap.org").collection("way"));
            assert_eq!(origins(&result), [Some("urn:atgeo:openstreetmap.org:way:2".to_owned())]);
        }
    }

    both! {
        fn searches_a_whole_dataset_when_only_the_dataset_is_named(kind: Kind) {
            let fake = setup();
            let result = kind.client(&fake).ok(REGION, SearchOptions::new().dataset("openstreetmap.org"));
            let mut got: Vec<String> = result.places.iter().map(|h| h.origin.clone().unwrap()).collect();
            got.sort();
            assert_eq!(got, [
                "https://example.org/r/u",
                "urn:atgeo:openstreetmap.org:node:1",
                "urn:atgeo:openstreetmap.org:relation:3",
                "urn:atgeo:openstreetmap.org:way:2",
            ]);
            assert_eq!(param(&first(&fake), "dataset").as_deref(), Some("openstreetmap.org"));
            assert_eq!(param(&first(&fake), "collection"), None);
        }
    }

    both! {
        fn filters_to_one_collection_dropping_records_with_no_origin(kind: Kind) {
            let fake = setup();
            let result = kind.client(&fake).ok(REGION, SearchOptions::new().dataset("openstreetmap.org").collection("way"));
            assert_eq!(origins(&result), [Some("urn:atgeo:openstreetmap.org:way:2".to_owned())]);
            assert_eq!(param(&first(&fake), "collection").as_deref(), Some("way"));
        }
    }

    both! {
        fn defaults_to_overturemaps_org_place_when_nothing_is_named(kind: Kind) {
            let fake = setup();
            kind.client(&fake).ok(REGION, SearchOptions::new());
            assert_eq!(param(&first(&fake), "dataset").as_deref(), Some("overturemaps.org"));
            assert_eq!(param(&first(&fake), "collection").as_deref(), Some("place"));
        }
    }

    both! {
        fn takes_overturemaps_org_for_a_collection_named_alone(kind: Kind) {
            let fake = setup();
            let _ = kind.client(&fake).search(REGION, SearchOptions::new().collection("division"));
            assert_eq!(param(&first(&fake), "dataset").as_deref(), Some("overturemaps.org"));
            assert_eq!(param(&first(&fake), "collection").as_deref(), Some("division"));
        }
    }
}

// The clients' own URL checks (spec §4, URLs).
mod urls {
    use super::*;

    both! {
        fn refuses_a_server_that_is_not_an_http_or_https_url_before_any_request(kind: Kind) {
            let fake = FakeServer::new();
            for server in ["places.atgeo.org", "ftp://places.atgeo.org", "file:///tmp", "https://", "", "/xrpc"] {
                let client = kind.with_server(&fake, server);
                let is_invalid = |r: Result<(), Error>| matches!(r, Err(Error::InvalidArgument(ArgumentError::InvalidServer)));
                assert!(is_invalid(client.search(REGION, SearchOptions::new()).map(drop)), "{server:?}");
                assert!(is_invalid(client.get_place("urn:x").map(drop)), "{server:?}");
                assert!(is_invalid(client.describe_service().map(drop)), "{server:?}");
                assert!(is_invalid(client.describe_tileset("x").map(drop)), "{server:?}");
            }
            assert_eq!(fake.request_count(), 0);
        }
    }

    both! {
        fn checks_the_region_before_the_server(kind: Kind) {
            let client = kind.with_server(&FakeServer::new(), "nope");
            let e = client.search(Region::circle(f64::NAN, 0.0, 1.0), SearchOptions::new()).unwrap_err();
            assert!(matches!(e, Error::InvalidArgument(ArgumentError::InvalidRegion(_))), "{e:?}");
        }
    }

    both! {
        fn counts_a_scheme_less_tile_url_as_a_failed_tile_without_a_request(kind: Kind) {
            let good = url("a");
            let fake = FakeServer::builder()
                .tilesets(vec![overture(vec![tile(&good, vec![rec(simple("a", "A", vec![point(37.772, -122.412)]))])])])
                .tiles_for({
                    let good = good.clone();
                    move |_| TilesFor::Urls(vec!["tiles.example/b.json".into(), "file:///etc/passwd".into(), good.clone()])
                })
                .build();
            let result = kind.client(&fake).ok(REGION, SearchOptions::new());
            assert_eq!((result.tiles.fetched, result.tiles.failed, result.complete), (1, 2, false));
            assert_eq!(origins(&result), [Some(place_origin("a"))]);
            assert!(fake.requests().iter().all(|r| r.starts_with("https://")), "{:?}", fake.requests());
        }
    }

    both! {
        fn prepares_a_tile_url_holding_a_space_and_a_tab(kind: Kind) {
            let fake = FakeServer::builder()
                .tiles_for(|_| TilesFor::Urls(vec!["https://tiles.example/a b\t.json".into()]))
                .build();
            let result = kind.client(&fake).ok(REGION, SearchOptions::new());
            assert!(fake.requests().contains(&"https://tiles.example/a%20b.json".to_owned()), "{:?}", fake.requests());
            assert_eq!(result.tiles.failed, 1); // the fake has no such tile: a 404
        }
    }

    both! {
        fn strips_trailing_slashes_from_the_server(kind: Kind) {
            let fake = FakeServer::new();
            kind.with_server(&fake, "https://places.atgeo.org///").describe_service().unwrap();
            assert_eq!(fake.requests(), [format!("{DEFAULT_SERVER}/xrpc/org.atgeo.describeService?")]);
        }
    }
}
