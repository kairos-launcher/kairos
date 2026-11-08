//! File and folder search. The index is built in the background on the first `f ` search,
//! refreshed when older than ten minutes, and dropped when the launcher has been hidden for a
//! while, so it costs nothing at rest. Paths are stored as (parent, name) pairs in one string
//! buffer to keep memory low.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::Value;

use crate::core::paths::expand_home;
use crate::core::sync::lock;
use crate::core::{clipboard, fuzzy, icons, process};
use crate::plugin;
use crate::plugin::{Error, ExecOutcome, Icon, Provider, Query, ResultItem};

const ID: &str = "builtin-files";
const TRIGGER: &str = "f ";
const STALE_AFTER: Duration = Duration::from_secs(600);
const NO_PARENT: u32 = u32::MAX;

#[derive(Deserialize, Debug, Clone)]
#[serde(default)]
pub struct Settings {
    pub roots: Vec<String>,
    pub excludes: Vec<String>,
    pub hidden: bool,
    pub max_depth: usize,
    pub max_entries: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Self { roots: vec!["~".into()], excludes: vec![], hidden: false, max_depth: 8, max_entries: 100_000 }
    }
}

struct Entry {
    parent: u32,
    start: u32,
    len: u32,
    is_dir: bool,
}

#[derive(Default)]
pub struct Index {
    names: String,
    entries: Vec<Entry>,
}

impl Index {
    fn name(&self, i: usize) -> &str {
        let e = &self.entries[i];
        &self.names[e.start as usize..(e.start + e.len) as usize]
    }

    fn push(&mut self, parent: u32, name: &str, is_dir: bool) -> u32 {
        let start = self.names.len() as u32;
        self.names.push_str(name);
        self.entries.push(Entry { parent, start, len: name.len() as u32, is_dir });
        (self.entries.len() - 1) as u32
    }

    fn path(&self, mut i: usize) -> PathBuf {
        let mut parts = vec![];
        loop {
            parts.push(self.name(i));
            match self.entries[i].parent {
                NO_PARENT => break,
                p => i = p as usize,
            }
        }
        parts.iter().rev().collect()
    }

    fn depth(&self, mut i: usize) -> usize {
        let mut d = 0;
        while self.entries[i].parent != NO_PARENT {
            i = self.entries[i].parent as usize;
            d += 1;
        }
        d
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Walk `roots` breadth first (so a hit on `max_entries` drops the deepest files).
    pub fn build(roots: &[PathBuf], s: &Settings) -> Self {
        let mut ix = Index::default();
        let mut queue: VecDeque<(PathBuf, u32, usize)> = VecDeque::new();
        for root in roots.iter().filter(|r| r.is_dir()) {
            let i = ix.push(NO_PARENT, &root.to_string_lossy(), true);
            queue.push_back((root.clone(), i, 0));
        }
        while let Some((dir, idx, depth)) = queue.pop_front() {
            if depth >= s.max_depth {
                continue;
            }
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            for e in rd.flatten() {
                if ix.len() >= s.max_entries {
                    return ix;
                }
                let name = e.file_name();
                let Some(name) = name.to_str() else { continue };
                if (!s.hidden && name.starts_with('.')) || s.excludes.iter().any(|x| x == name) {
                    continue;
                }
                // Symlinks are listed but never followed (no loops, no leaving the roots).
                let Ok(ft) = e.file_type() else { continue };
                let i = ix.push(idx, name, ft.is_dir());
                if ft.is_dir() {
                    queue.push_back((e.path(), i, depth + 1));
                }
            }
        }
        ix.names.shrink_to_fit();
        ix.entries.shrink_to_fit();
        ix
    }

    /// Best matches for `text`: fuzzy on the name; a `dir/part/name` query also needs the
    /// folder part to appear in the path.
    pub fn search(&self, text: &str, limit: usize) -> Vec<(PathBuf, bool, f64)> {
        let (dir_part, name_part) = match text.rsplit_once('/') {
            Some((d, n)) => (d.to_lowercase(), n),
            None => (String::new(), text),
        };
        if name_part.is_empty() {
            return vec![];
        }
        let mut hits: Vec<(usize, f64)> = (0..self.entries.len())
            .filter(|&i| self.entries[i].parent != NO_PARENT)
            .filter_map(|i| fuzzy::score(name_part, self.name(i)).map(|s| (i, s)))
            .collect();
        hits.sort_by(|a, b| b.1.total_cmp(&a.1));
        let mut out = vec![];
        for (i, s) in hits {
            if out.len() >= limit {
                break;
            }
            let path = self.path(i);
            if !dir_part.is_empty()
                && !path.parent().is_some_and(|p| p.to_string_lossy().to_lowercase().contains(&dir_part))
            {
                continue;
            }
            let is_dir = self.entries[i].is_dir;
            let score = (s + if is_dir { 0.02 } else { 0.0 } - self.depth(i) as f64 * 0.005).clamp(0.0, 1.0);
            out.push((path, is_dir, score));
        }
        out.sort_by(|a, b| b.2.total_cmp(&a.2));
        out
    }
}

#[derive(Default)]
struct State {
    index: Option<Arc<Index>>,
    built: Option<Instant>,
    building: bool,
}

pub struct FilesProvider {
    settings: Settings,
    state: Arc<Mutex<State>>,
}

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_default()
}

