//! Switch to open windows. Each desktop has its own way to list and focus windows, so a
//! backend is picked once from the environment and driven through its command-line tool
//! (Window Calls over D-Bus on GNOME, hyprctl, swaymsg, niri msg, wmctrl) or Win32.
//! The list is fetched when the launcher is shown and filtered per keystroke.

use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::core::sync::lock;
use crate::core::{fuzzy, icons, process};
use crate::plugin;
use crate::plugin::{Error, ExecOutcome, Icon, Provider, Query, ResultItem};

const ID: &str = "builtin-windows";
/// A list younger than this is reused instead of fetched again.
const FRESH_FOR: Duration = Duration::from_secs(1);
/// A failed listing (e.g. the GNOME extension is missing) is retried this rarely.
const RETRY_AFTER: Duration = Duration::from_secs(60);
const WINDOW_CALLS_URL: &str = "https://extensions.gnome.org/extension/4724/window-calls/";
const GNOME_PATH: &str = "/org/gnome/Shell/Extensions/Windows";
const GNOME_IFACE: &str = "org.gnome.Shell.Extensions.Windows";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Win {
    /// Backend-specific handle passed back to focus/close.
    pub id: String,
    pub title: String,
    /// Application name or class, for display and matching.
    pub app: String,
    /// Icon theme names to try, best first.
    pub icon_names: Vec<String>,
    pub workspace: Option<String>,
    pub pid: Option<u32>,
    pub focused: bool,
    /// Full path of the window's executable, where its icon comes from (Windows only).
    pub exe: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Backend {
    Gnome,
    Hyprland,
    Sway,
    Niri,
    X11,
    Win32,
    Unsupported,
}

/// Pick the backend from environment variables (`env` looks one up).
pub fn detect(env: impl Fn(&str) -> Option<String>) -> Backend {
    if cfg!(windows) {
        return Backend::Win32;
    }
    let has = |k: &str| env(k).is_some_and(|v| !v.is_empty());
    if has("HYPRLAND_INSTANCE_SIGNATURE") {
        return Backend::Hyprland;
    }
    if has("NIRI_SOCKET") {
        return Backend::Niri;
    }
    if has("SWAYSOCK") {
        return Backend::Sway;
    }
    let wayland = has("WAYLAND_DISPLAY") || env("XDG_SESSION_TYPE").is_some_and(|v| v == "wayland");
    if wayland {
        let desktop = env("XDG_CURRENT_DESKTOP").unwrap_or_default().to_lowercase();
        return if desktop.split(':').any(|d| d == "gnome") { Backend::Gnome } else { Backend::Unsupported };
    }
    if has("DISPLAY") { Backend::X11 } else { Backend::Unsupported }
}

#[derive(Debug, PartialEq)]
/// Why the window list or a window command failed.
enum BackendError {
    /// The tool this backend needs is not installed.
    Missing(String),
    Failed(String),
}

struct Cache {
    wins: Vec<Win>,
    at: Option<Instant>,
    error: Option<BackendError>,
}

impl Cache {
    fn stale(&self) -> bool {
        let ttl = if self.error.is_some() { RETRY_AFTER } else { FRESH_FOR };
        self.at.is_none_or(|t| t.elapsed() > ttl)
    }
}

pub struct WindowsProvider {
    backend: Backend,
    cache: Mutex<Cache>,
}

impl WindowsProvider {
    pub fn new() -> Self {
        Self::with_backend(detect(|k| std::env::var(k).ok()))
    }

    fn with_backend(backend: Backend) -> Self {
        Self { backend, cache: Mutex::new(Cache { wins: vec![], at: None, error: None }) }
    }

    /// The cached list, fetched first if it is missing or old.
    fn fresh(&self) -> MutexGuard<'_, Cache> {
        let mut g = lock(&self.cache);
        if g.stale() {
            match list(self.backend) {
                Ok(w) => (g.wins, g.error) = (w, None),
                Err(e) => (g.wins, g.error) = (vec![], Some(e)),
            }
            g.at = Some(Instant::now());
        }
        g
    }

    fn invalidate(&self) {
        lock(&self.cache).at = None;
    }

    /// The single row shown under `win ` when windows can't be listed.
    fn hint(&self, e: &BackendError) -> ResultItem {
        let icon = icons::themed(&["dialog-information"], "?");
        match (self.backend, e) {
            (Backend::Gnome, _) => ResultItem::new(
                "hint",
                "Install the Window Calls GNOME extension to switch windows",
                "Enter opens extensions.gnome.org; log out and back in after installing",
                1.0,
            )
            .with_icon(icon)
            .with_action("open", "Open extension page"),
            (Backend::Unsupported, _) => ResultItem::new(
                "hint",
                "Window switching isn't supported on this desktop yet",
                "Works on GNOME, Hyprland, Sway, niri, X11 and Windows",
                1.0,
            )
            .with_icon(icon),
            (_, BackendError::Missing(tool)) => {
                ResultItem::new("hint", format!("Install {tool} to switch windows"), "", 1.0).with_icon(icon)
            }
            (_, BackendError::Failed(msg)) => {
                ResultItem::new("hint", "Couldn't list windows", msg.clone(), 1.0).with_icon(icon)
            }
        }
    }
}

