//! A port of the reference's `test/ranking.test.ts`: every scenario of `fixtures/ranking-scenarios.json`,
//! driven through the client and the fake with one tile, checked for membership as the fixture asks.

#[macro_use]
mod common;

use atgeo::{SearchOptions, SortMode};
use common::*;

fn sort_of(v: Option<&Value>) -> SortMode {
    match v.and_then(Value::as_str) {
        None | Some("fused") => SortMode::Fused,
        Some("distance") => SortMode::Distance,
        Some("similarity") => SortMode::Similarity,
        Some(other) => panic!("unknown sort {other}"),
    }
}

/// Runs every scenario and returns one line per failed expectation.
fn failures(kind: Kind) -> Vec<String> {
    let scenarios: Vec<Value> = serde_json::from_str(include_str!("fixtures/ranking-scenarios.json")).unwrap();
    assert_eq!(scenarios.len(), 19);
    let mut failures = Vec::new();
    for s in &scenarios {
        let name = s["name"].as_str().unwrap();
        let records = s["records"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| record(r["value"].clone(), r["importance"].as_i64(), None))
            .collect();
        const TILE: &str = "https://places.atgeo.org/tiles/t0.json";
        let mut builder = FakeServer::builder().tilesets(vec![overture(vec![tile(TILE, records)])]);
        // A scenario with "overview": true has the fake's getTiles answer it (contract §8).
        if s.get("overview").and_then(Value::as_bool) == Some(true) {
            builder = builder.tiles_for(|_| TilesFor::Answer { tiles: vec![TILE.to_owned()], overview: Some(true) });
        }
        let fake = builder.build();
        // The scenario names the dataset and collection it is a search of (contract §8); there are no defaults.
        let mut options = SearchOptions::new(s["dataset"].as_str().unwrap())
            .collection(s["collection"].as_str().unwrap())
            .sort(sort_of(s.get("sort")));
        if let Some(text) = s.get("text").and_then(Value::as_str) {
            options = options.text(text);
        }
        let result = match kind.client(&fake).search(region_from_json(&s["region"]), options) {
            Ok(r) => r,
            Err(e) => {
                failures.push(format!("{name}: {e}"));
                continue;
            }
        };
        let got: Vec<String> = result.places.iter().map(|h| h.origin.clone().unwrap_or_default()).collect();
        let top = s["expect"]["top"].as_array().unwrap();
        for t in top {
            let k = t["k"].as_u64().unwrap() as usize;
            for want in t["origins"].as_array().unwrap() {
                if !got.iter().take(k).any(|o| o == want.as_str().unwrap()) {
                    failures.push(format!("{name}: {want} not in the top {k} of {got:?}"));
                }
            }
        }
        for absent in s["expect"].get("absent").and_then(Value::as_array).into_iter().flatten() {
            if got.iter().any(|o| o == absent.as_str().unwrap()) {
                failures.push(format!("{name}: {absent} present in {got:?}"));
            }
        }
        if top.is_empty() && !got.is_empty() {
            failures.push(format!("{name}: expected no hits, got {got:?}"));
        }
    }
    failures
}

// describe("ranking scenarios"): one test collecting every scenario's failures.
both! {
    fn ranking_scenarios(kind: Kind) {
        let failures = failures(kind);
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
}