/// `$HOME/x` -> `~/x`.
fn tilde(p: &Path) -> String {
    match p.strip_prefix(home()) {
        Ok(rest) if !home().as_os_str().is_empty() => format!("~/{}", rest.display()).trim_end_matches('/').to_string(),
        _ => p.display().to_string(),
    }
}

fn icon_for(path: &Path, is_dir: bool) -> Icon {
    if is_dir {
        return icons::themed(&["folder"], "/");
    }
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    let names: &[&str] = match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "bmp" | "tiff" | "heic" => &["image-x-generic"],
        "mp3" | "flac" | "ogg" | "wav" | "m4a" | "opus" => &["audio-x-generic"],
        "mp4" | "mkv" | "webm" | "avi" | "mov" => &["video-x-generic"],
        "zip" | "gz" | "tar" | "xz" | "7z" | "rar" | "bz2" | "zst" => &["package-x-generic", "application-x-archive"],
        "pdf" => &["application-pdf", "x-office-document"],
        "doc" | "docx" | "odt" => &["x-office-document"],
        "xls" | "xlsx" | "ods" | "csv" => &["x-office-spreadsheet"],
        "sh" | "py" | "rs" | "js" | "ts" | "c" | "h" | "cpp" | "go" | "java" | "toml" | "json" | "yaml" | "yml" => {
            &["text-x-script", "text-x-generic"]
        }
        _ => &["text-x-generic"],
    };
    let glyph = path
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.chars().next())
        .map_or("F".into(), |c| c.to_uppercase().to_string());
    icons::themed(names, &glyph)
}

impl FilesProvider {
    pub fn new(settings: &Value) -> Self {
        Self { settings: serde_json::from_value(settings.clone()).unwrap_or_default(), state: Arc::default() }
    }

    fn roots(&self) -> Vec<PathBuf> {
        self.settings.roots.iter().map(expand_home).collect()
    }

    /// The current index (possibly stale), starting a (re)build in the background if needed.
    fn index(&self) -> Option<Arc<Index>> {
        let mut st = lock(&self.state);
        let stale = st.built.is_none_or(|t| t.elapsed() > STALE_AFTER);
        if stale && !st.building {
            st.building = true;
            let (state, roots, settings) = (self.state.clone(), self.roots(), self.settings.clone());
            std::thread::spawn(move || {
                let ix = Arc::new(Index::build(&roots, &settings));
                *lock(&state) = State { index: Some(ix), built: Some(Instant::now()), building: false };
            });
        }
        st.index.clone()
    }

