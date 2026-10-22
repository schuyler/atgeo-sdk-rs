//! Body decoding and the lenient readers (contract §1, §16). Every response is read by hand from a
//! `serde_json::Value`: a field of the wrong type is absent, never an error.

use std::sync::Arc;

use serde_json::{Map, Value};

use crate::cache::TileRecord;
use crate::error::{AtgeoError, DecodingError};
use crate::types::{
    CategoryCount, CollectionAttribution, CollectionInfo, CollectionRef, Extent, ServedTileset, ServiceDescription,
    Tileset,
};

/// Decodes a body as `JSON.parse(await response.text())` would: invalid UTF-8 becomes U+FFFD and a leading
/// byte-order mark is dropped. A body that fails to parse is retried once with every lone-surrogate escape
/// rewritten to U+FFFD, since a Rust `String` cannot hold one.
pub(crate) fn parse_body(body: &[u8]) -> Result<Value, serde_json::Error> {
    let text = String::from_utf8_lossy(body);
    let text = text.strip_prefix('\u{FEFF}').unwrap_or(&text);
    serde_json::from_str(text).or_else(|e| match repair_lone_surrogates(text) {
        Some(repaired) => serde_json::from_str(&repaired),
        None => Err(e),
    })
}

/// Rewrites every `\u` escape of a lone surrogate to `�`; `None` when there is none.
fn repair_lone_surrogates(text: &str) -> Option<String> {
    let b = text.as_bytes();
    let hex = |i: usize| text.get(i..i + 4).and_then(|h| u16::from_str_radix(h, 16).ok());
    let (mut out, mut i, mut last, mut changed) = (String::with_capacity(text.len()), 0, 0, false);
    while i < b.len() {
        if b[i] != b'\\' {
            i += 1;
            continue;
        }
        if b.get(i + 1) != Some(&b'u') {
            i += 2; // \\, \", \n …: skip the pair
            continue;
        }
        let Some(u) = hex(i + 2) else {
            i += 2;
            continue;
        };
        let pair = (0xD800..0xDC00).contains(&u)
            && b.get(i + 6) == Some(&b'\\')
            && b.get(i + 7) == Some(&b'u')
            && hex(i + 8).is_some_and(|l| (0xDC00..0xE000).contains(&l));
        if pair {
            i += 12;
            continue;
        }
        if (0xD800..0xE000).contains(&u) {
            out.push_str(&text[last..i]);
            out.push_str("\\uFFFD");
            last = i + 6;
            changed = true;
        }
        i += 6;
    }
    changed.then(|| {
        out.push_str(&text[last..]);
        out
    })
}

/// A JSON number through a double, truncated toward zero, absent at 2^63 or beyond in magnitude.
pub(crate) fn importance_of(v: Option<&Value>) -> Option<i64> {
    let d = v?.as_f64()?; // None for anything but a JSON number
    // The guard comes first because `as` saturates where the contract wants absent.
    (d.is_finite() && d.abs() < 9_223_372_036_854_775_808.0).then_some(d as i64)
}

/// JavaScript truthiness.
pub(crate) fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_none_or(|d| d != 0.0 && !d.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true, // any array or object, empty or not
    }
}

/// A non-2xx answer as an `AtgeoError`: `error` and `message` are read only when they are strings.
pub(crate) fn error_from(status: u16, body: &[u8]) -> AtgeoError {
    let parsed = parse_body(body).ok();
    let obj = parsed.as_ref().and_then(Value::as_object);
    let field = |k: &str| obj.and_then(|o| o.get(k)).and_then(Value::as_str).map(str::to_owned);
    AtgeoError::new(
        field("error").unwrap_or_else(|| "HttpError".to_owned()),
        field("message").unwrap_or_else(|| format!("HTTP {status}")),
        status,
    )
}

/// A tile list: the string entries of `tiles` and the truthiness of `overview`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TileList {
    pub(crate) tiles: Vec<String>,
    pub(crate) overview: bool,
}

pub(crate) fn tile_list(body: &[u8]) -> Result<TileList, DecodingError> {
    let value = parse_body(body).map_err(DecodingError::json)?;
    let Some(Value::Array(tiles)) = value.as_object().and_then(|o| o.get("tiles")) else {
        return Err(DecodingError::shape("getTiles response has no tiles array"));
    };
    Ok(TileList {
        tiles: tiles.iter().filter_map(|t| t.as_str().map(str::to_owned)).collect(),
        overview: truthy(value.get("overview")),
    })
}

