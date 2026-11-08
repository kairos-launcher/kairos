//! Where browsers keep bookmarks and history, and reading them.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::Value;

/// Most history pages read per browser profile.
const HISTORY_LIMIT: usize = 2000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub title: String,
    pub url: String,
    /// The bookmark folder (empty for history and the top level).
    pub folder: String,
    pub history: bool,
}

/// A browser profile folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Holds `places.sqlite`.
    Firefox(PathBuf),
    /// Holds `Bookmarks` (JSON) and `History` (SQLite).
    Chromium(PathBuf),
}

pub fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// Every profile of the supported browsers found for this user.
pub fn detect() -> Vec<Source> {
    let mut out = vec![];
    for root in firefox_roots() {
        for dir in subdirs(&root).into_iter().chain(subdirs(&root.join("Profiles"))) {
            if dir.join("places.sqlite").is_file() {
                out.push(Source::Firefox(dir));
            }
        }
    }
    for root in chromium_roots() {
        for dir in subdirs(&root) {
            let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if (name == "Default" || name.starts_with("Profile ")) && dir.join("Bookmarks").is_file() {
                out.push(Source::Chromium(dir));
            }
        }
    }
    out
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
    let mut v: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    v.sort();
    v
}

fn env_dir(k: &str) -> Option<PathBuf> {
    std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from)
}

fn firefox_roots() -> Vec<PathBuf> {
    let mut v = vec![];
    if cfg!(windows) {
        v.extend(env_dir("APPDATA").map(|d| d.join(r"Mozilla\Firefox")));
    } else if let Some(h) = env_dir("HOME") {
        if cfg!(target_os = "macos") {
            v.push(h.join("Library/Application Support/Firefox"));
        } else {
            v.push(h.join(".mozilla/firefox"));
            v.push(h.join("snap/firefox/common/.mozilla/firefox"));
            v.push(h.join(".var/app/org.mozilla.firefox/.mozilla/firefox"));
            v.push(h.join(".librewolf"));
        }
    }
    v
}

fn chromium_roots() -> Vec<PathBuf> {
    let mut v = vec![];
    if cfg!(windows) {
        if let Some(l) = env_dir("LOCALAPPDATA") {
            for b in [r"Google\Chrome", "Chromium", r"BraveSoftware\Brave-Browser", r"Microsoft\Edge", "Vivaldi"] {
                v.push(l.join(b).join("User Data"));
            }
        }
    } else if let Some(h) = env_dir("HOME") {
        if cfg!(target_os = "macos") {
            let a = h.join("Library/Application Support");
            for b in ["Google/Chrome", "Chromium", "BraveSoftware/Brave-Browser", "Microsoft Edge", "Vivaldi"] {
                v.push(a.join(b));
            }
        } else {
            let c = env_dir("XDG_CONFIG_HOME").unwrap_or_else(|| h.join(".config"));
            for b in [
                "google-chrome",
                "google-chrome-beta",
                "chromium",
                "BraveSoftware/Brave-Browser",
                "microsoft-edge",
                "vivaldi",
            ] {
                v.push(c.join(b));
            }
            v.push(h.join("snap/chromium/common/chromium"));
            v.push(h.join(".var/app/com.google.Chrome/config/google-chrome"));
            v.push(h.join(".var/app/org.chromium.Chromium/config/chromium"));
            v.push(h.join(".var/app/com.brave.Browser/config/BraveSoftware/Brave-Browser"));
        }
    }
    v
}

/// Files whose change means the index must be read again.
fn watched(s: &Source, history: bool) -> Vec<PathBuf> {
    match s {
        // Firefox writes into the WAL file first.
        Source::Firefox(d) => vec![d.join("places.sqlite"), d.join("places.sqlite-wal")],
        Source::Chromium(d) if history => vec![d.join("Bookmarks"), d.join("History")],
        Source::Chromium(d) => vec![d.join("Bookmarks")],
    }
}

/// Read every source. A source that fails is skipped (with a note on stderr).
pub fn load_all(sources: &[Source], history: bool) -> (Vec<Entry>, Vec<(PathBuf, Option<SystemTime>)>) {
    let mut entries = vec![];
    let mut seen = vec![];
    for s in sources {
        // Times taken before reading, so a change during the read is noticed next time.
        seen.extend(watched(s, history).into_iter().map(|p| {
            let t = mtime(&p);
            (p, t)
        }));
        let got = match s {
            Source::Firefox(d) => firefox(d, history),
            Source::Chromium(d) => chromium(d, history),
        };
        match got {
            Ok(e) => entries.extend(e),
            Err(e) => eprintln!("bookmarks: {}: {e}", source_dir(s).display()),
        }
    }
    entries.retain(|e| is_web(&e.url));
    (entries, seen)
}