impl Provider for WindowsProvider {
    /// Windows: the icon of a window's executable (the id is its path, from a row of ours).
    #[cfg(windows)]
    fn icon(&self, icon_id: &str) -> Option<Icon> {
        icons::win::exe_icon(icon_id)
    }

    fn id(&self) -> &str {
        ID
    }

    fn needs_refresh(&self) -> bool {
        self.backend != Backend::Unsupported && lock(&self.cache).stale()
    }

    fn refresh(&self) {
        drop(self.fresh());
    }

    /// Hidden: the next show must see the windows as they are then.
    fn reap_idle(&self) {
        *lock(&self.cache) = Cache { wins: vec![], at: None, error: None };
    }

    fn query(&self, q: &Query) -> Vec<ResultItem> {
        let global = q.trigger.is_none();
        let text = q.text.trim();
        if global && (text.chars().count() < 2 || self.backend == Backend::Unsupported) {
            return vec![];
        }
        let g = self.fresh();
        if let Some(e) = &g.error {
            return if global { vec![] } else { vec![self.hint(e)] };
        }
        rank(&g.wins, text, global, q.limit, std::process::id())
    }

    fn execute(&self, result_id: &str, action_id: &str, _query: &str) -> plugin::Result<ExecOutcome> {
        if result_id == "hint" {
            if action_id == "open" {
                process::open(WINDOW_CALLS_URL)?;
                return Ok(ExecOutcome::Close);
            }
            return Ok(ExecOutcome::KeepOpen);
        }
        let id = result_id.strip_prefix("win:").ok_or("not a window")?;
        let r = match action_id {
            "focus" => command(self.backend, Op::Focus, id),
            "close" => command(self.backend, Op::Close, id),
            _ => return Err(Error::UnknownAction(action_id.into())),
        };
        self.invalidate();
        r.map_err(|e| match e {
            BackendError::Missing(t) => format!("{t} not found"),
            BackendError::Failed(m) => m,
        })?;
        Ok(ExecOutcome::Close)
    }
}

/// Matching windows as result rows. `me` is the launcher's pid, whose own window is skipped.
fn rank(wins: &[Win], text: &str, global: bool, limit: usize, me: u32) -> Vec<ResultItem> {
    let mut scored: Vec<(&Win, f64)> = wins
        .iter()
        .filter(|w| w.pid != Some(me))
        .enumerate()
        .filter_map(|(i, w)| {
            let s = if text.is_empty() {
                // Backend order (most recent first where known), the focused window last.
                0.5 - i as f64 * 0.001 - if w.focused { 0.1 } else { 0.0 }
            } else {
                let s = [fuzzy::score(text, &w.title), fuzzy::score(text, &w.app).map(|s| s * 0.9)]
                    .into_iter()
                    .flatten()
                    .reduce(f64::max)?;
                if global { s * 0.85 } else { s }
            };
            Some((w, s))
        })
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    scored.truncate(limit.max(1) * 2);
    scored
        .into_iter()
        .map(|(w, s)| {
            let mut sub = if w.app.is_empty() { "Window".to_string() } else { format!("Window · {}", w.app) };
            if let Some(ws) = &w.workspace {
                sub.push_str(" · ");
                sub.push_str(ws);
            }
            let title = if w.title.is_empty() { w.app.clone() } else { w.title.clone() };
            let glyph: String = title.chars().next().map_or_else(|| "W".into(), |c| c.to_uppercase().collect());
            // Windows: the executable's icon, asked for lazily so only shown rows pay for it.
            let icon = w.exe.clone().map_or_else(
                || icons::themed(&w.icon_names.iter().map(String::as_str).collect::<Vec<_>>(), &glyph),
                |exe| Icon { id: Some(exe), ..Default::default() },
            );
            ResultItem::new(format!("win:{}", w.id), title, sub, s)
                .with_icon(icon)
                .with_action("focus", "Switch to")
                .with_action("close", "Close window")
        })
        .collect()
}