/// A tile's usable records; `None` for a body that is not a JSON object with a `records` array.
pub(crate) fn tile_records(body: &[u8]) -> Option<Vec<TileRecord>> {
    let Ok(Value::Object(mut payload)) = parse_body(body) else { return None };
    let Some(Value::Array(records)) = payload.remove("records") else { return None };
    Some(
        records
            .into_iter()
            .filter_map(|r| {
                let Value::Object(mut r) = r else { return None };
                let Some(Value::Object(value)) = r.remove("value") else { return None };
                let uri = match r.get("uri") {
                    Some(Value::String(s)) => Some(s.clone()),
                    _ => None,
                };
                Some(TileRecord { value: Arc::new(value), uri, importance: importance_of(r.get("importance")) })
            })
            .collect(),
    )
}

/// A getPlace body: its `record` object and `importance`.
pub(crate) fn place_view(body: &[u8]) -> Result<(Map<String, Value>, Option<i64>), DecodingError> {
    let value = parse_body(body).map_err(DecodingError::json)?;
    let Value::Object(mut view) = value else {
        return Err(DecodingError::shape("getPlace response is not an object"));
    };
    let importance = importance_of(view.get("importance"));
    match view.remove("record") {
        Some(Value::Object(record)) => Ok((record, importance)),
        _ => Err(DecodingError::shape("getPlace response has no record object")),
    }
}

fn describe_body(body: &[u8], what: &str) -> Result<Map<String, Value>, DecodingError> {
    match parse_body(body).map_err(DecodingError::json)? {
        Value::Object(o) => Ok(o),
        _ => Err(DecodingError::shape(&format!("{what} response is not an object"))),
    }
}

fn string(o: &Map<String, Value>, k: &str) -> Option<String> {
    o.get(k).and_then(Value::as_str).map(str::to_owned)
}

fn strings(o: &Map<String, Value>, k: &str) -> Option<Vec<String>> {
    match o.get(k) {
        Some(Value::Array(a)) => Some(a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()),
        _ => None,
    }
}

/// The object elements of the list at `k`, read by `f`; `None` when it is missing or not an array.
fn objects<T>(o: &Map<String, Value>, k: &str, f: impl Fn(&Map<String, Value>) -> T) -> Option<Vec<T>> {
    match o.get(k) {
        Some(Value::Array(a)) => Some(a.iter().filter_map(Value::as_object).map(f).collect()),
        _ => None,
    }
}

fn collection_ref(o: &Map<String, Value>) -> CollectionRef {
    CollectionRef { dataset: string(o, "dataset"), collection: string(o, "collection") }
}

fn attribution(o: &Map<String, Value>) -> CollectionAttribution {
    CollectionAttribution {
        dataset: string(o, "dataset"),
        collection: string(o, "collection"),
        release: string(o, "release"),
        source: string(o, "source"),
        attribution: string(o, "attribution"),
        licenses: strings(o, "licenses"),
    }
}

fn collection_info(o: &Map<String, Value>) -> CollectionInfo {
    let a = attribution(o);
    CollectionInfo {
        dataset: a.dataset,
        collection: a.collection,
        release: a.release,
        source: a.source,
        attribution: a.attribution,
        licenses: a.licenses,
        record_count: importance_of(o.get("recordCount")),
        location_formats: strings(o, "locationFormats"),
        containment_types: strings(o, "containmentTypes"),
        attributes: strings(o, "attributes"),
        categories: objects(o, "categories", |c| CategoryCount {
            value: string(c, "value"),
            count: importance_of(c.get("count")),
        }),
    }
}

pub(crate) fn service_description(body: &[u8]) -> Result<ServiceDescription, DecodingError> {
    let o = describe_body(body, "describeService")?;
    Ok(ServiceDescription {
        did: string(&o, "did"),
        tilesets: objects(&o, "tilesets", |t| ServedTileset {
            slug: string(t, "slug"),
            collections: objects(t, "collections", collection_ref).unwrap_or_default(),
        })
        .unwrap_or_default(),
    })
}

