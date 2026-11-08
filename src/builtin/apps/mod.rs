//! Built-in app launcher: indexes installed applications and launches them.
//! Linux: freedesktop `.desktop` files. Windows: Start Menu `.lnk` shortcuts, plus packaged
//! (Store / MSIX) apps such as the new Teams, which have no shortcut (see `win`).
//!
//! Apps that take files or URLs (`%f %F %u %U` in `Exec=`; every Windows shortcut) also get
//! arguments: `chrome github.com` launches Chrome with `github.com`, and Tab on such an app
//! completes its name so the argument can follow.
//!
//! Linux apps are also found by their `Keywords=` (`browser` finds Firefox), and their desktop
//! actions (`[Desktop Action …]`, e.g. "New Private Window") are in the Tab menu and found as
//! rows of their own (`private window`).
//!
//! Indexed at startup. Each time the launcher is shown it rescans if any app folder changed
//! (adding, removing or renaming an entry changes its folder's modification time), or if the
//! index is older than [`FALLBACK`], which covers files edited in place.

use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::{Duration, Instant, SystemTime};

use crate::core::fuzzy;
use crate::core::sync::{read, write};
use crate::plugin;
use crate::plugin::{Action, Error, ExecOutcome, Icon, Provider, Query, ResultItem};

#[cfg(windows)]
#[allow(unsafe_code)]
mod win;

const ID: &str = "builtin-apps";
/// Rescan even without a folder change after this long.
const FALLBACK: Duration = Duration::from_mins(30);

/// Every folder the last scan looked at (also missing roots) with its modification time.
type Seen = Vec<(PathBuf, Option<SystemTime>)>;

#[derive(Clone, Debug, PartialEq)]
pub struct App {
    pub id: String,
    pub name: String,
    pub comment: String,
    /// Straight from `Icon=` after parsing; replaced by an absolute image path (or `None`
    /// if no PNG exists) when the index is built.
    pub icon: Option<String>,
    /// Linux: argv. Windows: single element, the .lnk path or `shell:AppsFolder\<app id>`.
    pub exec: Vec<String>,
    /// Where typed arguments go, if the app takes any.
    pub args: Option<ArgSlot>,
    /// `Keywords=`: more words that find the app.
    pub keywords: Vec<String>,
    /// Desktop actions listed in `Actions=`, in that order.
    pub actions: Vec<DesktopAction>,
}

/// A `[Desktop Action <id>]` of an app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DesktopAction {
    pub id: String,
    pub name: String,
    pub exec: Vec<String>,
}

/// Action ids of a row: the app itself, or one of its desktop actions (`desktop:<id>`).
const LAUNCH: &str = "launch";
const DESKTOP_PREFIX: &str = "desktop:";
/// Result id of a desktop-action row: `<app id>#<action id>`.
const ACTION_SEP: char = '#';
/// Keyword and desktop-action matches rank a little below matches of the app's name.
const KEYWORD_WEIGHT: f64 = 0.8;
const ACTION_WEIGHT: f64 = 0.85;
/// Shortest text that finds desktop actions by their own name (`new` would list them all).
const MIN_ACTION_CHARS: usize = 4;

/// Position in `exec` of the `Exec=` field code that takes files or URLs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArgSlot {
    pub at: usize,
    /// `%F` / `%U`: a list, so the text is split into several arguments.
    pub many: bool,
    /// `%f` / `%F`: files, so a leading `~` is expanded.
    pub files: bool,
}

/// An argument head must match the app name at least this well (substring or acronym), so
/// one stray letter does not offer every app.
const MIN_HEAD_SCORE: f64 = 0.7;

#[derive(Default)]
struct Index {
    apps: Vec<App>,
    built: Option<Instant>,
    dirs: Seen,
}

pub struct AppsProvider {
    index: RwLock<Index>,
}

impl AppsProvider {
    pub fn new() -> Self {
        Self { index: RwLock::new(Index::default()) }
    }

    fn reindex(&self) {
        let (mut apps, dirs) = scan(&roots());
        resolve_icons(&mut apps);
        *write(&self.index) = Index { apps, built: Some(Instant::now()), dirs };
    }

    fn stale(&self) -> bool {
        let g = read(&self.index);
        g.built.is_none_or(|t| t.elapsed() > FALLBACK) || changed(&g.dirs)
    }
}

/// Has any folder seen by the last scan changed (or appeared, or gone) since?
fn changed(dirs: &Seen) -> bool {
    dirs.iter().any(|(d, t)| crate::config::mtime(d) != *t)
}

impl Provider for AppsProvider {
    fn id(&self) -> &str {
        ID
    }