// ---------------------------------------------------------------- running tools

fn run(argv: &[&str]) -> Result<String, BackendError> {
    let (prog, args) = argv.split_first().ok_or(BackendError::Failed("empty command".into()))?;
    let out = Command::new(prog).args(args).stdin(Stdio::null()).stderr(Stdio::piped()).output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            BackendError::Missing(prog.to_string())
        } else {
            BackendError::Failed(format!("{prog}: {e}"))
        }
    })?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let line = err.lines().next().unwrap_or("").trim();
        return Err(BackendError::Failed(if line.is_empty() {
            format!("{prog}: {}", out.status)
        } else {
            line.to_string()
        }));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn json(s: &str) -> Result<Value, BackendError> {
    serde_json::from_str(s).map_err(|e| BackendError::Failed(format!("unexpected output: {e}")))
}

/// Call a Window Calls method; returns its single string result (empty if it has none).
fn gnome_call(method: &str, arg: Option<&str>) -> Result<String, BackendError> {
    if process::which("busctl").is_some() {
        let mut argv =
            vec!["busctl", "--user", "--json=short", "call", "org.gnome.Shell", GNOME_PATH, GNOME_IFACE, method];
        if let Some(a) = arg {
            argv.extend(["u", a]);
        }
        let out = run(&argv)?;
        return Ok(if out.trim().is_empty() { String::new() } else { parse_busctl(&out)? });
    }
    let m = format!("{GNOME_IFACE}.{method}");
    let mut argv =
        vec!["gdbus", "call", "--session", "--dest", "org.gnome.Shell", "--object-path", GNOME_PATH, "--method", &m];
    argv.extend(arg);
    Ok(parse_gdbus_string(&run(&argv)?).unwrap_or_default())
}

/// PIDs that own an open window, or `None` when this desktop cannot list windows (e.g. GNOME
/// without the Window Calls extension).
pub(crate) fn window_pids() -> Option<std::collections::HashSet<u32>> {
    let wins = list(detect(|k| std::env::var(k).ok())).ok()?;
    Some(wins.iter().filter_map(|w| w.pid).collect())
}

fn list(backend: Backend) -> Result<Vec<Win>, BackendError> {
    match backend {
        Backend::Gnome => {
            let mut wins = parse_window_calls(&json(&gnome_call("List", None)?)?);
            // Older Window Calls versions leave the title out of List.
            for w in wins.iter_mut().filter(|w| w.title.is_empty()) {
                w.title = gnome_call("GetTitle", Some(&w.id)).unwrap_or_default();
            }
            Ok(wins)
        }
        Backend::Hyprland => Ok(parse_hyprland(&json(&run(&["hyprctl", "-j", "clients"])?)?)),
        Backend::Sway => Ok(parse_sway(&json(&run(&["swaymsg", "-r", "-t", "get_tree"])?)?)),
        Backend::Niri => Ok(parse_niri(&json(&run(&["niri", "msg", "-j", "windows"])?)?)),
        Backend::X11 => Ok(parse_wmctrl(&run(&["wmctrl", "-lxp"])?)),
        #[cfg(windows)]
        Backend::Win32 => Ok(win32::list()),
        _ => Err(BackendError::Failed("unsupported desktop".into())),
    }
}

#[derive(Clone, Copy)]
enum Op {
    Focus,
    Close,
}

fn command(backend: Backend, op: Op, id: &str) -> Result<(), BackendError> {
    let focus = matches!(op, Op::Focus);
    match backend {
        Backend::Gnome => gnome_call(if focus { "Activate" } else { "Close" }, Some(id)).map(drop),
        Backend::Hyprland => {
            let addr = format!("address:{id}");
            run(&["hyprctl", "dispatch", if focus { "focuswindow" } else { "closewindow" }, &addr]).map(drop)
        }
        Backend::Sway => run(&["swaymsg", &format!("[con_id={id}]"), if focus { "focus" } else { "kill" }]).map(drop),
        Backend::Niri => {
            run(&["niri", "msg", "action", if focus { "focus-window" } else { "close-window" }, "--id", id]).map(drop)
        }
        Backend::X11 => run(&["wmctrl", if focus { "-ia" } else { "-ic" }, id]).map(drop),
        #[cfg(windows)]
        Backend::Win32 => win32::command(id, focus).map_err(BackendError::Failed),
        _ => Err(BackendError::Failed("unsupported desktop".into())),
    }
}

