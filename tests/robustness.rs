//! Contract §16 and the tolerance rules of §1: what the client does with bodies a compliant server never
//! sends, served through the fake's raw-response override.

#[macro_use]
mod common;

use atgeo::{Error, SearchOptions, TileFailureKind};
use common::*;

const LIST: &str = "https://places.atgeo.org/xrpc/org.atgeo.getTiles?dataset=overturemaps.org&collection=place&bbox=-122.42%2C37.77%2C-122.41%2C37.78";
const TILE: &str = "https://tiles.example/t.json";

/// A fake whose getTiles names `TILE`, which answers with `body`.
fn tile_serving(body: &str) -> FakeServer {
    let fake = FakeServer::builder().tiles_for(|_| TilesFor::Urls(vec![TILE.into()])).build();
    fake.raw(TILE, 200, body);
    fake
}

fn geo(lat: &str, lon: &str) -> Value {
    json!({ "$type": "community.lexicon.location.geo", "latitude": lat, "longitude": lon })
}

fn here(id: &str) -> Value {
    json!({ "origin": place_origin(id), "name": id, "locations": [geo("37.775", "-122.415")] })
}

fn records_body(records: Value) -> String {
    json!({ "records": records }).to_string()
}

mod tile_lists {
    use super::*;

    both! {
        fn a_get_tiles_body_with_no_tiles_array_is_a_decoding_error_and_caches_nothing(kind: Kind) {
            for body in ["{}", "[]", r#"{"tiles":"x"}"#, "null", "not json", ""] {
                let fake = FakeServer::new();
                fake.raw(LIST, 200, body);
                let client = kind.client(&fake);
                let e = client.search(REGION, SearchOptions::new()).unwrap_err();
                assert!(matches!(e, Error::Decoding(_)), "{body:?}: {e:?}");
                let _ = client.search(REGION, SearchOptions::new());
                assert_eq!(fake.count_of(LIST), 2, "{body:?}: a failed list must not be cached");
            }
        }
    }

    both! {
        fn skips_tile_entries_that_are_not_strings(kind: Kind) {
            let fake = FakeServer::new();
            fake.raw(LIST, 200, json!({ "tiles": [1, null, {}, ["x"], TILE] }).to_string());
            fake.raw(TILE, 200, records_body(json!([{ "value": here("a") }])));
            let result = kind.client(&fake).ok(REGION, SearchOptions::new());
            assert_eq!((result.tiles.fetched, result.tiles.failed), (1, 0));
            assert_eq!(origins(&result), [Some(place_origin("a"))]);
        }
    }

    both! {
        fn reads_overview_by_javascript_truthiness(kind: Kind) {
            for (overview, expected) in [(json!(1), true), (json!("yes"), true), (json!([]), true), (json!(0), false), (json!(""), false), (json!(null), false)] {
                let fake = FakeServer::new();
                fake.raw(LIST, 200, json!({ "tiles": [], "overview": overview }).to_string());
                assert_eq!(kind.client(&fake).ok(REGION, SearchOptions::new()).overview, expected, "{overview}");
            }
        }
    }

    both! {
        fn decodes_a_body_with_a_byte_order_mark_and_invalid_utf8(kind: Kind) {
            let fake = FakeServer::new();
            let mut body = b"\xEF\xBB\xBF".to_vec();
            body.extend_from_slice(b"{\"tiles\":[],\"junk\":\"\xFF\"}");
            fake.raw(LIST, 200, body);
            assert!(kind.client(&fake).search(REGION, SearchOptions::new()).is_ok());
        }
    }
}

mod tiles {
    use super::*;

