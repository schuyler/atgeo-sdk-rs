//! Concurrency and cancellation (spec §5, contract §4.4, §11, §14). These have no counterpart in the
//! reference's tests.

#[macro_use]
mod common;

use std::sync::Arc;
use std::time::Duration;

use atgeo::{AtgeoClient, Region, SearchOptions};
use common::*;
use futures_util::FutureExt;

fn twenty_tiles() -> FakeServer {
    let tiles = (0..20)
        .map(|i| {
            tile(&url(&format!("t{i:02}")), vec![rec(simple(&format!("p{i}"), "P", vec![point(37.775, -122.415)]))])
        })
        .collect();
    let fake = FakeServer::with_tilesets(vec![overture(tiles)]);
    fake.delay_all(Duration::from_millis(20));
    fake
}

both! {
    fn keeps_at_most_eight_tile_requests_in_flight(kind: Kind) {
        let fake = twenty_tiles();
        let result = kind.client(&fake).ok(REGION, SearchOptions::new());
        assert_eq!(result.tiles.fetched, 20);
        assert_eq!(fake.peak_in_flight(), 8);
        assert_eq!(fake.in_flight(), 0);
    }
}

both! {
    fn uses_as_many_workers_as_tiles_below_eight(kind: Kind) {
        for n in [1, 3, 7] {
            let tiles = (0..n).map(|i| tile(&url(&format!("t{i}")), vec![])).collect();
            let fake = FakeServer::with_tilesets(vec![overture(tiles)]);
            fake.delay_all(Duration::from_millis(20));
            kind.client(&fake).ok(REGION, SearchOptions::new());
            assert_eq!(fake.peak_in_flight(), n, "{n} tiles");
        }
    }
}

both! {
    fn a_failed_get_tiles_caches_nothing_so_a_retry_asks_again(kind: Kind) {
        let fake = FakeServer::builder().tiles_for(|_| TilesFor::Error("InvalidBbox", "no")).build();
        let client = kind.client(&fake);
        assert!(client.search(REGION, SearchOptions::new()).is_err());
        assert!(client.search(REGION, SearchOptions::new()).is_err());
        assert_eq!(fake.tile_list_requests().len(), 2);
    }
}

#[test]
fn the_async_client_starts_tile_requests_in_index_order() {
    let fake = twenty_tiles();
    runtime().block_on(AtgeoClient::with_transport(fake.clone()).search_places(REGION, SearchOptions::new())).unwrap();
    let tiles: Vec<String> = fake.requests().into_iter().filter(|r| !r.contains("/xrpc/")).collect();
    let mut sorted = tiles.clone();
    sorted.sort();
    assert_eq!(tiles, sorted);
}

#[test]
fn dropping_a_search_drops_every_request_in_flight_and_starts_no_more() {
    let fake = twenty_tiles();
    fake.delay_all(Duration::from_secs(5));
    let client = AtgeoClient::with_transport(fake.clone());
    let rt = runtime();
    let outcome = rt.block_on(async {
        tokio::time::timeout(Duration::from_millis(50), client.search_places(REGION, SearchOptions::new())).await
    });
    assert!(outcome.is_err(), "the search should still have been running");
    assert_eq!(fake.dropped(), 8);
    assert_eq!(fake.in_flight(), 0);
    assert_eq!(fake.request_count(), 1 + 8);
    // Nothing left behind starts later.
    rt.block_on(async { tokio::time::sleep(Duration::from_secs(10)).await });
    assert_eq!(fake.request_count(), 1 + 8);
}

#[test]
fn the_first_get_tiles_error_returns_at_once_and_drops_the_other_half() {
    let fake = FakeServer::builder()
        .tiles_for(
            |bbox| if bbox.starts_with("170") { TilesFor::Error("BboxTooLarge", "no") } else { TilesFor::Urls(vec![]) },
        )
        .build();
    let second_half = "https://places.atgeo.org/xrpc/org.atgeo.getTiles?dataset=overturemaps.org&collection=place&bbox=-180.00%2C10.00%2C-170.00%2C20.00";
    let first_half = "https://places.atgeo.org/xrpc/org.atgeo.getTiles?dataset=overturemaps.org&collection=place&bbox=170.00%2C10.00%2C180.00%2C20.00";
    // Both requests are in flight when the first fails.
    fake.delay(first_half, Duration::from_millis(10));
    fake.delay(second_half, Duration::from_secs(60));
    let client = AtgeoClient::with_transport(fake.clone());
    let rt = runtime();
    let started = rt.block_on(async { tokio::time::Instant::now() });
    let e =
        rt.block_on(client.search_places(Region::bbox(170.0, 10.0, -170.0, 20.0), SearchOptions::new())).unwrap_err();
    assert_eq!(code_of(&e), "BboxTooLarge");
    assert!(rt.block_on(async { tokio::time::Instant::now() }) - started < Duration::from_secs(1));
    assert_eq!(fake.dropped(), 1);
    assert_eq!(fake.requests().iter().filter(|r| r.as_str() == second_half).count(), 1);
}

