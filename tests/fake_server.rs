//! A port of the reference's `test/fakeServer.test.ts`: the fake checks its own configuration and traffic.

mod common;

use atgeo::HttpTransport;
use common::*;

const TILE: &str = "https://tiles.example/a.json";

fn ok() -> Value {
    simple("a", "Alpha", vec![point(37.772, -122.412)])
}

fn build(records: Vec<Value>, slug: Option<&str>) -> Result<FakeServer, String> {
    let mut t = overture(vec![tile(TILE, records)]);
    t.slug = slug.map(str::to_owned);
    FakeServer::builder().tilesets(vec![t]).try_build()
}

fn get(fake: &FakeServer, url: &str) -> atgeo::HttpResponse {
    runtime().block_on(HttpTransport::get(fake, url)).unwrap()
}

fn json_of(r: &atgeo::HttpResponse) -> Value {
    serde_json::from_slice(&r.body).unwrap()
}

// describe("fake server configuration")
mod configuration {
    use super::*;

    #[test]
    fn accepts_a_valid_configuration() {
        assert!(build(vec![record(ok(), Some(3), None)], Some("overture-place")).is_ok());
    }

    #[test]
    fn rejects_a_record_value_missing_its_name() {
        let mut nameless = ok();
        nameless.as_object_mut().unwrap().remove("name");
        assert!(build(vec![rec(nameless)], Some("s")).unwrap_err().contains("name"));
    }

    #[test]
    fn rejects_a_tile_record_missing_its_value() {
        assert!(build(vec![json!({ "importance": 3 })], Some("s")).unwrap_err().contains("\"value\""));
    }

    #[test]
    fn rejects_a_float_importance() {
        let r = json!({ "value": ok(), "importance": 0.5 });
        assert!(build(vec![r], Some("s")).unwrap_err().contains("integer"));
    }

    #[test]
    fn rejects_a_descriptor_missing_its_slug() {
        assert!(build(vec![], None).unwrap_err().contains("slug"));
    }
}

// describe("fake server requests")
mod requests {
    use super::*;

    #[test]
    fn answers_400_invalid_request_to_get_tiles_without_a_dataset() {
        let fake = build(vec![rec(ok())], Some("s")).unwrap();
        let r = get(&fake, "https://places.atgeo.org/xrpc/org.atgeo.getTiles?bbox=-122.42,37.77,-122.41,37.78");
        assert_eq!(r.status, 400);
        assert_eq!(json_of(&r)["error"], "InvalidRequest");
    }

    #[test]
    fn answers_400_invalid_request_to_get_place_with_a_non_uri_uri() {
        let fake = build(vec![rec(ok())], Some("s")).unwrap();
        let r = get(&fake, "https://places.atgeo.org/xrpc/org.atgeo.getPlace?uri=not%20a%20uri");
        assert_eq!(r.status, 400);
    }

    #[test]
    fn answers_400_to_an_unknown_parameter_and_a_bbox_off_the_grid() {
        let fake = FakeServer::new();
        let r =
            get(&fake, "https://places.atgeo.org/xrpc/org.atgeo.getTiles?dataset=d&bbox=0.00,0.00,0.01,0.01&text=x");
        assert_eq!((r.status, json_of(&r)["error"].as_str()), (400, Some("InvalidRequest")));
        let r = get(&fake, "https://places.atgeo.org/xrpc/org.atgeo.getTiles?dataset=d&bbox=0.001,0.00,0.01,0.01");
        assert_eq!((r.status, json_of(&r)["error"].as_str()), (400, Some("InvalidRequest")));
        assert!(bbox_pattern("-122.42,37.77,-122.41,37.78"));
        assert!(!bbox_pattern("-122.42,37.77,-122.41"));
        assert!(!bbox_pattern("1e2.00,0.00,0.00,0.00"));
    }

    #[test]
    fn serves_tile_bodies_with_published_at_attribution_and_records() {
        let fake = build(vec![record(ok(), Some(3), None)], Some("overture-place")).unwrap();
        let body = json_of(&get(&fake, TILE));
        let mut keys: Vec<&String> = body.as_object().unwrap().keys().collect();
        keys.sort();
        assert_eq!(keys, ["attribution", "publishedAt", "records"]);
        assert_eq!(body["records"], json!([{ "value": ok(), "importance": 3 }]));
    }

    #[test]
    fn answers_describe_service_with_a_did_and_tilesets() {
        let body = json_of(&get(
            &build(vec![], Some("overture-place")).unwrap(),
            "https://places.atgeo.org/xrpc/org.atgeo.describeService",
        ));
        assert_eq!(
            body,
            json!({
                "did": "did:web:places.atgeo.org",
                "tilesets": [{ "slug": "overture-place", "collections": [{ "dataset": "overturemaps.org", "collection": "place" }] }],
            })
        );
    }
}