pub(crate) fn tileset(body: &[u8]) -> Result<Tileset, DecodingError> {
    let o = describe_body(body, "describeTileset")?;
    Ok(Tileset {
        slug: string(&o, "slug"),
        published_at: string(&o, "publishedAt"),
        extent: o.get("extent").and_then(Value::as_object).map(|e| Extent {
            north: string(e, "north"),
            south: string(e, "south"),
            east: string(e, "east"),
            west: string(e, "west"),
        }),
        collections: objects(&o, "collections", collection_info).unwrap_or_default(),
        references: objects(&o, "references", attribution),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn importance_follows_the_contract() {
        let i = |v: Value| importance_of(Some(&v));
        assert_eq!(i(json!(1.9)), Some(1));
        assert_eq!(i(json!(-1.9)), Some(-1));
        assert_eq!(i(json!(true)), None);
        assert_eq!(i(json!("7")), None);
        assert_eq!(i(json!([7])), None);
        assert_eq!(i(json!(null)), None);
        assert_eq!(i(json!(9007199254740993u64)), Some(9007199254740992));
        assert_eq!(i(json!(9223372036854775807i64)), None);
        assert_eq!(i(json!(-9223372036854775808i64)), None);
        assert_eq!(i(json!(9223372036854774784i64)), Some(9223372036854774784));
        assert_eq!(i(serde_json::from_str("18446744073709551616").unwrap()), None);
        assert_eq!(i(json!(1e300)), None);
        assert_eq!(importance_of(None), None);
    }

    #[test]
    fn truthiness_is_javascripts() {
        let t = |v: Value| truthy(Some(&v));
        for v in [json!(null), json!(false), json!(0), json!(-0.0), json!(0.0), json!("")] {
            assert!(!t(v.clone()), "{v}");
        }
        for v in [json!(true), json!(1), json!(-1), json!("false"), json!("0"), json!([]), json!({}), json!(0.5)] {
            assert!(t(v.clone()), "{v}");
        }
        assert!(!truthy(None));
    }

    #[test]
    fn reads_error_bodies_field_by_field() {
        let e = error_from(400, br#"{"error":"BboxTooLarge","message":"too big"}"#);
        assert_eq!((e.code.as_str(), e.message.as_str(), e.status), ("BboxTooLarge", "too big", 400));
        let e = error_from(404, br#"{"error":"PlaceNotFound"}"#);
        assert_eq!((e.code.as_str(), e.message.as_str()), ("PlaceNotFound", "HTTP 404"));
        let e = error_from(500, br#"{"error":7,"message":["x"]}"#);
        assert_eq!((e.code.as_str(), e.message.as_str()), ("HttpError", "HTTP 500"));
        for body in [&b""[..], b"null", b"[1]", b"\"x\"", b"<html>"] {
            let e = error_from(429, body);
            assert_eq!((e.code.as_str(), e.message.as_str(), e.status), ("HttpError", "HTTP 429", 429));
        }
        assert_eq!(error_from(429, b"").to_string(), "HTTP 429 (HttpError)");
    }

    #[test]
    fn decodes_bodies_leniently() {
        assert_eq!(parse_body(b"\xEF\xBB\xBF{\"a\":1}").unwrap(), json!({"a": 1}));
        assert_eq!(parse_body(b"{\"a\":\"\xFF\"}").unwrap(), json!({"a": "\u{FFFD}"}));
        assert_eq!(parse_body(br#"{"a":1,"a":2}"#).unwrap(), json!({"a": 2}));
        assert_eq!(parse_body(b"1e-400").unwrap().as_f64(), Some(0.0));
        assert!(parse_body(b"-0").unwrap().as_f64().unwrap().is_sign_negative());
        assert_eq!(parse_body(b"18446744073709551617").unwrap().as_f64(), Some(18446744073709551616.0));
        // The margins CONFORMANCE.md lists.
        assert!(parse_body(b"1e400").is_err());
        assert!(parse_body(&[b'['; 200]).is_err());
    }

    #[test]
    fn repairs_lone_surrogates() {
        let p = |s: &str| parse_body(s.as_bytes()).unwrap();
        assert_eq!(p(r#""\ud800""#), json!("\u{FFFD}"));
        assert_eq!(p(r#""\udc00x""#), json!("\u{FFFD}x"));
        assert_eq!(p(r#""\udc00\ud800""#), json!("\u{FFFD}\u{FFFD}"));
        assert_eq!(p(r#""\ud800😀""#), json!("\u{FFFD}😀"));
        assert_eq!(p(r#"{"\ud800":1}"#), json!({"\u{FFFD}": 1}));
        assert_eq!(p(r#""\\\ud800""#), json!("\\\u{FFFD}"));
        assert_eq!(p(r#""😀""#), json!("😀"));
        assert_eq!(p(r#""\\ud800""#), json!("\\ud800"));
        assert_eq!(repair_lone_surrogates(r#""\\ud800""#), None);
        assert_eq!(repair_lone_surrogates(r#""😀""#), None);
    }

    #[test]
    fn reads_a_tile_list() {
        let l = tile_list(br#"{"tiles":["b",1,null,"a"],"overview":1}"#).unwrap();
        assert_eq!(l, TileList { tiles: vec!["b".into(), "a".into()], overview: true });
        assert!(!tile_list(br#"{"tiles":[]}"#).unwrap().overview);
        for body in [&b"{}"[..], b"[]", br#"{"tiles":"x"}"#, b"null", b"not json"] {
            assert!(tile_list(body).is_err(), "{}", String::from_utf8_lossy(body));
        }
    }

    #[test]
    fn reads_tile_records_skipping_unusable_ones() {
        let body = br#"{"records":[
            {"value":{"name":"a"},"uri":"u","importance":3},
            {"value":{"name":"b"},"uri":7,"importance":"3"},
            null, 3, "x", [], {"value":null}, {"value":"s"}, {"importance":1}
        ]}"#;
        let records = tile_records(body).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!((records[0].uri.as_deref(), records[0].importance), (Some("u"), Some(3)));
        assert_eq!((records[1].uri.as_deref(), records[1].importance), (None, None));
        assert_eq!(tile_records(br#"{"records":[]}"#).unwrap().len(), 0);
        for body in [&b"{}"[..], b"[]", br#"{"records":{}}"#, b"null", b"x"] {
            assert!(tile_records(body).is_none());
        }
    }

    #[test]
    fn reads_a_place_view() {
        let (record, importance) = place_view(br#"{"uri":"u","record":{"name":"x"},"importance":4}"#).unwrap();
        assert_eq!((Value::Object(record), importance), (json!({"name": "x"}), Some(4)));
        for body in [&b"null"[..], b"{}", br#"{"record":"x"}"#, br#"{"record":null}"#, b"[]"] {
            assert!(place_view(body).is_err());
        }
    }

    #[test]
    fn reads_describe_bodies_leniently() {
        let s = service_description(
            br#"{"did":5,"tilesets":[{"slug":"a","collections":[{"dataset":"d","collection":"c"},3]},7,{}],"x":1}"#,
        )
        .unwrap();
        assert_eq!(s.did, None);
        assert_eq!(s.tilesets.len(), 2);
        assert_eq!(s.tilesets[0].collections.len(), 1);
        assert_eq!(s.tilesets[1], ServedTileset::default());
        assert_eq!(service_description(br#"{"tilesets":"x"}"#).unwrap().tilesets, vec![]);
        for body in [&b"null"[..], b"false", b"0", b"\"\"", b"[]"] {
            assert!(service_description(body).is_err());
            assert!(tileset(body).is_err());
        }

        let t = tileset(
            br#"{"slug":"osm","publishedAt":"2026","extent":{"north":"1","south":2},
                 "collections":[{"dataset":"o","recordCount":3.7,"licenses":["l",1],"categories":[{"value":"cafe","count":2},null,{}]}],
                 "references":[]}"#,
        )
        .unwrap();
        assert_eq!(t.slug.as_deref(), Some("osm"));
        assert_eq!(t.extent.as_ref().unwrap().north.as_deref(), Some("1"));
        assert_eq!(t.extent.as_ref().unwrap().south, None);
        let c = &t.collections[0];
        assert_eq!((c.record_count, c.licenses.clone()), (Some(3), Some(vec!["l".to_owned()])));
        assert_eq!(c.categories.as_ref().unwrap().len(), 2);
        assert_eq!(c.attributes, None);
        assert_eq!(t.references, Some(vec![]));
        let t = tileset(br#"{"extent":"x","collections":{},"references":"y"}"#).unwrap();
        assert_eq!((t.extent, t.collections, t.references), (None, vec![], None));
    }
}