    fn warm_up(&self) {
        self.reindex();
    }

    fn refresh(&self) {
        self.reindex();
    }

    fn needs_refresh(&self) -> bool {
        self.stale()
    }

    fn query(&self, q: &Query) -> Vec<ResultItem> {
        let g = read(&self.index);
        let icon = |a: &App| a.icon.clone().map(|i| Icon { path: Some(i), ..Default::default() });
        let launch =
            |title: &str| Action { id: LAUNCH.into(), title: title.into(), default: true, ..Default::default() };
        let mut out: Vec<ResultItem> = vec![];
        for a in &g.apps {
            if let Some((score, rest)) = match_app(a, q.text) {
                let mut actions = vec![launch("Launch")];
                actions.extend(a.actions.iter().map(|d| Action {
                    id: format!("{DESKTOP_PREFIX}{}", d.id),
                    title: d.name.clone(),
                    ..Default::default()
                }));
                out.push(ResultItem {
                    id: a.id.clone(),
                    title: a.name.clone(),
                    subtitle: rest.map_or_else(|| a.comment.clone(), |r| format!("Open {r}")),
                    icon: icon(a),
                    score,
                    actions,
                    // Tab completes the name, ready for an argument (unless one is typed). Apps
                    // with desktop actions show those on Tab instead; arguments still work typed.
                    complete: (a.args.is_some() && rest.is_none() && a.actions.is_empty())
                        .then(|| format!("{} ", a.name)),
                    fallback: false,
                    provider: String::new(),
                    query: String::new(),
                });
            }
            for d in &a.actions {
                let Some(score) = match_action(a, d, q.text) else { continue };
                out.push(ResultItem {
                    id: format!("{}{ACTION_SEP}{}", a.id, d.id),
                    title: d.name.clone(),
                    subtitle: a.name.clone(),
                    icon: icon(a),
                    score,
                    actions: vec![launch("Run")],
                    complete: None,
                    fallback: false,
                    provider: String::new(),
                    query: String::new(),
                });
            }
        }
        out.sort_by(|a, b| b.score.total_cmp(&a.score));
        out.truncate(q.limit.max(1) * 3);
        out
    }

    fn execute(&self, result_id: &str, action_id: &str, query: &str) -> plugin::Result<ExecOutcome> {
        // A desktop-action row, or an action from an app row's menu.
        let (app_id, desktop) = match (result_id.split_once(ACTION_SEP), action_id.strip_prefix(DESKTOP_PREFIX)) {
            (_, Some(d)) => (result_id, Some(d)),
            (Some((app, d)), None) if action_id == LAUNCH => (app, Some(d)),
            (None, None) if action_id == LAUNCH => (result_id, None),
            _ => return Err(Error::UnknownAction(action_id.into())),
        };
        let app = {
            let g = read(&self.index);
            g.apps.iter().find(|a| a.id == app_id).cloned().ok_or("app no longer indexed")?
        };
        if let Some(d) = desktop {
            let d = app.actions.iter().find(|a| a.id == d).ok_or("no such action")?;
            crate::core::process::spawn(&d.exec).map_err(|e| format!("{}: {e}", d.name))?;
            return Ok(ExecOutcome::Close);
        }
        let rest = match_app(&app, query).and_then(|(_, rest)| rest);
        launch(&app, rest)?;
        Ok(ExecOutcome::Close)
    }
}

/// How well `text` finds a desktop action: by its own name (`private window`, at least
/// `MIN_ACTION_CHARS`), or by the app's name followed by more words (`firefox private`). So a
/// few letters typed for an app, or the app's name alone, do not list its actions as rows.
fn match_action(app: &App, action: &DesktopAction, text: &str) -> Option<f64> {
    let text = text.trim();
    let own = (text.chars().count() >= MIN_ACTION_CHARS).then(|| fuzzy::score(text, &action.name)).flatten();
    let with_app = text
        .contains(char::is_whitespace)
        .then(|| fuzzy::score(text, &format!("{} {}", app.name, action.name)))
        .flatten();
    own.into_iter().chain(with_app).filter(|s| *s >= MIN_HEAD_SCORE).reduce(f64::max).map(|s| s * ACTION_WEIGHT)
}

