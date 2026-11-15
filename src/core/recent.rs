//! The entries run last, newest first, as they looked when run: shown when Down or Up is pressed
//! in an empty search box.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::core::frecency::PendingSave;
use crate::plugin::ResultItem;

/// Entries kept on disk; more than are shown, so raising `max_results` still finds some.
const MAX_ENTRIES: usize = 50;

#[derive(Serialize, Deserialize, Clone)]
struct Entry {
    item: ResultItem,
    /// `ResultItem` does not serialize these two.
    provider: String,
    query: String,
}

impl Entry {
    fn key(&self) -> (&str, &str) {
        (&self.provider, &self.item.id)
    }
}

pub struct Recent {
    path: Option<PathBuf>,
    entries: Vec<Entry>,
}

impl Recent {
    pub fn load(path: Option<PathBuf>) -> Self {
        let entries = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self { path, entries }
    }

    /// `item` was run: it becomes the first entry (moved there if it was already listed).
    /// Returns the file write to do once the caller has released the lock on `self`.
    #[must_use = "the entry is only persisted once the returned save is written"]
    pub fn record(&mut self, item: &ResultItem) -> Option<PendingSave> {
        let mut stored = item.clone();
        stored.score = 0.0;
        let entry = Entry { item: stored, provider: item.provider.clone(), query: item.query.clone() };
        self.entries.retain(|e| e.key() != entry.key());
        self.entries.insert(0, entry);
        self.entries.truncate(MAX_ENTRIES);
        let path = self.path.clone()?;
        let json = serde_json::to_string(&self.entries).ok()?;
        Some(PendingSave::new(path, json))
    }

    /// Up to `limit` entries, newest first, of the providers `keep` accepts.
    pub fn items(&self, limit: usize, keep: impl Fn(&str) -> bool) -> Vec<ResultItem> {
        self.entries
            .iter()
            .filter(|e| keep(&e.provider))
            .take(limit)
            .map(|e| ResultItem { provider: e.provider.clone(), query: e.query.clone(), ..e.item.clone() })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(provider: &str, id: &str) -> ResultItem {
        let mut r = ResultItem::new(id, id, "", 0.7);
        r.provider = provider.into();
        r.query = format!("q-{id}");
        r
    }

    fn ids(v: &[ResultItem]) -> Vec<&str> {
        v.iter().map(|r| r.id.as_str()).collect()
    }

    #[test]
    fn newest_first_and_a_rerun_moves_to_the_front() {
        let mut r = Recent::load(None);
        for id in ["a", "b", "c", "a"] {
            assert!(r.record(&item("p", id)).is_none(), "no file, nothing to save");
        }
        assert_eq!(ids(&r.items(8, |_| true)), ["a", "c", "b"]);
        assert_eq!(ids(&r.items(2, |_| true)), ["a", "c"]);
        // Same id from another provider is a different entry.
        let _ = r.record(&item("other", "b"));
        let all = r.items(8, |_| true);
        assert_eq!(ids(&all), ["b", "a", "c", "b"]);
        assert_eq!(ids(&r.items(8, |p| p == "p")), ["a", "c", "b"]);
    }

    #[test]
    fn keeps_at_most_max_entries() {
        let mut r = Recent::load(None);
        for n in 0..MAX_ENTRIES + 5 {
            let _ = r.record(&item("p", &n.to_string()));
        }
        let all = r.items(usize::MAX, |_| true);
        assert_eq!(all.len(), MAX_ENTRIES);
        assert_eq!(all[0].id, (MAX_ENTRIES + 4).to_string());
    }

    #[test]
    fn saves_keep_provider_and_query() {
        let tmp = crate::test_support::tmp();
        let path = tmp.path().join("recent.json");
        let mut r = Recent::load(Some(path.clone()));
        r.record(&item("p", "a")).expect("has a file").write();
        let back = Recent::load(Some(path)).items(8, |_| true);
        assert_eq!(back.len(), 1);
        assert_eq!((back[0].provider.as_str(), back[0].query.as_str()), ("p", "q-a"));
        assert!(back[0].score.abs() < f64::EPSILON);
    }
}
