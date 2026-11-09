//! A port of the reference's `test/keywordFallback.test.ts`: `search_nearby_then_by_keyword` (contract §18): the
//! solid-hit decision and the merge (`fixtures/keyword-fallback.json`), argument order, the server's refusal of
//! a keyword the qualifying check let through, the pins of #60, and §12 item 7.

#[macro_use]
mod common;

use std::time::Duration;

use atgeo::{ArgumentError, Error, FallbackHit, FallbackOptions, KeywordRegionMode, NearbyOptions, Region, SortMode};
use common::*;

fn scenarios() -> Vec<Value> {
    serde_json::from_str(include_str!("fixtures/keyword-fallback.json")).unwrap()
}

/// A raw server: getTiles (any query) lists `tiles`, each tile is served verbatim and searchPlaces (any query)
/// answers `answer`. The bodies need not validate against the lexicons.
fn raw_server(dataset: &str, collection: Option<&str>, tiles: &[(String, Value)], answer: &Value) -> FakeServer {
    let mut tileset = overture(vec![]);
    tileset.collections = vec![json!({ "dataset": dataset, "collection": collection.unwrap_or("c") })];
    let listed: Vec<String> = tiles.iter().map(|(u, _)| u.clone()).collect();
    let fake = FakeServer::builder().tilesets(vec![tileset]).tiles_for(move |_| TilesFor::Urls(listed.clone())).build();
    for (url, body) in tiles {
        fake.raw(url, 200, serde_json::to_vec(body).unwrap());
    }
    fake.raw_method("org.atgeo.searchPlaces", 200, serde_json::to_vec(answer).unwrap());
    fake
}

fn keyword_requests(fake: &FakeServer) -> Vec<String> {
    fake.requests().into_iter().filter(|r| r.contains("org.atgeo.searchPlaces")).collect()
}

fn mode(s: &str) -> KeywordRegionMode {
    match s {
        "none" => KeywordRegionMode::None,
        "degree" => KeywordRegionMode::Degree,
        other => panic!("unknown keywordRegion {other}"),
    }
}

fn sort(s: &str) -> SortMode {
    match s {
        "fused" => SortMode::Fused,
        "distance" => SortMode::Distance,
        "similarity" => SortMode::Similarity,
        other => panic!("unknown sort {other}"),
    }
}

fn origin_of(h: &FallbackHit) -> Option<String> {
    h.origin().map(str::to_owned)
}

// describe("keyword fallback scenarios"): a fresh client per scenario, since scenarios reuse tile URLs and the
// caches are keyed by dataset, collection and bbox.
both! {
    fn has_the_scenarios_the_fixture_holds_and_passes_each(kind: Kind) {
        let scenarios = scenarios();
        assert_eq!(scenarios.len(), 7);
        let mut failures = Vec::new();
        for s in &scenarios {
            let name = s["name"].as_str().unwrap();
            let dataset = s["dataset"].as_str().unwrap();
            let collection = s.get("collection").and_then(Value::as_str);
            let tiles: Vec<(String, Value)> = s["tiles"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| (t["url"].as_str().unwrap().to_owned(), t["body"].clone()))
                .collect();
            let fake = raw_server(dataset, collection, &tiles, &s["keywordAnswer"]);
            let mut options = FallbackOptions::new(
                dataset,
                s["keyword"].as_str().unwrap(),
                mode(s["keywordRegion"].as_str().unwrap()),
            );
            if let Some(c) = collection {
                options = options.collection(c);
            }
            if let Some(so) = s.get("sort").and_then(Value::as_str) {
                options = options.sort(sort(so));
            }
            let result = match kind.client(&fake).fallback(region_from_json(&s["region"]), options) {
                Ok(r) => r,
                Err(e) => {
                    failures.push(format!("{name}: {e}"));
                    continue;
                }
            };
            let want_origins: Vec<Option<String>> = s["expect"]["origins"]
                .as_array()
                .unwrap()
                .iter()
                .map(|o| o.as_str().map(str::to_owned))
                .collect();
            let got_origins: Vec<_> = result.places.iter().map(origin_of).collect();
            if got_origins != want_origins {
                failures.push(format!("{name}: origins {got_origins:?} != {want_origins:?}"));
            }
            let d = &s["expect"]["disclosed"];
            let want = (d["keyword"].as_bool().unwrap(), d["region"].as_f64());
            if (result.disclosed.keyword, result.disclosed.region) != want {
                failures.push(format!("{name}: disclosed {:?} != {want:?}", result.disclosed));
            }
            let requests = keyword_requests(&fake);
            let requested = s["expect"]["keywordRequested"].as_bool().unwrap();
            if requests.len() != usize::from(requested) {
                failures.push(format!("{name}: {} searchPlaces requests", requests.len()));
            } else if requested {
                let mut want = vec![("dataset".to_owned(), dataset.to_owned())];
                if let Some(c) = collection {
                    want.push(("collection".into(), c.into()));
                }
                want.push(("q".into(), s["keyword"].as_str().unwrap().into()));
                if let Some(b) = s["expect"].get("bbox").and_then(Value::as_str) {
                    want.push(("bbox".into(), b.into()));
                }
                let mut got = params(&requests[0]);
                got.sort();
                want.sort();
                if got != want {
                    failures.push(format!("{name}: params {got:?} != {want:?}"));
                }
            }
            // Each hit says where it came from.
            let vias: Vec<_> = result.places.iter().map(FallbackHit::via).collect();
            let keyword_hits = vias.iter().take_while(|v| **v == "keyword").count();
            if vias[keyword_hits..].iter().any(|v| *v != "tiles") {
                failures.push(format!("{name}: keyword hits do not lead: {vias:?}"));
            }
        }
        assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
    }
}

