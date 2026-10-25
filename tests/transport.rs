//! A port of the reference's `test/transport.test.ts`: failed tiles, typed errors and XRPC URLs. The real
//! HTTP transports are tested in `transports.rs`.

#[macro_use]
mod common;

use std::time::Duration;

use atgeo::{Error, Region, SearchOptions, SearchResult, TileFailureKind};
use common::*;

fn two_tiles(fail_tiles: &[&str]) -> FakeServer {
    FakeServer::builder()
        .fail_tiles(fail_tiles)
        .tilesets(vec![overture(vec![
            tile(&url("a"), vec![rec(simple("a", "A", vec![point(37.772, -122.412)]))]),
            tile(&url("b"), vec![rec(simple("b", "B", vec![point(37.774, -122.414)]))]),
        ])])
        .build()
}

/// Each failed tile's URL, kind and status, in the order the result gives them.
fn failures(result: &SearchResult) -> Vec<(String, TileFailureKind, Option<u16>)> {
    result.tile_failures.iter().map(|f| (f.url.clone(), f.kind, f.status)).collect()
}

/// The error a failed tile carries; every failure a search reports has one.
fn error_of(result: &SearchResult, i: usize) -> &Error {
    result.tile_failures[i].error.as_deref().expect("a reported failure carries its error")
}

// describe("failed tiles")
mod failed_tiles {
    use super::*;

    both! {
        fn reports_a_complete_search(kind: Kind) {
            let result = kind.client(&two_tiles(&[])).ok(REGION, overture_options());
            assert!(result.complete);
            assert_eq!((result.tiles.fetched, result.tiles.failed), (2, 0));
            assert!(result.tile_failures.is_empty());
        }
    }

    both! {
        fn returns_surviving_results_marked_incomplete_when_a_tile_answers_non_200(kind: Kind) {
            let result = kind.client(&two_tiles(&[&url("b")])).ok(REGION, overture_options());
            assert!(!result.complete);
            assert_eq!((result.tiles.fetched, result.tiles.failed), (1, 1));
            assert_eq!(origins(&result), [Some(place_origin("a"))]);
            assert_eq!(failures(&result), [(url("b"), TileFailureKind::Status, Some(500))]);
            match error_of(&result, 0) {
                Error::Atgeo(e) => assert_eq!((e.status, e.code.as_str()), (500, "HttpError")),
                other => panic!("{other:?}"),
            }
        }
    }

    both! {
        fn returns_an_incomplete_result_when_a_tile_fetch_rejects(kind: Kind) {
            let fake = two_tiles(&[]);
            fake.network_error(&url("a"));
            let result = kind.client(&fake).ok(REGION, overture_options());
            assert!(!result.complete);
            assert_eq!((result.tiles.fetched, result.tiles.failed), (1, 1));
            assert_eq!(origins(&result), [Some(place_origin("b"))]);
            assert_eq!(failures(&result), [(url("a"), TileFailureKind::Transport, None)]);
            // The transport's own error, unwrapped from nothing but the crate's Error::Transport.
            match error_of(&result, 0) {
                Error::Transport(e) => {
                    let io = e.downcast_ref::<std::io::Error>().expect("the fake's io::Error");
                    assert_eq!(io.to_string(), "network disabled");
                }
                other => panic!("{other:?}"),
            }
        }
    }

    both! {
        fn does_not_retry_a_failed_tile(kind: Kind) {
            let fake = two_tiles(&[&url("b")]);
            kind.client(&fake).ok(REGION, overture_options());
            assert_eq!(fake.count_of(&url("b")), 1);
        }
    }

    both! {
        fn reports_every_tile_failed_without_throwing(kind: Kind) {
            let result = kind.client(&two_tiles(&[&url("a"), &url("b")])).ok(REGION, overture_options());
            assert!(result.places.is_empty());
            assert!(!result.complete);
            assert_eq!((result.tiles.fetched, result.tiles.failed), (0, 2));
            assert_eq!(result.tile_failures.len(), 2);
        }
    }

