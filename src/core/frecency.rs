use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const HALF_LIFE_DAYS: f64 = 14.0;
/// Launch count at which the frequency component saturates.
const SATURATION: u32 = 20;
/// Entries whose boost decayed below this are forgotten (one launch: after about two months).
const MIN_BOOST: f64 = 0.01;
const MAX_ENTRIES: usize = 2000;

#[derive(Serialize, Deserialize, Clone, Copy)]
struct Entry {
    count: u32,
    last: u64,
}

pub struct Frecency {
    path: Option<PathBuf>,
    map: HashMap<String, Entry>,
    /// Launch count at which the boost reaches 1.0 (1: a single launch counts fully).
    saturation: u32,
    half_life_days: f64,
}

/// Boost in 0.0..=1.0 of `e` at time `at`: frequency (log-scaled, full at `saturation`
/// launches) decayed by recency.
fn boost(e: Entry, at: u64, saturation: u32, half_life_days: f64) -> f64 {
    let freq = ((1.0 + f64::from(e.count)).ln() / (1.0 + f64::from(saturation)).ln()).min(1.0);
    let age_days = at.saturating_sub(e.last) as f64 / 86_400.0;
    freq * 0.5f64.powf(age_days / half_life_days)
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

impl Frecency {
    /// Launch history: grows with the launch count, saturating at 20, 14-day half-life.
    pub fn load(path: Option<PathBuf>) -> Self {
        Self::load_with(path, SATURATION, HALF_LIFE_DAYS)
    }

    /// Like `load`, with the launch count at which the boost saturates and its half-life.
    pub fn load_with(path: Option<PathBuf>, saturation: u32, half_life_days: f64) -> Self {
        let map = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self { path, map, saturation: saturation.max(1), half_life_days }
    }

    /// Boost in 0.0..=1.0: frequency (log-scaled) decayed by recency.
    pub fn boost(&self, key: &str) -> f64 {
        self.boost_at(key, now())
    }

    fn boost_at(&self, key: &str, at: u64) -> f64 {
        self.map.get(key).map_or(0.0, |e| boost(*e, at, self.saturation, self.half_life_days))
    }

    /// Count a launch of `key`. Returns the file write to do once the caller has released
    /// whatever lock guards `self`, so disk I/O never blocks other searches.
    #[must_use = "the launch is only persisted once the returned save is written"]
    pub fn record(&mut self, key: &str) -> Option<PendingSave> {
        let now = now();
        let e = self.map.entry(key.to_string()).or_insert(Entry { count: 0, last: 0 });
        e.count = e.count.saturating_add(1);
        e.last = now;
        self.prune(now);
        let path = self.path.clone()?;
        let json = serde_json::to_string(&self.map).ok()?;
        Some(PendingSave { path, json })
    }

    /// `record`, after forgetting the other keys that start with `group`: within a group only the
    /// latest launch is remembered.
    #[must_use = "the launch is only persisted once the returned save is written"]
    pub fn record_only(&mut self, key: &str, group: &str) -> Option<PendingSave> {
        self.map.retain(|k, _| k == key || !k.starts_with(group));
        self.record(key)
    }

    /// Forget entries whose boost has decayed to nothing, and keep at most `MAX_ENTRIES`, so
    /// the file does not grow forever.
    fn prune(&mut self, now: u64) {
        let (sat, half) = (self.saturation, self.half_life_days);
        self.map.retain(|_, e| boost(*e, now, sat, half) >= MIN_BOOST);
        if self.map.len() > MAX_ENTRIES {
            let mut by_boost: Vec<(f64, String)> =
                self.map.iter().map(|(k, e)| (boost(*e, now, sat, half), k.clone())).collect();
            by_boost.sort_by(|a, b| b.0.total_cmp(&a.0));
            for (_, k) in by_boost.into_iter().skip(MAX_ENTRIES) {
                self.map.remove(&k);
            }
        }
    }
}

/// A snapshot of the launch history, to be written to disk.
pub struct PendingSave {
    path: PathBuf,
    json: String,
}

impl PendingSave {
    pub fn write(self) {
        // Keys can contain typed text (shell commands, searches), so the file is private.
        if let Err(e) = crate::core::fs::write_private(&self.path, self.json.as_bytes()) {
            log::warn!("frecency: {}: {e}", self.path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::float_cmp, reason = "an unknown key is exactly zero")]
    fn more_launches_rank_higher_and_decay() {
        let mut f = Frecency::load(None);
        for k in ["a", "b", "b", "b"] {
            assert!(f.record(k).is_none(), "no file, nothing to save");
        }
        let t = now();
        assert!(f.boost_at("b", t) > f.boost_at("a", t));
        assert_eq!(f.boost_at("missing", t), 0.0);
        assert!(f.boost_at("b", t + 30 * 86_400) < f.boost_at("b", t) / 3.0);
    }

    #[test]
    fn saturation_one_counts_a_single_launch_fully() {
        let mut f = Frecency::load_with(None, 1, 30.0);
        let _ = f.record("a");
        assert!(f.boost_at("a", now()) > 0.99);
    }

    #[test]
    fn record_only_keeps_the_latest_key_of_a_group() {
        let mut f = Frecency::load_with(None, 1, 30.0);
        for k in ["fi\x1fa", "fo\x1fa", "fi\x1fb"] {
            let _ = f.record_only(k, &format!("{}\x1f", &k[..2]));
        }
        let mut keys: Vec<_> = f.map.keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["fi\x1fb", "fo\x1fa"]);
    }

    #[test]
    fn old_entries_are_forgotten_and_saves_carry_the_map() {
        let tmp_path = crate::test_support::tmp();
        let path = tmp_path.path().join("path.json");
        let mut f = Frecency::load(Some(path.clone()));
        f.map.insert("stale".into(), Entry { count: 1, last: 0 });
        let save = f.record("fresh").expect("has a file");
        assert!(!f.map.contains_key("stale"), "decayed entries are pruned");
        save.write();
        let back = Frecency::load(Some(path.clone()));
        assert!(back.boost("fresh") > 0.0);
    }
}
