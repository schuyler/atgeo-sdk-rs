//! A port of the reference's `test/keyword.test.ts`: `search_by_keyword`'s request, hits, attribution and
//! argument checks (contract §16), and §12 item 6, on both clients.

#[macro_use]
mod common;

use atgeo::{ArgumentError, Error, KeywordOptions, KeywordRegion};
use common::*;

const DIVISION: &str = "urn:atgeo:overturemaps.org:division:d1";

fn starbucks_1() -> Value {
    place("starbucks-1", "Starbucks", vec![point(37.775, -122.418)], json!({ "futureKey": { "a": 1 } }))
}

fn tilesets() -> Vec<FakeTileset> {
    vec![
        overture(vec![tile(
            "https://tiles.example/p.json",
            vec![
                record(simple("starbucks-3", "Starbucks", vec![point(37.7, -122.4)]), Some(5), None),
                record(starbucks_1(), Some(5), None),
                record(simple("starbucks-2", "Starbucks Reserve", vec![point(37.8, -122.3)]), Some(9), None),
                record(simple("mcd", "McDonald's", vec![point(37.78, -122.41)]), Some(7), None),
            ],
        )]),
        FakeTileset {
            slug: Some("overture-division".into()),
            collections: vec![json!({ "dataset": "overturemaps.org", "collection": "division" })],
            references: None,
            overview: None,
            tiles: vec![tile(
                "https://tiles.example/d.json",
                vec![record(
                    place("d1", "Starbucks District", vec![point(37.9, -122.5)], json!({ "origin": DIVISION })),
                    Some(1),
                    None,
                )],
            )],
        },
    ]
}

fn setup() -> FakeServer {
    FakeServer::with_tilesets(tilesets())
}

/// A request's decoded parameters, sorted by name.
fn sorted_params(url: &str) -> Vec<(String, String)> {
    let mut p = params(url);
    p.sort();
    p
}

fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = list.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    v.sort();
    v
}

// describe("searchByKeyword")
mod search_by_keyword {
    use super::*;

    both! {
        fn returns_the_servers_matches_in_the_servers_order_as_keyword_hits(kind: Kind) {
            let result = kind.client(&setup()).keyword("starb", KeywordOptions::new("overturemaps.org").collection("place")).unwrap();
            let origins: Vec<_> = result.places.iter().map(|h| h.origin.clone().unwrap()).collect();
            assert_eq!(origins, [place_origin("starbucks-2"), place_origin("starbucks-1"), place_origin("starbucks-3")]);
            let hit = &result.places[1];
            assert_eq!(hit.via(), "keyword");
            assert_eq!(hit.name(), Some("Starbucks"));
            assert_eq!(hit.importance, Some(5));
            assert_eq!(collection_json(&hit.collection), json!({ "dataset": "overturemaps.org", "collection": "place" }));
            let location = hit.location();
            assert_eq!(location.point.map(|p| (p.lat, p.lon)), Some((37.775, -122.418)));
            assert!(location.bbox.is_none());
            // The value is surfaced as served, unknown keys included.
            assert_eq!(Value::Object((*hit.place).clone()), starbucks_1());
        }
    }

    both! {
        fn credits_each_hits_own_collection_and_the_union_of_the_places_attribution(kind: Kind) {
            let result = kind.client(&setup()).keyword("starb", KeywordOptions::new("overturemaps.org")).unwrap();
            let collections: Vec<_> =
                result.places.iter().map(|h| h.collection.as_ref().map(|c| c.collection.clone())).collect();
            let place_or_division = |c: &str| Some(c.to_owned());
            assert_eq!(
                collections,
                [place_or_division("place"), place_or_division("place"), place_or_division("place"), place_or_division("division")]
            );
            assert_eq!(
                attribution_json(&result.attribution),
                json!([
                    { "dataset": "overturemaps.org", "collection": "division" },
                    { "dataset": "overturemaps.org", "collection": "place" },
                ])
            );
        }
    }

    both! {
        fn sends_exactly_dataset_and_q_when_given_no_collection_or_region(kind: Kind) {
            let fake = setup();
            kind.client(&fake).keyword("Starb", KeywordOptions::new("overturemaps.org")).unwrap();
            let requests = fake.requests();
            assert_eq!(requests.len(), 1);
            assert!(requests[0].contains("/xrpc/org.atgeo.searchPlaces?"), "{}", requests[0]);
            assert_eq!(sorted_params(&requests[0]), pairs(&[("dataset", "overturemaps.org"), ("q", "Starb")]));
        }
    }

    both! {
        fn sends_the_collection_and_the_snapped_region_when_given(kind: Kind) {
            let fake = setup();
            let options = KeywordOptions::new("overturemaps.org")
                .collection("place")
                .region(KeywordRegion::new(-122.43, 37.77, -122.41, 37.78));
            kind.client(&fake).keyword("Starb", options).unwrap();
            let requests = fake.requests();
            assert_eq!(requests.len(), 1);
            assert_eq!(
                sorted_params(&requests[0]),
                pairs(&[("dataset", "overturemaps.org"), ("collection", "place"), ("q", "Starb"), ("bbox", "-123,37,-122,38")])
            );
        }
    }

    both! {
        fn sends_the_keyword_exactly_as_passed_encoded_for_the_wire(kind: Kind) {
            let fake = setup();
            let keyword = "  Zanzibár & +quokka=#%  ";
            kind.client(&fake).keyword(keyword, KeywordOptions::new("overturemaps.org")).ok();
            let requests = fake.requests();
            assert_eq!(param(&requests[0], "q").as_deref(), Some(keyword));
            assert!(requests[0].contains("%2B") && !requests[0].contains('+'), "{}", requests[0]);
        }
    }

