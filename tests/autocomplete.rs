//! A port of the reference's `test/autocomplete.test.ts`: the qualifying predicate's skips and the
//! autocompleter's supersession (contract §17, §19), the caller's cancellation (§11) and §12 item 8. The
//! autocompleter exists on the async client only, so these tests drive it directly.

mod common;

use std::future::Future;
use std::time::Duration;

use atgeo::{ArgumentError, AtgeoClient, Error, KeywordAutocomplete, KeywordOptions, KeywordRegion};
use common::*;

const SEARCH: &str =
    "https://places.atgeo.org/xrpc/org.atgeo.searchPlaces?dataset=overturemaps.org&collection=place&q=";

fn tiles() -> Vec<FakeTile> {
    vec![tile(
        "https://tiles.example/p.json",
        vec![record(simple("zc", "Zanzibar Cafe", vec![point(37.775, -122.418)]), Some(5), None)],
    )]
}

fn setup() -> (FakeServer, AtgeoClient<FakeServer>) {
    let fake = FakeServer::with_tilesets(vec![overture(tiles())]);
    let client = AtgeoClient::with_transport(fake.clone());
    (fake, client)
}

fn options() -> KeywordOptions {
    KeywordOptions::new("overturemaps.org").collection("place")
}

fn q_values(fake: &FakeServer) -> Vec<String> {
    fake.requests().iter().map(|r| param(r, "q").unwrap()).collect()
}

fn nothing(r: &atgeo::KeywordSearchResult) -> bool {
    r.places.is_empty() && r.attribution.is_empty() && (r.disclosed.keyword, r.disclosed.region) == (false, None)
}

/// Polls the future once on a paused runtime, long enough for it to send its request, and no longer.
async fn start<F: Future + Unpin>(call: &mut F) {
    tokio::select! {
        _ = call => panic!("the call finished"),
        _ = tokio::time::sleep(Duration::from_millis(10)) => {}
    }
}

// describe("autocompleteByKeyword")
mod autocomplete_by_keyword {
    use super::*;

    // A one-character prefix, a Hangul syllable (one character, though NFKD makes it two), and a consonant with
    // a spacing vowel sign (one character once the sign is stripped).
    #[test]
    fn makes_no_request_for_a_keyword_that_cannot_qualify_and_resolves_an_empty_result_that_disclosed_nothing() {
        for prefix in ["a", "하", "का", "", "  ", "a b'"] {
            let (fake, client) = setup();
            let ac = client.autocomplete_by_keyword(options()).unwrap();
            let result = runtime().block_on(ac.search_by_keyword(prefix)).unwrap();
            assert!(nothing(&result), "{prefix:?}");
            assert_eq!(fake.request_count(), 0, "{prefix:?}");
        }
    }

    #[test]
    fn sends_a_prefix_that_qualifies_as_typed_and_returns_the_keyword_hits() {
        let (fake, client) = setup();
        let ac = client.autocomplete_by_keyword(options()).unwrap();
        let result = runtime().block_on(ac.search_by_keyword("Zan")).unwrap();
        assert_eq!(fake.request_count(), 1);
        let mut got = params(&fake.requests()[0]);
        got.sort();
        assert_eq!(
            got,
            [("collection", "place"), ("dataset", "overturemaps.org"), ("q", "Zan")]
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
        );
        let origins: Vec<_> = result.places.iter().map(|h| h.origin.clone()).collect();
        assert_eq!(origins, [Some(place_origin("zc"))]);
        assert_eq!((result.disclosed.keyword, result.disclosed.region), (true, None));
    }

    // §12 item 8: it sends exactly what searchByKeyword does, for every string, prefixes included.
    #[test]
    fn sends_every_prefix_that_qualifies_exactly_as_typed_and_nothing_for_the_rest() {
        let (fake, client) = setup();
        let ac = client.autocomplete_by_keyword(options()).unwrap();
        let typed = "Zanzibár  quokka";
        let rt = runtime();
        let mut expected = Vec::new();
        for (i, _) in typed.char_indices().chain([(typed.len(), ' ')]).skip(1) {
            let prefix = &typed[..i];
            let before = fake.request_count();
            let _ = rt.block_on(ac.search_by_keyword(prefix));
            if prefix.chars().count() >= 2 {
                expected.push(prefix.to_owned());
                assert_eq!(fake.request_count(), before + 1, "{prefix:?}");
            } else {
                assert_eq!(fake.request_count(), before, "{prefix:?}");
            }
        }
        assert_eq!(q_values(&fake), expected);
        assert!(fake.requests().iter().all(|r| r.starts_with(SEARCH)));
    }

