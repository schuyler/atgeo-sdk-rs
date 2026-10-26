//! Hit construction and ordering (contract §8).

use std::cmp::Ordering;

use crate::attribution::collection_of;
use crate::cache::TileRecord;
use crate::locations::{Locations, distance_to, locate, record_key};
use crate::text::Query;
use crate::types::{Attribution, SearchHit, SortMode};

const JW_THRESHOLD: f64 = 0.6;

/// Relative spherical area; a point is 0, so it ranks before any box containing it.
fn specificity(locations: &Locations) -> f64 {
    match (locations.point, &locations.bbox) {
        (None, Some(b)) => {
            let rad = std::f64::consts::PI / 180.0;
            let width: f64 = b.segments().as_slice().iter().map(|&(lo, hi)| (hi - lo) * rad).sum();
            ((b.north * rad).sin() - (b.south * rad).sin()) * width
        }
        _ => 0.0,
    }
}

fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

/// Ascending, a NaN after every number. Subtraction ties -0 with 0, and so does this; f64::total_cmp
/// would not. A NaN tied with everything would not be a total order, which `sort_by` may panic over.
fn ascending(a: f64, b: f64) -> Ordering {
    a.partial_cmp(&b).unwrap_or_else(|| a.is_nan().cmp(&b.is_nan()))
}

/// Ordinal comparison by UTF-16 code units, as JavaScript compares strings.
pub(crate) fn cmp_utf16(a: &str, b: &str) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

struct Ranked {
    hit: SearchHit,
    /// 1 or 2 with text, `None` without (contract §7).
    tier: Option<u8>,
    specificity: f64,
}

fn by_tier(a: &Ranked, b: &Ranked) -> Ordering {
    a.tier.cmp(&b.tier)
}

fn by_score(a: &Ranked, b: &Ranked) -> Ordering {
    ascending(round3(b.hit.score.unwrap_or(0.0)), round3(a.hit.score.unwrap_or(0.0)))
}

fn by_importance(a: &Ranked, b: &Ranked) -> Ordering {
    b.hit.importance.unwrap_or(0).cmp(&a.hit.importance.unwrap_or(0))
}

fn by_distance(a: &Ranked, b: &Ranked) -> Ordering {
    ascending(a.hit.distance, b.hit.distance)
}

fn by_specificity(a: &Ranked, b: &Ranked) -> Ordering {
    ascending(a.specificity, b.specificity)
}

fn by_origin(a: &Ranked, b: &Ranked) -> Ordering {
    match (&a.hit.origin, &b.hit.origin) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(x), Some(y)) => cmp_utf16(x, y),
    }
}

/// Scores, filters on the text threshold, and sorts records that already passed the region filter.
/// `query` is `None` without text. With text on an `overview` answer, tier-2 (fuzzy only) hits are dropped.
pub(crate) fn rank<'a>(
    records: impl IntoIterator<Item = (&'a TileRecord, &'a [Attribution])>,
    center: (f64, f64),
    query: Option<&Query>,
    sort: SortMode,
    overview: bool,
) -> Vec<SearchHit> {
    let mut ranked: Vec<Ranked> = records
        .into_iter()
        .filter_map(|(record, attribution)| {
            let score = query.map(|q| q.score(&record.value));
            if score.is_some_and(|s| s < JW_THRESHOLD) {
                return None;
            }
            let tier = query.map(|q| q.tier(&record.value));
            if overview && tier == Some(2) {
                return None;
            }
            let locations = locate(&record.value);
            // The region filter already required a location, so there is a distance.
            let distance = distance_to(&locations, center.0, center.1)?;
            Some(Ranked {
                hit: SearchHit {
                    origin: record_key(record).map(str::to_owned),
                    collection: collection_of(&record.value, attribution),
                    place: record.value.clone(),
                    importance: record.importance,
                    distance,
                    score,
                },
                tier,
                specificity: specificity(&locations),
            })
        })
        .collect();

    // sort_by is stable, so hits that tie on everything keep their concatenated order.
    match sort {
        SortMode::Distance => ranked.sort_by(|a, b| {
            by_tier(a, b)
                .then_with(|| by_distance(a, b))
                .then_with(|| by_specificity(a, b))
                .then_with(|| by_importance(a, b))
                .then_with(|| by_score(a, b))
                .then_with(|| by_origin(a, b))
        }),
        SortMode::Similarity => ranked.sort_by(|a, b| {
            by_tier(a, b).then_with(|| by_score(a, b)).then_with(|| by_distance(a, b)).then_with(|| by_origin(a, b))
        }),
        SortMode::Fused => ranked.sort_by(|a, b| {
            by_tier(a, b)
                .then_with(|| by_score(a, b))
                .then_with(|| by_importance(a, b))
                .then_with(|| by_distance(a, b))
                .then_with(|| by_origin(a, b))
        }),
    }
    ranked.into_iter().map(|r| r.hit).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascending_ties_signed_zeros_and_puts_nan_last() {
        assert_eq!(ascending(-0.0, 0.0), Ordering::Equal);
        assert_eq!(ascending(f64::NAN, 1e300), Ordering::Greater);
        assert_eq!(ascending(1.0, f64::NAN), Ordering::Less);
        assert_eq!(ascending(f64::NAN, f64::NAN), Ordering::Equal);
    }

    #[test]
    fn compares_strings_by_utf16_code_units() {
        assert_eq!("\u{FF61}".cmp("\u{10000}"), Ordering::Less);
        assert_eq!(cmp_utf16("\u{FF61}", "\u{10000}"), Ordering::Greater);
        let mut urls = vec!["https://b", "https://a/9", "https://a/10", "https://a", "https://A"];
        urls.sort_by(|a, b| cmp_utf16(a, b));
        assert_eq!(urls, ["https://A", "https://a", "https://a/10", "https://a/9", "https://b"]);
    }

    #[test]
    fn round3_rounds_half_away_from_zero() {
        assert_eq!(round3(0.0005), 0.001);
        assert_eq!(round3(0.9048123), 0.905);
    }
}
