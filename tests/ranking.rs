//! A port of the reference's `test/ranking.test.ts`: every scenario of `fixtures/ranking-scenarios.json`,
//! driven through the client and the fake with one tile, checked for membership as the fixture asks.

#[macro_use]
mod common;

use atgeo::{NearbyOptions, SortMode};
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
    assert_eq!(scenarios.len(), 22);
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
        let mut options = NearbyOptions::new(s["dataset"].as_str().unwrap())
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

fn kept(kind: Kind, names: &[&str], text: &str) -> Vec<String> {
    let records: Vec<Value> = names
        .iter()
        .enumerate()
        .map(|(i, n)| record(simple(&format!("n{i}"), n, vec![point(37.772, -122.412)]), None, None))
        .collect();
    let fake = FakeServer::with_tilesets(vec![overture(vec![tile("https://places.atgeo.org/tiles/t0.json", records)])]);
    let result = kind.client(&fake).ok(REGION, overture_options().text(text));
    result.places.iter().map(|h| h.name().unwrap().to_owned()).collect()
}

// Contract §7: tier 1 is kept at any score, tier 2 needs 0.8, and a query whose tokens all fold away keeps 0.6.
both! {
    fn keeps_a_word_match_at_any_score_and_a_fuzzy_match_only_from_08(kind: Kind) {
        // `qu` scores about 0.41 against the long name, but it is the start of a word of it: tier 1.
        assert_eq!(kept(kind, &["Bar & Grill at the Old Quarry"], "qu"), ["Bar & Grill at the Old Quarry"]);
        // Fuzzy: 0.8 and above is kept (`kostco` about 0.89), a random string finds nothing (`xqzjvkwpfhgy` 0.64
        // against `Qwizify`, which the old 0.6 threshold kept).
        assert_eq!(kept(kind, &["Costco"], "kostco"), ["Costco"]);
        assert!(kept(kind, &["Qwizify"], "xqzjvkwpfhgy").is_empty());
        // A fuzzy match below the threshold stays out (Starbux/Home Depot scores under 0.6; the fixture's
        // fuzzy-match-needs-0.8 scenario covers the band from 0.6 to 0.8).
        assert!(kept(kind, &["Home Depot"], "Starbux").is_empty());
    }
}

both! {
    fn keeps_a_query_that_folds_to_no_words_at_the_old_threshold(kind: Kind) {
        // "&" has no tokens after folding, so every record is tier 1; with the old threshold of 0.6 a name
        // that scores under it is dropped and one over it kept, however short.
        let got = kept(kind, &["&", "Zanzibar Cafe"], "&");
        assert_eq!(got, ["&"]);
    }
}