    #[test]
    fn sends_the_snapped_region_when_the_autocompleter_has_one() {
        let (fake, client) = setup();
        let ac = client
            .autocomplete_by_keyword(options().region(KeywordRegion::new(-122.43, 37.77, -122.41, 37.78)))
            .unwrap();
        let result = runtime().block_on(ac.search_by_keyword("Zan")).unwrap();
        assert_eq!(param(&fake.requests()[0], "bbox").as_deref(), Some("-123,37,-122,38"));
        assert_eq!((result.disclosed.keyword, result.disclosed.region), (true, Some(1.0)));
    }

    // The previous call is superseded when the next is made, so one that had not started is never sent.
    #[test]
    fn supersedes_a_previous_call_that_was_never_polled_and_never_sends_it() {
        let (fake, client) = setup();
        let ac = client.autocomplete_by_keyword(options()).unwrap();
        let first = ac.search_by_keyword("Za");
        let second = ac.search_by_keyword("Zan");
        // Polled in the order that would let the first send, if it were not already superseded.
        let (first, second) = runtime().block_on(async { (first.await, second.await) });
        assert!(matches!(first, Err(Error::Superseded)), "{first:?}");
        assert_eq!(second.unwrap().places.len(), 1);
        assert_eq!(q_values(&fake), ["Zan"]);
    }

    #[test]
    fn supersedes_a_previous_call_in_flight_dropping_its_request() {
        let (fake, client) = setup();
        fake.delay(&format!("{SEARCH}Za"), Duration::from_secs(5));
        let ac = client.autocomplete_by_keyword(options()).unwrap();
        runtime().block_on(async {
            let mut first = Box::pin(ac.search_by_keyword("Za"));
            start(&mut first).await;
            assert_eq!(fake.in_flight(), 1);
            let second = ac.search_by_keyword("Zan").await.unwrap();
            assert_eq!(second.places.len(), 1);
            let first = first.await;
            assert!(matches!(first, Err(Error::Superseded)), "{first:?}");
        });
        assert_eq!(q_values(&fake), ["Za", "Zan"]);
        assert_eq!((fake.in_flight(), fake.dropped()), (0, 1), "the superseded request was dropped");
    }

    #[test]
    fn a_call_that_finished_is_not_changed_by_a_later_call() {
        let (_fake, client) = setup();
        let ac = client.autocomplete_by_keyword(options()).unwrap();
        let rt = runtime();
        let first = rt.block_on(ac.search_by_keyword("Za"));
        rt.block_on(ac.search_by_keyword("Zan")).unwrap();
        assert!(first.is_ok());
    }

    #[test]
    fn a_keyword_that_cannot_qualify_still_supersedes_the_previous_call() {
        let (fake, client) = setup();
        fake.delay(&format!("{SEARCH}Zan"), Duration::from_secs(5));
        let ac = client.autocomplete_by_keyword(options()).unwrap();
        runtime().block_on(async {
            let mut first = Box::pin(ac.search_by_keyword("Zan"));
            start(&mut first).await;
            let skipped = ac.search_by_keyword("Z").await.unwrap();
            assert!(nothing(&skipped));
            assert!(matches!(first.await, Err(Error::Superseded)));
        });
    }

    #[test]
    fn a_call_that_resolved_without_a_request_is_not_superseded_later() {
        let (_fake, client) = setup();
        let ac = client.autocomplete_by_keyword(options()).unwrap();
        let skipped = ac.search_by_keyword("Z");
        let next = ac.search_by_keyword("Zan");
        let (skipped, next) = runtime().block_on(async { (skipped.await, next.await) });
        assert!(nothing(&skipped.unwrap()));
        assert!(next.is_ok());
    }
}

// describe("autocompleteByKeyword arguments")
mod arguments {
    use super::*;

    #[test]
    fn refuses_a_bad_region_at_construction_with_no_request() {
        let (fake, client) = setup();
        for region in [KeywordRegion::new(0.0, 10.0, 1.0, 5.0), KeywordRegion::new(f64::NAN, 0.0, 1.0, 1.0)] {
            let err = client.autocomplete_by_keyword(options().region(region)).unwrap_err();
            assert!(matches!(err, Error::InvalidArgument(ArgumentError::InvalidRegion(_))), "{err:?}");
        }
        assert_eq!(fake.request_count(), 0);
    }

    #[test]
    fn the_autocompleter_and_its_calls_can_cross_threads() {
        fn send<T: Send>(_: &T) {}
        fn sync<T: Sync>(_: &T) {}
        let (_fake, client) = setup();
        let ac: KeywordAutocomplete<'_, FakeServer> = client.autocomplete_by_keyword(options()).unwrap();
        sync(&ac);
        send(&ac);
        send(&ac.search_by_keyword("Zan"));
    }