// ---------------------------------------------------------------- parsers

fn str_of(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}

fn pid_of(v: &Value, k: &str) -> Option<u32> {
    v.get(k).and_then(Value::as_u64).and_then(|p| u32::try_from(p).ok()).filter(|&p| p > 0)
}

/// Icon theme candidates for an app id / class: as given, then lowercase.
fn icon_names(names: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for n in names.iter().filter(|n| !n.is_empty()) {
        for c in [n.to_string(), n.to_lowercase()] {
            if !out.contains(&c) {
                out.push(c);
            }
        }
    }
    out
}

/// `busctl --json=short call` output: `{"type":"s","data":["…"]}`.
fn parse_busctl(out: &str) -> Result<String, BackendError> {
    json(out)?
        .pointer("/data/0")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or(BackendError::Failed("unexpected D-Bus reply".into()))
}

/// The string in gdbus output such as `('[{"id":1}]',)`, with GVariant escapes undone.
fn parse_gdbus_string(out: &str) -> Option<String> {
    let s = out.trim().strip_prefix("('")?.strip_suffix("',)")?;
    let mut r = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            r.push(c);
            continue;
        }
        match it.next()? {
            'n' => r.push('\n'),
            't' => r.push('\t'),
            'u' => r.push(char::from_u32(u32::from_str_radix(&it.by_ref().take(4).collect::<String>(), 16).ok()?)?),
            'U' => r.push(char::from_u32(u32::from_str_radix(&it.by_ref().take(8).collect::<String>(), 16).ok()?)?),
            other => r.push(other),
        }
    }
    Some(r)
}

/// Window Calls `List`: `[{"id":123,"wm_class":"firefox","title":"…","pid":1,"focus":false,…}]`.
fn parse_window_calls(v: &Value) -> Vec<Win> {
    let Some(arr) = v.as_array() else { return vec![] };
    arr.iter()
        // 0 is Meta.WindowType.NORMAL; dialogs, menus and the desktop icons window are skipped.
        .filter(|w| w.get("window_type").and_then(Value::as_u64).is_none_or(|t| t == 0))
        .filter_map(|w| {
            let id = w.get("id").and_then(Value::as_u64)?;
            let class = str_of(w, "wm_class");
            let instance = str_of(w, "wm_class_instance");
            let other_ws = w.get("in_current_workspace").and_then(Value::as_bool) == Some(false);
            Some(Win {
                id: id.to_string(),
                title: str_of(w, "title"),
                app: if class.is_empty() { instance.clone() } else { class.clone() },
                icon_names: icon_names(&[&class, &instance]),
                workspace: other_ws.then(|| "other workspace".to_string()),
                pid: pid_of(w, "pid"),
                focused: w.get("focus").and_then(Value::as_bool).unwrap_or(false),
                exe: None,
            })
        })
        .collect()
}

/// `hyprctl -j clients`, most recently focused first.
fn parse_hyprland(v: &Value) -> Vec<Win> {
    let Some(arr) = v.as_array() else { return vec![] };
    let mut wins: Vec<(i64, Win)> = arr
        .iter()
        .filter(|c| c.get("mapped").and_then(Value::as_bool) != Some(false))
        .filter_map(|c| {
            let class = str_of(c, "class");
            let class = if class.is_empty() { str_of(c, "initialClass") } else { class };
            let order = c.get("focusHistoryID").and_then(Value::as_i64).unwrap_or(i64::MAX);
            let w = Win {
                id: c.get("address").and_then(Value::as_str)?.to_string(),
                title: str_of(c, "title"),
                icon_names: icon_names(&[&class]),
                app: class,
                workspace: c.pointer("/workspace/name").and_then(Value::as_str).map(|n| format!("workspace {n}")),
                pid: pid_of(c, "pid"),
                focused: order == 0,
                exe: None,
            };
            Some((order, w))
        })
        .collect();
    wins.sort_by_key(|(o, _)| *o);
    wins.into_iter().map(|(_, w)| w).collect()
}

