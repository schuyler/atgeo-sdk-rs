//! Normalization, Jaro-Winkler and record scoring (contract §7).

use std::collections::HashSet;

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

    /// Whether the query has text but no words once folded (`&`): every record is tier 1 for it, and
    /// ranking keeps those at a score of 0.6 (contract §7).
    pub(crate) fn wordless(&self) -> bool {
        self.folded.is_empty()
    }

    /// 1 if every query word begins a word of one single name, else 2 (contract §7); 1 for a query that folds
    /// to no words. It is computed for every record when there is text, since it decides which threshold
    /// applies. `str::starts_with` is a prefix in UTF-16 code units too: a UTF-8 byte prefix of a valid
    /// string is a code point prefix.
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

/// Whether the server's keyword search would accept the keyword (contract §17): after `normalize`, folding,
/// dropping spacing and enclosing marks (which `normalize` leaves), recomposing and deduplicating, one of its
/// first 12 words has two code points. Not match tier 1's tokenization: that takes 12 words before folding
/// and does not deduplicate.
pub(crate) fn qualifies(keyword: &str) -> bool {
    let normalized = normalize(keyword);
    let mut seen: HashSet<String> = HashSet::new();
    let mut words: Vec<String> = Vec::new();
    for word in normalized.split(' ').flat_map(fold) {
        let word: String = word
            .chars()
            .filter(|c| !matches!(c.general_category(), GeneralCategory::SpacingMark | GeneralCategory::EnclosingMark))
            .nfc()
            .collect();
        if word.is_empty() || !seen.insert(word.clone()) {
            continue;
        }
        words.push(word);
        if words.len() == MAX_QUERY_TOKENS {
            break;
        }
    }
    // Code points, not UTF-16 units and not bytes: `chars()` counts Unicode scalars.
    words.iter().any(|w| w.chars().count() >= 2)
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

    // qualifying.test.ts
    #[test]
    fn qualifying_keywords_fixture() {
        let cases: Vec<Value> =
            serde_json::from_str(include_str!("../tests/fixtures/qualifying-keywords.json")).unwrap();
        assert_eq!(cases.len(), 23);
        let failures: Vec<String> = cases
            .iter()
            .filter_map(|c| {
                let (name, keyword) = (c["name"].as_str().unwrap(), c["keyword"].as_str().unwrap());
                let want = c["qualifies"].as_bool().unwrap();
                (qualifies(keyword) != want).then(|| format!("{name}: qualifies({keyword:?}) is not {want}"))
            })
            .collect();
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn qualifying_counts_code_points_and_does_not_reuse_the_tier_tokenization() {
        // Two code points, four UTF-16 units and eight bytes.
        assert!(qualifies("\u{1F600}\u{1F601}"));
        // One astral code point is one character, though two UTF-16 units.
        assert!(!qualifies("\u{1F600}"));
        // The first 12 distinct words: duplicates do not use a slot, so the 13th word is the 12th distinct.
        let mut words: Vec<String> = vec!["a".to_string(); 11];
        words.push("a".into());
        words.push("bb".into());
        assert!(qualifies(&words.join(" ")));
        // Twelve distinct one-letter words, then a long one: past the cap.
        let mut words: Vec<String> = ('a'..='l').map(String::from).collect();
        words.push("long".into());
        assert!(!qualifies(&words.join(" ")));
        // A precomposed Hangul syllable is one character after NFC, though NFKD makes it two.
        assert!(!qualifies("\u{D558}"));
        // A consonant with a spacing vowel sign is one character once the sign is removed.
        assert!(!qualifies("\u{0915}\u{093E}"));
    }

    // The fixture of lone surrogates is vendored and not run: a Rust string cannot hold one, and a body that
    // has one reads it as U+FFFD (contract §7, §14; CONFORMANCE.md), so the case cannot reach `tier`. All that
    // is checked is that the file is the one the contract describes, read the way a body is.
    #[test]
    fn the_lone_surrogate_fixture_is_vendored_and_not_run() {
        let raw = include_str!("../tests/fixtures/match-tiers-lone-surrogates.json");
        assert!(serde_json::from_str::<Value>(raw).is_err(), "serde_json cannot hold a lone surrogate");
        let Value::Array(cases) = crate::wire::parse_body(raw.as_bytes()).unwrap() else { panic!() };
        assert_eq!(cases.len(), 4);
        assert!(raw.to_ascii_lowercase().contains("\\ud8") && !raw.contains('\u{FFFD}'));
    }

    #[test]
    fn a_query_with_no_words_once_folded_is_wordless() {
        assert!(Query::new("&").unwrap().wordless());
        assert!(!Query::new("a&b").unwrap().wordless());
    }
}