fn source_dir(s: &Source) -> &Path {
    match s {
        Source::Firefox(d) | Source::Chromium(d) => d,
    }
}

/// Only links the launcher will open (`open_url` allows `https`, `http`).
fn is_web(url: &str) -> bool {
    let l = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    l.starts_with("https://") || l.starts_with("http://")
}

/// A private copy of a browser's database (plus its WAL), which the running browser keeps locked.
/// Removed on drop.
struct DbCopy {
    dir: PathBuf,
    conn: rusqlite::Connection,
}

impl DbCopy {
    fn open(db: &Path) -> Result<Self, String> {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("kairos-bookmarks-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let name = db.file_name().ok_or("no file name")?;
        let copy = dir.join(name);
        let res = (|| {
            std::fs::copy(db, &copy).map_err(|e| format!("copy: {e}"))?;
            let wal = db.with_file_name(format!("{}-wal", name.to_string_lossy()));
            if wal.is_file() {
                let _ = std::fs::copy(&wal, dir.join(format!("{}-wal", name.to_string_lossy())));
            }
            rusqlite::Connection::open(&copy).map_err(|e| e.to_string())
        })();
        match res {
            Ok(conn) => Ok(Self { dir, conn }),
            Err(e) => {
                let _ = std::fs::remove_dir_all(&dir);
                Err(e)
            }
        }
    }
}

impl Drop for DbCopy {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn firefox(dir: &Path, history: bool) -> Result<Vec<Entry>, String> {
    let db = DbCopy::open(&dir.join("places.sqlite"))?;
    let mut out = vec![];
    let mut st = db
        .conn
        .prepare(
            "SELECT COALESCE(b.title, ''), p.url, COALESCE(f.title, '')
             FROM moz_bookmarks b JOIN moz_places p ON b.fk = p.id
             LEFT JOIN moz_bookmarks f ON b.parent = f.id
             WHERE b.type = 1",
        )
        .map_err(|e| e.to_string())?;
    let rows = st
        .query_map([], |r| {
            Ok(Entry {
                title: r.get(0)?,
                url: r.get(1)?,
                folder: firefox_folder(r.get::<_, String>(2)?),
                history: false,
            })
        })
        .map_err(|e| e.to_string())?;
    out.extend(rows.flatten());
    if history {
        let mut st = db
            .conn
            .prepare(&format!(
                "SELECT COALESCE(title, ''), url FROM moz_places
                 WHERE visit_count > 0 AND hidden = 0 ORDER BY frecency DESC LIMIT {HISTORY_LIMIT}"
            ))
            .map_err(|e| e.to_string())?;
        let rows = st
            .query_map([], |r| Ok(Entry { title: r.get(0)?, url: r.get(1)?, folder: String::new(), history: true }))
            .map_err(|e| e.to_string())?;
        out.extend(rows.flatten());
    }
    Ok(out)
}

/// Firefox's built-in folders have internal names; the toolbar and menu say nothing useful.
fn firefox_folder(name: String) -> String {
    match name.as_str() {
        "toolbar" | "menu" | "unfiled" | "mobile" | "" => String::new(),
        _ => name,
    }
}

fn chromium(dir: &Path, history: bool) -> Result<Vec<Entry>, String> {
    let text = std::fs::read_to_string(dir.join("Bookmarks")).map_err(|e| e.to_string())?;
    let mut out = parse_chromium_bookmarks(&text)?;
    if history {
        match chromium_history(&dir.join("History")) {
            Ok(h) => out.extend(h),
            Err(e) => eprintln!("bookmarks: {}: history: {e}", dir.display()),
        }
    }
    Ok(out)
}

pub fn parse_chromium_bookmarks(text: &str) -> Result<Vec<Entry>, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let mut out = vec![];
    if let Some(roots) = v["roots"].as_object() {
        for root in roots.values() {
            // The roots themselves (Bookmarks bar, Other bookmarks) are not shown as folders.
            for child in root["children"].as_array().into_iter().flatten() {
                walk_chromium(child, "", &mut out);
            }
        }
    }
    Ok(out)
}

fn walk_chromium(node: &Value, folder: &str, out: &mut Vec<Entry>) {
    let name = node["name"].as_str().unwrap_or("");
    match node["type"].as_str() {
        Some("url") => {
            if let Some(url) = node["url"].as_str() {
                out.push(Entry { title: name.into(), url: url.into(), folder: folder.into(), history: false });
            }
        }
        Some("folder") => {
            let path = if folder.is_empty() { name.to_string() } else { format!("{folder}/{name}") };
            for child in node["children"].as_array().into_iter().flatten() {
                walk_chromium(child, &path, out);
            }
        }
        _ => {}
    }
}

fn chromium_history(db: &Path) -> Result<Vec<Entry>, String> {
    let db = DbCopy::open(db)?;
    let mut st = db
        .conn
        .prepare(&format!(
            "SELECT COALESCE(title, ''), url FROM urls WHERE hidden = 0
             ORDER BY visit_count DESC, last_visit_time DESC LIMIT {HISTORY_LIMIT}"
        ))
        .map_err(|e| e.to_string())?;
    let rows = st
        .query_map([], |r| Ok(Entry { title: r.get(0)?, url: r.get(1)?, folder: String::new(), history: true }))
        .map_err(|e| e.to_string())?;
    Ok(rows.flatten().collect())
}

/// `https://www.github.com/x` -> `github.com` (lowercase input).
pub fn host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
    host.strip_prefix("www.").unwrap_or(host)
}

