//! A port of the reference's `test/keywordRegions.test.ts`: the whole-degree bbox a keyword search sends for each
//! input box (`fixtures/keyword-regions.json`, contract §16). The fake refuses a bbox with decimals and one the
//! real server would call invalid, so a case passes only if the server accepts what was sent, whatever the
//! fixture's arithmetic says.

#[macro_use]
mod common;

use atgeo::{KeywordOptions, KeywordRegion};
use common::*;

/// `^(0|-?[1-9]\d*)(,(0|-?[1-9]\d*)){3}$`: plain integers, no `-0`, no `.0`.
fn plain_integers(s: &str) -> bool {
    let parts: Vec<&str> = s.split(',').collect();
    parts.len() == 4
        && parts.iter().all(|p| {
            let digits = p.strip_prefix('-').unwrap_or(p);
            *p == "0" || (!digits.starts_with('0') && !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
        })
}

fn run(kind: Kind, bbox: [f64; 4]) -> (FakeServer, Result<(), atgeo::Error>) {
    let fake = FakeServer::with_tilesets(vec![overture(vec![])]);
    let result = kind
        .client(&fake)
        .keyword(
            "Zanzibar",
            KeywordOptions::new("overturemaps.org")
                .collection("place")
                .region(KeywordRegion::new(bbox[0], bbox[1], bbox[2], bbox[3])),
        )
        .map(|_| ());
    (fake, result)
}

both! {
    fn sends_the_fixtures_bbox_for_every_case_and_the_server_accepts_it(kind: Kind) {
        let cases: Vec<Value> = serde_json::from_str(include_str!("fixtures/keyword-regions.json")).unwrap();
        assert_eq!(cases.len(), 72);
        let mut failures = Vec::new();
        for c in &cases {
            let name = c["name"].as_str().unwrap();
            let b: Vec<f64> = c["bbox"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
            let sent = c["sent"].as_str().unwrap();
            let (fake, result) = run(kind, [b[0], b[1], b[2], b[3]]);
            if let Err(e) = result {
                failures.push(format!("{name}: the server refused {sent:?} for {b:?}: {e}"));
                continue;
            }
            let requests = fake.requests();
            if requests.len() != 1 {
                failures.push(format!("{name}: {} requests", requests.len()));
                continue;
            }
            let got = param(&requests[0], "bbox");
            if got.as_deref() != Some(sent) {
                failures.push(format!("{name}: sent {got:?}, expected {sent:?}"));
            }
            if !plain_integers(sent) {
                failures.push(format!("{name}: {sent:?} is not plain integers"));
            }
        }
        assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
    }
}

both! {
    fn keeps_every_digit_of_a_precise_box_off_the_wire(kind: Kind) {
        let (fake, result) = run(kind, [-122.4194155, 37.7749295, -122.4094155, 37.7849295]);
        result.unwrap();
        let requests = fake.requests();
        assert_eq!(param(&requests[0], "bbox").as_deref(), Some("-123,37,-122,38"));
        for r in &requests {
            for needle in ["7749295", "4194155", "4094155", "7849295"] {
                assert!(!r.contains(needle), "{needle} in {r}");
            }
        }
    }
}

#[test]
fn the_plain_integer_pattern_rejects_what_it_should() {
    assert!(plain_integers("0,-1,10,-180"));
    for bad in ["-0,0,1,2", "0.0,0,1,2", "01,0,1,2", "1,2,3", "1,2,3,4,5", "1,2,3,", "1,2,3,-", "1e1,2,3,4"] {
        assert!(!plain_integers(bad), "{bad}");
    }
}