fn setup_bbox() -> Region {
    Region::bbox(-122.42, 37.77, -122.41, 37.78)
}

fn zanzibar() -> FakeServer {
    FakeServer::with_tilesets(vec![overture(vec![tile(
        "https://tiles.example/p.json",
        vec![record(simple("zc", "Zanzibar Cafe", vec![point(37.772, -122.412)]), Some(5), None)],
    )])])
}

// describe("searchNearbyThenByKeyword arguments"): each case is wrong in its own way and in every way after it,
// so the error names the first. The dataset and the keyword are strings and the keyword region an enum, which the
// types guarantee; the checks that remain are the region and the sort.
mod arguments {
    use super::*;

    both! {
        fn refuses_a_bad_region_before_the_sort_with_no_request(kind: Kind) {
            let fake = zanzibar();
            let options = FallbackOptions::new("overturemaps.org", "  ", KeywordRegionMode::None).sort(SortMode::Similarity);
            let err = kind.client(&fake).fallback(Region::bbox(0.0, 10.0, 1.0, 5.0), options).unwrap_err();
            assert!(matches!(err, Error::InvalidArgument(ArgumentError::InvalidRegion("invalid bbox"))), "{err:?}");
            assert_eq!(fake.request_count(), 0);
        }
    }

    both! {
        fn refuses_similarity_when_the_normalized_keyword_is_empty_with_no_request(kind: Kind) {
            let fake = zanzibar();
            let options = FallbackOptions::new("overturemaps.org", "  ", KeywordRegionMode::None).sort(SortMode::Similarity);
            let err = kind.client(&fake).fallback(setup_bbox(), options).unwrap_err();
            assert!(matches!(err, Error::InvalidArgument(ArgumentError::SimilarityRequiresText)), "{err:?}");
            assert_eq!(fake.request_count(), 0);
        }
    }
}

// Contract §18 step 8 (#56): the qualifying predicate can judge a keyword sendable that the server refuses. NFKD
// turns "ﬁ" into "fi", two characters, but the server keeps the ligature whole, one character, and answers
// KeywordRequired. The fallback then keeps its nearby result instead of failing.
mod when_the_server_refuses_a_keyword_the_sdk_judged_qualifying {
    use super::*;

    both! {
        fn returns_the_nearby_result_disclosing_the_keyword_it_sent(kind: Kind) {
            let fake = zanzibar();
            let result = kind
                .client(&fake)
                .fallback(setup_bbox(), FallbackOptions::new("overturemaps.org", "ﬁ", KeywordRegionMode::Degree).collection("place"))
                .unwrap();
            let nearby = kind
                .client(&zanzibar())
                .ok(setup_bbox(), NearbyOptions::new("overturemaps.org").collection("place").text("ﬁ"));
            assert_eq!(keyword_requests(&fake).len(), 1);
            // The keyword went out and was refused; everything else is the nearby search's.
            assert_eq!((result.disclosed.keyword, result.disclosed.region), (true, Some(0.01)));
            let expected: Vec<FallbackHit> = nearby.places.iter().cloned().map(FallbackHit::Nearby).collect();
            assert_eq!(result.places, expected);
            assert_eq!(result.attribution, nearby.attribution);
            assert_eq!((result.complete, result.overview, result.tiles), (nearby.complete, nearby.overview, nearby.tiles));
        }
    }

