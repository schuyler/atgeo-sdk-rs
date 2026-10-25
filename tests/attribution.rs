//! A port of the reference's `test/attribution.test.ts`: every scenario of `fixtures/attribution-scenarios.json`
//! (contract §13), and the same scenarios with the tiles arriving in reverse URL order (R7).
//!
//! The bodies need not validate against the lexicons, so the tiles are served through the fake's raw-response
//! override, which bypasses its validation. The fake answers every getTiles call with the scenario's tile URLs in
//! the listed order, which is deliberately not sorted (contract §4.3).

#[macro_use]
mod common;

use std::time::Duration;

use atgeo::{SearchOptions, SearchResult};
use common::*;

/// The default error body of a tile that has a `status` and no `body`.
const DEFAULT_ERROR_BODY: &str = r#"{"error":"InternalServerError","message":"upstream error"}"#;

fn scenarios() -> Vec<Value> {
    serde_json::from_str(include_str!("fixtures/attribution-scenarios.json")).unwrap()
}

fn tile_urls(s: &Value) -> Vec<String> {
    s["tiles"].as_array().unwrap().iter().map(|t| t["url"].as_str().unwrap().to_owned()).collect()
}

/// A fake that serves the scenario: getTiles lists the tile URLs as the scenario does, and each tile is served
/// verbatim with its status. `delays` holds per-URL response delays.
fn server_for(s: &Value, delays: &[(String, Duration)]) -> FakeServer {
    let dataset = s["dataset"].as_str().unwrap();
    // The fake checks the dataset and collection of a getTiles call against its tilesets; the tiles themselves
    // are served raw, so this tileset carries none.
    let collection = s.get("collection").and_then(Value::as_str).unwrap_or("c");
    let mut tileset = overture(vec![]);
    tileset.collections = vec![json!({ "dataset": dataset, "collection": collection })];
    let listed = tile_urls(s);
    let fake = FakeServer::builder().tilesets(vec![tileset]).tiles_for(move |_| TilesFor::Urls(listed.clone())).build();
    for t in s["tiles"].as_array().unwrap() {
        let status = t.get("status").and_then(Value::as_u64).unwrap_or(200) as u16;
        let body = match t.get("body") {
            Some(body) => serde_json::to_vec(body).unwrap(),
            None => DEFAULT_ERROR_BODY.as_bytes().to_vec(),
        };
        fake.raw(t["url"].as_str().unwrap(), status, body);
    }
    for (url, d) in delays {
        fake.delay(url, *d);
    }
    fake
}

fn options_for(s: &Value) -> SearchOptions {
    // The search has no text and the default sort.
    let o = SearchOptions::new(s["dataset"].as_str().unwrap());
    match s.get("collection").and_then(Value::as_str) {
        Some(c) => o.collection(c),
        None => o,
    }
}

/// A hit as the (origin, collection) pair the fixture compares, in a form that sorts.
fn pair_key(origin: &Option<String>, collection: &Value) -> String {
    format!("{origin:?}|{collection}")
}