    both! {
        fn reports_failures_in_url_order_not_completion_order(kind: Kind) {
            let here = |id: &str| vec![rec(simple(id, id, vec![point(37.772, -122.412)]))];
            let fake = FakeServer::builder()
                .fail_tiles(&[&url("e")])
                .tilesets(vec![overture(vec![
                    tile(&url("a"), here("a")),
                    tile(&url("c"), here("c")),
                    tile(&url("d"), here("d")),
                    tile(&url("e"), here("e")),
                ])])
                .tiles_for(|_| {
                    let mut urls: Vec<String> = ["e", "d", "c", "a"].iter().map(|n| url(n)).collect();
                    urls.push("ftp://tiles.example/b.json".into());
                    TilesFor::Urls(urls)
                })
                .build();
            fake.network_error(&url("c"));
            fake.raw(&url("d"), 200, "not json");
            // The decode failure finishes after the status failure that follows it in URL order.
            fake.delay(&url("d"), Duration::from_millis(30));
            fake.delay(&url("a"), Duration::from_millis(20));
            fake.delay(&url("e"), Duration::from_millis(10));
            let result = kind.client(&fake).ok(REGION, overture_options());
            assert_eq!(
                failures(&result),
                [
                    ("ftp://tiles.example/b.json".to_owned(), TileFailureKind::Refused, None),
                    (url("c"), TileFailureKind::Transport, None),
                    (url("d"), TileFailureKind::Decode, None),
                    (url("e"), TileFailureKind::Status, Some(500)),
                ]
            );
            assert_eq!((result.tiles.fetched, result.tiles.failed, result.complete), (1, 4, false));
            assert_eq!(origins(&result), [Some(place_origin("a"))]);
        }
    }

    both! {
        fn a_cached_tile_is_not_reported_and_a_failure_is_reported_again(kind: Kind) {
            let fake = two_tiles(&[&url("b")]);
            let client = kind.client(&fake);
            let first = client.ok(REGION, overture_options());
            let second = client.ok(REGION, overture_options());
            assert_eq!(fake.count_of(&url("a")), 1, "the good tile came from the cache");
            assert_eq!(first.tile_failures, second.tile_failures);
            assert_eq!(failures(&second), [(url("b"), TileFailureKind::Status, Some(500))]);
        }
    }
}

// describe("typed server errors")
mod typed_server_errors {
    use super::*;

    both! {
        fn surfaces_bbox_too_large_from_get_tiles(kind: Kind) {
            let fake = FakeServer::builder().tiles_for(|_| TilesFor::Error("BboxTooLarge", "bbox too large")).build();
            match kind.client(&fake).search(REGION, overture_options()).unwrap_err() {
                Error::Atgeo(e) => {
                    assert_eq!((e.code.as_str(), e.status), ("BboxTooLarge", 400));
                    assert_eq!(e.message, "bbox too large");
                }
                other => panic!("{other:?}"),
            }
        }
    }