/// How well `text` finds `app`, and the argument text if it was typed after the app's name.
/// The whole text matching the name (or comment) wins, so `visual studio` is just a search.
/// Otherwise, for an app that takes arguments, the best-matching leading words are the name
/// and the rest is the argument (`chrome github.com`); ties go to the longer name.
fn match_app<'a>(app: &App, text: &'a str) -> Option<(f64, Option<&'a str>)> {
    let by_name = fuzzy::score(text, &app.name);
    let by_comment = fuzzy::score(text, &app.comment).map(|s| s * 0.4);
    let by_keyword = app
        .keywords
        .iter()
        .filter_map(|k| fuzzy::score(text, k).filter(|s| *s >= MIN_HEAD_SCORE))
        .reduce(f64::max)
        .map(|s| s * KEYWORD_WEIGHT);
    if let Some(s) = by_name.into_iter().chain(by_comment).chain(by_keyword).reduce(f64::max) {
        return Some((s, None));
    }
    app.args?;
    let text = text.trim();
    let mut best: Option<(f64, &str)> = None;
    for (i, _) in text.char_indices().filter(|(_, c)| c.is_whitespace()) {
        let Some(s) = fuzzy::score(&text[..i], &app.name).filter(|s| *s >= MIN_HEAD_SCORE) else { continue };
        if best.is_none_or(|(b, _)| s >= b) {
            best = Some((s, text[i..].trim_start()));
        }
    }
    best.map(|(s, rest)| (s, Some(rest)))
}

/// Replace icon names with image file paths; apps without a loadable icon get `None`.
#[cfg(unix)]
fn resolve_icons(apps: &mut [App]) {
    let mut resolver = crate::core::icons::IconResolver::new();
    for a in apps {
        a.icon = a.icon.take().and_then(|name| resolver.resolve(&name)).map(|p| p.to_string_lossy().into_owned());
    }
}

/// Windows: each shortcut's icon, extracted once (see `cached_file_icon`). Icons of apps that
/// are gone are deleted.
#[cfg(windows)]
fn resolve_icons(apps: &mut [App]) {
    use crate::core::icons::win;
    let dir = win::cache_dir(APP_ICONS);
    let mut keep = std::collections::HashSet::new();
    for a in apps.iter_mut() {
        let file = windows_icon(&a.id, &dir);
        if let Some(name) = file.as_deref().and_then(Path::file_name) {
            keep.insert(name.to_os_string());
        }
        a.icon = file.map(|f| f.to_string_lossy().into_owned());
    }
    let Ok(rd) = std::fs::read_dir(&dir) else { return };
    for e in rd.flatten().filter(|e| !keep.contains(&e.file_name())) {
        let _ = std::fs::remove_file(e.path());
    }
}

#[cfg(not(any(unix, windows)))]
fn resolve_icons(apps: &mut [App]) {
    for a in apps {
        a.icon = None;
    }
}

// ---------------------------------------------------------------- Linux

/// Folders holding `.desktop` files, highest priority first (user overrides system).
#[cfg(unix)]
pub(crate) fn desktop_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = vec![];
    if let Some(d) = dirs::data_dir() {
        dirs.push(d.join("applications"));
    }
    let xdg = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    dirs.extend(xdg.split(':').filter(|s| !s.is_empty()).map(|d| PathBuf::from(d).join("applications")));
    dirs.push("/var/lib/flatpak/exports/share/applications".into());
    if let Some(h) = dirs::home_dir() {
        dirs.push(h.join(".local/share/flatpak/exports/share/applications"));
    }
    dirs.push("/var/lib/snapd/desktop/applications".into());
    // Paths compare by component, so `/usr/share/` and `/usr/share` count as the same.
    let mut unique: Vec<PathBuf> = vec![];
    for d in dirs {
        if !unique.contains(&d) {
            unique.push(d);
        }
    }
    unique
}

#[cfg(unix)]
fn roots() -> Vec<PathBuf> {
    desktop_dirs()
}

#[cfg(unix)]
fn scan(roots: &[PathBuf]) -> (Vec<App>, Seen) {
    let mut apps: Vec<App> = vec![];
    let mut seen: Seen = vec![];
    for dir in roots {
        walk(dir, &mut seen, &mut |path| {
            if path.extension().is_none_or(|e| e != "desktop") {
                return;
            }
            let Some(id) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else { return };
            // Earlier dirs take precedence (user overrides system).
            if apps.iter().any(|a| a.id == id) {
                return;
            }
            if let Ok(text) = std::fs::read_to_string(path)
                && let Some(app) = parse_desktop(&id, &text)
            {
                apps.push(app);
            }
        });
    }
    apps.sort_by_key(|a| a.name.to_lowercase());
    (apps, seen)
}