    both! {
        fn fails_a_tile_whose_body_is_unusable_and_carries_on(kind: Kind) {
            // Nesting past serde_json's recursion limit is a parse error, not a stack overflow (contract §4.4).
            let deep = format!(r#"{{"records":[{{"value":{}1{}}}]}}"#, "[".repeat(100_000), "]".repeat(100_000));
            for body in ["not json", "null", "[]", "{}", r#"{"records":{}}"#, r#"{"records":"x"}"#, "1e400", deep.as_str()] {
                let result = kind.client(&tile_serving(body)).ok(REGION, SearchOptions::new());
                assert_eq!((result.tiles.fetched, result.tiles.failed, result.complete), (0, 1, false), "{body:?}");
                let [failure] = &result.tile_failures[..] else { panic!("{:?}", result.tile_failures) };
                assert_eq!((failure.url.as_str(), failure.kind, failure.status), (TILE, TileFailureKind::Decode, None));
                match failure.error.as_deref() {
                    Some(Error::Decoding(e)) => {
                        let expected = match body {
                            "null" | "[]" => "tile is not an object",
                            "{}" | r#"{"records":{}}"# | r#"{"records":"x"}"# => "tile has no records array",
                            _ => "response is not JSON",
                        };
                        assert_eq!(e.message(), expected, "{body:?}");
                    }
                    other => panic!("{body:?}: {other:?}"),
                }
            }
        }
    }

    both! {
        fn a_tile_with_no_records_is_used_not_failed(kind: Kind) {
            for body in [r#"{"records":[]}"#, r#"{"records":[null, 1, {"value": 2}]}"#] {
                let result = kind.client(&tile_serving(body)).ok(REGION, SearchOptions::new());
                assert_eq!((result.tiles.fetched, result.tiles.failed, result.complete), (1, 0, true), "{body}");
                assert!(result.tile_failures.is_empty(), "{body}");
            }
        }
    }

    both! {
        fn skips_unusable_records_and_keeps_the_tile(kind: Kind) {
            let body = records_body(json!([
                null, 3, "x", [1], {}, { "value": null }, { "value": 7 }, { "value": [here("arr")] },
                { "value": here("a") },
            ]));
            let result = kind.client(&tile_serving(&body)).ok(REGION, SearchOptions::new());
            assert_eq!((result.tiles.fetched, result.tiles.failed), (1, 0));
            assert_eq!(origins(&result), [Some(place_origin("a"))]);
        }
    }

    both! {
        fn a_scalar_value_does_not_consume_its_uri_as_a_dedupe_key(kind: Kind) {
            let body = records_body(json!([
                { "value": "scalar", "uri": "at://x/1" },
                { "value": { "name": "kept", "locations": [geo("37.775", "-122.415")] }, "uri": "at://x/1" },
            ]));
            let result = kind.client(&tile_serving(&body)).ok(REGION, SearchOptions::new().dataset("overturemaps.org"));
            assert_eq!(origins(&result), [Some("at://x/1".to_owned())]);
        }
    }

    both! {
        fn reads_a_wrong_typed_uri_and_importance_as_absent(kind: Kind) {
            let mut no_origin = here("x");
            no_origin.as_object_mut().unwrap().remove("origin");
            let body = records_body(json!([
                { "value": no_origin, "uri": 42, "importance": "7" },
                { "value": here("b"), "importance": [3] },
                { "value": here("c"), "importance": 9223372036854775807i64 },
                { "value": here("d"), "importance": 2.9 },
            ]));
            let result = kind.client(&tile_serving(&body)).ok(REGION, SearchOptions::new().dataset("overturemaps.org"));
            let by_name = |n: &str| result.places.iter().find(|h| h.name() == Some(n)).unwrap();
            assert_eq!((by_name("x").origin.clone(), by_name("x").importance), (None, None));
            assert_eq!(by_name("b").importance, None);
            assert_eq!(by_name("c").importance, None);
            assert_eq!(by_name("d").importance, Some(2));
        }
    }

    both! {
        fn skips_null_and_non_object_locations_and_variants(kind: Kind) {
            let value = json!({
                "origin": place_origin("a"), "name": "Nope",
                "locations": [null, 5, "x", geo("37.775", "-122.415")],
                "variants": [null, 3, "Alpha", { "name": 4 }, { "name": "Alpha" }],
            });
            let result = kind.client(&tile_serving(&records_body(json!([{ "value": value }])))).ok(REGION, SearchOptions::new().text("Alpha"));
            assert_eq!(origins(&result), [Some(place_origin("a"))]);
            assert_eq!(result.places[0].score, Some(1.0));
        }
    }

    both! {
        fn reads_no_location_from_a_coordinate_in_an_array(kind: Kind) {
            let value = json!({ "origin": place_origin("a"), "name": "A", "locations": [{ "$type": "community.lexicon.location.geo", "latitude": ["37.775"], "longitude": "-122.415" }] });
            let result = kind.client(&tile_serving(&records_body(json!([{ "value": value }])))).ok(REGION, SearchOptions::new());
            assert!(result.places.is_empty());
            assert_eq!(result.tiles.fetched, 1);
        }
    }

    both! {
        fn reads_coordinates_given_as_json_numbers(kind: Kind) {
            let value = json!({ "origin": place_origin("a"), "name": "A", "locations": [{ "$type": "community.lexicon.location.geo", "latitude": 37.775, "longitude": -122.415 }] });
            let result = kind.client(&tile_serving(&records_body(json!([{ "value": value }])))).ok(REGION, SearchOptions::new());
            assert_eq!(origins(&result), [Some(place_origin("a"))]);
        }
    }

    both! {
        fn keeps_a_tile_holding_a_lone_surrogate(kind: Kind) {
            let body = r#"{"records":[{"value":{"origin":"urn:atgeo:overturemaps.org:place:a","name":"A\ud800","locations":[{"$type":"community.lexicon.location.geo","latitude":"37.775","longitude":"-122.415"}]}}]}"#;
            let result = kind.client(&tile_serving(body)).ok(REGION, SearchOptions::new());
            assert_eq!(result.tiles.fetched, 1);
            assert_eq!(result.places[0].name(), Some("A\u{FFFD}"));
        }
    }

    both! {
        fn surfaces_unknown_and_nested_fields_untouched(kind: Kind) {
            let mut value = here("a");
            value["future"] = json!({ "deep": [1, 2.5, null, true, "x", { "k": [] }], "big": 18446744073709551615u64 });
            let result = kind.client(&tile_serving(&records_body(json!([{ "value": value.clone() }])))).ok(REGION, SearchOptions::new());
            assert_eq!(*result.places[0].place, map(&value));
        }
    }
}

mod errors_and_other_calls {
    use super::*;

