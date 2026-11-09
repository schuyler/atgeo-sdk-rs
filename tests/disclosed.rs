//! A port of the reference's `test/disclosed.test.ts`: what each result reports the server learned during that
//! call (contract §4.11), and §12 item 9: `disclosed` agrees with the captured requests.

#[macro_use]
mod common;

use atgeo::{FallbackOptions, KeywordOptions, KeywordRegion, KeywordRegionMode, NearbyOptions};
use common::*;

fn tiles() -> Vec<FakeTile> {
    vec![tile(
        "https://tiles.example/p.json",
        vec![record(simple("zc", "Zanzibar Cafe", vec![point(37.772, -122.412)]), Some(5), None)],
    )]
}

fn setup() -> FakeServer {
    FakeServer::with_tilesets(vec![overture(tiles())])
}

fn nearby() -> NearbyOptions {
    overture_options()
}

fn keyword_requests(fake: &FakeServer) -> usize {
    fake.requests().iter().filter(|r| r.contains("org.atgeo.searchPlaces")).count()
}

fn d(r: &atgeo::Disclosure) -> (bool, Option<f64>) {
    (r.keyword, r.region)
}

// describe("disclosed")
mod disclosed {
    use super::*;

    both! {
        fn is_the_001_grid_no_keyword_on_a_cold_nearby_search_whose_hits_come_from_tiles(kind: Kind) {
            let result = kind.client(&setup()).ok(REGION, nearby().text("Zanzibar"));
            assert_eq!(d(&result.disclosed), (false, Some(0.01)));
            assert!(!result.places.is_empty());
            assert!(result.places.iter().all(|h| h.via() == "tiles"));
        }
    }

    both! {
        fn reports_no_region_for_a_nearby_search_answered_from_cache(kind: Kind) {
            let client = kind.client(&setup());
            client.ok(REGION, nearby());
            let warm = client.ok(REGION, nearby().text("Zanzibar"));
            assert_eq!(d(&warm.disclosed), (false, None));
        }
    }

    both! {
        fn reports_the_keyword_and_a_1_degree_region_for_a_keyword_search_with_a_region(kind: Kind) {
            let options = KeywordOptions::new("overturemaps.org").collection("place")
                .region(KeywordRegion::new(-122.42, 37.77, -122.41, 37.78));
            let result = kind.client(&setup()).keyword("Zanzibar", options).unwrap();
            assert_eq!(d(&result.disclosed), (true, Some(1.0)));
        }
    }

    both! {
        fn reports_the_keyword_and_no_region_for_a_keyword_search_without_one(kind: Kind) {
            let result = kind.client(&setup()).keyword("Zanzibar", KeywordOptions::new("overturemaps.org").collection("place")).unwrap();
            assert_eq!(d(&result.disclosed), (true, None));
        }
    }
}

// #60: what counts as sent for the region (contract §4.11).
mod region_counting {
    use super::*;

    both! {
        fn counts_a_get_tiles_request_as_sent_even_when_it_lists_no_tiles(kind: Kind) {
            let fake = FakeServer::builder()
                .tilesets(vec![overture(tiles())])
                .tiles_for(|_| TilesFor::Urls(vec![]))
                .build();
            let result = kind.client(&fake).ok(REGION, nearby());
            assert_eq!(fake.request_count(), 1);
            assert_eq!(d(&result.disclosed), (false, Some(0.01)));
        }
    }

    both! {
        fn does_not_count_a_tile_the_client_refused_to_fetch(kind: Kind) {
            // The tile list is cached by the first search, so the second sends nothing: its only tile is refused.
            let fake = FakeServer::builder()
                .tilesets(vec![overture(tiles())])
                .tiles_for(|_| TilesFor::Urls(vec!["ftp://tiles.example/p.json".into()]))
                .build();
            let client = kind.client(&fake);
            let cold = client.ok(REGION, nearby());
            // A refused tile on a cold call: the list request still counts.
            assert_eq!(d(&cold.disclosed), (false, Some(0.01)));
            let before = fake.request_count();
            let warm = client.ok(REGION, nearby());
            assert_eq!(fake.request_count(), before);
            let kinds: Vec<_> = warm.tile_failures.iter().map(|f| f.kind).collect();
            assert_eq!(kinds, [atgeo::TileFailureKind::Refused]);
            assert_eq!(d(&warm.disclosed), (false, None));
        }
    }

    both! {
        fn counts_a_tile_fetch_that_fails_and_a_tile_whose_list_is_cached_but_whose_tile_was_evicted(kind: Kind) {
            // A failed fetch still told the server.
            let fake = FakeServer::builder().tilesets(vec![overture(tiles())]).fail_tiles(&["https://tiles.example/p.json"]).build();
            let client = kind.client(&fake);
            let first = client.ok(REGION, nearby());
            assert_eq!((first.complete, d(&first.disclosed)), (false, (false, Some(0.01))));
            // The list is cached; the tile never entered the cache, so the next call fetches it again.
            let second = client.ok(REGION, nearby());
            assert_eq!(d(&second.disclosed), (false, Some(0.01)));
            assert_eq!(fake.tile_list_requests().len(), 1);

            // A budget of zero evicts every tile at once: the warm list does not make a warm call.
            let fake = setup();
            let client = kind.configured(&fake, Config { budget: Some(0), ..Config::default() });
            client.ok(REGION, nearby());
            let again = client.ok(REGION, nearby());
            assert_eq!(fake.tile_list_requests().len(), 1);
            assert_eq!(d(&again.disclosed), (false, Some(0.01)));
        }
    }
}

// §12 item 9: `keyword` is true exactly when a searchPlaces request was made, and `region` is the finest grid
// among the regions that went out.
mod agrees_with_the_requests {
    use super::*;

    fn fallback(region: KeywordRegionMode, keyword: &str) -> FallbackOptions {
        FallbackOptions::new("overturemaps.org", keyword, region).collection("place")
    }

    both! {
        fn for_every_branch_of_the_fallback(kind: Kind) {
            // (keyword, mode, warm first, expected keyword request, expected disclosed)
            type Case = (&'static str, KeywordRegionMode, bool, bool, (bool, Option<f64>));
            let cases: [Case; 6] = [
                ("Zanzibar", KeywordRegionMode::Degree, false, false, (false, Some(0.01))), // solid hit: no request
                ("Quokka", KeywordRegionMode::Degree, false, true, (true, Some(0.01))),
                ("Quokka", KeywordRegionMode::None, false, true, (true, Some(0.01))),
                ("Quokka", KeywordRegionMode::Degree, true, true, (true, Some(1.0))),
                ("Quokka", KeywordRegionMode::None, true, true, (true, None)),
                ("q", KeywordRegionMode::Degree, true, false, (false, None)), // cannot qualify: no request
            ];
            for (keyword, mode, warm, sent, want) in cases {
                let fake = setup();
                let client = kind.client(&fake);
                if warm {
                    client.ok(REGION, nearby());
                }
                let result = client.fallback(REGION, fallback(mode, keyword)).unwrap();
                assert_eq!(keyword_requests(&fake), usize::from(sent), "{keyword} {mode:?} warm={warm}");
                assert_eq!(d(&result.disclosed), want, "{keyword} {mode:?} warm={warm}");
                assert_eq!(result.disclosed.keyword, keyword_requests(&fake) > 0);
            }
        }
    }
}
