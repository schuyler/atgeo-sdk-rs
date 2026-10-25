//! A port of the reference's `test/privacy.test.ts` (contract §12). The transport seam carries only a URL,
//! so the method (always GET), headers (the transport's own, checked in `transports.rs`) and body (never
//! one) are fixed by construction; the tests that assert them check the URLs.

#[macro_use]
mod common;

use atgeo::Region;
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
                kind.client(&fake).ok(Region::circle(37.7749295, -122.4194155, 500.0), overture_options().text(text));
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
            kind.client(&fake).ok(Region::circle(37.7749295, -122.4194155, 500.0), overture_options().text("Zanzibar Quokka"));
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
            kind.client(&fake).ok(Region::bbox(-122.43, 37.77, -122.41, 37.78), overture_options().text("Zanzibar Quokka"));
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
            kind.client(&fake).ok(Region::circle(37.7749295, -122.4194155, 123.456789), overture_options().text("Zanzibar Quokka"));
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
            kind.client(&fake).ok(Region::bbox(-122.4194155, 37.7749295, -122.4094155, 37.7849295), overture_options());
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

// describe("failed tiles carry no search text")
mod failed_tiles_carry_no_search_text {
    use super::*;
    use atgeo::TileFailureKind;

    both! {
        fn leaves_every_form_of_the_text_out_of_tile_failures(kind: Kind) {
            let urls: Vec<String> = ["a", "b", "c", "d"].iter().map(|n| url(n)).collect();
            let records = || vec![rec(simple("r1", "Zanzibar Quokka Cafe", vec![point(37.775, -122.418)]))];
            let fake = FakeServer::builder()
                .fail_tiles(&[urls[0].as_str()])
                .tilesets(vec![overture(urls.iter().map(|u| tile(u, records())).collect())])
                .build();
            fake.raw(&urls[1], 200, "<html>not a tile</html>");
            fake.network_error(&urls[2]);
            fake.raw(&urls[3], 500, "null");
            let text = "  ZANZIBÁR   quokka ";
            let result = kind.client(&fake).ok(Region::circle(37.7749295, -122.4194155, 500.0), overture_options().text(text));
            let kinds: Vec<_> = result.tile_failures.iter().map(|f| f.kind).collect();
            assert_eq!(kinds, [
                TileFailureKind::Status, TileFailureKind::Decode, TileFailureKind::Transport, TileFailureKind::Status,
            ]);
            let haystack = result
                .tile_failures
                .iter()
                .map(|f| format!("{f:?} {:?} {}", f.error, f.error.as_ref().map_or(String::new(), |e| e.to_string())))
                .collect::<String>()
                .to_lowercase();
            for needle in [text.to_lowercase(), text.trim().to_lowercase(), "zanzibar quokka".into(), "zanzib".into(), "quokka".into()] {
                assert!(!haystack.contains(&needle), "{needle:?} in {haystack:?}");
            }
        }
    }
}
