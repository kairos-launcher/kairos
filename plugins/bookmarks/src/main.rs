//! Bookmarks plugin: browser bookmarks (and optionally history) in launcher searches.
//! Firefox (`places.sqlite`) and Chromium-based browsers (`Bookmarks` JSON, `History`).
//! JSON-RPC 2.0 over stdio, one message per line (see docs/develop/protocol.md).
//!
//! The index is read in the background when the plugin starts and again when a source file
//! changed. Enter opens the link in the default browser through the launcher (`open_url`), so the
//! plugin itself runs nothing.

mod sources;

use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use sources::{Entry, Source};

/// Plain searches need this many characters before bookmarks are offered.
const MIN_GLOBAL_CHARS: usize = 2;
/// How often the source files are checked for changes while queries come in.
const CHECK_EVERY: Duration = Duration::from_secs(5);

#[derive(Default)]
struct Index {
    entries: Vec<Entry>,
    /// Each source file with the modification time it was read at.
    seen: Vec<(std::path::PathBuf, Option<std::time::SystemTime>)>,
    checked: Option<Instant>,
}

struct Bookmarks {
    index: Arc<Mutex<Index>>,
    history: bool,
    sources: Vec<Source>,
    /// A reload is running in the background.
    loading: Arc<Mutex<bool>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Bookmarks {
    fn new(settings: &Value, sources: Vec<Source>) -> Self {
        Self {
            index: Arc::default(),
            history: settings["history"].as_bool().unwrap_or(false),
            sources,
            loading: Arc::default(),
        }
    }

    /// Reload in the background if never loaded or a source changed (checked every few seconds).
    fn refresh_if_needed(&self) {
        {
            let mut idx = lock(&self.index);
            let due = idx.checked.is_none_or(|t| t.elapsed() >= CHECK_EVERY);
            if !due {
                return;
            }
            idx.checked = Some(Instant::now());
            let changed = idx.seen.is_empty() || idx.seen.iter().any(|(p, t)| sources::mtime(p) != *t);
            if !changed {
                return;
            }
        }
        if std::mem::replace(&mut *lock(&self.loading), true) {
            return;
        }
        let (index, loading, srcs, history) =
            (self.index.clone(), self.loading.clone(), self.sources.clone(), self.history);
        std::thread::spawn(move || {
            let (entries, seen) = sources::load_all(&srcs, history);
            {
                let mut idx = lock(&index);
                idx.entries = entries;
                idx.seen = seen;
            }
            *lock(&loading) = false;
        });
    }

    /// Wait (briefly) for the first load; used by tests.
    #[cfg(test)]
    fn wait_loaded(&self) {
        for _ in 0..500 {
            if !*lock(&self.loading) {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn query(&self, text: &str, prefixed: bool, limit: usize) -> Vec<Value> {
        self.refresh_if_needed();
        let text = text.trim();
        if !prefixed && text.chars().count() < MIN_GLOBAL_CHARS {
            return vec![];
        }
        let idx = lock(&self.index);
        let mut hits: Vec<(f64, &Entry)> = if text.is_empty() {
            idx.entries.iter().filter(|e| !e.history).map(|e| (0.5, e)).collect()
        } else {
            idx.entries.iter().filter_map(|e| score(text, e).map(|s| (s, e))).collect()
        };
        hits.sort_by(|a, b| b.0.total_cmp(&a.0));
        // The same page saved in two browsers is listed once.
        let mut urls = std::collections::HashSet::new();
        hits.into_iter()
            .filter(|(_, e)| urls.insert(e.url.as_str()))
            .take(limit.max(1))
            .map(|(s, e)| item(e, if prefixed { s } else { s * 0.8 }))
            .collect()
    }

    fn handle(&mut self, method: &str, params: &Value) -> Result<Value, String> {
        Ok(match method {
            "initialize" => {
                *self = Bookmarks::new(&params["settings"], sources::detect());
                self.refresh_if_needed();
                json!({ "api": "0.1", "capabilities": { "streaming": false, "actions": true } })
            }
            "query" => {
                let limit = usize::try_from(params["limit"].as_u64().unwrap_or(8)).unwrap_or(8);
                let prefixed = !params["trigger"].is_null();
                json!({ "results": self.query(params["text"].as_str().unwrap_or(""), prefixed, limit) })
            }
            // Both actions are performed by the launcher (`kind`); nothing to do here.
            "execute" => json!({ "ok": false, "message": "unknown action" }),
            "shutdown" => json!({}),
            _ => return Err("unknown method".into()),
        })
    }
}

/// How well `text` finds an entry: every word must occur in the title or the address. Title
/// matches beat address matches, a match at a word start beats one inside a word, and history
/// ranks below bookmarks.
fn score(text: &str, e: &Entry) -> Option<f64> {
    let title = e.title.to_lowercase();
    let url = e.url.to_lowercase();
    let host = sources::host(&url);
    let mut total = 0.0;
    let mut words = 0;
    for w in text.to_lowercase().split_whitespace() {
        words += 1;
        let at_word_start = |hay: &str| {
            hay.match_indices(w)
                .any(|(i, _)| i == 0 || !hay[..i].chars().next_back().is_some_and(char::is_alphanumeric))
        };
        total += if title.starts_with(w) {
            1.0
        } else if at_word_start(&title) {
            0.9
        } else if host.starts_with(w) || host.contains(&format!(".{w}")) {
            0.85
        } else if title.contains(w) {
            0.7
        } else if url.contains(w) {
            0.55
        } else {
            return None;
        };
    }
    if words == 0 {
        return None;
    }
    let s = total / f64::from(words);
    Some(if e.history { s * 0.85 } else { s })
}

fn item(e: &Entry, score: f64) -> Value {
    let title = if e.title.trim().is_empty() { e.url.clone() } else { e.title.clone() };
    let mut subtitle = sources::short_url(&e.url);
    if !e.folder.is_empty() {
        subtitle = format!("{} · {subtitle}", e.folder);
    }
    json!({
        "id": e.url,
        "title": title,
        "subtitle": subtitle,
        "score": score.clamp(0.0, 1.0),
        "actions": [
            { "id": "open", "title": "Open", "default": true, "kind": "open_url", "value": e.url },
            { "id": "copy", "title": "Copy link", "kind": "copy", "value": e.url },
        ],
    })
}

fn main() {
    let mut plugin = Bookmarks::new(&Value::Null, vec![]);
    let mut out = std::io::stdout().lock();
    // stdin closing means: exit
    for line in std::io::stdin().lock().lines().map_while(Result::ok) {
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        let (method, id) = (msg["method"].as_str().unwrap_or(""), &msg["id"]);
        if id.is_null() {
            continue; // notifications ($/cancel): queries are fast enough to ignore them
        }
        let reply = match plugin.handle(method, &msg["params"]) {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(message) => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": message } }),
        };
        let _ = writeln!(out, "{reply}");
        let _ = out.flush();
        if method == "shutdown" {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(title: &str, url: &str, history: bool) -> Entry {
        Entry { title: title.into(), url: url.into(), folder: String::new(), history }
    }

    #[test]
    fn scoring_prefers_titles_word_starts_and_bookmarks() {
        let gh = entry("GitHub", "https://github.com/", false);
        let docs = entry("Rust docs", "https://doc.rust-lang.org/std/", false);
        assert_eq!(score("git", &gh), Some(1.0));
        assert_eq!(score("docs", &docs), Some(0.9));
        assert_eq!(score("rust-lang", &docs), Some(0.85), "host");
        assert_eq!(score("std", &docs), Some(0.55), "address only");
        assert_eq!(score("rust std", &docs), Some(f64::midpoint(1.0, 0.55)));
        assert_eq!(score("nope", &docs), None);
        assert_eq!(score("ub", &gh), Some(0.7), "inside a word");
        assert!(score("git", &entry("GitHub", "https://github.com/", true)).unwrap() < 1.0);
    }

    fn plugin_with(entries: Vec<Entry>) -> Bookmarks {
        let p = Bookmarks::new(&Value::Null, vec![]);
        {
            let mut idx = lock(&p.index);
            idx.entries = entries;
            idx.seen = vec![("/nonexistent".into(), None)];
            idx.checked = Some(Instant::now());
        }
        p
    }

    #[test]
    fn plain_searches_need_two_letters_and_rank_lower() {
        let p = plugin_with(vec![entry("GitHub", "https://github.com/", false)]);
        assert_eq!(p.query("g", false, 8).len(), 0);
        let plain = p.query("gi", false, 8);
        let bm = p.query("gi", true, 8);
        assert!(plain[0]["score"].as_f64() < bm[0]["score"].as_f64());
        assert_eq!(bm[0]["actions"][0]["kind"], "open_url");
        assert_eq!(bm[0]["actions"][1]["value"], "https://github.com/");
        assert_eq!(p.query("", true, 8).len(), 1, "`bm ` alone lists bookmarks");
    }

    #[test]
    fn duplicates_across_browsers_are_listed_once() {
        let p = plugin_with(vec![
            entry("GitHub", "https://github.com/", false),
            entry("GitHub", "https://github.com/", false),
            entry("GitHub Docs", "https://docs.github.com/", false),
        ]);
        assert_eq!(p.query("github", true, 8).len(), 2);
    }

    #[test]
    fn protocol_round_trip() {
        let mut p = Bookmarks::new(&Value::Null, vec![]);
        let init = p.handle("initialize", &json!({ "settings": { "history": false } })).unwrap();
        assert_eq!(init["api"], "0.1");
        p.wait_loaded();
        let r = p.handle("query", &json!({ "text": "zzzz-no-such-bookmark", "trigger": "bm ", "limit": 8 })).unwrap();
        assert_eq!(r["results"].as_array().unwrap().len(), 0);
        assert!(p.handle("bogus", &Value::Null).is_err());
    }
}