/// `swaymsg -t get_tree`: windows are the nodes that have a `pid`.
fn parse_sway(tree: &Value) -> Vec<Win> {
    fn walk(n: &Value, ws: Option<&str>, out: &mut Vec<Win>) {
        let ws = if n.get("type").and_then(Value::as_str) == Some("workspace") {
            n.get("name").and_then(Value::as_str)
        } else {
            ws
        };
        if let (Some(id), Some(_)) = (n.get("id").and_then(Value::as_u64), n.get("pid")) {
            let app_id = str_of(n, "app_id");
            let class = n.pointer("/window_properties/class").and_then(Value::as_str).unwrap_or("").to_string();
            out.push(Win {
                id: id.to_string(),
                title: str_of(n, "name"),
                app: if app_id.is_empty() { class.clone() } else { app_id.clone() },
                icon_names: icon_names(&[&app_id, &class]),
                workspace: ws.filter(|w| *w != "__i3_scratch").map(|w| format!("workspace {w}")),
                pid: pid_of(n, "pid"),
                focused: n.get("focused").and_then(Value::as_bool).unwrap_or(false),
                exe: None,
            });
        }
        for key in ["nodes", "floating_nodes"] {
            for c in n.get(key).and_then(Value::as_array).into_iter().flatten() {
                walk(c, ws, out);
            }
        }
    }
    let mut out = vec![];
    walk(tree, None, &mut out);
    out
}

/// `niri msg -j windows`.
fn parse_niri(v: &Value) -> Vec<Win> {
    let Some(arr) = v.as_array() else { return vec![] };
    arr.iter()
        .filter_map(|w| {
            let app_id = str_of(w, "app_id");
            Some(Win {
                id: w.get("id").and_then(Value::as_u64)?.to_string(),
                title: str_of(w, "title"),
                icon_names: icon_names(&[&app_id]),
                app: app_id,
                workspace: None,
                pid: pid_of(w, "pid"),
                focused: w.get("is_focused").and_then(Value::as_bool).unwrap_or(false),
                exe: None,
            })
        })
        .collect()
}

/// `wmctrl -lxp`: `0x03a00003  0 1234   Navigator.firefox  host  Title with spaces`.
fn parse_wmctrl(out: &str) -> Vec<Win> {
    out.lines()
        .filter_map(|line| {
            let mut rest = line.trim_start();
            let mut cols = [""; 5];
            for c in &mut cols {
                if rest.is_empty() {
                    return None;
                }
                let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
                *c = &rest[..end];
                rest = rest[end..].trim_start();
            }
            let [id, desktop, pid, wm_class, _host] = cols;
            // -1 is "on all desktops": panels, docks and desktop icons.
            if desktop == "-1" {
                return None;
            }
            let (instance, class) = wm_class.rsplit_once('.').unwrap_or((wm_class, wm_class));
            Some(Win {
                id: id.to_string(),
                title: rest.trim_end().to_string(),
                app: class.to_string(),
                icon_names: icon_names(&[instance, class]),
                workspace: None,
                pid: pid.parse().ok().filter(|&p| p > 0),
                focused: false,
                exe: None,
            })
        })
        .collect()
}

// ---------------------------------------------------------------- Windows

#[cfg(windows)]
#[allow(unsafe_code)]
mod win32 {
    //! Thin wrappers over the Win32 window APIs. Every `unsafe` block is a single FFI call
    //! with plain values or pointers to locals; the logic around them is safe Rust.

    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