#[cfg(unix)]
pub fn parse_desktop(id: &str, text: &str) -> Option<App> {
    /// Where the line being read belongs.
    enum Section {
        Entry,
        Action(usize),
        Other,
    }
    let mut section = Section::Other;
    let (mut name, mut comment, mut icon, mut exec) = (None, String::new(), None, None);
    let (mut hidden, mut is_app, mut terminal) = (false, false, false);
    let (mut keywords, mut listed) = (vec![], vec![]);
    // (id, Name, Exec) of every `[Desktop Action …]` section, listed or not.
    let mut actions: Vec<(String, Option<String>, Option<String>)> = vec![];
    let list = |v: &str| v.split(';').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect::<Vec<_>>();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            section = match line.strip_prefix("[Desktop Action ").and_then(|l| l.strip_suffix(']')) {
                _ if line == "[Desktop Entry]" => Section::Entry,
                Some(a) => {
                    actions.push((a.trim().to_string(), None, None));
                    Section::Action(actions.len() - 1)
                }
                None => Section::Other,
            };
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = v.trim();
        match (&section, k.trim()) {
            (Section::Entry, "Name") => name = Some(v.to_string()),
            (Section::Entry, "Comment") => comment = v.to_string(),
            (Section::Entry, "Icon") => icon = Some(v.to_string()),
            (Section::Entry, "Exec") => exec = Some(v.to_string()),
            (Section::Entry, "Type") => is_app = v == "Application",
            (Section::Entry, "NoDisplay" | "Hidden") => hidden |= v == "true",
            (Section::Entry, "Terminal") => terminal = v == "true",
            (Section::Entry, "Keywords") => keywords = list(v),
            (Section::Entry, "Actions") => listed = list(v),
            (Section::Action(i), "Name") => actions[*i].1 = Some(v.to_string()),
            (Section::Action(i), "Exec") => actions[*i].2 = Some(v.to_string()),
            _ => {}
        }
    }
    if hidden || !is_app {
        return None;
    }
    let in_terminal = |argv: &mut Vec<String>| {
        if terminal {
            argv.splice(0..0, ["x-terminal-emulator".to_string(), "-e".to_string()]);
        }
    };
    let (mut argv, mut args) = split_exec(&exec?);
    if argv.is_empty() {
        return None;
    }
    in_terminal(&mut argv);
    if terminal && let Some(a) = &mut args {
        a.at += 2;
    }
    // Only the actions `Actions=` lists are shown (the spec), in its order.
    let actions = listed
        .iter()
        .filter_map(|id| {
            let (_, name, exec) = actions.iter().find(|(a, _, _)| a == id)?;
            let mut exec = split_exec(exec.as_deref()?).0;
            if exec.is_empty() {
                return None;
            }
            in_terminal(&mut exec);
            Some(DesktopAction { id: id.clone(), name: name.clone()?, exec })
        })
        .collect();
    Some(App { id: id.to_string(), name: name?, comment, icon, exec: argv, args, keywords, actions })
}

/// Split an `Exec=` value into argv: honours double quotes and backslash escapes,
/// and drops field codes (%f, %U, ...); `%%` becomes `%`. The first file or URL code
/// (`%f %F %u %U`) is returned as the place for typed arguments. Empty arguments are dropped.
#[cfg(unix)]
pub fn split_exec(s: &str) -> (Vec<String>, Option<ArgSlot>) {
    let mut args = vec![];
    let mut slot = None;
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => quoted = !quoted,
            '\\' if quoted => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            '%' => match chars.next() {
                Some('%') => cur.push('%'),
                Some(code @ ('f' | 'F' | 'u' | 'U')) if slot.is_none() => {
                    // After the word being built, if the code is glued to one.
                    let at = args.len() + usize::from(!cur.is_empty());
                    slot = Some(ArgSlot { at, many: code.is_uppercase(), files: code.eq_ignore_ascii_case(&'f') });
                }
                _ => {}
            },
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    args.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        args.push(cur);
    }
    (args, slot)
}

/// The app's argv with the typed argument text put where its field code was.
#[cfg(unix)]
fn argv_with(app: &App, rest: Option<&str>) -> Vec<String> {
    let mut argv = app.exec.clone();
    let (Some(slot), Some(rest)) = (app.args, rest) else { return argv };
    let mut extra = if slot.many { crate::core::process::split_args(rest) } else { vec![rest.to_string()] };
    if slot.files {
        for a in &mut extra {
            *a = crate::core::paths::expand_home(&*a).to_string_lossy().into_owned();
        }
    }
    let at = slot.at.min(argv.len());
    argv.splice(at..at, extra);
    argv
}

#[cfg(unix)]
fn launch(app: &App, rest: Option<&str>) -> Result<(), String> {
    crate::core::process::spawn(&argv_with(app, rest)).map_err(|e| format!("{}: {e}", app.name))
}

// -------------------------------------------------------------- Windows