/// One line per failed expectation of the scenario against its result.
fn check(s: &Value, result: &SearchResult, failures: &mut Vec<String>) {
    let name = s["name"].as_str().unwrap();
    let want_attribution = &s["expect"]["attribution"];
    let got_attribution = attribution_json(&result.attribution);
    if &got_attribution != want_attribution {
        failures.push(format!("{name}: attribution {got_attribution} != {want_attribution}"));
    }
    let mut got: Vec<String> =
        result.places.iter().map(|h| pair_key(&h.origin, &collection_json(&h.collection))).collect();
    let mut want: Vec<String> = s["expect"]["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| pair_key(&h["origin"].as_str().map(str::to_owned), &h["collection"]))
        .collect();
    got.sort();
    want.sort();
    if got != want {
        failures.push(format!("{name}: hits {got:?} != {want:?}"));
    }
}

#[test]
fn has_the_scenarios_the_contract_describes() {
    assert_eq!(scenarios().len(), 12);
}

// describe("attribution scenarios"): a fresh client per scenario, since scenarios reuse tile URLs with different
// bodies and the tile-list cache is keyed by dataset, collection and bbox.
both! {
    fn attribution_scenarios(kind: Kind) {
        let mut failures = Vec::new();
        for s in &scenarios() {
            let fake = server_for(s, &[]);
            let result = match kind.client(&fake).search(region_from_json(&s["region"]), options_for(s)) {
                Ok(r) => r,
                Err(e) => {
                    failures.push(format!("{}: {e}", s["name"]));
                    continue;
                }
            };
            check(s, &result, &mut failures);
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
}

// describe("attribution scenarios, tiles arriving in reverse URL order"): the result must not depend on which
// tile arrives first, since contract §4.10 walks the tiles in URL order. The lowest URL answers last.
both! {
    fn attribution_scenarios_with_tiles_arriving_in_reverse_url_order(kind: Kind) {
        let mut failures = Vec::new();
        let mut ran = 0;
        for s in scenarios().iter().filter(|s| s["tiles"].as_array().unwrap().len() > 1) {
            let mut urls = tile_urls(s);
            urls.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            let delays: Vec<(String, Duration)> = urls
                .iter()
                .enumerate()
                .map(|(i, u)| (u.clone(), Duration::from_millis(25 * (urls.len() - 1 - i) as u64)))
                .collect();
            let fake = server_for(s, &delays);
            match kind.client(&fake).search(region_from_json(&s["region"]), options_for(s)) {
                Ok(result) => check(s, &result, &mut failures),
                Err(e) => failures.push(format!("{}: {e}", s["name"])),
            }
            ran += 1;
        }
        assert!(ran > 0, "the fixture has scenarios with more than one tile");
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
}

// The reverse-order rerun above only means something if the lowest URL really arrives last. This pins the
// arrival order itself, on a pair whose two copies differ, so that a union taken in arrival order would pick
// the higher URL's release.
both! {
    fn the_lowest_url_wins_the_pair_although_it_arrives_last(kind: Kind) {
        let (a, b) = ("https://places.atgeo.org/tiles/a.json", "https://places.atgeo.org/tiles/b.json");
        let body = |release: &str| {
            json!({ "records": [], "attribution": [{ "dataset": "d", "collection": "c", "release": release }] })
        };
        let fake = FakeServer::builder()
            .tilesets(vec![{
                let mut t = overture(vec![]);
                t.collections = vec![json!({ "dataset": "d", "collection": "c" })];
                t
            }])
            .tiles_for(move |_| TilesFor::Urls(vec![b.to_owned(), a.to_owned()]))
            .build();
        fake.raw(a, 200, body("from-a").to_string());
        fake.raw(b, 200, body("from-b").to_string());
        fake.delay(a, Duration::from_millis(50));
        let result = kind.client(&fake).ok(REGION, SearchOptions::new("d"));
        assert_eq!(attribution_json(&result.attribution), json!([{ "dataset": "d", "collection": "c", "release": "from-a" }]));
    }
}

// Contract §13: the sort of 4.10 uses the comparator of 4.3, UTF-16 code units, which no fixture pins. U+FF61 is
// one code unit above the surrogates; U+10000 is two code units, D800 DC00. By code point U+FF61 sorts first; by
// code units it sorts last.
both! {
    fn sorts_the_attribution_by_utf16_code_units(kind: Kind) {
        let a = "https://places.atgeo.org/tiles/a.json";
        let (low, astral) = ("\u{FF61}", "\u{10000}");
        let fake = FakeServer::builder()
            .tilesets(vec![{
                let mut t = overture(vec![]);
                t.collections = vec![json!({ "dataset": "d", "collection": "c" })];
                t
            }])
            .tiles_for(move |_| TilesFor::Urls(vec![a.to_owned()]))
            .build();
        let entry = |dataset: &str, collection: &str| json!({ "dataset": dataset, "collection": collection });
        let body = json!({ "records": [], "attribution": [
            entry(low, "z"), entry(astral, "z"), entry("d", astral), entry("d", low), entry("D", "c"),
        ] });
        fake.raw(a, 200, body.to_string());
        let result = kind.client(&fake).ok(REGION, SearchOptions::new("d"));
        assert_eq!(
            attribution_json(&result.attribution),
            json!([entry("D", "c"), entry("d", astral), entry("d", low), entry(astral, "z"), entry(low, "z")])
        );
    }
}

// Each hit takes its collection from the tile its own record came from, even when the tiles' attributions
// differ (contract §8), and a record the text filter drops does not shift the others onto its tile's.
fn two_tiles(a_name: &str) -> FakeServer {
    let (a, b) = ("https://places.atgeo.org/tiles/a.json", "https://places.atgeo.org/tiles/b.json");
    let record = |c: &str, n: &str, name: &str| {
        json!({ "value": {
            "$type": "org.atgeo.place", "origin": format!("urn:atgeo:d:{c}:{n}"), "name": name,
            "locations": [point(37.775, -122.415)],
        } })
    };
    let fake = FakeServer::builder()
        .tilesets(vec![{
            let mut t = overture(vec![]);
            t.collections = vec![json!({ "dataset": "d", "collection": "c1" })];
            t
        }])
        .tiles_for(move |_| TilesFor::Urls(vec![a.to_owned(), b.to_owned()]))
        .build();
    let body = |c: &str, n: &str, name: &str| {
        json!({ "records": [record(c, n, name)], "attribution": [{ "dataset": "d", "collection": c }] }).to_string()
    };
    fake.raw(a, 200, body("c1", "1", a_name));
    fake.raw(b, 200, body("c2", "2", "Cafe"));
    fake
}

both! {
    fn each_hit_takes_the_collection_of_its_own_tile(kind: Kind) {
        let result = kind.client(&two_tiles("Cafe")).ok(REGION, SearchOptions::new("d"));
        let mut got: Vec<(String, Value)> =
            result.places.iter().map(|h| (h.origin.clone().unwrap(), collection_json(&h.collection))).collect();
        got.sort_by(|x, y| x.0.cmp(&y.0));
        assert_eq!(got, [
            ("urn:atgeo:d:c1:1".to_owned(), json!({ "dataset": "d", "collection": "c1" })),
            ("urn:atgeo:d:c2:2".to_owned(), json!({ "dataset": "d", "collection": "c2" })),
        ]);
    }
}

both! {
    fn a_record_the_text_filter_drops_does_not_shift_the_collections_of_the_rest(kind: Kind) {
        let result = kind.client(&two_tiles("Zzzzqqq")).ok(REGION, SearchOptions::new("d").text("Cafe"));
        assert_eq!(origins(&result), [Some("urn:atgeo:d:c2:2".to_owned())]);
        assert_eq!(collection_json(&result.places[0].collection), json!({ "dataset": "d", "collection": "c2" }));
    }
}