    const PLACE: &str = "https://places.atgeo.org/xrpc/org.atgeo.getPlace?uri=urn%3Ax";
    const SERVICE: &str = "https://places.atgeo.org/xrpc/org.atgeo.describeService?";
    const TILESET: &str = "https://places.atgeo.org/xrpc/org.atgeo.describeTileset?slug=s";

    both! {
        fn reads_only_string_error_and_message_fields(kind: Kind) {
            for (body, code, message) in [
                (r#"{"error":3,"message":["m"]}"#, "HttpError", "HTTP 418"),
                (r#"{"error":"Teapot"}"#, "Teapot", "HTTP 418"),
                (r#"{"message":"short and stout"}"#, "HttpError", "short and stout"),
                ("null", "HttpError", "HTTP 418"),
                (r#"["Teapot"]"#, "HttpError", "HTTP 418"),
                ("<html>", "HttpError", "HTTP 418"),
            ] {
                let fake = FakeServer::new();
                fake.raw(PLACE, 418, body);
                match kind.client(&fake).get_place("urn:x").unwrap_err() {
                    Error::Atgeo(e) => assert_eq!((e.code.as_str(), e.message.as_str(), e.status), (code, message, 418), "{body}"),
                    other => panic!("{body}: {other:?}"),
                }
            }
        }
    }

    both! {
        fn a_get_place_body_without_a_record_object_is_a_decoding_error(kind: Kind) {
            for body in ["null", "{}", r#"{"uri":"u","record":"x"}"#, r#"{"record":null}"#, "[]", "nope"] {
                let fake = FakeServer::new();
                fake.raw(PLACE, 200, body);
                let e = kind.client(&fake).get_place("urn:x").unwrap_err();
                assert!(matches!(e, Error::Decoding(_)), "{body}: {e:?}");
            }
            let fake = FakeServer::new();
            fake.raw(PLACE, 200, r#"{"record":{"name":"n"},"importance":"9"}"#);
            let found = kind.client(&fake).get_place("urn:x").unwrap();
            assert_eq!((found.name(), found.importance, found.origin.as_str()), (Some("n"), None, "urn:x"));
        }
    }

    both! {
        fn a_describe_body_that_is_not_an_object_is_a_decoding_error_and_is_not_cached(kind: Kind) {
            for body in ["null", "false", "0", "\"\"", "[]"] {
                let fake = FakeServer::new();
                fake.raw(SERVICE, 200, body);
                fake.raw(TILESET, 200, body);
                let client = kind.client(&fake);
                assert!(matches!(client.describe_service(), Err(Error::Decoding(_))), "{body}");
                assert!(matches!(client.describe_service(), Err(Error::Decoding(_))), "{body}");
                assert_eq!(fake.count_of(SERVICE), 2, "{body}");
                assert!(matches!(client.describe_tileset("s"), Err(Error::Decoding(_))), "{body}");
            }
        }
    }

    both! {
        fn reads_describe_bodies_leniently(kind: Kind) {
            let fake = FakeServer::new();
            fake.raw(SERVICE, 200, r#"{"did":1,"tilesets":[{"slug":"a","collections":"x"},"junk",{"collections":[{"dataset":"d"},null]}],"new":true}"#);
            let s = kind.client(&fake).describe_service().unwrap();
            assert_eq!(s.did, None);
            assert_eq!(s.tilesets.len(), 2);
            assert_eq!(s.tilesets[0].slug.as_deref(), Some("a"));
            assert!(s.tilesets[0].collections.is_empty());
            assert_eq!(s.tilesets[1].collections.len(), 1);
            assert_eq!(s.tilesets[1].collections[0].dataset.as_deref(), Some("d"));
        }
    }
}