    #[test]
    fn a_server_that_is_not_an_http_url_fails_the_call_before_any_request() {
        let fake = FakeServer::with_tilesets(vec![overture(tiles())]);
        let client = AtgeoClient::with_transport(fake.clone()).server("not a url");
        let ac = client.autocomplete_by_keyword(options()).unwrap();
        let err = runtime().block_on(ac.search_by_keyword("Zan")).unwrap_err();
        assert!(matches!(err, Error::InvalidArgument(ArgumentError::InvalidServer)), "{err:?}");
        assert_eq!(fake.request_count(), 0);
    }
}

// Contract §19 (#56): a keyword the SDK judged qualifying but the server refuses with KeywordRequired resolves
// empty, as a keyword the SDK skips does, except that the keyword did go out.
mod when_the_server_refuses_a_keyword_the_sdk_judged_qualifying {
    use super::*;

    #[test]
    fn resolves_empty_for_a_ligature_disclosing_the_keyword_it_sent() {
        let (fake, client) = setup();
        let ac = client.autocomplete_by_keyword(options()).unwrap();
        let result = runtime().block_on(ac.search_by_keyword("ﬁ")).unwrap();
        assert_eq!(fake.request_count(), 1);
        assert!(result.places.is_empty() && result.attribution.is_empty());
        assert_eq!((result.disclosed.keyword, result.disclosed.region), (true, None));
    }

    #[test]
    fn reports_the_region_it_sent_with_the_refused_keyword() {
        let (_fake, client) = setup();
        let ac = client
            .autocomplete_by_keyword(options().region(KeywordRegion::new(-122.42, 37.77, -122.41, 37.78)))
            .unwrap();
        let result = runtime().block_on(ac.search_by_keyword("ﬁ")).unwrap();
        assert_eq!((result.disclosed.keyword, result.disclosed.region), (true, Some(1.0)));
    }

    #[test]
    fn any_other_refusal_is_an_error() {
        let (fake, client) = setup();
        fake.raw_method("org.atgeo.searchPlaces", 400, r#"{"error":"InvalidRequest","message":"x"}"#);
        let ac = client.autocomplete_by_keyword(options()).unwrap();
        let err = runtime().block_on(ac.search_by_keyword("Zan")).unwrap_err();
        assert_eq!(code_of(&err), "InvalidRequest");
    }
}

// #60: rules of section 19 and section 11.
mod cancellation {
    use super::*;

    #[test]
    fn dropping_a_call_cancels_its_request_and_the_caller_gets_no_answer() {
        let (fake, client) = setup();
        fake.delay(&format!("{SEARCH}Zan"), Duration::from_secs(5));
        let ac = client.autocomplete_by_keyword(options()).unwrap();
        runtime().block_on(async {
            let mut call = Box::pin(ac.search_by_keyword("Zan"));
            start(&mut call).await;
            drop(call);
            tokio::time::sleep(Duration::from_secs(60)).await;
        });
        assert_eq!((fake.request_count(), fake.in_flight(), fake.dropped()), (1, 0, 1));
        // The next call is not affected by the dropped one.
        let result = runtime().block_on(ac.search_by_keyword("Zan"));
        assert!(result.is_ok());
    }

    #[test]
    fn does_not_supersede_a_call_on_another_autocompleter_of_the_same_client() {
        let (fake, client) = setup();
        fake.delay(&format!("{SEARCH}Za"), Duration::from_millis(200));
        let (one, other) =
            (client.autocomplete_by_keyword(options()).unwrap(), client.autocomplete_by_keyword(options()).unwrap());
        runtime().block_on(async {
            // The first call is in flight when the second, on the other autocompleter, is made and finishes.
            let mut first = Box::pin(one.search_by_keyword("Za"));
            start(&mut first).await;
            assert_eq!(fake.in_flight(), 1);
            assert!(other.search_by_keyword("Zan").await.is_ok());
            let first = first.await.unwrap();
            assert!(first.disclosed.keyword);
        });
        assert_eq!(fake.dropped(), 0);
    }

    #[test]
    fn a_superseded_call_ignores_a_later_drop_and_a_later_call_is_unaffected() {
        let (fake, client) = setup();
        let ac = client.autocomplete_by_keyword(options()).unwrap();
        let first = ac.search_by_keyword("Za");
        let second = runtime().block_on(ac.search_by_keyword("Zan"));
        assert!(second.is_ok());
        drop(first);
        assert_eq!(q_values(&fake), ["Zan"]);
    }
}
