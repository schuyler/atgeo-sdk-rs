//! The tile cache, an LRU of tile bodies capped by their size, and the one-hour TTL cache that holds tile
//! lists and the service description (contract §10).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use crate::rank::cmp_utf16;
use crate::types::Place;

/// A tile record as the client keeps it: `value`, `uri` and `importance`, nothing else.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TileRecord {
    pub(crate) value: Place,
    pub(crate) uri: Option<String>,
    pub(crate) importance: Option<i64>,
}

struct Entry {
    body: Arc<[u8]>,
    /// The keys (contract §4.6) of the tile's usable records, for the getPlace scan.
    keys: HashSet<String>,
    tick: u64,
    generation: u64,
}

/// A cached tile as a search or the getPlace scan reads it.
pub(crate) struct CachedTile {
    pub(crate) url: String,
    pub(crate) generation: u64,
    pub(crate) body: Arc<[u8]>,
}

/// Tile bodies by URL, least recently used first out, within `budget` bytes of body.
///
/// Recency is a tick from a counter; `order` maps each entry's tick back to its URL, so the least recently
/// used tile is `order`'s first entry. Each insert also gets a generation, so that a parsed copy of a tile
/// kept elsewhere can tell whether it still belongs to the entry the cache holds.
pub(crate) struct TileCache {
    budget: u64,
    total: u64,
    counter: u64,
    entries: HashMap<String, Entry>,
    order: BTreeMap<u64, String>,
}

impl TileCache {
    pub(crate) fn new(budget: u64) -> Self {
        TileCache { budget, total: 0, counter: 0, entries: HashMap::new(), order: BTreeMap::new() }
    }

    fn next(&mut self) -> u64 {
        self.counter += 1;
        self.counter
    }

    /// The tile at `url`, marked most recently used.
    pub(crate) fn get(&mut self, url: &str) -> Option<CachedTile> {
        let tick = self.next();
        let entry = self.entries.get_mut(url)?;
        self.order.remove(&entry.tick);
        entry.tick = tick;
        self.order.insert(tick, url.to_owned());
        Some(CachedTile { url: url.to_owned(), generation: entry.generation, body: entry.body.clone() })
    }

    /// Stores a tile, then evicts the least recently used tiles until the total is within the budget: a
    /// tile larger than the budget evicts every other tile and then itself. Returns its generation.
    pub(crate) fn insert(&mut self, url: &str, body: Arc<[u8]>, keys: HashSet<String>) -> u64 {
        if let Some(old) = self.entries.remove(url) {
            self.order.remove(&old.tick);
            self.total -= old.body.len() as u64;
        }
        let tick = self.next();
        self.total += body.len() as u64;
        self.entries.insert(url.to_owned(), Entry { body, keys, tick, generation: tick });
        self.order.insert(tick, url.to_owned());
        while self.total > self.budget {
            let Some((_, victim)) = self.order.pop_first() else { break };
            if let Some(e) = self.entries.remove(&victim) {
                self.total -= e.body.len() as u64;
            }
        }
        tick
    }

    /// The tile with the lowest URL, by UTF-16 code units, that holds a record with this key. Touches no
    /// recency.
    pub(crate) fn find(&self, key: &str) -> Option<CachedTile> {
        self.entries
            .iter()
            .filter(|(_, e)| e.keys.contains(key))
            .min_by(|(a, _), (b, _)| cmp_utf16(a, b))
            .map(|(url, e)| CachedTile { url: url.clone(), generation: e.generation, body: e.body.clone() })
    }

    #[cfg(test)]
    fn urls_by_recency(&self) -> Vec<&str> {
        self.order.values().map(String::as_str).collect()
    }

    #[cfg(test)]
    fn total(&self) -> u64 {
        self.total
    }
}

pub(crate) const TTL: Duration = Duration::from_secs(3600);

/// Values that expire `TTL` after they are set, by the clock the caller passes in.
pub(crate) struct TtlCache<T> {
    entries: HashMap<String, (T, SystemTime)>,
}

impl<T: Clone> TtlCache<T> {
    pub(crate) fn new() -> Self {
        TtlCache { entries: HashMap::new() }
    }

    /// Fresh while `now < set + TTL`; a miss from that instant on.
    pub(crate) fn get(&self, key: &str, now: SystemTime) -> Option<T> {
        self.entries.get(key).filter(|(_, expires)| now < *expires).map(|(v, _)| v.clone())
    }

