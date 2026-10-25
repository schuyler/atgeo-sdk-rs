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
                let e = client.search(REGION, overture_options()).unwrap_err();
                assert!(matches!(e, Error::Decoding(_)), "{body:?}: {e:?}");
                let _ = client.search(REGION, overture_options());
                assert_eq!(fake.count_of(LIST), 2, "{body:?}: a failed list must not be cached");
            }
        }
    }

    both! {
        fn skips_tile_entries_that_are_not_strings(kind: Kind) {
            let fake = FakeServer::new();
            fake.raw(LIST, 200, json!({ "tiles": [1, null, {}, ["x"], TILE] }).to_string());
            fake.raw(TILE, 200, records_body(json!([{ "value": here("a") }])));
            let result = kind.client(&fake).ok(REGION, overture_options());
            assert_eq!((result.tiles.fetched, result.tiles.failed), (1, 0));
            assert_eq!(origins(&result), [Some(place_origin("a"))]);
        }
    }

    both! {
        fn reads_overview_by_javascript_truthiness(kind: Kind) {
            for (overview, expected) in [(json!(1), true), (json!("yes"), true), (json!([]), true), (json!(0), false), (json!(""), false), (json!(null), false)] {
                let fake = FakeServer::new();
                fake.raw(LIST, 200, json!({ "tiles": [], "overview": overview }).to_string());
                assert_eq!(kind.client(&fake).ok(REGION, overture_options()).overview, expected, "{overview}");
            }
        }
    }

    both! {
        fn decodes_a_body_with_a_byte_order_mark_and_invalid_utf8(kind: Kind) {
            let fake = FakeServer::new();
            let mut body = b"\xEF\xBB\xBF".to_vec();
            body.extend_from_slice(b"{\"tiles\":[],\"junk\":\"\xFF\"}");
            fake.raw(LIST, 200, body);
            assert!(kind.client(&fake).search(REGION, overture_options()).is_ok());
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
                let result = kind.client(&tile_serving(body)).ok(REGION, overture_options());
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
                let result = kind.client(&tile_serving(body)).ok(REGION, overture_options());
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
            let result = kind.client(&tile_serving(&body)).ok(REGION, overture_options());
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
            let result = kind.client(&tile_serving(&body)).ok(REGION, SearchOptions::new("overturemaps.org"));
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
            let result = kind.client(&tile_serving(&body)).ok(REGION, SearchOptions::new("overturemaps.org"));
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
            let result = kind.client(&tile_serving(&records_body(json!([{ "value": value }])))).ok(REGION, overture_options().text("Alpha"));
            assert_eq!(origins(&result), [Some(place_origin("a"))]);
            assert_eq!(result.places[0].score, Some(1.0));
        }
    }

    both! {
        fn reads_no_location_from_a_coordinate_in_an_array(kind: Kind) {
            let value = json!({ "origin": place_origin("a"), "name": "A", "locations": [{ "$type": "community.lexicon.location.geo", "latitude": ["37.775"], "longitude": "-122.415" }] });
            let result = kind.client(&tile_serving(&records_body(json!([{ "value": value }])))).ok(REGION, overture_options());
            assert!(result.places.is_empty());
            assert_eq!(result.tiles.fetched, 1);
        }
    }

    both! {
        fn reads_coordinates_given_as_json_numbers(kind: Kind) {
            let value = json!({ "origin": place_origin("a"), "name": "A", "locations": [{ "$type": "community.lexicon.location.geo", "latitude": 37.775, "longitude": -122.415 }] });
            let result = kind.client(&tile_serving(&records_body(json!([{ "value": value }])))).ok(REGION, overture_options());
            assert_eq!(origins(&result), [Some(place_origin("a"))]);
        }
    }

    both! {
        fn keeps_a_tile_holding_a_lone_surrogate(kind: Kind) {
            let body = r#"{"records":[{"value":{"origin":"urn:atgeo:overturemaps.org:place:a","name":"A\ud800","locations":[{"$type":"community.lexicon.location.geo","latitude":"37.775","longitude":"-122.415"}]}}]}"#;
            let result = kind.client(&tile_serving(body)).ok(REGION, overture_options());
            assert_eq!(result.tiles.fetched, 1);
            assert_eq!(result.places[0].name(), Some("A\u{FFFD}"));
        }
    }

    both! {
        fn surfaces_unknown_and_nested_fields_untouched(kind: Kind) {
            let mut value = here("a");
            value["future"] = json!({ "deep": [1, 2.5, null, true, "x", { "k": [] }], "big": 18446744073709551615u64 });
            let result = kind.client(&tile_serving(&records_body(json!([{ "value": value.clone() }])))).ok(REGION, overture_options());
            assert_eq!(*result.places[0].place, map(&value));
        }
    }
}