/// The address without scheme, `www.` and a trailing slash, at most 80 characters.
pub fn short_url(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let rest = rest.strip_prefix("www.").unwrap_or(rest).trim_end_matches('/');
    match rest.char_indices().nth(80) {
        Some((i, _)) => format!("{}…", &rest[..i]),
        None => rest.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chromium_bookmarks_with_folders() {
        let json = r#"{"roots":{
            "bookmark_bar":{"type":"folder","name":"Bookmarks bar","children":[
                {"type":"url","name":"GitHub","url":"https://github.com/"},
                {"type":"folder","name":"Work","children":[
                    {"type":"folder","name":"Docs","children":[{"type":"url","name":"Wiki","url":"https://wiki.example/"}]}
                ]}
            ]},
            "other":{"type":"folder","name":"Other","children":[{"type":"url","name":"JS","url":"javascript:alert(1)"}]},
            "sync_transaction_version":"1"
        }}"#;
        let got = parse_chromium_bookmarks(json).unwrap();
        assert_eq!(got.len(), 3);
        assert_eq!((got[1].title.as_str(), got[1].folder.as_str()), ("Wiki", "Work/Docs"));
        assert_eq!(got[0].folder, "");
        assert!(!is_web(&got[2].url), "javascript: links are dropped later");
        assert!(parse_chromium_bookmarks("not json").is_err());
    }

    #[test]
    fn firefox_places_are_read_from_a_copy() {
        let dir = std::env::temp_dir().join(format!("kairos-bm-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        {
            let c = rusqlite::Connection::open(dir.join("places.sqlite")).unwrap();
            c.execute_batch(
                "CREATE TABLE moz_places (id INTEGER PRIMARY KEY, url TEXT, title TEXT, visit_count INTEGER, hidden INTEGER, frecency INTEGER);
                 CREATE TABLE moz_bookmarks (id INTEGER PRIMARY KEY, type INTEGER, fk INTEGER, parent INTEGER, title TEXT);
                 INSERT INTO moz_places VALUES (1, 'https://rust-lang.org/', 'Rust', 5, 0, 100), (2, 'place:sort=8', NULL, 0, 0, 0), (3, 'https://news.example/', 'News', 9, 0, 200);
                 INSERT INTO moz_bookmarks VALUES (10, 2, NULL, 1, 'toolbar'), (11, 2, NULL, 10, 'Dev'), (12, 1, 1, 11, 'Rust lang'), (13, 1, 2, 10, 'Recent');",
            )
            .unwrap();
        }
        let src = vec![Source::Firefox(dir.clone())];
        let (entries, seen) = load_all(&src, false);
        assert_eq!(
            entries,
            vec![Entry {
                title: "Rust lang".into(),
                url: "https://rust-lang.org/".into(),
                folder: "Dev".into(),
                history: false
            }]
        );
        assert_eq!(seen.len(), 2);
        let (with_history, _) = load_all(&src, true);
        assert_eq!(
            with_history.iter().filter(|e| e.history).map(|e| e.title.as_str()).collect::<Vec<_>>(),
            ["News", "Rust"]
        );
        let leftovers = std::fs::read_dir(std::env::temp_dir())
            .unwrap()
            .flatten()
            .filter(|e| {
                e.file_name().to_string_lossy().starts_with(&format!("kairos-bookmarks-{}-", std::process::id()))
            })
            .count();
        assert_eq!(leftovers, 0, "copies are removed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hosts_and_short_urls() {
        assert_eq!(host("https://www.github.com/x?y"), "github.com");
        assert_eq!(host("http://me@host:8080/"), "host:8080");
        assert_eq!(short_url("https://www.rust-lang.org/"), "rust-lang.org");
        assert!(short_url(&format!("https://x.org/{}", "a".repeat(100))).ends_with('…'));
    }
}