    use windows_sys::Win32::Foundation::{HWND, LPARAM, TRUE};
    use windows_sys::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GW_OWNER, GWL_EXSTYLE, GetWindow, GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW,
        GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible, PostMessageW, SW_RESTORE, SetForegroundWindow,
        ShowWindow, WM_CLOSE, WS_EX_TOOLWINDOW,
    };
    use windows_sys::core::BOOL;

    use super::Win;

    pub fn list() -> Vec<Win> {
        top_level_windows().into_iter().filter(|&h| is_switchable(h)).filter_map(describe).collect()
    }

    /// Every top-level window handle, in Z order.
    fn top_level_windows() -> Vec<HWND> {
        unsafe extern "system" fn push(hwnd: HWND, lp: LPARAM) -> BOOL {
            // SAFETY: `lp` is the `&mut Vec<HWND>` that `top_level_windows` passes to
            // EnumWindows, which calls this only while that borrow is alive.
            let out = unsafe { &mut *(lp as *mut Vec<HWND>) };
            out.push(hwnd);
            TRUE
        }
        let mut out: Vec<HWND> = vec![];
        // SAFETY: `push` matches WNDENUMPROC and `out` outlives the call.
        unsafe { EnumWindows(Some(push), (&raw mut out) as LPARAM) };
        out
    }

    /// Visible, unowned, not a tool window and not cloaked (UWP shells, other desktops).
    fn is_switchable(hwnd: HWND) -> bool {
        // `hwnd` came from EnumWindows. A window closed since then makes these calls fail
        // harmlessly: Win32 validates handles and we never dereference one.
        // SAFETY: plain query on a window handle (see above).
        let visible = unsafe { IsWindowVisible(hwnd) } != 0;
        // SAFETY: plain query on a window handle.
        let owned = !unsafe { GetWindow(hwnd, GW_OWNER) }.is_null();
        // SAFETY: plain query on a window handle.
        let ex_style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
        let mut cloaked: u32 = 0;
        let size = std::mem::size_of::<u32>() as u32;
        // SAFETY: the out pointer is a live local `u32` and `size` is its size.
        unsafe { DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED as u32, (&raw mut cloaked).cast(), size) };
        visible && !owned && ex_style & WS_EX_TOOLWINDOW == 0 && cloaked == 0
    }

    fn describe(hwnd: HWND) -> Option<Win> {
        let title = window_title(hwnd)?;
        let mut pid = 0u32;
        // SAFETY: the out pointer is a live local.
        unsafe { GetWindowThreadProcessId(hwnd, &raw mut pid) };
        let exe = exe_path(pid);
        let app = exe.as_deref().and_then(|p| std::path::Path::new(p).file_stem());
        Some(Win {
            id: (hwnd as isize).to_string(),
            title,
            app: app.map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
            icon_names: vec![],
            workspace: None,
            pid: Some(pid).filter(|&p| p > 0),
            focused: false,
            exe,
        })
    }

    fn window_title(hwnd: HWND) -> Option<String> {
        // SAFETY: plain query on a window handle.
        let len = usize::try_from(unsafe { GetWindowTextLengthW(hwnd) }).ok().filter(|&n| n > 0)?;
        let mut buf = vec![0u16; len + 1];
        let cap = i32::try_from(buf.len()).ok()?;
        // SAFETY: `buf` holds `cap` u16s and stays alive for the call.
        let n = unsafe { GetWindowTextW(hwnd, buf.as_mut_ptr(), cap) };
        let n = usize::try_from(n).ok()?;
        Some(String::from_utf16_lossy(buf.get(..n)?))
    }

    /// Full path of the process's executable.
    fn exe_path(pid: u32) -> Option<String> {
        // SAFETY: plain call; a null result is checked below.
        let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if raw.is_null() {
            return None;
        }
        // SAFETY: `raw` is a valid process handle that nothing else owns; OwnedHandle closes
        // it on every return path.
        let process = unsafe { OwnedHandle::from_raw_handle(raw) };
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        // SAFETY: `buf` holds `len` u16s; `len` is updated to the characters written.
        let ok = unsafe {
            QueryFullProcessImageNameW(process.as_raw_handle(), PROCESS_NAME_WIN32, buf.as_mut_ptr(), &raw mut len)
        };
        if ok == 0 {
            return None;
        }
        Some(String::from_utf16_lossy(buf.get(..len as usize)?))
    }

    pub fn command(id: &str, focus: bool) -> Result<(), String> {
        let hwnd = id.parse::<isize>().map_err(|_| "bad window id")? as HWND;
        // Win32 validates window handles itself: a stale or bogus one (the id is plain text)
        // makes a call fail; it is never dereferenced by us.
        // SAFETY: handle validated by Win32 (see above).
        if unsafe { IsWindow(hwnd) } == 0 {
            return Err("window is gone".into());
        }
        if !focus {
            // SAFETY: posting a message takes no pointers.
            unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) };
            return Ok(());
        }
        // SAFETY: plain query on a window handle.
        if unsafe { IsIconic(hwnd) } != 0 {
            // SAFETY: plain call on a window handle.
            unsafe { ShowWindow(hwnd, SW_RESTORE) };
        }
        // SAFETY: plain call on a window handle.
        if unsafe { SetForegroundWindow(hwnd) } == 0 {
            return Err("Windows refused to switch to the window".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[cfg(not(windows))]
    use std::collections::HashMap;

    use super::*;

    #[cfg(not(windows))]
    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |k| m.get(k).cloned()
    }

    #[cfg(not(windows))]
    #[test]
    fn detects_backend_from_env() {
        assert_eq!(
            detect(env(&[
                ("XDG_CURRENT_DESKTOP", "ubuntu:GNOME"),
                ("WAYLAND_DISPLAY", "wayland-0"),
                ("DISPLAY", ":0")
            ])),
            Backend::Gnome
        );
        assert_eq!(
            detect(env(&[("XDG_CURRENT_DESKTOP", "ubuntu:GNOME"), ("XDG_SESSION_TYPE", "x11"), ("DISPLAY", ":0")])),
            Backend::X11
        );
        assert_eq!(detect(env(&[("HYPRLAND_INSTANCE_SIGNATURE", "abc"), ("WAYLAND_DISPLAY", "w")])), Backend::Hyprland);
        assert_eq!(detect(env(&[("SWAYSOCK", "/run/sway.sock"), ("WAYLAND_DISPLAY", "w")])), Backend::Sway);
        assert_eq!(detect(env(&[("NIRI_SOCKET", "/run/niri.sock"), ("WAYLAND_DISPLAY", "w")])), Backend::Niri);
        assert_eq!(detect(env(&[("XDG_CURRENT_DESKTOP", "KDE"), ("WAYLAND_DISPLAY", "w")])), Backend::Unsupported);
        assert_eq!(detect(env(&[])), Backend::Unsupported);
    }

    #[test]
    fn window_calls_via_busctl_and_gdbus() {
        let list = r#"[{"in_current_workspace":true,"wm_class":"firefox","wm_class_instance":"Navigator","pid":42,"id":2973934,"frame_type":0,"window_type":0,"focus":false,"title":"Rust – Mozilla Firefox"},{"wm_class":"Gjs","id":5,"window_type":4,"title":"menu"},{"in_current_workspace":false,"wm_class":"org.gnome.Nautilus","wm_class_instance":"org.gnome.Nautilus","pid":7,"id":9,"window_type":0,"focus":true}]"#;
        let busctl = serde_json::json!({"type":"s","data":[list]}).to_string();
        let parsed = parse_window_calls(&json(&parse_busctl(&busctl).unwrap()).unwrap());
        assert_eq!(parsed.len(), 2, "non-normal window skipped");
        assert_eq!(
            (parsed[0].id.as_str(), parsed[0].title.as_str(), parsed[0].app.as_str(), parsed[0].pid),
            ("2973934", "Rust – Mozilla Firefox", "firefox", Some(42))
        );
        assert_eq!(parsed[0].icon_names, ["firefox", "Navigator", "navigator"]);
        assert_eq!(parsed[1].workspace.as_deref(), Some("other workspace"));
        assert!(parsed[1].focused && parsed[1].title.is_empty());

        let gdbus = r#"('[{"id":1,"title":"It\'s \\"quoted\\" é"}]',)"#;
        let s = parse_gdbus_string(gdbus).unwrap();
        assert_eq!(s, r#"[{"id":1,"title":"It's \"quoted\" é"}]"#);
        assert_eq!(parse_window_calls(&json(&s).unwrap())[0].title, r#"It's "quoted" é"#);
        assert_eq!(parse_gdbus_string("()"), None);
    }

    #[test]
    fn hyprland_clients_in_focus_order() {
        let v = serde_json::json!([
            {"address":"0xa","mapped":true,"workspace":{"id":1,"name":"1"},"class":"kitty","title":"zsh","pid":10,"focusHistoryID":1},
            {"address":"0xb","mapped":true,"workspace":{"id":2,"name":"web"},"class":"","initialClass":"firefox","title":"Home","pid":11,"focusHistoryID":0},
            {"address":"0xc","mapped":false,"class":"x","title":"hidden","pid":12,"focusHistoryID":2}
        ]);
        let w = parse_hyprland(&v);
        assert_eq!(w.iter().map(|w| w.id.as_str()).collect::<Vec<_>>(), ["0xb", "0xa"]);
        assert_eq!(
            (w[0].app.as_str(), w[0].workspace.as_deref(), w[0].focused),
            ("firefox", Some("workspace web"), true)
        );
    }

    #[test]
    fn sway_tree_leaves() {
        let v = serde_json::json!({"id":1,"type":"root","nodes":[
            {"id":2,"type":"output","name":"eDP-1","nodes":[
                {"id":3,"type":"workspace","name":"1","nodes":[
                    {"id":4,"type":"con","name":"vim","app_id":"foot","pid":20,"focused":true,"nodes":[]}
                ],"floating_nodes":[
                    {"id":5,"type":"floating_con","name":"Steam","app_id":null,"pid":21,"window_properties":{"class":"steam"},"nodes":[]}
                ]}
            ]}
        ]});
        let w = parse_sway(&v);
        assert_eq!(w.len(), 2);
        assert_eq!(
            (w[0].id.as_str(), w[0].app.as_str(), w[0].workspace.as_deref(), w[0].focused),
            ("4", "foot", Some("workspace 1"), true)
        );
        assert_eq!((w[1].app.as_str(), w[1].title.as_str()), ("steam", "Steam"));
    }

    #[test]
    fn niri_windows() {
        let v = serde_json::json!([{"id":12,"title":"Files","app_id":"org.gnome.Nautilus","pid":30,"workspace_id":1,"is_focused":false}]);
        let w = parse_niri(&v);
        assert_eq!((w[0].id.as_str(), w[0].app.as_str(), w[0].pid), ("12", "org.gnome.Nautilus", Some(30)));
        assert_eq!(w[0].icon_names, ["org.gnome.Nautilus", "org.gnome.nautilus"]);
    }

    #[test]
    fn wmctrl_lines() {
        let out = "0x03a00003  0 1234   Navigator.firefox     host Rust  -  Mozilla Firefox\n\
                   0x01000007 -1 99     desktop_window.Nautilus  host Desktop\n\
                   0x04400001  1 0      xterm.XTerm  host  \n\
                   garbage\n";
        let w = parse_wmctrl(out);
        assert_eq!(w.len(), 2);
        assert_eq!(
            (w[0].id.as_str(), w[0].app.as_str(), w[0].title.as_str(), w[0].pid),
            ("0x03a00003", "firefox", "Rust  -  Mozilla Firefox", Some(1234))
        );
        assert_eq!((w[1].app.as_str(), w[1].title.as_str(), w[1].pid), ("XTerm", "", None));
    }

    fn win(id: &str, title: &str, app: &str, pid: u32, focused: bool) -> Win {
        Win { id: id.into(), title: title.into(), app: app.into(), pid: Some(pid), focused, ..Default::default() }
    }

    #[test]
    fn a_known_executable_gives_the_row_its_icon() {
        let exe = r"C:\Apps\editor.exe";
        let wins = [
            Win { exe: Some(exe.into()), ..win("1", "Notes", "editor", 5, false) },
            win("2", "Shell", "term", 6, false),
        ];
        let rows = rank(&wins, "", false, 8, 99);
        let icon = |id: &str| rows.iter().find(|r| r.id == id).and_then(|r| r.icon.clone()).unwrap();
        assert_eq!(icon("win:1").id.as_deref(), Some(exe), "fetched lazily, by path");
        assert!(icon("win:2").id.is_none(), "no executable: the icon theme");
    }

    #[test]
    fn ranking_filters_and_orders() {
        let wins = [
            win("1", "Rust – Mozilla Firefox", "firefox", 1, true),
            win("2", "notes.md - VS Code", "code", 2, false),
            win("3", "kairos", "kairos", 99, false),
        ];
        let all = rank(&wins, "", false, 8, 99);
        assert_eq!(
            all.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["win:2", "win:1"],
            "own window skipped, focused last"
        );
        let fx = rank(&wins, "firefox", false, 8, 99);
        assert_eq!(fx.len(), 1);
        assert_eq!((fx[0].title.as_str(), fx[0].subtitle.as_str()), ("Rust – Mozilla Firefox", "Window · firefox"));
        assert_eq!(fx[0].default_action().unwrap().id, "focus");
        let global = rank(&wins, "firefox", true, 8, 99);
        assert!(global[0].score < fx[0].score, "global rows rank below apps");
    }

    #[test]
    fn hints_when_windows_cannot_be_listed() {
        let p = WindowsProvider::with_backend(Backend::Unsupported);
        let q = |text, trigger| Query { text, trigger, limit: 8 };
        assert!(p.query(&q("firefox", None)).is_empty(), "nothing in global search");
        let r = p.query(&q("", Some("win ")));
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].id, "hint");
        let g = WindowsProvider::with_backend(Backend::Gnome);
        assert_eq!(g.hint(&BackendError::Failed("x".into())).default_action().unwrap().id, "open");
        let x = WindowsProvider::with_backend(Backend::X11);
        assert_eq!(x.hint(&BackendError::Missing("wmctrl".into())).title, "Install wmctrl to switch windows");
    }
}