    fn row(path: &Path, is_dir: bool, score: f64) -> ResultItem {
        let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
        let parent = path.parent().map(tilde).unwrap_or_default();
        let mut r = ResultItem::new(path.to_string_lossy(), name, parent, score)
            .with_icon(icon_for(path, is_dir))
            .with_action("open", "Open")
            .with_action("folder", "Open containing folder")
            .with_action("copy", "Copy path");
        if is_dir {
            r = r.with_complete(format!("{TRIGGER}{}/", tilde(path)));
        }
        r
    }

    /// `f ~/Documents/rep`: list a folder directly, filtered by what follows the last `/`.
    fn browse(text: &str, limit: usize, hidden: bool) -> Vec<ResultItem> {
        let (dir, filter) = match text.rsplit_once('/') {
            Some((d, f)) => (expand_home(if d.is_empty() { "/" } else { d }), f),
            None => (expand_home(text), ""),
        };
        let Ok(rd) = std::fs::read_dir(&dir) else {
            return vec![ResultItem::new("missing", "No such folder", tilde(&dir), 1.0)];
        };
        let mut out: Vec<ResultItem> = rd
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') && !hidden && !filter.starts_with('.') {
                    return None;
                }
                let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
                let score =
                    if filter.is_empty() { 0.5 + if is_dir { 0.1 } else { 0.0 } } else { fuzzy::score(filter, &name)? };
                Some((name, is_dir, score, e.path()))
            })
            .map(|(_, is_dir, score, path)| Self::row(&path, is_dir, score))
            .collect();
        out.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.title.to_lowercase().cmp(&b.title.to_lowercase())));
        out.truncate(limit.max(1) * 4);
        out
    }
}

impl Provider for FilesProvider {
    fn id(&self) -> &str {
        ID
    }

    fn query(&self, q: &Query) -> Vec<ResultItem> {
        let text = q.text.trim();
        if text.is_empty() {
            return vec![
                ResultItem::new("hint", "Type a file or folder name", "f report · f proj/notes · f ~/Documents/", 1.0)
                    .with_icon(icons::themed(&["folder"], "/")),
            ];
        }
        if text.starts_with('/') || text.starts_with('~') {
            return Self::browse(text, q.limit, self.settings.hidden);
        }
        match self.index() {
            Some(ix) => ix.search(text, q.limit.max(1) * 3).into_iter().map(|(p, d, s)| Self::row(&p, d, s)).collect(),
            None => vec![
                ResultItem::new("indexing", "Indexing files…", "Results appear as you keep typing", 1.0)
                    .with_icon(icons::themed(&["folder"], "/")),
            ],
        }
    }

    fn execute(&self, result_id: &str, action_id: &str, _query: &str) -> plugin::Result<ExecOutcome> {
        let path = Path::new(result_id);
        if !path.is_absolute() {
            return Err("nothing to open".into());
        }
        match action_id {
            "open" => process::open(result_id)?,
            "folder" => process::open(&path.parent().unwrap_or(path).to_string_lossy())?,
            "copy" => clipboard::copy(result_id)?,
            _ => return Err(Error::UnknownAction(action_id.into())),
        }
        Ok(ExecOutcome::Close)
    }