/// The all-users and per-user Start Menu `Programs` folders.
#[cfg(windows)]
fn roots() -> Vec<PathBuf> {
    ["ProgramData", "APPDATA"]
        .iter()
        .filter_map(std::env::var_os)
        .map(|p| PathBuf::from(p).join(r"Microsoft\Windows\Start Menu\Programs"))
        .collect()
}

#[cfg(windows)]
fn scan(roots: &[PathBuf]) -> (Vec<App>, Seen) {
    let mut apps: Vec<App> = vec![];
    let mut seen: Seen = vec![];
    for root in roots {
        walk(root, &mut seen, &mut |path| {
            if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("lnk")) {
                let Some(name) = path.file_stem().map(|n| n.to_string_lossy().into_owned()) else { return };
                if name.to_lowercase().contains("uninstall") || apps.iter().any(|a| a.name == name) {
                    return;
                }
                apps.push(App {
                    id: path.to_string_lossy().to_string(),
                    name,
                    comment: String::new(),
                    icon: None,
                    exec: vec![path.to_string_lossy().to_string()],
                    // A shortcut does not say whether its program takes arguments; offer them.
                    args: Some(ArgSlot { at: 1, many: true, files: false }),
                    keywords: vec![],
                    actions: vec![],
                });
            }
        });
    }
    for (name, id) in win::packaged_apps() {
        if apps.iter().any(|a| a.name == name) {
            continue;
        }
        let target = format!("{}{id}", win::APPS_FOLDER);
        // Packaged apps take no command line through the shell.
        apps.push(App {
            id: target.clone(),
            name,
            comment: String::new(),
            icon: None,
            exec: vec![target],
            args: None,
            keywords: vec![],
            actions: vec![],
        });
    }
    // Installing a package adds the user's data folder for it here, so a new packaged app is
    // noticed. Not walked: only the folder's own modification time matters.
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let packages = PathBuf::from(local).join("Packages");
        let t = crate::config::mtime(&packages);
        seen.push((packages, t));
    }
    apps.sort_by_key(|a| a.name.to_lowercase());
    (apps, seen)
}

/// The icon of an indexed app (`id` is a shortcut path or an Apps folder name), cached in `dir`.
#[cfg(windows)]
fn windows_icon(id: &str, dir: &Path) -> Option<PathBuf> {
    use crate::core::icons::win;
    if id.starts_with(self::win::APPS_FOLDER) {
        win::cached_shell_icon(id, dir)
    } else {
        win::cached_file_icon(Path::new(id), dir)
    }
}

#[cfg(windows)]
fn launch(app: &App, rest: Option<&str>) -> Result<(), String> {
    let shortcut = app.exec.first().ok_or("no shortcut")?;
    crate::core::process::open_with_params(shortcut, rest).map_err(|e| format!("{}: {e}", app.name))
}

// ---------------------------------------------------- icons named by users

/// Windows: the folder of extracted shortcut icons, shared by the index and [`icon_of_app`].
#[cfg(windows)]
const APP_ICONS: &str = "app-icons";
/// How long an answer of [`icon_of_app`] is kept (an app installed later shows up after this).
const NAMED_TTL: Duration = Duration::from_mins(5);

/// An icon a user named (`@kairos.icon firefox`, a custom action's `icon = "code"`): the icon
/// theme's, else the icon of the installed app with that name.
pub fn named_icon(name: &str) -> Option<Icon> {
    crate::core::icons::theme_icon(&[name]).or_else(|| icon_of_app(name))
}

/// The icon of the installed app called `name`, ignoring case: its desktop id (`firefox`, or
/// the snap and flatpak forms `firefox_firefox` and `org.mozilla.firefox`; Linux), its name, or
/// one word of its name (`chrome` for Google Chrome). Covers apps that ship their own image
/// instead of a theme icon (snap, flatpak) and Windows, which has no icon theme. Answers are
/// kept for [`NAMED_TTL`], since rows ask on every query.
pub fn icon_of_app(name: &str) -> Option<Icon> {
    static CACHE: std::sync::Mutex<Vec<(String, Instant, Option<String>)>> = std::sync::Mutex::new(Vec::new());
    let want = name.trim().to_lowercase();
    if want.is_empty() {
        return None;
    }
    let mut cache = crate::core::sync::lock(&CACHE);
    cache.retain(|(_, at, _)| at.elapsed() < NAMED_TTL);
    let path = if let Some((_, _, hit)) = cache.iter().find(|(n, _, _)| *n == want) {
        hit.clone()
    } else {
        let found = app_icon_in(&roots(), &want);
        cache.push((want, Instant::now(), found.clone()));
        found
    };
    path.map(|p| Icon { path: Some(p), ..Default::default() })
}