    both! {
        fn carries_the_http_status_whatever_the_body_says(kind: Kind) {
            for (status, body, code) in [
                (404, r#"{"error":"PlaceNotFound"}"#, "PlaceNotFound"),
                (429, "", "HttpError"),
                (503, r#"{"error":"InvalidRequest","message":"a 5xx with a 4xx-sounding code"}"#, "InvalidRequest"),
            ] {
                let fake = FakeServer::new();
                fake.answer_everything(status, body);
                match kind.client(&fake).get_place("urn:atgeo:overturemaps.org:place:x").unwrap_err() {
                    Error::Atgeo(e) => assert_eq!((e.status, e.code.as_str()), (status, code)),
                    other => panic!("{other:?}"),
                }
            }
        }
    }

    both! {
        fn surfaces_collection_not_found_from_get_tiles(kind: Kind) {
            let e = kind.client(&FakeServer::new()).search(REGION, SearchOptions::new("overturemaps.org").collection("nope")).unwrap_err();
            assert_eq!(code_of(&e), "CollectionNotFound");
        }
    }

    both! {
        fn surfaces_dataset_not_found_from_get_tiles(kind: Kind) {
            let e = kind.client(&FakeServer::new()).search(REGION, SearchOptions::new("example.org")).unwrap_err();
            assert_eq!(code_of(&e), "DatasetNotFound");
        }
    }

    both! {
        fn throws_when_either_antimeridian_half_is_bbox_too_large(kind: Kind) {
            for prefix in ["170", "-180"] {
                let a = url("a");
                let fake = FakeServer::builder()
                    .tilesets(vec![overture(vec![tile(&a, vec![rec(simple("a", "A", vec![point(15.0, 175.0)]))])])])
                    .tiles_for(move |bbox| {
                        if bbox.starts_with(prefix) { TilesFor::Error("BboxTooLarge", "too large") } else { TilesFor::Urls(vec![a.clone()]) }
                    })
                    .build();
                let e = kind.client(&fake).search(Region::bbox(170.0, 10.0, -170.0, 20.0), overture_options()).unwrap_err();
                assert_eq!(code_of(&e), "BboxTooLarge", "{prefix}");
            }
        }
    }

    both! {
        fn does_not_subdivide_after_bbox_too_large(kind: Kind) {
            let fake = FakeServer::builder().tiles_for(|_| TilesFor::Error("BboxTooLarge", "too large")).build();
            let _ = kind.client(&fake).search(REGION, overture_options());
            assert_eq!(fake.request_count(), 1);
        }
    }

    both! {
        fn surfaces_a_network_failure_as_a_transport_error(kind: Kind) {
            let fake = FakeServer::new();
            fake.go_offline();
            let e = kind.client(&fake).search(REGION, overture_options()).unwrap_err();
            let Error::Transport(inner) = e else { panic!("{e:?}") };
            assert_eq!(inner.downcast_ref::<std::io::Error>().unwrap().to_string(), "network disabled");
        }
    }
}

// describe("XRPC URLs")
mod xrpc_urls {
    use super::*;

    both! {
        fn defaults_to_places_atgeo_org(kind: Kind) {
            let fake = two_tiles(&[]);
            kind.client(&fake).ok(REGION, overture_options());
            let first = &fake.requests()[0];
            assert_eq!(path_of(first), format!("{DEFAULT_SERVER}/xrpc/org.atgeo.getTiles"));
            assert_eq!(param(first, "dataset").as_deref(), Some("overturemaps.org"));
            assert_eq!(param(first, "collection").as_deref(), Some("place"));
            assert_eq!(param(first, "bbox").as_deref(), Some("-122.42,37.77,-122.41,37.78"));
        }
    }

    both! {
        fn targets_a_custom_server_for_every_xrpc_call(kind: Kind) {
            let base = "http://localhost:8000";
            let fake = FakeServer::builder()
                .server(base)
                .tilesets(vec![overture(vec![tile(&format!("{base}/tiles/a.json"), vec![rec(simple("a", "A", vec![point(37.772, -122.412)]))])])])
                .build();
            let client = kind.with_server(&fake, base);
            client.ok(REGION, overture_options());
            client.describe_service().unwrap();
            client.describe_tileset("overture-place").unwrap();
            let _ = client.get_place(&place_origin("zzz"));
            let xrpc: Vec<String> =
                fake.requests().iter().filter(|r| r.contains("/xrpc/")).map(|r| path_of(r).to_owned()).collect();
            assert_eq!(xrpc, [
                format!("{base}/xrpc/org.atgeo.getTiles"),
                format!("{base}/xrpc/org.atgeo.describeService"),
                format!("{base}/xrpc/org.atgeo.describeTileset"),
                format!("{base}/xrpc/org.atgeo.getPlace"),
            ]);
            assert!(fake.requests().contains(&format!("{base}/tiles/a.json")));
        }
    }

    both! {
        fn percent_encodes_query_values_so_the_server_decodes_the_exact_strings(kind: Kind) {
            let fake = FakeServer::new();
            let client = kind.client(&fake);
            let _ = client.get_place("urn:x:a+b c&d=e#f%g");
            let _ = client.describe_tileset("a+b c");
            let requests = fake.requests();
            assert!(requests[0].ends_with("?uri=urn%3Ax%3Aa%2Bb%20c%26d%3De%23f%25g"), "{}", requests[0]);
            assert_eq!(param(&requests[0], "uri").as_deref(), Some("urn:x:a+b c&d=e#f%g"));
            assert_eq!(param(&requests[1], "slug").as_deref(), Some("a+b c"));
        }
    }
}