#[test]
fn a_search_answered_entirely_from_cache_completes_on_its_first_poll() {
    let fake = twenty_tiles();
    let client = AtgeoClient::with_transport(fake.clone());
    let first = runtime().block_on(client.search_places(REGION, SearchOptions::new())).unwrap();
    let before = fake.request_count();
    let second = client.search_places(REGION, SearchOptions::new().text("P")).now_or_never();
    assert!(second.is_some(), "a fully cached search must not wait");
    assert_eq!(second.unwrap().unwrap().places.len(), first.places.len());
    assert_eq!(fake.request_count(), before);
    let place = client.get_place(&place_origin("p3")).now_or_never();
    assert_eq!(place.unwrap().unwrap().name(), Some("P"));
}

#[test]
fn an_invalid_region_fails_on_the_first_poll() {
    let fake = FakeServer::new();
    let client = AtgeoClient::with_transport(fake.clone());
    let r = client.search_places(Region::circle(91.0, 0.0, 1.0), SearchOptions::new()).now_or_never();
    assert!(matches!(r, Some(Err(atgeo::Error::InvalidArgument(_)))));
    assert_eq!(fake.request_count(), 0);
}

fn assert_send_sync<T: Send + Sync>() {}
fn assert_send<T: Send>(_: &T) {}

#[test]
fn the_client_is_send_and_sync_and_its_futures_are_send() {
    assert_send_sync::<AtgeoClient<FakeServer>>();
    #[cfg(feature = "reqwest")]
    assert_send_sync::<AtgeoClient>();
    #[cfg(feature = "blocking")]
    assert_send_sync::<atgeo::blocking::AtgeoClient<FakeServer>>();
    let client = AtgeoClient::with_transport(FakeServer::new());
    assert_send(&client.search_places(REGION, SearchOptions::new()));
    assert_send(&client.get_place("x"));
    assert_send(&client.describe_service());
    assert_send(&client.describe_tileset("x"));
}

#[test]
fn a_shared_client_runs_under_tokio_spawn() {
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_time().build().unwrap();
    let fake = twenty_tiles();
    let client = Arc::new(AtgeoClient::with_transport(fake.clone()));
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let client = client.clone();
            rt.spawn(async move { client.search_places(REGION, SearchOptions::new()).await.map(|r| r.tiles.fetched) })
        })
        .collect();
    for h in handles {
        assert_eq!(rt.block_on(h).unwrap().unwrap(), 20);
    }
}

#[test]
fn one_transport_can_serve_two_clients_through_a_reference() {
    let fake = twenty_tiles();
    let (a, b) = (AtgeoClient::with_transport(&fake), AtgeoClient::with_transport(Arc::new(fake.clone())));
    let rt = runtime();
    rt.block_on(a.search_places(REGION, SearchOptions::new())).unwrap();
    rt.block_on(b.search_places(REGION, SearchOptions::new())).unwrap();
    // Two clients share nothing: each fetched every tile.
    assert_eq!(fake.count_of(&url("t00")), 2);
}

#[cfg(feature = "blocking")]
mod blocking {
    use super::*;
    use atgeo::{BoxError, HttpResponse};

    struct PanicsOnTiles(FakeServer);

    impl atgeo::blocking::HttpTransport for PanicsOnTiles {
        fn get(&self, url: &str) -> Result<HttpResponse, BoxError> {
            if !url.contains("/xrpc/") {
                panic!("transport panicked on {url}");
            }
            atgeo::blocking::HttpTransport::get(&self.0, url)
        }
    }

    #[test]
    #[should_panic(expected = "transport panicked")]
    fn a_panic_in_a_worker_thread_reaches_the_caller() {
        let client = atgeo::blocking::AtgeoClient::with_transport(PanicsOnTiles(twenty_tiles()));
        let _ = client.search_places(REGION, SearchOptions::new());
    }

    #[test]
    fn the_blocking_client_waits_for_both_halves_and_returns_the_first_error_in_call_order() {
        let fake = FakeServer::builder()
            .tiles_for(|bbox| {
                if bbox.starts_with("170") {
                    TilesFor::Error("BboxTooLarge", "first")
                } else {
                    TilesFor::Error("InvalidBbox", "second")
                }
            })
            .build();
        let first_half = "https://places.atgeo.org/xrpc/org.atgeo.getTiles?dataset=overturemaps.org&collection=place&bbox=170.00%2C10.00%2C180.00%2C20.00";
        fake.delay(first_half, Duration::from_millis(100));
        let client = atgeo::blocking::AtgeoClient::with_transport(fake.clone());
        let e = client.search_places(Region::bbox(170.0, 10.0, -170.0, 20.0), SearchOptions::new()).unwrap_err();
        assert_eq!(code_of(&e), "BboxTooLarge");
        assert_eq!(fake.dropped(), 0);
        assert_eq!(fake.in_flight(), 0);
    }
}
