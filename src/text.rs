//! Normalization, Jaro-Winkler and record scoring (contract §7).

use serde_json::{Map, Value};
use unicode_normalization::UnicodeNormalization;
use unicode_properties::{GeneralCategory, UnicodeGeneralCategory};

const MAX_QUERY_TOKENS: usize = 12;

/// JavaScript's `\s`, which is not Unicode's White_Space: it has U+FEFF and lacks U+0085.
fn is_js_whitespace(c: char) -> bool {
    matches!(c, '\u{9}'..='\u{D}' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'..='\u{200A}'
        | '\u{2028}' | '\u{2029}' | '\u{202F}' | '\u{205F}' | '\u{3000}' | '\u{FEFF}')
}

pub(crate) fn normalize(s: &str) -> String {
    // 1. lowercase, 2. NFKD, 3. lowercase again, each over the whole string: Final_Sigma needs the
    // context, which char::to_lowercase does not have.
    let folded = s.to_lowercase().nfkd().collect::<String>().to_lowercase();
    let mut out = String::with_capacity(folded.len());
    let mut pending_space = false;
    for c in folded.chars() {
        if c.general_category() == GeneralCategory::NonspacingMark {
            continue; // 4. drop Mn
        }
        if is_js_whitespace(c) {
            pending_space = !out.is_empty(); // 5. collapse and trim
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(c);
    }
    out
}

/// Jaro-Winkler over UTF-16 code units, as the reference compares strings.
pub(crate) fn jaro_winkler(a: &[u16], b: &[u16]) -> f64 {
    let sim = jaro(a, b);
    let prefix = a.iter().zip(b).take(4).take_while(|(x, y)| x == y).count();
    sim + prefix as f64 * 0.1 * (1.0 - sim)
}

fn jaro(a: &[u16], b: &[u16]) -> f64 {
    if a == b {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let reach = (a.len().max(b.len()) / 2).saturating_sub(1);
    let mut a_matched = vec![false; a.len()];
    let mut b_matched = vec![false; b.len()];
    let mut matches = 0usize;
    for i in 0..a.len() {
        // An empty range when i - reach is past the end of b (`starbux` against `bar`).
        let hi = (b.len() - 1).min(i + reach);
        for j in i.saturating_sub(reach)..=hi {
            if !b_matched[j] && a[i] == b[j] {
                a_matched[i] = true;
                b_matched[j] = true;
                matches += 1;
                break;
            }
        }
    }
    if matches == 0 {
        return 0.0;
    }
    let mut transpositions = 0usize;
    let mut j = 0;
    for i in 0..a.len() {
        if !a_matched[i] {
            continue;
        }
        while !b_matched[j] {
            j += 1;
        }
        if a[i] != b[j] {
            transpositions += 1;
        }
        j += 1;
    }
    let m = matches as f64;
    (m / a.len() as f64 + m / b.len() as f64 + (m - transpositions as f64 / 2.0) / m) / 3.0
}

/// The words of a normalized name or query word for tier matching (contract §7): apostrophes (U+0027, U+2019,
/// U+02BC) vanish, and each run of punctuation (general category P*) separates words. Symbols do not.
fn fold(s: &str) -> Vec<String> {
    let spaced: String = s
        .chars()
        .filter(|&c| !matches!(c, '\'' | '\u{2019}' | '\u{02BC}'))
        .map(|c| match c.general_category() {
            GeneralCategory::ConnectorPunctuation
            | GeneralCategory::DashPunctuation
            | GeneralCategory::OpenPunctuation
            | GeneralCategory::ClosePunctuation
            | GeneralCategory::InitialPunctuation
            | GeneralCategory::FinalPunctuation
            | GeneralCategory::OtherPunctuation => ' ',
            _ => c,
        })
        .collect();
    spaced.split(' ').filter(|t| !t.is_empty()).map(str::to_owned).collect()
}

/// The normalized, non-empty `name` and `variants[].name` of a place: the names a query is scored and tiered against.
fn names(value: &Map<String, Value>) -> impl Iterator<Item = String> + '_ {
    let variants = match value.get("variants") {
        Some(Value::Array(vs)) => vs.as_slice(),
        _ => &[],
    };
    std::iter::once(value.get("name"))
        .chain(variants.iter().map(|v| v.as_object().and_then(|o| o.get("name"))))
        .filter_map(|n| n.and_then(Value::as_str))
        .map(normalize)
        .filter(|n| !n.is_empty())
}

/// A normalized, non-empty query, split once for every record it scores.
pub(crate) struct Query {
    whole: Vec<u16>,
    tokens: Vec<Vec<u16>>,
    /// The first twelve words, each folded (contract §7); a word that folds to nothing still used its slot.
    folded: Vec<String>,
}

impl Query {
    /// `normalized` must already be normalized; `None` when it is empty, which means no text.
    pub(crate) fn new(normalized: &str) -> Option<Query> {
        if normalized.is_empty() {
            return None;
        }
        Some(Query {
            whole: normalized.encode_utf16().collect(),
            tokens: normalized.split(' ').take(MAX_QUERY_TOKENS).map(|t| t.encode_utf16().collect()).collect(),
            folded: normalized.split(' ').take(MAX_QUERY_TOKENS).flat_map(fold).collect(),
        })
    }

    fn score_name(&self, name: &str) -> f64 {
        let name_units: Vec<u16> = name.encode_utf16().collect();
        let name_tokens: Vec<Vec<u16>> = name.split(' ').map(|n| n.encode_utf16().collect()).collect();
        let sum: f64 = self
            .tokens
            .iter()
            .map(|t| name_tokens.iter().map(|n| jaro_winkler(t, n)).fold(f64::NEG_INFINITY, f64::max))
            .sum();
        0.5 * jaro_winkler(&self.whole, &name_units) + 0.5 * (sum / self.tokens.len() as f64)
    }

    /// The best score of the place's `name` and its `variants[].name`, at least 0.
    pub(crate) fn score(&self, value: &Map<String, Value>) -> f64 {
        names(value).map(|n| self.score_name(&n)).fold(0.0, f64::max)
    }

    /// 1 if every query word begins a word of one single name, else 2 (contract §7); 1 for a query that folds
    /// to no words. Call it for a record that scored at least 0.6. `str::starts_with` is a prefix in UTF-16
    /// code units too: a UTF-8 byte prefix of a valid string is a code point prefix.
    pub(crate) fn tier(&self, value: &Map<String, Value>) -> u8 {
        if self.folded.is_empty() {
            return 1;
        }
        let exact = names(value).any(|name| {
            let words = fold(&name);
            self.folded.iter().all(|q| words.iter().any(|w| w.starts_with(q.as_str())))
        });
        if exact { 1 } else { 2 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn jw(a: &str, b: &str) -> f64 {
        jaro_winkler(&a.encode_utf16().collect::<Vec<_>>(), &b.encode_utf16().collect::<Vec<_>>())
    }

    // normalize.test.ts
    #[test]
    fn normalize_fixture() {
        let cases: Vec<Value> = serde_json::from_str(include_str!("../tests/fixtures/normalize.json")).unwrap();
        assert_eq!(cases.len(), 41);
        let failures: Vec<String> = cases
            .iter()
            .filter_map(|c| {
                let (input, expected) = (c["input"].as_str().unwrap(), c["expected"].as_str().unwrap());
                let got = normalize(input);
                (got != expected).then(|| format!("normalize({input:?}) = {got:?}, expected {expected:?}"))
            })
            .collect();
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn normalize_keeps_u200b_and_u0085_and_strips_ufeff() {
        assert_eq!(normalize("a\u{200B}b"), "a\u{200B}b");
        assert_eq!(normalize("a\u{85}b"), "a\u{85}b");
        assert_eq!(normalize("\u{FEFF}a\u{FEFF}\u{3000}b\u{FEFF}"), "a b");
    }

    // similarity.test.ts
    #[test]
    fn jaro_winkler_fixture() {
        let cases: Vec<Value> = serde_json::from_str(include_str!("../tests/fixtures/jaro-winkler.json")).unwrap();
        assert_eq!(cases.len(), 33);
        let mut failures = Vec::new();
        for c in &cases {
            let (a, b, score) = (c["a"].as_str().unwrap(), c["b"].as_str().unwrap(), c["score"].as_f64().unwrap());
            let got = jw(a, b);
            if (got - score).abs() >= 5e-7 {
                failures.push(format!("scores {a:?} / {b:?}: {got}, expected {score}"));
            }
            if got != jw(b, a) {
                failures.push(format!("is symmetric for {a:?} / {b:?}: {got} against {}", jw(b, a)));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    // Non-BMP cases the fixture cannot tell apart from a code-point implementation (spec §6).
    #[test]
    fn jaro_winkler_counts_utf16_code_units() {
        assert!((jw("😀😁", "😀😂") - 0.8833333333333334).abs() < 1e-15);
        assert!((jw("pizza", "pizza 🍕") - 0.925).abs() < 1e-15);
        assert!((jw("a😀", "a") - 0.8).abs() < 1e-15);
    }

    #[test]
    fn jaro_winkler_survives_an_empty_inner_range() {
        assert!(jw("starbux", "bar") > 0.0);
        assert_eq!(jw("a", "b"), 0.0);
        assert_eq!(jw("", "a"), 0.0);
        assert_eq!(jw("", ""), 1.0);
    }

    #[test]
    fn scores_the_contract_examples() {
        let q = Query::new(&normalize("Starbux")).unwrap();
        let s = |name: &str| q.score(json!({ "name": name }).as_object().unwrap());
        assert_eq!((s("Starbucks") * 10000.0).round() / 10000.0, 0.9048);
        assert_eq!((s("Star Bakery") * 10000.0).round() / 10000.0, 0.874);
        assert_eq!((s("Stardust Bar") * 10000.0).round() / 10000.0, 0.853);
        assert!(s("Home Depot") < 0.6);

        let munich = Query::new("munich").unwrap();
        let v = json!({ "name": "München", "variants": [{ "name": "Munich" }] });
        assert_eq!(munich.score(v.as_object().unwrap()), 1.0);
    }

    #[test]
    fn skips_variants_that_are_not_objects_with_string_names() {
        let q = Query::new("munich").unwrap();
        let v = json!({ "name": 7, "variants": [null, 3, "Munich", { "name": ["Munich"] }, { "name": "Munich" }] });
        assert_eq!(q.score(v.as_object().unwrap()), 1.0);
        let none = json!({ "variants": "Munich" });
        assert_eq!(q.score(none.as_object().unwrap()), 0.0);
    }

    // matchTiers.test.ts
    #[test]
    fn match_tiers_fixture() {
        let cases: Vec<Value> = serde_json::from_str(include_str!("../tests/fixtures/match-tiers.json")).unwrap();
        assert_eq!(cases.len(), 36);
        let failures: Vec<String> = cases
            .iter()
            .filter_map(|c| {
                let (name, query) = (c["name"].as_str().unwrap(), c["query"].as_str().unwrap());
                // `place` is `{ name, variants? }`, with `variants` a list of `{ name }`: a place value as served.
                let place = c["place"].as_object().unwrap();
                let got = Query::new(&normalize(query)).unwrap().tier(place);
                let want = c["tier"].as_u64().unwrap() as u8;
                (got != want).then(|| format!("{name}: tier({query:?}) = {got}, expected {want}"))
            })
            .collect();
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn counts_only_the_first_twelve_words_one_at_a_time() {
        let words: Vec<String> = (0..13).map(|i| format!("w{i}")).collect();
        let q = Query::new(&words.join(" ")).unwrap();
        assert_eq!(q.tokens.len(), 12);
    }
}