    /// Free the index while the launcher is hidden.
    fn reap_idle(&self) {
        let mut st = lock(&self.state);
        if !st.building {
            *st = State::default();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> (tempfile::TempDir, PathBuf) {
        let tmp = crate::test_support::tmp();
        let root = tmp.path().to_path_buf();
        for d in ["projects/kairos/src", "projects/kairos/target/debug", "notes/.secret", "a/b/c/d/e"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        for f in [
            "projects/kairos/src/main.rs",
            "projects/kairos/target/debug/main.rs",
            "notes/report.md",
            "notes/.secret/report.md",
            "a/b/c/d/e/deep.txt",
            "report-old.txt",
        ] {
            std::fs::write(root.join(f), "x").unwrap();
        }
        (tmp, root)
    }

    fn settings(extra: &str) -> Settings {
        let m = crate::plugin::manifest::Manifest::parse(include_str!("plugin.toml"), "files").unwrap();
        let user: toml::Table = toml::from_str(extra).unwrap();
        serde_json::from_value(m.effective_settings(Some(&user))).unwrap()
    }

    #[test]
    fn index_skips_excluded_hidden_and_too_deep() {
        let (_tmp, root) = tree();
        let ix = Index::build(std::slice::from_ref(&root), &settings("max_depth = 4"));
        let found: Vec<PathBuf> =
            ix.search("main", 10).iter().map(|h| h.0.strip_prefix(&root).unwrap().to_path_buf()).collect();
        assert_eq!(found, [PathBuf::from("projects/kairos/src/main.rs")], "target/ is excluded");
        assert_eq!(
            ix.search("report", 10).iter().filter(|h| h.0.to_string_lossy().contains(".secret")).count(),
            0,
            "hidden skipped"
        );
        assert!(ix.search("deep", 10).is_empty(), "deeper than max_depth");
        let all = Index::build(std::slice::from_ref(&root), &settings("max_depth = 10\nhidden = true"));
        assert_eq!(all.search("deep", 10).len(), 1);
        assert!(all.search("report", 10).iter().any(|h| h.0.to_string_lossy().contains(".secret")));
        let capped = Index::build(std::slice::from_ref(&root), &settings("max_entries = 3"));
        assert_eq!(capped.len(), 3);
    }

    #[test]
    fn folder_part_filters_and_names_rank() {
        let (_tmp, root) = tree();
        let ix = Index::build(std::slice::from_ref(&root), &settings(""));
        let hits = ix.search("notes/rep", 10);
        assert_eq!(hits.len(), 1);
        assert!(hits[0].0.ends_with("notes/report.md"));
        let hits = ix.search("report", 10);
        assert!(hits[0].0.ends_with("report-old.txt") || hits[0].0.ends_with("report.md"));
        let kairos = ix.search("kairos", 10);
        assert!(kairos[0].1, "folders are found too");
    }

    #[test]
    fn browsing_lists_a_folder_and_completes_into_subfolders() {
        let (_tmp, root) = tree();
        let rows = FilesProvider::browse(&format!("{}/", root.join("notes").display()), 8, false);
        assert_eq!(rows.iter().map(|r| r.title.as_str()).collect::<Vec<_>>(), ["report.md"]);
        let rows = FilesProvider::browse(&format!("{}/proj", root.display()), 8, false);
        assert_eq!(rows[0].title, "projects");
        assert_eq!(rows[0].complete.as_deref(), Some(format!("f {}/", tilde(&root.join("projects"))).as_str()));
    }

    #[test]
    fn index_builds_in_background_and_is_dropped_when_idle() {
        let (_tmp, root) = tree();
        let p = FilesProvider::new(&serde_json::json!({ "roots": [root.to_string_lossy()] }));
        let first = p.query(&Query { text: "report", trigger: Some(TRIGGER), limit: 8 });
        if first[0].id == "indexing" {
            let t = Instant::now();
            while p.state.lock().unwrap().index.is_none() && t.elapsed() < Duration::from_secs(5) {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        assert!(
            p.query(&Query { text: "report", trigger: Some(TRIGGER), limit: 8 }).iter().any(|r| r.title == "report.md")
        );
        p.reap_idle();
        assert!(p.state.lock().unwrap().index.is_none());
    }
}

#[cfg(test)]
mod dev {
    use super::*;

    /// Dev check on this machine's home: `cargo test --release real_home_index -- --ignored --nocapture`.
    #[test]
    #[ignore = "dev check that indexes the real home folder"]
    fn real_home_index() {
        let m = crate::plugin::manifest::Manifest::parse(include_str!("plugin.toml"), "files").unwrap();
        let s: Settings = serde_json::from_value(m.effective_settings(None)).unwrap();
        let t = Instant::now();
        let ix = Index::build(&[home()], &s);
        let built = t.elapsed();
        let t = Instant::now();
        let hits = ix.search("kairos", 24);
        eprintln!(
            "{} entries, ~{} KB, built in {built:?}, search in {:?}, top: {:?}",
            ix.len(),
            (ix.names.capacity() + ix.entries.capacity() * std::mem::size_of::<Entry>()) / 1024,
            t.elapsed(),
            hits.first().map(|h| &h.0)
        );
    }
}