    both! {
        fn reports_the_one_degree_grid_when_the_nearby_half_was_answered_from_cache(kind: Kind) {
            let fake = zanzibar();
            let client = kind.client(&fake);
            let options = || NearbyOptions::new("overturemaps.org").collection("place");
            client.ok(setup_bbox(), options());
            let result = client
                .fallback(setup_bbox(), FallbackOptions::new("overturemaps.org", "ﬁ", KeywordRegionMode::Degree).collection("place"))
                .unwrap();
            assert_eq!((result.disclosed.keyword, result.disclosed.region), (true, Some(1.0)));
            let result = client
                .fallback(setup_bbox(), FallbackOptions::new("overturemaps.org", "ﬁ", KeywordRegionMode::None).collection("place"))
                .unwrap();
            assert_eq!((result.disclosed.keyword, result.disclosed.region), (true, None));
        }
    }

    both! {
        fn still_fails_with_any_other_refusal_of_the_keyword_request(kind: Kind) {
            let fake = zanzibar();
            fake.raw_method("org.atgeo.searchPlaces", 400, r#"{"error":"InvalidRequest","message":"q is too long"}"#);
            let err = kind
                .client(&fake)
                .fallback(setup_bbox(), FallbackOptions::new("overturemaps.org", "Quokka", KeywordRegionMode::None).collection("place"))
                .unwrap_err();
            assert_eq!(code_of(&err), "InvalidRequest");
        }
    }

    both! {
        fn still_fails_when_the_keyword_request_cannot_be_made(kind: Kind) {
            let fake = zanzibar();
            fake.network_error(&format!("{DEFAULT_SERVER}/xrpc/org.atgeo.searchPlaces?dataset=overturemaps.org&collection=place&q=Quokka"));
            let err = kind
                .client(&fake)
                .fallback(setup_bbox(), FallbackOptions::new("overturemaps.org", "Quokka", KeywordRegionMode::None).collection("place"))
                .unwrap_err();
            assert!(matches!(err, Error::Transport(_)), "{err:?}");
        }
    }
}

// #60: fallback rules no test killed before. A raw server, so bodies and attribution can be chosen freely.
mod pins {
    use super::*;

    const TILE: &str = "https://places.atgeo.org/tiles/t.json";

    fn loc() -> Value {
        json!([{ "$type": "community.lexicon.location.geo", "latitude": "37.775000", "longitude": "-122.415000" }])
    }

    fn entry(attribution: &str) -> Value {
        json!({ "dataset": "overturemaps.org", "collection": "place", "attribution": attribution })
    }

    fn raw(tile: Value, answer: Value) -> FakeServer {
        raw_server("overturemaps.org", None, &[(TILE.to_owned(), tile)], &answer)
    }

    both! {
        fn never_leaves_out_a_nearby_hit_with_no_origin_even_when_a_keyword_hit_has_none_either(kind: Kind) {
            // Neither record has an origin or a uri, so both hits have origin None.
            let tile = json!({ "records": [{ "value": { "name": "Quokka Lounge", "locations": loc() } }], "attribution": [] });
            let answer = json!({ "places": [{ "value": { "name": "Qwokka Bar", "locations": loc() }, "attribution": [] }] });
            let result = kind
                .client(&raw(tile, answer))
                .fallback(setup_bbox(), FallbackOptions::new("overturemaps.org", "Qwokka", KeywordRegionMode::None))
                .unwrap();
            let got: Vec<_> = result.places.iter().map(|h| (h.via(), h.name().map(str::to_owned))).collect();
            assert_eq!(got, [("keyword", Some("Qwokka Bar".to_owned())), ("tiles", Some("Quokka Lounge".to_owned()))]);
        }
    }

    both! {
        fn keeps_the_nearby_attribution_entry_when_both_halves_carry_the_same_collection(kind: Kind) {
            let tile = json!({ "records": [], "attribution": [entry("from the tiles")] });
            let answer = json!({ "places": [{
                "value": { "origin": "urn:atgeo:overturemaps.org:place:k", "name": "Kumquat", "locations": loc() },
                "attribution": [entry("from the keyword answer")],
            }] });
            let result = kind
                .client(&raw(tile, answer))
                .fallback(setup_bbox(), FallbackOptions::new("overturemaps.org", "Kumquat", KeywordRegionMode::None))
                .unwrap();
            assert_eq!(attribution_json(&result.attribution), json!([entry("from the tiles")]));
        }
    }

