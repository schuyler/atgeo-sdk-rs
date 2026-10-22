//! A port of the reference's `test/privacy.test.ts` (contract §12). The transport seam carries only a URL,
//! so the method (always GET), headers (the transport's own, checked in `transports.rs`) and body (never
//! one) are fixed by construction; the tests that assert them check the URLs.

#[macro_use]
mod common;

use atgeo::{Region, SearchOptions};
use common::*;

const TILE: &str = "https://tiles.example/t1.json";

fn setup() -> FakeServer {
    FakeServer::with_tilesets(vec![overture(vec![tile(
        TILE,
        vec![rec(simple("r1", "Zanzibar Quokka Cafe", vec![point(37.775, -122.418)]))],
    )])])
}

/// Each request's URL, raw and percent-decoded, lowercased.
fn wire(fake: &FakeServer) -> Vec<String> {
    fake.requests().iter().flat_map(|u| [u.to_lowercase(), decode(u).to_lowercase()]).collect()
}

fn encode_uri_component(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

// describe("search text stays local")
mod search_text_stays_local {
    use super::*;

    both! {
        fn does_not_put_the_text_on_the_wire(kind: Kind) {
            for text in ["Zanzibar Quokka", "  ZANZIBÁR   quokka ", "Zanzibar"] {
                let fake = setup();
                kind.client(&fake).ok(Region::circle(37.7749295, -122.4194155, 500.0), SearchOptions::new().text(text));
                assert!(fake.request_count() > 0);
                let normalized = "zanzibar quokka";
                let mut needles = vec![text.to_lowercase(), text.trim().to_lowercase(), normalized.to_owned()];
                needles.extend(normalized.split(' ').map(str::to_owned));
                needles.extend(["zanzib".to_owned(), "quokka".to_owned(), encode_uri_component(text).to_lowercase()]);
                for haystack in wire(&fake) {
                    for needle in &needles {
                        assert!(!haystack.contains(needle.as_str()), "{needle:?} in {haystack:?}");
                    }
                }
            }
        }
    }

    both! {
        fn issues_only_get_tiles_and_tile_requests(kind: Kind) {
            let fake = setup();
            kind.client(&fake).ok(Region::circle(37.7749295, -122.4194155, 500.0), SearchOptions::new().text("Zanzibar Quokka"));
            let kinds: Vec<String> = fake
                .requests()
                .iter()
                .map(|r| {
                    if r.contains("/xrpc/org.atgeo.getTiles?") {
                        "tiles".to_owned()
                    } else if r == TILE {
                        "tile".to_owned()
                    } else {
                        r.clone()
                    }
                })
                .collect();
            assert_eq!(kinds, ["tiles", "tile"]);
        }
    }

    both! {
        fn sends_nothing_but_the_snapped_box_dataset_and_collection(kind: Kind) {
            let fake = setup();
            kind.client(&fake).ok(Region::bbox(-122.43, 37.77, -122.41, 37.78), SearchOptions::new().text("Zanzibar Quokka"));
            let lists = fake.tile_list_requests();
            assert_eq!(lists.len(), 1);
            let names: Vec<String> = params(&lists[0]).into_iter().map(|(k, _)| k).collect();
            assert_eq!(names, ["dataset", "collection", "bbox"]);
        }
    }
}

// describe("precise coordinates stay local")
mod precise_coordinates_stay_local {
    use super::*;

    both! {
        fn keeps_the_circle_centre_off_the_wire(kind: Kind) {
            let fake = setup();
            kind.client(&fake).ok(Region::circle(37.7749295, -122.4194155, 123.456789), SearchOptions::new().text("Zanzibar Quokka"));
            assert!(fake.request_count() > 0);
            for h in wire(&fake) {
                for needle in ["37.7749295", "122.4194155", "7749295", "4194155", "123.456789"] {
                    assert!(!h.contains(needle), "{needle} in {h}");
                }
            }
        }
    }

    both! {
        fn keeps_full_precision_bbox_corners_off_the_wire(kind: Kind) {
            let fake = setup();
            kind.client(&fake).ok(Region::bbox(-122.4194155, 37.7749295, -122.4094155, 37.7849295), SearchOptions::new());
            assert!(fake.request_count() > 0);
            for h in wire(&fake) {
                for needle in ["7749295", "4194155", "4094155", "7849295"] {
                    assert!(!h.contains(needle), "{needle} in {h}");
                }
            }
        }
    }
}

// Contract §12, item 4.
mod get_place_sends_the_origin_and_nothing_else {
    use super::*;

    both! {
        fn on_a_cache_miss(kind: Kind) {
            let fake = setup();
            let _ = kind.client(&fake).get_place(&place_origin("r1"));
            let requests = fake.requests();
            assert_eq!(requests.len(), 1);
            assert_eq!(params(&requests[0]), [("uri".to_owned(), place_origin("r1"))]);
        }
    }
}
