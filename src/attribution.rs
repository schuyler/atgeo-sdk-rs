//! Attribution and hit collections (contract §4.7, §4.10, §8).

use std::collections::HashSet;

use serde_json::{Map, Value};

use crate::rank::cmp_utf16;
use crate::types::{Attribution, CollectionKey};

/// `urn:atgeo:{dataset}:{collection}:`, with the trailing colon: `way` must not match `wayside`.
pub(crate) fn collection_prefix(dataset: &str, collection: &str) -> String {
    format!("urn:atgeo:{dataset}:{collection}:")
}

/// The test of §4.7, shared with the hit's collection: `value.origin` is a string starting with the prefix.
/// The origin only, never the record's `uri`.
pub(crate) fn in_collection(value: &Map<String, Value>, prefix: &str) -> bool {
    value.get("origin").and_then(Value::as_str).is_some_and(|o| o.starts_with(prefix))
}

/// The first entry, in served order, whose collection the origin belongs to.
pub(crate) fn collection_of(value: &Map<String, Value>, attribution: &[Attribution]) -> Option<CollectionKey> {
    attribution
        .iter()
        .find(|a| in_collection(value, &collection_prefix(&a.dataset, &a.collection)))
        .map(|a| CollectionKey { dataset: a.dataset.clone(), collection: a.collection.clone() })
}

/// The union of the lists, in the order given: the first entry for each `(dataset, collection)` wins, whole
/// and unmerged. Sorted by dataset, then collection, by UTF-16 code units (§4.3). Entries are copies.
pub(crate) fn merge<'a>(lists: impl IntoIterator<Item = &'a [Attribution]>) -> Vec<Attribution> {
    let mut seen: HashSet<(&str, &str)> = HashSet::new();
    let mut out: Vec<Attribution> = Vec::new();
    for list in lists {
        for a in list {
            if seen.insert((a.dataset.as_str(), a.collection.as_str())) {
                out.push(a.clone());
            }
        }
    }
    out.sort_by(|a, b| cmp_utf16(&a.dataset, &b.dataset).then_with(|| cmp_utf16(&a.collection, &b.collection)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(dataset: &str, collection: &str, release: Option<&str>) -> Attribution {
        Attribution {
            dataset: dataset.into(),
            collection: collection.into(),
            release: release.map(Into::into),
            ..Attribution::default()
        }
    }

    fn value(origin: Value) -> Map<String, Value> {
        json!({ "origin": origin }).as_object().unwrap().clone()
    }

    #[test]
    fn the_collection_is_the_first_entry_whose_prefix_starts_the_origin() {
        let list = [entry("a:b", "c", None), entry("a", "b:c", None), entry("a", "b", None)];
        let k = collection_of(&value(json!("urn:atgeo:a:b:c:1")), &list).unwrap();
        assert_eq!((k.dataset.as_str(), k.collection.as_str()), ("a:b", "c"));
        assert_eq!(collection_of(&value(json!("urn:atgeo:a:bb:1")), &list), None);
        assert_eq!(collection_of(&value(json!("x urn:atgeo:a:b:c:1")), &list), None);
        assert_eq!(collection_of(&value(json!(5)), &list), None);
        assert_eq!(collection_of(&Map::new(), &list), None);
        assert!(!in_collection(&value(json!("urn:atgeo:d:wayside:1")), &collection_prefix("d", "way")));
    }

    #[test]
    fn merges_first_wins_by_tuple_and_sorts_by_code_units() {
        let one = [entry("a|b", "c", Some("1")), entry("a", "b|c", Some("2")), entry("d", "x", Some("first"))];
        let two = [entry("d", "x", Some("second")), entry("D", "x", None), entry("\u{FF61}", "x", None)];
        let two_b = [entry("\u{10000}", "x", None)];
        let got = merge([&one[..], &two[..], &two_b[..]]);
        let keys: Vec<(&str, &str)> = got.iter().map(|a| (a.dataset.as_str(), a.collection.as_str())).collect();
        assert_eq!(keys, [("D", "x"), ("a", "b|c"), ("a|b", "c"), ("d", "x"), ("\u{10000}", "x"), ("\u{FF61}", "x")]);
        assert_eq!(got[3].release.as_deref(), Some("first"));
        assert!(merge([]).is_empty());
    }
}