    both! {
        fn unions_the_attribution_of_both_halves_sorted_by_dataset_and_collection(kind: Kind) {
            let tile = json!({ "records": [], "attribution": [{ "dataset": "z.org", "collection": "a" }] });
            let answer = json!({ "places": [{
                "value": { "origin": "urn:atgeo:overturemaps.org:place:k", "name": "Kumquat", "locations": loc() },
                "attribution": [{ "dataset": "a.org", "collection": "b" }],
            }] });
            let result = kind
                .client(&raw(tile, answer))
                .fallback(setup_bbox(), FallbackOptions::new("overturemaps.org", "Kumquat", KeywordRegionMode::None))
                .unwrap();
            assert_eq!(
                attribution_json(&result.attribution),
                json!([{ "dataset": "a.org", "collection": "b" }, { "dataset": "z.org", "collection": "a" }])
            );
            // The keyword hit credits its own collection by its own attribution, as a keyword search does.
            assert!(result.places[0].collection().is_none());
        }
    }

    both! {
        fn carries_the_nearby_searchs_failed_tiles_and_still_falls_back(kind: Kind) {
            let tile = json!({ "records": [], "attribution": [] });
            let fake = raw(tile, json!({ "places": [] }));
            fake.raw(TILE, 500, r#"{"error":"x"}"#);
            let result = kind
                .client(&fake)
                .fallback(setup_bbox(), FallbackOptions::new("overturemaps.org", "Kumquat", KeywordRegionMode::None))
                .unwrap();
            assert_eq!(keyword_requests(&fake).len(), 1);
            assert_eq!((result.complete, result.tiles.failed, result.tile_failures.len()), (false, 1, 1));
        }
    }
}

// §12 item 7: when the fallback does send its keyword, no nearby-half request carries it.
both! {
    fn puts_the_keyword_in_the_search_places_request_only(kind: Kind) {
        let fake = zanzibar();
        let keyword = "Kumquat Lagoon";
        kind.client(&fake)
            .fallback(
                Region::circle(37.7749295, -122.4194155, 500.0),
                FallbackOptions::new("overturemaps.org", keyword, KeywordRegionMode::None).collection("place"),
            )
            .unwrap();
        let (search, nearby): (Vec<String>, Vec<String>) =
            fake.requests().into_iter().partition(|r| r.contains("org.atgeo.searchPlaces"));
        assert_eq!(search.len(), 1);
        assert!(!nearby.is_empty());
        for haystack in &nearby {
            let decoded = decode(haystack).to_lowercase();
            for needle in ["kumquat lagoon", "kumquat", "lagoon", "kumqu"] {
                assert!(!haystack.to_lowercase().contains(needle) && !decoded.contains(needle), "{needle} in {haystack}");
            }
        }
    }
}

// Cancellation (contract §11) is dropping the future, and a dropped future runs no more code: the keyword
// request, which comes after the nearby half in the same future, is never made.
mod cancellation {
    use super::*;
    use atgeo::AtgeoClient;

    #[test]
    fn a_call_dropped_during_the_nearby_half_never_sends_its_keyword() {
        let fake = zanzibar();
        fake.delay_all(Duration::from_secs(5));
        let client = AtgeoClient::with_transport(fake.clone());
        let rt = runtime();
        rt.block_on(async {
            let call = client.search_nearby_then_by_keyword(
                setup_bbox(),
                FallbackOptions::new("overturemaps.org", "Quokka", KeywordRegionMode::None).collection("place"),
            );
            tokio::select! {
                _ = call => panic!("finished"),
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
            }
            // Time goes on, and nothing more is sent.
            tokio::time::sleep(Duration::from_secs(60)).await;
        });
        assert!(keyword_requests(&fake).is_empty());
        assert_eq!(fake.in_flight(), 0);
    }

    #[test]
    fn a_call_that_is_never_polled_sends_nothing() {
        let fake = zanzibar();
        let client = AtgeoClient::with_transport(fake.clone());
        let call = client.search_nearby_then_by_keyword(
            setup_bbox(),
            FallbackOptions::new("overturemaps.org", "Quokka", KeywordRegionMode::None),
        );
        drop(call);
        assert_eq!(fake.request_count(), 0);
    }
}