/// The best match for `want` (lower case) among the apps below `roots`, as an image path.
fn app_icon_in(roots: &[PathBuf], want: &str) -> Option<String> {
    let (mut apps, _) = scan(roots);
    // Windows: the shortcut's icon is extracted, so only matching apps are looked at.
    let rank = |a: &App| {
        let id = Path::new(&a.id).file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
        let name = a.name.to_lowercase();
        if cfg!(unix) && id == want {
            Some(0)
        } else if cfg!(unix) && (id.ends_with(&format!("_{want}")) || id.ends_with(&format!(".{want}"))) {
            Some(1)
        } else if name == want {
            Some(2)
        } else if name.split_whitespace().any(|w| w == want) {
            Some(3)
        } else {
            None
        }
    };
    // Stable: equal ranks keep the alphabetical order of the scan.
    apps.retain(|a| rank(a).is_some());
    apps.sort_by_key(|a| rank(a));
    apps.iter().find_map(icon_path)
}

/// One app's icon as an image path; unlike [`resolve_icons`] nothing in the cache is pruned.
#[cfg(unix)]
fn icon_path(app: &App) -> Option<String> {
    crate::core::icons::app_icon(app.icon.as_deref()?)?.path
}

/// One app's icon as an image path; unlike [`resolve_icons`] nothing in the cache is pruned.
#[cfg(windows)]
fn icon_path(app: &App) -> Option<String> {
    let file = windows_icon(&app.id, &crate::core::icons::win::cache_dir(APP_ICONS))?;
    Some(file.to_string_lossy().into_owned())
}

#[cfg(not(any(unix, windows)))]
fn icon_path(_: &App) -> Option<String> {
    None
}

// --------------------------------------------------------------- shared