    both! {
        fn sends_a_keyword_the_server_cannot_use_and_fails_with_the_servers_keyword_required(kind: Kind) {
            let fake = setup();
            let err = kind.client(&fake).keyword("a", KeywordOptions::new("overturemaps.org")).unwrap_err();
            assert!(matches!(err, Error::Atgeo(_)));
            assert_eq!(code_of(&err), "KeywordRequired");
            assert_eq!(fake.request_count(), 1);
        }
    }

    both! {
        fn reports_the_keyword_and_the_region_it_sent(kind: Kind) {
            let client = kind.client(&setup());
            let without = client.keyword("Starb", KeywordOptions::new("overturemaps.org")).unwrap();
            assert_eq!((without.disclosed.keyword, without.disclosed.region), (true, None));
            let with = client
                .keyword("Starb", KeywordOptions::new("overturemaps.org").region(KeywordRegion::new(-123.0, 37.0, -122.0, 38.0)))
                .unwrap();
            assert_eq!((with.disclosed.keyword, with.disclosed.region), (true, Some(1.0)));
        }
    }

    both! {
        fn is_not_cached_and_nothing_from_the_answer_enters_a_cache(kind: Kind) {
            let fake = setup();
            let client = kind.client(&fake);
            let options = || KeywordOptions::new("overturemaps.org").collection("place");
            let first = client.keyword("Starb", options()).unwrap();
            let second = client.keyword("Starb", options()).unwrap();
            assert_eq!(first, second);
            assert_eq!(fake.request_count(), 2, "each call is one request");
            // get_place on a keyword hit's origin finds nothing cached: it asks the server.
            let origin = first.places[0].origin.clone().unwrap();
            client.get_place(&origin).unwrap();
            assert_eq!(fake.request_count(), 3);
            assert!(fake.requests()[2].contains("org.atgeo.getPlace"));
            assert!(fake.tile_list_requests().is_empty());
        }
    }

    both! {
        fn fails_on_a_server_error_and_on_a_body_without_places(kind: Kind) {
            let fake = setup();
            fake.raw_method("org.atgeo.searchPlaces", 500, r#"{"error":"Boom","message":"x"}"#);
            let err = kind.client(&fake).keyword("Starb", KeywordOptions::new("overturemaps.org")).unwrap_err();
            let Error::Atgeo(e) = err else { panic!("{err:?}") };
            assert_eq!((e.code.as_str(), e.status), ("Boom", 500));

            let fake = setup();
            fake.raw_method("org.atgeo.searchPlaces", 200, r#"{"places":{}}"#);
            let err = kind.client(&fake).keyword("Starb", KeywordOptions::new("overturemaps.org")).unwrap_err();
            assert!(matches!(err, Error::Decoding(_)), "{err:?}");
        }
    }

    both! {
        fn reads_the_answer_leniently_skipping_elements_it_cannot_use(kind: Kind) {
            let fake = setup();
            let body = json!({ "places": [
                { "value": { "name": "A", "origin": "o:a" }, "importance": 3.9, "attribution": "no" },
                null, 5, { "uri": "u" }, { "value": 4 },
                { "value": { "name": "B" }, "uri": "u:b" },
                { "value": { "name": "C" } },
            ] });
            fake.raw_method("org.atgeo.searchPlaces", 200, body.to_string());
            let result = kind.client(&fake).keyword("Starb", KeywordOptions::new("overturemaps.org")).unwrap();
            let got: Vec<_> = result.places.iter().map(|h| (h.origin.as_deref(), h.importance)).collect();
            assert_eq!(got, [(Some("o:a"), Some(3)), (Some("u:b"), None), (None, None)]);
            assert!(result.attribution.is_empty());
        }
    }
}

// describe("searchByKeyword arguments")
mod arguments {
    use super::*;

    both! {
        fn refuses_a_bbox_that_fails_the_bbox_validation_with_no_request(kind: Kind) {
            let fake = setup();
            let client = kind.client(&fake);
            for region in [
                KeywordRegion::new(0.0, 10.0, 1.0, 5.0),
                KeywordRegion::new(f64::NAN, 0.0, 1.0, 1.0),
                KeywordRegion::new(-181.0, 0.0, 1.0, 1.0),
                KeywordRegion::new(0.0, -91.0, 1.0, 1.0),
                KeywordRegion::new(0.0, 0.0, f64::INFINITY, 1.0),
            ] {
                let err = client.keyword("ab", KeywordOptions::new("overturemaps.org").region(region)).unwrap_err();
                assert!(matches!(err, Error::InvalidArgument(ArgumentError::InvalidRegion("invalid bbox"))), "{region:?}: {err:?}");
            }
            assert_eq!(fake.request_count(), 0);
        }
    }

    both! {
        fn accepts_a_box_that_crosses_the_antimeridian_or_has_no_width(kind: Kind) {
            let fake = setup();
            let client = kind.client(&fake);
            for region in [KeywordRegion::new(170.0, 10.0, -170.0, 20.0), KeywordRegion::new(5.0, 5.0, 5.0, 5.0)] {
                client.keyword("ab", KeywordOptions::new("overturemaps.org").region(region)).unwrap();
            }
            assert_eq!(fake.request_count(), 2);
        }
    }

    both! {
        fn refuses_a_server_that_is_not_an_http_url_before_any_request(kind: Kind) {
            let fake = setup();
            let client = kind.with_server(&fake, "not a url");
            let err = client.keyword("ab", KeywordOptions::new("overturemaps.org")).unwrap_err();
            assert!(matches!(err, Error::InvalidArgument(ArgumentError::InvalidServer)));
            assert_eq!(fake.request_count(), 0);
        }
    }
}