// describe("tile attribution")
mod tile_attribution {
    use super::*;

    fn search(kind: Kind, tile_body: Value) -> atgeo::SearchResult {
        kind.client(&tile_serving(&tile_body.to_string())).ok(REGION, overture_options())
    }

    fn good() -> Value {
        json!({ "value": here("good"), "importance": 4 })
    }

    both! {
        fn gives_a_search_whose_tiles_lack_attribution_an_empty_attribution_and_none_hit_collections(kind: Kind) {
            let result = search(kind, json!({ "records": [good()] }));
            assert_eq!(result.attribution, vec![]);
            assert_eq!(result.places.len(), 1);
            assert!(result.places.iter().all(|h| h.collection.is_none()));
        }
    }

    both! {
        fn does_not_fail_the_tile_for_an_attribution_that_is_not_an_array(kind: Kind) {
            for attribution in [json!("x"), json!({}), Value::Null, json!(5)] {
                let result = search(kind, json!({ "records": [good()], "attribution": attribution }));
                assert_eq!((result.complete, result.tiles.fetched, result.tiles.failed), (true, 1, 0), "{attribution}");
                assert!(result.tile_failures.is_empty(), "{attribution}");
                assert_eq!(result.attribution, vec![], "{attribution}");
                assert_eq!(origins(&result), [Some(place_origin("good"))], "{attribution}");
            }
        }
    }

    both! {
        fn drops_the_entries_that_lack_a_string_dataset_and_collection_and_keeps_the_tile(kind: Kind) {
            let result = search(kind, json!({
                "records": [good()],
                "attribution": [null, 5, { "dataset": "d" }, { "collection": "c" }, { "dataset": 1, "collection": "c" },
                                { "dataset": "overturemaps.org", "collection": "place" }],
            }));
            assert!(result.complete);
            assert_eq!(attribution_json(&result.attribution), overture_attribution());
            assert_eq!(collection_json(&result.places[0].collection), json!({ "dataset": "overturemaps.org", "collection": "place" }));
        }
    }

    both! {
        fn reads_each_field_of_an_entry_on_its_own(kind: Kind) {
            let result = search(kind, json!({
                "records": [good()],
                "attribution": [{
                    "dataset": "d", "collection": "c", "release": 5, "source": "s", "attribution": ["x"],
                    "licenses": ["l1", 2, null, "l2"], "unknown": true,
                }, { "dataset": "e", "collection": "c", "licenses": "l" }],
            }));
            assert_eq!(attribution_json(&result.attribution), json!([
                { "dataset": "d", "collection": "c", "source": "s", "licenses": ["l1", "l2"] },
                { "dataset": "e", "collection": "c" },
            ]));
        }
    }

    both! {
        fn does_not_let_a_non_2xx_tile_contribute_the_attribution_its_body_carries(kind: Kind) {
            let fake = tile_serving("");
            let body = json!({ "records": [good()], "attribution": [{ "dataset": "d", "collection": "c" }] });
            fake.raw(TILE, 503, body.to_string());
            let result = kind.client(&fake).ok(REGION, overture_options());
            assert_eq!((result.tiles.fetched, result.tiles.failed), (0, 1));
            assert_eq!(result.attribution, vec![]);
        }
    }

