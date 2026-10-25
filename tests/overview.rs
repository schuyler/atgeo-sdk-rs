//! A port of the reference's `test/overview.test.ts`.

#[macro_use]
mod common;

use atgeo::Region;
use common::*;

const EAST: &str = "https://tiles.example/east.json";
const WEST: &str = "https://tiles.example/west.json";

fn setup(kind: Kind, tiles_for: Option<fn(&str) -> TilesFor>, fail_tiles: &[&str]) -> (FakeServer, Client) {
    let mut b = FakeServer::builder().fail_tiles(fail_tiles).tilesets(vec![overture(vec![
        tile(EAST, vec![rec(simple("e", "East", vec![point(37.773, -122.413)]))]),
        tile(WEST, vec![rec(simple("w", "West", vec![point(37.772, -122.412)]))]),
    ])]);
    if let Some(f) = tiles_for {
        b = b.tiles_for(f);
    }
    let fake = b.build();
    let client = kind.client(&fake);
    (fake, client)
}

fn both_tiles(overview: Option<bool>) -> TilesFor {
    TilesFor::Answer { tiles: vec![EAST.into(), WEST.into()], overview }
}

// describe("overview answers")
mod overview_answers {
    use super::*;

    both! {
        fn flags_an_overview_answer_whose_tiles_all_load_as_overview_and_complete(kind: Kind) {
            let (_, client) = setup(kind, Some(|_| both_tiles(Some(true))), &[]);
            let result = client.ok(REGION, overture_options());
            assert!(result.overview);
            assert!(result.complete);
        }
    }

    both! {
        fn does_not_flag_a_regular_answer_with_a_failed_tile(kind: Kind) {
            let (_, client) = setup(kind, None, &[WEST]);
            let result = client.ok(REGION, overture_options());
            assert!(!result.overview);
            assert!(!result.complete);
        }
    }

    both! {
        fn reads_an_answer_without_the_key_as_regular(kind: Kind) {
            let (_, client) = setup(kind, Some(|_| both_tiles(None)), &[]);
            assert!(!client.ok(REGION, overture_options()).overview);
        }
    }

    both! {
        fn keeps_the_flag_when_the_tile_list_cache_answers(kind: Kind) {
            let (fake, client) = setup(kind, Some(|_| both_tiles(Some(true))), &[]);
            client.ok(REGION, overture_options());
            let second = client.ok(REGION, overture_options());
            assert_eq!(fake.tile_list_requests().len(), 1);
            assert!(second.overview);
        }
    }

    both! {
        fn flags_a_split_antimeridian_search_when_only_one_half_is_overview(kind: Kind) {
            let (_, client) = setup(
                kind,
                Some(|bbox| {
                    if bbox.starts_with("170") {
                        TilesFor::Answer { tiles: vec![EAST.into()], overview: Some(true) }
                    } else {
                        TilesFor::Answer { tiles: vec![WEST.into()], overview: None }
                    }
                }),
                &[],
            );
            assert!(client.ok(Region::bbox(170.0, 10.0, -170.0, 20.0), overture_options()).overview);
        }
    }
}