/// Call `f` for every file below `dir`, and record each folder (also a missing `dir`) in `seen`.
/// The time is taken before listing, so a change made during the scan is noticed next time.
fn walk(dir: &Path, seen: &mut Seen, f: &mut dyn FnMut(&Path)) {
    seen.push((dir.to_path_buf(), crate::config::mtime(dir)));
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, seen, f);
        } else {
            f(&p);
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn strips_field_codes_and_handles_quotes() {
        let argv = |s: &str| split_exec(s).0;
        assert_eq!(argv("firefox %u"), ["firefox"]);
        assert_eq!(argv(r#""/opt/My App/run" --flag "a b" %F"#), ["/opt/My App/run", "--flag", "a b"]);
        assert_eq!(argv("sh -c 'echo 100%%'"), ["sh", "-c", "'echo", "100%'"]);
        assert_eq!(argv(r#"run "" x"#), ["run", "x"]);
    }

    #[test]
    fn remembers_where_arguments_go() {
        let slot = |s: &str| split_exec(s).1;
        assert_eq!(slot("firefox %u"), Some(ArgSlot { at: 1, many: false, files: false }));
        assert_eq!(slot("code --new %F --wait"), Some(ArgSlot { at: 2, many: true, files: true }));
        assert_eq!(slot("app %i %U %f"), Some(ArgSlot { at: 1, many: true, files: false }));
        assert_eq!(slot("calc"), None);
        assert_eq!(slot("echo 100%%u"), None);
        let term = parse_desktop("t.desktop", "[Desktop Entry]\nType=Application\nName=T\nExec=vim %F\nTerminal=true")
            .unwrap();
        assert_eq!(term.args.map(|a| a.at), Some(3), "after x-terminal-emulator -e vim");
    }

    fn app(name: &str, exec: &str) -> App {
        let (exec, args) = split_exec(exec);
        App {
            id: "a".into(),
            name: name.into(),
            comment: String::new(),
            icon: None,
            exec,
            args,
            keywords: vec![],
            actions: vec![],
        }
    }

    #[test]
    fn text_after_the_app_name_is_its_argument() {
        let chrome = app("Google Chrome", "google-chrome %U");
        assert_eq!(match_app(&chrome, "chrome github.com").unwrap().1, Some("github.com"));
        assert_eq!(match_app(&chrome, "google chrome  github.com").unwrap().1, Some("github.com"));
        assert_eq!(match_app(&chrome, "chrome").unwrap().1, None);
        assert!(match_app(&chrome, "x github.com").is_none(), "head too weak");
        let code = app("Visual Studio Code", "code %F");
        assert_eq!(match_app(&code, "visual studio").unwrap().1, None, "a plain search");
        assert!(match_app(&app("Calculator", "calc"), "calc 2").is_none(), "takes no arguments");
    }

    #[test]
    fn arguments_go_into_the_field_code_slot() {
        let rest = Some(r#"--incognito "a b""#);
        assert_eq!(argv_with(&app("C", "chrome %U --x"), rest), ["chrome", "--incognito", "a b", "--x"]);
        assert_eq!(argv_with(&app("F", "firefox %u"), Some("a b")), ["firefox", "a b"]);
        assert_eq!(argv_with(&app("F", "firefox %u"), None), ["firefox"]);
        if let Some(home) = dirs::home_dir() {
            let got = argv_with(&app("E", "evince %f"), Some("~/x.pdf"));
            assert_eq!(got[1], home.join("x.pdf").to_string_lossy());
        }
    }

    #[test]
    fn parses_and_filters_desktop_entries() {
        let ok = "[Desktop Entry]\nType=Application\nName=Foo\nComment=Does foo\nExec=foo %U\nIcon=foo\n[Desktop Action x]\nName=Other\nExec=bar";
        let app = parse_desktop("foo.desktop", ok).unwrap();
        assert_eq!((app.name.as_str(), app.exec.as_slice()), ("Foo", &["foo".to_string()][..]));
        assert!(
            parse_desktop(
                "h.desktop",
                &format!("{ok}\n").replace("Type=Application", "Type=Application\nNoDisplay=true")
            )
            .is_none()
        );
        assert!(parse_desktop("l.desktop", "[Desktop Entry]\nType=Link\nName=L\nExec=x").is_none());
    }

    const FIREFOX: &str = "[Desktop Entry]
Type=Application
Name=Firefox
Name[de]=Firefox-Browser
Comment=Browse the web
Exec=firefox %u
Icon=firefox
Keywords=Internet;WWW;Browser;Web;
Actions=new-window;new-private-window;missing;

[Desktop Action new-private-window]
Name=New Private Window
Name[de]=Neues privates Fenster
Exec=firefox --private-window %u

[Desktop Action new-window]
Name=New Window
Exec=firefox --new-window %u

[Desktop Action not-listed]
Name=Hidden
Exec=firefox --hidden
";

    #[test]
    fn reads_keywords_and_listed_desktop_actions_in_order() {
        let ff = parse_desktop("firefox.desktop", FIREFOX).unwrap();
        assert_eq!(ff.name, "Firefox");
        assert_eq!(ff.keywords, ["Internet", "WWW", "Browser", "Web"]);
        let names: Vec<&str> = ff.actions.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["New Window", "New Private Window"], "listed ones, in Actions= order");
        assert_eq!(ff.actions[1].exec, ["firefox", "--private-window"]);
        let term = parse_desktop(
            "t.desktop",
            "[Desktop Entry]\nType=Application\nName=T\nExec=htop\nTerminal=true\nActions=tree\n[Desktop Action tree]\nName=Tree\nExec=htop -t\n",
        )
        .unwrap();
        assert_eq!(term.actions[0].exec, ["x-terminal-emulator", "-e", "htop", "-t"]);
    }

    fn provider_with(apps: Vec<App>) -> AppsProvider {
        let p = AppsProvider::new();
        *write(&p.index) = Index { apps, built: Some(Instant::now()), dirs: vec![] };
        p
    }

    fn search(p: &AppsProvider, text: &str) -> Vec<ResultItem> {
        p.query(&Query { text, trigger: None, limit: 8 })
    }

    #[test]
    fn keywords_and_desktop_actions_are_searchable() {
        let p = provider_with(vec![parse_desktop("firefox.desktop", FIREFOX).unwrap()]);
        let rows = search(&p, "browser");
        assert_eq!(rows.first().map(|r| r.title.as_str()), Some("Firefox"), "found by keyword");
        assert!(rows[0].score < 1.0, "below a name match");

        let rows = search(&p, "private window");
        assert_eq!(rows[0].title, "New Private Window");
        assert_eq!((rows[0].subtitle.as_str(), rows[0].id.as_str()), ("Firefox", "firefox.desktop#new-private-window"));

        let rows = search(&p, "firefox");
        assert_eq!(rows[0].title, "Firefox", "the app comes before its actions");
        let menu: Vec<&str> = rows[0].actions.iter().map(|a| a.title.as_str()).collect();
        assert_eq!(menu, ["Launch", "New Window", "New Private Window"]);
        assert_eq!(rows[0].complete, None, "Tab shows the actions instead of completing");
        assert!(search(&p, "fi").iter().all(|r| r.title == "Firefox"), "a few letters list no actions");
        assert!(search(&p, "firefox").iter().all(|r| r.title == "Firefox"), "nor does the name alone");
        assert!(search(&p, "new").iter().all(|r| r.title == "Firefox"), "too short for action names");
        // `priv` may be an address for Firefox, so that row stays first; the action follows.
        let rows = search(&p, "firefox priv");
        assert_eq!((rows[0].title.as_str(), rows[0].subtitle.as_str()), ("Firefox", "Open priv"));
        assert_eq!(rows[1].title, "New Private Window");
    }

    #[test]
    fn desktop_actions_execute_by_row_or_menu() {
        let mut ff = parse_desktop("firefox.desktop", FIREFOX).unwrap();
        // Something that runs anywhere and exits at once.
        ff.actions[1].exec = vec!["true".into()];
        let p = provider_with(vec![ff]);
        assert!(p.execute("firefox.desktop#new-private-window", LAUNCH, "").is_ok());
        assert!(p.execute("firefox.desktop", "desktop:new-private-window", "").is_ok());
        assert!(p.execute("firefox.desktop", "desktop:nope", "").is_err());
        assert!(p.execute("firefox.desktop", "bogus", "").is_err());
    }

    const FOO: &str = "[Desktop Entry]\nType=Application\nName=Foo\nExec=foo\n";

    #[test]
    fn folder_changes_are_noticed() {
        let tmp = crate::test_support::tmp();
        let root = tmp.path().join("applications");
        let sub = root.join("vendor");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(root.join("foo.desktop"), FOO).unwrap();
        let missing = tmp.path().join("flatpak");
        let roots = [root.clone(), missing.clone()];

        let (apps, seen) = scan(&roots);
        assert_eq!(apps.len(), 1);
        assert_eq!(seen.len(), 3, "both roots (one missing) and the subfolder");
        assert!(!changed(&seen));

        // File timestamps can be coarse: make sure the change lands on a later tick.
        let later = || std::thread::sleep(Duration::from_millis(20));
        later();
        std::fs::write(sub.join("bar.desktop"), FOO.replace("Foo", "Bar")).unwrap();
        assert!(changed(&seen), "file added in a subfolder");

        let (_, seen) = scan(&roots);
        later();
        std::fs::remove_file(root.join("foo.desktop")).unwrap();
        assert!(changed(&seen), "file removed");

        let (_, seen) = scan(&roots);
        std::fs::create_dir(&missing).unwrap();
        assert!(changed(&seen), "missing root appeared");
    }

    #[test]
    fn apps_lend_their_icons_to_names() {
        let dir = crate::test_support::tmp();
        let png = dir.path().join("own.png");
        std::fs::write(&png, b"png").unwrap();
        let entry =
            |name: &str| format!("[Desktop Entry]\nType=Application\nName={name}\nExec=x\nIcon={}\n", png.display());
        std::fs::write(dir.path().join("org.mozilla.firefox.desktop"), entry("Firefox Web Browser")).unwrap();
        std::fs::write(dir.path().join("google-chrome.desktop"), entry("Google Chrome")).unwrap();
        std::fs::write(dir.path().join("noicon.desktop"), "[Desktop Entry]\nType=Application\nName=Bare\nExec=x\n")
            .unwrap();
        let roots = [dir.path().to_path_buf()];
        let want = Some(png.to_string_lossy().into_owned());
        assert_eq!(app_icon_in(&roots, "firefox"), want, "flatpak-style desktop id");
        assert_eq!(app_icon_in(&roots, "google chrome"), want, "the name");
        assert_eq!(app_icon_in(&roots, "chrome"), want, "a word of the name");
        assert_eq!(app_icon_in(&roots, "bare"), None, "an app without a loadable icon");
        assert_eq!(app_icon_in(&roots, "fire"), None, "no partial words");
    }

    #[test]
    fn desktop_dirs_have_no_duplicates() {
        let dirs = desktop_dirs();
        for (i, d) in dirs.iter().enumerate() {
            assert!(!dirs[..i].contains(d), "{} listed twice", d.display());
        }
    }

    /// Dev check against this machine's real apps: `cargo test real_icons -- --ignored --nocapture`.
    #[test]
    #[ignore = "dev check against the apps installed on this machine"]
    fn real_icons_resolve_on_this_machine() {
        let started = std::time::Instant::now();
        let (mut apps, dirs) = scan(&roots());
        println!("{} folders checked on each show", dirs.len());
        resolve_icons(&mut apps);
        let with = apps.iter().filter(|a| a.icon.is_some()).count();
        println!("{} apps, {} with an icon ({:?} incl. theme detection)", apps.len(), with, started.elapsed());
        for a in apps.iter().filter(|a| a.icon.is_none()) {
            println!("  no icon: {}", a.name);
        }
        assert!(with * 10 >= apps.len() * 7, "expected most apps to resolve");
    }
}