    both! {
        fn does_not_let_a_tile_whose_body_fails_to_decode_contribute_its_attribution(kind: Kind) {
            // A 200 whose body has no records array is a failed tile, though its attribution is readable.
            let result = search(kind, json!({ "attribution": [{ "dataset": "d", "collection": "c" }] }));
            assert_eq!((result.tiles.fetched, result.tiles.failed), (0, 1));
            assert_eq!(result.attribution, vec![]);
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
        fn a_get_place_body_without_a_value_object_is_a_decoding_error(kind: Kind) {
            for body in [
                "null", "{}", r#"{"uri":"u","value":"x"}"#, r#"{"value":null}"#, r#"{"value":[]}"#, "[]", "nope",
                // The earlier shape, with `record` and no `value`, has no usable body (contract §1).
                r#"{"uri":"x","record":{"name":"X"}}"#,
            ] {
                let fake = FakeServer::new();
                fake.raw(PLACE, 200, body);
                let e = kind.client(&fake).get_place("urn:x").unwrap_err();
                assert!(matches!(e, Error::Decoding(_)), "{body}: {e:?}");
            }
            let fake = FakeServer::new();
            fake.raw(PLACE, 200, r#"{"uri":5,"value":{"name":"n"},"importance":"9"}"#);
            let found = kind.client(&fake).get_place("urn:x").unwrap();
            assert_eq!((found.name(), found.importance, found.origin.as_str()), (Some("n"), None, "urn:x"));
            assert_eq!(found.attribution, vec![]);
        }
    }

    both! {
        fn a_get_place_body_in_the_new_shape_succeeds_and_returns_its_attribution_as_served(kind: Kind) {
            let attribution = json!([
                { "dataset": "d", "collection": "c", "licenses": ["l"] },
                { "dataset": "d", "collection": "c", "release": "again" },
                7, { "dataset": "only" },
            ]);
            let fake = FakeServer::new();
            fake.raw(
                PLACE,
                200,
                json!({ "uri": "urn:x", "url": "https://x.example/p", "importance": 3, "value": { "name": "X" }, "attribution": attribution })
                    .to_string(),
            );
            let found = kind.client(&fake).get_place("urn:x").unwrap();
            // Served order, duplicates kept; the entries that name no collection are dropped (contract §1).
            assert_eq!(
                attribution_json(&found.attribution),
                json!([{ "dataset": "d", "collection": "c", "licenses": ["l"] }, { "dataset": "d", "collection": "c", "release": "again" }])
            );
            assert_eq!((found.name(), found.importance), (Some("X"), Some(3)));
        }
    }

    both! {
        fn a_get_place_attribution_that_is_not_an_array_is_empty(kind: Kind) {
            for attribution in [json!({}), Value::Null, json!("x"), json!(5)] {
                let fake = FakeServer::new();
                fake.raw(PLACE, 200, json!({ "value": { "name": "X" }, "attribution": attribution }).to_string());
                let found = kind.client(&fake).get_place("urn:x").unwrap();
                assert_eq!(found.attribution, vec![], "{attribution}");
            }
        }
    }

    both! {
        fn reads_max_tiles_and_an_overview_leniently(kind: Kind) {
            let fake = FakeServer::new();
            fake.raw(SERVICE, 200, r#"{"maxTiles":50.9,"tilesets":[]}"#);
            fake.raw(TILESET, 200, r#"{"overview":{"records":"7","other":1}}"#);
            let client = kind.client(&fake);
            assert_eq!(client.describe_service().unwrap().max_tiles, Some(50));
            assert_eq!(client.describe_tileset("s").unwrap().overview.unwrap().records, None);

            for (body, max_tiles) in [(r#"{"maxTiles":"50"}"#, None), (r#"{"maxTiles":null}"#, None), (r#"{}"#, None)] {
                let fake = FakeServer::new();
                fake.raw(SERVICE, 200, body);
                assert_eq!(kind.client(&fake).describe_service().unwrap().max_tiles, max_tiles, "{body}");
            }
            for body in [r#"{"overview":"x"}"#, r#"{"overview":[]}"#, r#"{"overview":null}"#, "{}"] {
                let fake = FakeServer::new();
                fake.raw(TILESET, 200, body);
                assert_eq!(kind.client(&fake).describe_tileset("s").unwrap().overview, None, "{body}");
            }
            let fake = FakeServer::new();
            fake.raw(TILESET, 200, r#"{"overview":{"records":1234}}"#);
            assert_eq!(kind.client(&fake).describe_tileset("s").unwrap().overview.unwrap().records, Some(1234));
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