    /// Drops expired entries first, so that a long-lived client under a panning map does not grow without
    /// bound.
    pub(crate) fn set(&mut self, key: &str, value: T, now: SystemTime) {
        self.entries.retain(|_, (_, expires)| now < *expires);
        // A clock within an hour of the end of time caches nothing, rather than overflowing.
        if let Some(expires) = now.checked_add(TTL) {
            self.entries.insert(key.to_owned(), (value, expires));
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(n: usize) -> Arc<[u8]> {
        vec![b' '; n].into()
    }

    fn keys(ks: &[&str]) -> HashSet<String> {
        ks.iter().map(|k| k.to_string()).collect()
    }

    #[test]
    fn reads_mark_recency() {
        let mut c = TileCache::new(100);
        for u in ["a", "b", "c"] {
            c.insert(u, body(10), keys(&[]));
        }
        assert!(c.get("a").is_some());
        assert_eq!(c.urls_by_recency(), ["b", "c", "a"]);
        assert!(c.get("zzz").is_none());
        // 105 bytes against 100: only the least recently used tile goes.
        c.insert("d", body(75), keys(&[]));
        assert_eq!(c.urls_by_recency(), ["c", "a", "d"]);
    }

    #[test]
    fn one_insert_evicts_several() {
        let mut c = TileCache::new(30);
        for u in ["a", "b", "c"] {
            c.insert(u, body(10), keys(&[]));
        }
        c.insert("d", body(25), keys(&[]));
        assert_eq!(c.urls_by_recency(), ["d"]);
        assert_eq!(c.total(), 25);
    }

    #[test]
    fn a_tile_larger_than_the_budget_evicts_everything_and_then_itself() {
        let mut c = TileCache::new(30);
        c.insert("a", body(10), keys(&[]));
        c.insert("b", body(31), keys(&[]));
        assert!(c.urls_by_recency().is_empty());
        assert_eq!(c.total(), 0);
        assert!(c.get("b").is_none());
    }

    #[test]
    fn a_tile_exactly_the_budget_stays() {
        let mut c = TileCache::new(30);
        c.insert("a", body(30), keys(&[]));
        assert_eq!(c.urls_by_recency(), ["a"]);
    }

    #[test]
    fn a_replacement_subtracts_the_old_size() {
        let mut c = TileCache::new(30);
        c.insert("a", body(20), keys(&[]));
        c.insert("b", body(5), keys(&[]));
        let g1 = c.get("a").unwrap().generation;
        let g2 = c.insert("a", body(25), keys(&[]));
        assert_eq!(c.total(), 30);
        assert_eq!(c.urls_by_recency(), ["b", "a"]);
        assert_ne!(g1, g2);
        assert_eq!(c.get("a").unwrap().generation, g2);
    }

    #[test]
    fn find_takes_the_lowest_url_by_code_units_and_leaves_recency_alone() {
        let mut c = TileCache::new(1000);
        // By str order "\u{FF61}" sorts first; by UTF-16 code units "\u{10000}" (D800 DC00) does.
        c.insert("https://t/\u{FF61}", body(1), keys(&["k", "x"]));
        c.insert("https://t/\u{10000}", body(1), keys(&["k"]));
        c.insert("https://t/~", body(1), keys(&["other"]));
        let before: Vec<String> = c.urls_by_recency().into_iter().map(String::from).collect();
        assert_eq!(c.find("k").unwrap().url, "https://t/\u{10000}");
        assert_eq!(c.find("x").unwrap().url, "https://t/\u{FF61}");
        assert!(c.find("nope").is_none());
        assert_eq!(c.urls_by_recency(), before);
    }

    #[test]
    fn a_tile_list_expires_after_an_hour() {
        let set = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let mut t = TtlCache::new();
        t.set("k", 1, set);
        assert_eq!(t.get("k", set + Duration::from_millis(3_599_999)), Some(1));
        assert_eq!(t.get("k", set + Duration::from_millis(3_600_000)), None);
        // A clock that steps backwards still reads it as fresh.
        assert_eq!(t.get("k", set - Duration::from_secs(10)), Some(1));
    }

    #[test]
    fn a_set_prunes_expired_entries() {
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let mut t = TtlCache::new();
        t.set("a", 1, t0);
        t.set("b", 2, t0 + Duration::from_secs(1800));
        t.set("c", 3, t0 + TTL);
        assert_eq!(t.len(), 2);
        // "a" was pruned, so even a clock read before its expiry finds nothing.
        assert_eq!(t.get("a", t0), None);
        assert_eq!(t.get("b", t0 + TTL), Some(2));
    }
}
