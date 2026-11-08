//! Find and stop your own processes: `kill fire`, `kill 1234` (exact PID), or a bare `kill `
//! for the biggest memory users. Processes that belong to a launched app (GNOME, KDE, snap and
//! flatpak record it in the cgroup scope) are shown as that app, with its name and icon; the
//! rest are grouped by process name. Linux reads `/proc`; Windows uses the Win32 process APIs
//! (Terminate closes a process's windows, as `taskkill` without `/F` does).

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::core::sync::lock;
use crate::core::{fuzzy, icons};
use crate::plugin;
use crate::plugin::{Error, ExecOutcome, Icon, Provider, Query, ResultItem};

const ID: &str = "builtin-processes";
const TRIGGER: &str = "kill ";
/// A list younger than this is reused instead of scanned again.
const FRESH_FOR: Duration = Duration::from_secs(1);
/// How long Terminate waits for a process to exit before offering Force kill.
const GRACE: Duration = Duration::from_secs(2);
const STILL_RUNNING: &str = "Still running · Enter force-kills";
/// A failed window listing (the desktop can't tell) is retried this rarely.
const WINDOWS_RETRY: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, PartialEq)]
pub struct Proc {
    pub pid: u32,
    pub ppid: u32,
    pub name: String,
    /// Resident memory in bytes.
    pub rss: u64,
    /// Desktop file id of the app this process belongs to (`firefox_firefox.desktop`).
    pub app: Option<String>,
    /// Full path of the executable, where its icon comes from (Windows only).
    pub exe: Option<String>,
}

/// What a desktop file says about an app.
#[derive(Clone, Debug)]
struct AppInfo {
    name: String,
    icon: Option<Icon>,
}

type Apps = HashMap<String, Option<AppInfo>>;

/// PIDs that own a window, `None` when the desktop can't list windows, with when it was asked.
type Windows = (Option<Instant>, Option<HashSet<u32>>);

pub struct ProcessesProvider {
    cache: Mutex<(Option<Instant>, Vec<Proc>)>,
    /// Desktop id → app, read on demand for the few apps that are running.
    apps: Mutex<Apps>,
    /// A result whose Terminate did not end it; its row then offers Force kill first.
    pending: Mutex<Option<String>>,
    windows: Mutex<Windows>,
    /// Whether to ask the desktop for windows at all (off in tests, which need no desktop).
    list_windows: bool,
    grace: Duration,
}

impl ProcessesProvider {
    pub fn new() -> Self {
        Self::with_grace(GRACE)
    }

    fn with_grace(grace: Duration) -> Self {
        Self {
            cache: Mutex::new((None, vec![])),
            apps: Mutex::new(HashMap::new()),
            pending: Mutex::new(None),
            windows: Mutex::new((None, None)),
            list_windows: !cfg!(test),
            grace,
        }
    }

    fn procs(&self) -> Vec<Proc> {
        let mut g = lock(&self.cache);
        if g.0.is_none_or(|t| t.elapsed() > FRESH_FOR) {
            *g = (Some(Instant::now()), list());
        }
        g.1.clone()
    }

    /// The current processes plus the apps they belong to.
    fn snapshot(&self, fresh: bool) -> (Vec<Proc>, Apps) {
        if fresh {
            lock(&self.cache).0 = None;
        }
        let procs = self.procs();
        let mut apps = lock(&self.apps);
        for id in procs.iter().filter_map(|p| p.app.as_deref()) {
            if !apps.contains_key(id) {
                apps.insert(id.to_string(), app_info(id));
            }
        }
        (procs, apps.clone())
    }

    /// PIDs with an open window, refreshed like the process list. `None`: unknown.
    fn window_pids(&self) -> Option<HashSet<u32>> {
        if !self.list_windows {
            return None;
        }
        let mut g = lock(&self.windows);
        let ttl = if g.1.is_some() { FRESH_FOR } else { WINDOWS_RETRY };
        if g.0.is_none_or(|t| t.elapsed() > ttl) {
            *g = (Some(Instant::now()), crate::builtin::windows::window_pids());
        }
        g.1.clone()
    }

    /// Send `targets` SIGTERM and wait for them. False if some are still running after the grace time.
    fn terminate(&self, targets: &[u32], result_id: &str) -> plugin::Result<bool> {
        let done = targets.iter().filter(|&&pid| kill(pid, false)).count();
        lock(&self.cache).0 = None;
        log::info!("{ID}: terminated {done}/{} for {result_id}", targets.len());
        if done == 0 {
            // Windows closes a process through its windows: one without any can only be
            // force-killed, so offer that right away.
            return if cfg!(windows) { Ok(false) } else { Err("permission denied".into()) };
        }
        Ok(self.wait_gone(targets))
    }

    /// True once none of `pids` is running any more, false if some still are after the grace time.
    fn wait_gone(&self, pids: &[u32]) -> bool {
        let end = Instant::now() + self.grace;
        loop {
            if !pids.iter().any(|&p| alive(p)) {
                return true;
            }
            if Instant::now() >= end {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Default for ProcessesProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl Provider for ProcessesProvider {
    fn id(&self) -> &str {
        ID
    }

    /// Hidden: hold nothing while nobody looks.
    fn reap_idle(&self) {
        *lock(&self.cache) = (None, vec![]);
        lock(&self.apps).clear();
        *lock(&self.pending) = None;
        *lock(&self.windows) = (None, None);
        // Keep the folder of extracted executable icons from growing without end.
        #[cfg(windows)]
        crate::icon_store::IconStore::prune(&crate::core::icons::win::cache_dir(crate::core::icons::win::EXE_ICONS));
    }

    /// Windows: the icon of an executable (the id is its path, from a row of ours).
    #[cfg(windows)]
    fn icon(&self, icon_id: &str) -> Option<Icon> {
        icons::win::exe_icon(icon_id)
    }

    fn query(&self, q: &Query) -> Vec<ResultItem> {
        let (procs, apps) = self.snapshot(false);
        let pending = lock(&self.pending).clone();
        // Only app rows are marked, so only ask the desktop when there are apps.
        let windows = if procs.iter().any(|p| p.app.is_some()) { self.window_pids() } else { None };
        rank(&procs, &apps, q.text.trim(), q.limit, pending.as_deref(), windows.as_ref())
    }

    fn execute(&self, result_id: &str, action_id: &str, query: &str) -> plugin::Result<ExecOutcome> {
        let force = match action_id {
            "terminate" => false,
            "force" => true,
            _ => return Err(Error::UnknownAction(action_id.into())),
        };
        // Fresh PIDs, so a stale list never signals a recycled PID.
        let (procs, apps) = self.snapshot(true);
        let group = members(&procs, &apps, result_id)?;
        if group.is_empty() {
            *lock(&self.pending) = None;
            return Err("process is no longer running".into());
        }
        if force {
            let pids: Vec<u32> = group.iter().map(|p| p.pid).collect();
            let done = pids.iter().filter(|&&pid| kill(pid, true)).count();
            lock(&self.cache).0 = None;
            *lock(&self.pending) = None;
            log::info!("{ID}: force-killed {done}/{} for {result_id}", pids.len());
            return if done == 0 { Err("permission denied".into()) } else { Ok(ExecOutcome::Close) };
        }
        // Terminate asks the top processes first, so an app can close its helpers itself, then
        // whatever of the group outlived them (children are not ended with their parent).
        let mut gone = self.terminate(&roots(&group), result_id)?;
        if gone {
            let (procs, apps) = self.snapshot(true);
            let rest: Vec<u32> = members(&procs, &apps, result_id)?.iter().map(|p| p.pid).collect();
            if !rest.is_empty() {
                // Windows: helpers usually follow their closed main window, and most have no
                // window to close, so give them the grace time first.
                gone = (cfg!(windows) && self.wait_gone(&rest)) || self.terminate(&rest, result_id)?;
            }
        }
        if gone {
            *lock(&self.pending) = None;
            return Ok(ExecOutcome::Close);
        }
        // Still there: stay open and show the row again with Force kill as its default.
        *lock(&self.pending) = Some(result_id.to_string());
        Ok(ExecOutcome::complete(format!("{TRIGGER}{query}")))
    }
}

/// The result id a process is listed under.
fn group_id(p: &Proc, apps: &Apps) -> String {
    match &p.app {
        Some(id) if matches!(apps.get(id), Some(Some(_))) => format!("app:{id}"),
        _ => format!("name:{}", p.name),
    }
}

/// The processes behind a result id.
fn members<'a>(procs: &'a [Proc], apps: &Apps, result_id: &str) -> plugin::Result<Vec<&'a Proc>> {
    match result_id.split_once(':') {
        Some(("pid", v)) => {
            let pid: u32 = v.parse().map_err(|_| Error::UnknownResult(result_id.into()))?;
            Ok(procs.iter().filter(|p| p.pid == pid).collect())
        }
        Some(("app" | "name", _)) => Ok(procs.iter().filter(|p| group_id(p, apps) == result_id).collect()),
        _ => Err(Error::UnknownResult(result_id.into())),
    }
}

/// Members whose parent is not in the group: the processes that own the rest.
fn roots(members: &[&Proc]) -> Vec<u32> {
    let pids: HashSet<u32> = members.iter().map(|p| p.pid).collect();
    members.iter().filter(|p| !pids.contains(&p.ppid)).map(|p| p.pid).collect()
}

/// Result rows for `text`: an exact PID, or groups (apps, then process names) by best match,
/// then by memory.
/// `windows` marks app rows none of whose processes has a window (when the desktop can tell).
fn rank(
    procs: &[Proc],
    apps: &Apps,
    text: &str,
    limit: usize,
    pending: Option<&str>,
    windows: Option<&HashSet<u32>>,
) -> Vec<ResultItem> {
    let generic = || icons::themed(&["application-x-executable", "utilities-system-monitor", "process-stop"], "×");
    let app_of = |p: &Proc| p.app.as_ref().and_then(|id| apps.get(id)).and_then(Option::as_ref);
    // The executable's icon, asked for lazily so only shown rows pay for it (Windows).
    let exe_icon = |p: &Proc| p.exe.clone().map(|exe| Icon { id: Some(exe), ..Default::default() });
    let finish = |item: ResultItem| {
        if pending == Some(item.id.as_str()) {
            ResultItem { subtitle: STILL_RUNNING.into(), actions: vec![], ..item }
                .with_action("force", "Force kill")
                .with_action("terminate", "Terminate (graceful)")
        } else {
            item.with_action("terminate", "Terminate (graceful)").with_action("force", "Force kill")
        }
    };

    if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) {
        return procs
            .iter()
            .filter(|p| text.parse() == Ok(p.pid))
            .map(|p| {
                let icon = app_of(p).and_then(|a| a.icon.clone()).or_else(|| exe_icon(p)).unwrap_or_else(generic);
                let title = format!("{} (pid {})", p.name, p.pid);
                finish(ResultItem::new(format!("pid:{}", p.pid), title, human(p.rss), 1.0).with_icon(icon))
            })
            .collect();
    }

    let mut groups: HashMap<String, Vec<&Proc>> = HashMap::new();
    for p in procs {
        groups.entry(group_id(p, apps)).or_default().push(p);
    }
    let mut ranked: Vec<(f64, u64, String, Vec<&Proc>)> = groups
        .into_iter()
        .filter_map(|(id, members)| {
            let score = if text.is_empty() {
                0.5
            } else {
                let by_comm = members.iter().filter_map(|m| fuzzy::score(text, &m.name)).reduce(f64::max);
                match app_of(members[0]).filter(|_| id.starts_with("app:")) {
                    // An app is found by its name, its desktop id or any of its processes.
                    Some(a) => {
                        let stem = id.trim_start_matches("app:").trim_end_matches(".desktop");
                        [
                            fuzzy::score(text, &a.name),
                            fuzzy::score(text, stem).map(|s| s * 0.9),
                            by_comm.map(|s| s * 0.8),
                        ]
                        .into_iter()
                        .flatten()
                        .reduce(f64::max)?
                    }
                    None => by_comm?,
                }
            };
            Some((score, members.iter().map(|m| m.rss).sum(), id, members))
        })
        .collect();
    ranked.sort_by(|a, b| b.0.total_cmp(&a.0).then(b.1.cmp(&a.1)).then(a.2.cmp(&b.2)));
    ranked.truncate(limit);
    ranked
        .into_iter()
        .map(|(score, rss, id, members)| {
            let app = app_of(members[0]).filter(|_| id.starts_with("app:"));
            let title = app.map_or_else(|| members[0].name.clone(), |a| a.name.clone());
            let icon = app.and_then(|a| a.icon.clone()).or_else(|| exe_icon(members[0])).unwrap_or_else(generic);
            let mut detail = if members.len() > 1 {
                format!("{} processes", members.len())
            } else {
                format!("pid {}", members[0].pid)
            };
            if app.is_some() && windows.is_some_and(|w| !members.iter().any(|m| w.contains(&m.pid))) {
                detail = format!("No window · {detail}");
            }
            finish(ResultItem::new(id, title, format!("{detail} · {}", human(rss)), score).with_icon(icon))
        })
        .collect()
}

fn human(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut n = bytes as f64 / 1024.0;
    for unit in ["KB", "MB"] {
        if n < 1024.0 {
            return format!("{n:.1} {unit}");
        }
        n /= 1024.0;
    }
    format!("{n:.1} GB")
}

/// PIDs never offered: the idle/init/system processes and the launcher itself.
fn protected(pid: u32) -> bool {
    matches!(pid, 0 | 1) || (cfg!(windows) && pid == 4) || pid == std::process::id()
}

/// The desktop file id a cgroup unit name stands for, following systemd's naming for apps:
/// `app[-<launcher>]-<id>-<random>.scope`, `app[-<launcher>]-<id>[@<random>].service`, and
/// snapd's `snap.<snap>.<app>-<uuid>.scope`. Other units (services, terminal tabs) are no app.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn scope_app(unit: &str) -> Option<String> {
    if let Some(rest) = unit.strip_prefix("snap.").and_then(|r| r.strip_suffix(".scope")) {
        let (snap, app) = rest.split_once('.')?;
        let app = strip_uuid(app)?;
        return Some(format!("{snap}_{app}.desktop"));
    }
    let rest = unit.strip_prefix("app-")?;
    let named = if let Some(s) = rest.strip_suffix(".scope") {
        let (head, random) = s.rsplit_once('-')?;
        if random.is_empty() {
            return None;
        }
        head
    } else {
        let s = rest.strip_suffix(".service")?;
        s.split_once('@').map_or(s, |(head, _)| head)
    };
    // Literal dashes inside the id are escaped (`\x2d`), so a raw dash ends the launcher name.
    let id = named.split_once('-').map_or(named, |(_launcher, id)| id);
    let id = unescape(id);
    (!id.is_empty()).then(|| format!("{id}.desktop"))
}

/// `firefox-f7c9c548-dd48-4843-9381-1248813380f7` (or `.` before the uuid) → `firefox`.
fn strip_uuid(s: &str) -> Option<&str> {
    let cut = s.len().checked_sub(37)?;
    let (head, tail) = s.split_at_checked(cut)?;
    let uuid = &tail[1..];
    let shaped = tail.starts_with(['-', '.'])
        && uuid.split('-').map(str::len).eq([8, 4, 4, 4, 12])
        && uuid.chars().all(|c| c == '-' || c.is_ascii_hexdigit());
    (shaped && !head.is_empty()).then_some(head)
}

/// Undo systemd's `\xNN` unit name escaping.
fn unescape(s: &str) -> String {
    let mut out = Vec::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\'
            && b.get(i + 1) == Some(&b'x')
            && let Some(v) = s.get(i + 2..i + 4).and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Name and icon from the app's desktop file; `None` if there is none or it is hidden.
#[cfg(unix)]
fn app_info(id: &str) -> Option<AppInfo> {
    // The first folder that has the file decides, as for the app launcher.
    let text = crate::builtin::apps::desktop_dirs().iter().find_map(|d| std::fs::read_to_string(d.join(id)).ok())?;
    let app = crate::builtin::apps::parse_desktop(id, &text)?;
    Some(AppInfo { icon: app.icon.as_deref().and_then(icons::app_icon), name: app.name })
}

#[cfg(not(unix))]
fn app_info(_id: &str) -> Option<AppInfo> {
    None
}

// ---------------------------------------------------------------- per platform

#[cfg(target_os = "linux")]
fn list() -> Vec<Proc> {
    use std::os::unix::fs::MetadataExt;
    let Ok(uid) = std::fs::metadata("/proc/self").map(|m| m.uid()) else { return vec![] };
    let Ok(entries) = std::fs::read_dir("/proc") else { return vec![] };
    // Apps the launcher starts share its scope; they must not all be shown as the launcher.
    let own_unit = std::fs::read_to_string("/proc/self/cgroup").ok().and_then(|c| unit(&c).map(str::to_string));
    entries
        .flatten()
        .filter_map(|e| {
            let pid: u32 = e.file_name().to_str()?.parse().ok()?;
            if protected(pid) || e.metadata().ok()?.uid() != uid {
                return None;
            }
            let dir = e.path();
            // Kernel threads and zombies have no command line. Any read can fail because
            // the process exited meanwhile; it is simply skipped.
            if std::fs::read(dir.join("cmdline")).ok()?.is_empty() {
                return None;
            }
            let name = std::fs::read_to_string(dir.join("comm")).ok()?.trim_end().to_string();
            let status = std::fs::read_to_string(dir.join("status")).ok()?;
            let app = std::fs::read_to_string(dir.join("cgroup"))
                .ok()
                .and_then(|c| unit(&c).filter(|u| Some(*u) != own_unit.as_deref()).and_then(scope_app));
            Some(Proc {
                pid,
                ppid: status_field(&status, "PPid:").and_then(|v| v.parse().ok()).unwrap_or(0),
                name,
                rss: status_field(&status, "VmRSS:").and_then(|v| v.parse::<u64>().ok()).map_or(0, |kb| kb * 1024),
                app,
                exe: None,
            })
        })
        .collect()
}

/// The unit a process runs in: the last part of its cgroup v2 path.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn unit(cgroup: &str) -> Option<&str> {
    cgroup.lines().find_map(|l| l.strip_prefix("0::"))?.rsplit('/').next().filter(|u| !u.is_empty())
}

/// The first word after `key` in `/proc/<pid>/status` (`VmRSS:  1234 kB` → `1234`).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn status_field<'a>(status: &'a str, key: &str) -> Option<&'a str> {
    status.lines().find_map(|l| l.strip_prefix(key))?.split_whitespace().next()
}

/// Whether `pid` still runs (an exited process waiting to be reaped does not).
#[cfg(target_os = "linux")]
fn alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/status"))
        .is_ok_and(|s| status_field(&s, "State:").is_some_and(|st| st != "Z" && st != "X"))
}

#[cfg(target_os = "linux")]
fn kill(pid: u32, force: bool) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else { return false };
    match sys::kill(pid, if force { libc::SIGKILL } else { libc::SIGTERM }) {
        Ok(()) => true,
        Err(e) => e.raw_os_error() == Some(libc::ESRCH), // already gone counts as done
    }
}

#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
mod sys {
    pub fn kill(pid: libc::pid_t, sig: libc::c_int) -> std::io::Result<()> {
        // SAFETY: kill(2) takes plain integers and touches no memory of ours.
        if unsafe { libc::kill(pid, sig) } == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
    }
}

#[cfg(windows)]
use win::{alive, kill, list};

#[cfg(windows)]
#[allow(unsafe_code)]
mod win {
    //! Thin wrappers over the Win32 process APIs. Every `unsafe` block is a single FFI call with
    //! plain values or pointers to locals; handles are closed by `OwnedHandle`.

    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

    use windows_sys::Win32::Foundation::{
        ERROR_INVALID_PARAMETER, GetLastError, HWND, INVALID_HANDLE_VALUE, LPARAM, TRUE, WAIT_TIMEOUT,
    };
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_ACCESS_RIGHTS, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        PROCESS_TERMINATE, QueryFullProcessImageNameW, TerminateProcess, WaitForSingleObject,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowThreadProcessId, IsWindowVisible, PostMessageW, WM_CLOSE,
    };
    use windows_sys::core::BOOL;

    use super::{Proc, protected};

    fn open(pid: u32, access: PROCESS_ACCESS_RIGHTS) -> Option<OwnedHandle> {
        // SAFETY: plain call; a null result is checked.
        let raw = unsafe { OpenProcess(access, 0, pid) };
        // SAFETY: `raw` is a valid process handle that nothing else owns.
        (!raw.is_null()).then(|| unsafe { OwnedHandle::from_raw_handle(raw) })
    }

    fn session(pid: u32) -> Option<u32> {
        let mut s = 0u32;
        // SAFETY: the out pointer is a live local.
        (unsafe { ProcessIdToSessionId(pid, &raw mut s) } != 0).then_some(s)
    }

    /// The processes of our own desktop session (the closest match to "my processes").
    pub fn list() -> Vec<Proc> {
        let Some(own) = session(std::process::id()) else { return vec![] };
        // SAFETY: plain call; the result is checked.
        let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if raw.is_null() || raw == INVALID_HANDLE_VALUE {
            return vec![];
        }
        // SAFETY: `raw` is a valid snapshot handle that nothing else owns.
        let snap = unsafe { OwnedHandle::from_raw_handle(raw) };
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut out = vec![];
        // SAFETY: `e` is a live local with `dwSize` set, as the API requires.
        let mut more = unsafe { Process32FirstW(snap.as_raw_handle(), &raw mut e) } != 0;
        while more {
            let pid = e.th32ProcessID;
            if !protected(pid) && session(pid) == Some(own) {
                let h = open(pid, PROCESS_QUERY_LIMITED_INFORMATION);
                let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
                out.push(Proc {
                    pid,
                    ppid: e.th32ParentProcessID,
                    name: String::from_utf16_lossy(&e.szExeFile[..len]),
                    rss: rss(h.as_ref()),
                    app: None,
                    exe: h.as_ref().and_then(exe_path),
                });
            }
            // SAFETY: as above.
            more = unsafe { Process32NextW(snap.as_raw_handle(), &raw mut e) } != 0;
        }
        out
    }

    /// Working set in bytes, 0 if the process cannot be asked.
    fn rss(h: Option<&OwnedHandle>) -> u64 {
        let Some(h) = h else { return 0 };
        let mut c = PROCESS_MEMORY_COUNTERS::default();
        let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        // SAFETY: `c` is a live local of `size` bytes.
        if unsafe { K32GetProcessMemoryInfo(h.as_raw_handle(), &raw mut c, size) } == 0 {
            return 0;
        }
        c.WorkingSetSize as u64
    }

    /// Full path of the process's executable.
    fn exe_path(h: &OwnedHandle) -> Option<String> {
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        // SAFETY: `buf` holds `len` u16s; `len` is updated to the characters written.
        let ok = unsafe {
            QueryFullProcessImageNameW(h.as_raw_handle(), PROCESS_NAME_WIN32, buf.as_mut_ptr(), &raw mut len)
        };
        (ok != 0).then(|| String::from_utf16_lossy(&buf[..(len as usize).min(buf.len())]))
    }

    /// Whether `pid` still runs. A process we may not open counts as gone.
    pub fn alive(pid: u32) -> bool {
        // SAFETY: a zero-timeout wait on a handle we own.
        open(pid, PROCESS_SYNCHRONIZE)
            .is_some_and(|h| unsafe { WaitForSingleObject(h.as_raw_handle(), 0) } == WAIT_TIMEOUT)
    }

    /// Force: end the process. Otherwise ask its visible windows to close; false if it has none.
    pub fn kill(pid: u32, force: bool) -> bool {
        if !force {
            return close_windows(pid);
        }
        match open(pid, PROCESS_TERMINATE) {
            // SAFETY: plain call on a handle we own.
            Some(h) => (unsafe { TerminateProcess(h.as_raw_handle(), 1) }) != 0,
            // A PID that no longer exists: already gone counts as done.
            // SAFETY: plain call.
            None => (unsafe { GetLastError() }) == ERROR_INVALID_PARAMETER,
        }
    }

    fn close_windows(pid: u32) -> bool {
        struct Search {
            pid: u32,
            found: Vec<HWND>,
        }
        unsafe extern "system" fn visit(hwnd: HWND, lp: LPARAM) -> BOOL {
            // SAFETY: `lp` is the `&mut Search` that `close_windows` passes to EnumWindows,
            // which calls this only while that borrow is alive.
            let s = unsafe { &mut *(lp as *mut Search) };
            let mut owner = 0u32;
            // SAFETY: the out pointer is a live local; `hwnd` comes from EnumWindows.
            unsafe { GetWindowThreadProcessId(hwnd, &raw mut owner) };
            // SAFETY: plain query on a window handle.
            if owner == s.pid && unsafe { IsWindowVisible(hwnd) } != 0 {
                s.found.push(hwnd);
            }
            TRUE
        }
        let mut s = Search { pid, found: vec![] };
        // SAFETY: `visit` matches WNDENUMPROC and `s` outlives the call.
        unsafe { EnumWindows(Some(visit), (&raw mut s) as LPARAM) };
        for &hwnd in &s.found {
            // SAFETY: posting a message takes no pointers; Win32 validates the handle.
            unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) };
        }
        !s.found.is_empty()
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
fn list() -> Vec<Proc> {
    vec![]
}

#[cfg(not(any(target_os = "linux", windows)))]
fn kill(_pid: u32, _force: bool) -> bool {
    false
}

/// No way to tell: report it gone, so the launcher closes.
#[cfg(not(any(target_os = "linux", windows)))]
fn alive(_pid: u32) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: u32, name: &str, rss: u64) -> Proc {
        Proc { pid, ppid: 1, name: name.into(), rss, app: None, exe: None }
    }

    fn in_app(pid: u32, ppid: u32, name: &str, app: &str) -> Proc {
        Proc { pid, ppid, name: name.into(), rss: 100, app: Some(app.into()), exe: None }
    }

    fn apps(list: &[(&str, &str)]) -> Apps {
        list.iter().map(|(id, name)| (id.to_string(), Some(AppInfo { name: name.to_string(), icon: None }))).collect()
    }

    #[test]
    fn digits_find_one_pid_and_names_are_grouped() {
        let procs = [p(10, "firefox", 100), p(11, "firefox", 200), p(12, "fish", 5)];
        let rows = rank(&procs, &Apps::new(), "11", 8, None, None);
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].id.as_str(), rows[0].title.as_str()), ("pid:11", "firefox (pid 11)"));
        assert!(rank(&procs, &Apps::new(), "99", 8, None, None).is_empty());

        let rows = rank(&procs, &Apps::new(), "firef", 8, None, None);
        assert_eq!(rows[0].id, "name:firefox");
        assert!(rows[0].subtitle.starts_with("2 processes"), "{}", rows[0].subtitle);
        assert_eq!(rows[0].default_action().unwrap().id, "terminate");
        assert!(rows[0].actions.iter().any(|a| a.id == "force"));
        assert!(rank(&procs, &Apps::new(), "zzz", 8, None, None).is_empty());
    }

    #[test]
    fn empty_query_lists_the_biggest_first() {
        let procs = [p(2, "small", 10), p(3, "big", 1000), p(4, "mid", 100), p(5, "mid", 100)];
        let titles: Vec<_> = rank(&procs, &Apps::new(), "", 2, None, None).into_iter().map(|r| r.title).collect();
        assert_eq!(titles, ["big", "mid"]);
    }

    #[test]
    fn app_processes_are_one_row_named_after_the_app() {
        let ff = "firefox_firefox.desktop";
        let procs = [
            in_app(100, 1, "firefox", ff),
            in_app(101, 100, "Web Content", ff),
            in_app(102, 100, "Isolated Web Co", ff),
            in_app(103, 102, "Web Content", ff),
            p(200, "bash", 10),
            // A desktop file that could not be read: grouped by process name instead.
            in_app(300, 1, "notifier", "gone.desktop"),
        ];
        let mut known_gone = apps(&[(ff, "Firefox")]);
        known_gone.insert("gone.desktop".into(), None);

        for text in ["fire", "web content", "firefox"] {
            let rows = rank(&procs, &known_gone, text, 8, None, None);
            assert_eq!(rows[0].id, format!("app:{ff}"), "{text}");
            assert_eq!(rows[0].title, "Firefox");
            assert!(rows[0].subtitle.starts_with("4 processes"), "{}", rows[0].subtitle);
        }
        let all = rank(&procs, &known_gone, "", 8, None, None);
        assert!(all.iter().any(|r| r.id == "name:notifier"));
        assert!(all.iter().any(|r| r.id == "name:bash"));

        let members = members(&procs, &known_gone, &format!("app:{ff}")).unwrap();
        assert_eq!(members.len(), 4);
        assert_eq!(roots(&members), [100], "only the main process is asked to quit");
    }

    #[test]
    fn apps_without_a_window_are_marked_when_the_desktop_can_tell() {
        let (ff, idea) = ("firefox.desktop", "idea.desktop");
        let procs =
            [in_app(10, 1, "firefox", ff), in_app(11, 10, "Web Content", ff), in_app(20, 1, "cef_server", idea)];
        let known = apps(&[(ff, "Firefox"), (idea, "IntelliJ IDEA")]);
        let sub = |windows: Option<&HashSet<u32>>, id: &str| {
            let rows = rank(&procs, &known, "", 8, None, windows);
            rows.into_iter().find(|r| r.id == format!("app:{id}")).unwrap().subtitle
        };
        let wins = HashSet::from([10]);
        assert!(sub(Some(&wins), idea).starts_with("No window · pid 20"), "{}", sub(Some(&wins), idea));
        assert!(sub(Some(&wins), ff).starts_with("2 processes"), "has a window");
        assert!(sub(None, idea).starts_with("pid 20"), "unknown: no mark");
    }

    #[test]
    fn a_row_that_survived_terminate_offers_force_first() {
        let procs = [p(10, "stubborn", 100)];
        let rows = rank(&procs, &Apps::new(), "stub", 8, Some("name:stubborn"), None);
        assert_eq!(rows[0].subtitle, STILL_RUNNING);
        assert_eq!(rows[0].default_action().unwrap().id, "force");
        assert_eq!(rows[0].actions.len(), 2);
    }

    #[test]
    fn scope_names_map_to_desktop_files() {
        let cases = [
            ("app-gnome-firefox-1234.scope", Some("firefox.desktop")),
            ("app-gnome-update\\x2dnotifier-8312.scope", Some("update-notifier.desktop")),
            (
                "app-gnome-org.gnome.Evolution\\x2dalarm\\x2dnotify-6074.scope",
                Some("org.gnome.Evolution-alarm-notify.desktop"),
            ),
            ("app-flatpak-org.mozilla.firefox-5555.scope", Some("org.mozilla.firefox.desktop")),
            ("app-org.kde.dolphin@a1b2c3.service", Some("org.kde.dolphin.desktop")),
            ("snap.firefox.firefox-f7c9c548-dd48-4843-9381-1248813380f7.scope", Some("firefox_firefox.desktop")),
            (
                "snap.intellij-idea-ultimate.intellij-idea-ultimate-b695ee48-2d58-4e9d-828b-37e19af16b6e.scope",
                Some("intellij-idea-ultimate_intellij-idea-ultimate.desktop"),
            ),
            ("snap.snapd-desktop-integration.snapd-desktop-integration.service", None),
            ("vte-spawn-7d730406-3a95-4071-8ed3-7ef47d5f083a.scope", None),
            ("pipewire.service", None),
            ("init.scope", None),
        ];
        for (u, want) in cases {
            assert_eq!(scope_app(u).as_deref(), want, "{u}");
        }
        let cg = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-gnome-firefox-1.scope\n";
        assert_eq!(unit(cg), Some("app-gnome-firefox-1.scope"));
        assert_eq!(unit("12:pids:/x\n"), None, "cgroup v1 only: no app");
    }

    #[test]
    fn sizes_and_parsers() {
        assert_eq!(human(512), "512 B");
        assert_eq!(human(1536), "1.5 KB");
        assert_eq!(human(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(human(3 * 1024 * 1024 * 1024), "3.0 GB");
        let status = "Name:\tx\nState:\tS (sleeping)\nPPid:\t42\nVmRSS:\t    2048 kB\n";
        assert_eq!(status_field(status, "VmRSS:"), Some("2048"));
        assert_eq!(status_field(status, "PPid:"), Some("42"));
        assert_eq!(status_field(status, "State:"), Some("S"));
        assert_eq!(status_field("Name:\tkthreadd\n", "VmRSS:"), None);
    }

    #[test]
    fn rows_without_an_app_icon_ask_for_the_executables_icon() {
        let exe = r"C:\Apps\tool.exe";
        let procs = [Proc { exe: Some(exe.into()), ..p(10, "tool.exe", 5) }, p(11, "other", 5)];
        let rows = rank(&procs, &HashMap::new(), "", 8, None, None);
        let icon = |id: &str| rows.iter().find(|r| r.id == id).and_then(|r| r.icon.clone()).unwrap();
        assert_eq!(icon("name:tool.exe").id.as_deref(), Some(exe), "fetched lazily, by path");
        assert!(icon("name:other").id.is_none(), "no executable path: the generic icon");
        let by_pid = rank(&procs, &HashMap::new(), "10", 8, None, None);
        assert_eq!(by_pid[0].icon.as_ref().and_then(|i| i.id.as_deref()), Some(exe));
    }

    #[test]
    fn unknown_ids_and_actions_are_errors() {
        let pr = ProcessesProvider::new();
        assert!(pr.execute("bogus", "terminate", "").is_err());
        assert!(pr.execute("pid:x", "terminate", "").is_err());
        assert!(pr.execute("name:a", "explode", "").is_err());
    }

    /// Starts and signals sacrificial processes. Each runs as a symlink with a unique name,
    /// so `comm` is unique to it.
    #[cfg(target_os = "linux")]
    mod live {
        use super::*;
        use std::os::unix::fs::symlink;
        use std::os::unix::process::ExitStatusExt;
        use std::path::PathBuf;
        use std::process::{Child, Command, Stdio};

        fn find_in_path(bin: &str) -> Option<PathBuf> {
            std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(bin)).find(|p| p.is_file())
        }

        /// Run `target` under the name `link_name`; the guard owns the symlink's folder.
        fn spawn_named(link_name: &str, target: &str, args: &[&str]) -> Option<(Child, tempfile::TempDir)> {
            let dir = crate::test_support::tmp();
            let link = dir.path().join(link_name);
            symlink(find_in_path(target)?, &link).ok()?;
            let mut child = Command::new(&link).args(args).stdin(Stdio::null()).stdout(Stdio::null()).spawn().ok()?;
            std::thread::sleep(Duration::from_millis(300)); // let exec finish so comm is the link name
            // A multicall binary (uutils coreutils) picks its tool from argv[0] and exits
            // when renamed; skip instead of testing a process that is already gone.
            if child.try_wait().ok()?.is_some() {
                return None;
            }
            Some((child, dir))
        }

        fn wait_exit(child: &mut Child, ms: u64) -> Option<std::process::ExitStatus> {
            let end = Instant::now() + Duration::from_millis(ms);
            while Instant::now() < end {
                if let Some(st) = child.try_wait().unwrap() {
                    return Some(st);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            None
        }

        fn provider() -> ProcessesProvider {
            ProcessesProvider::with_grace(Duration::from_millis(400))
        }

        fn find(p: &ProcessesProvider, text: &str) -> Vec<ResultItem> {
            p.query(&Query { text, trigger: Some(TRIGGER), limit: 8 })
        }

        /// Sleeps in a loop and ignores SIGTERM.
        const STUBBORN: &str = "trap '' TERM; while :; do sleep 1; done";
        /// Sleeps in a loop (a loop, so sh can't exec into `sleep` and lose its name).
        const IDLE: &str = "while :; do sleep 1; done";

        #[test]
        fn terminate_ends_a_matching_process_and_ignores_others() {
            let p = provider();
            let name = format!("zzkt{}", std::process::id() % 100_000);
            let Some((mut victim, _dir)) = spawn_named(&name, "sh", &["-c", IDLE]) else { return };
            let (mut bystander, _bdir) = spawn_named(&format!("{name}x"), "sh", &["-c", IDLE]).unwrap();

            let items = find(&p, &name);
            let hit = items.iter().find(|i| i.id == format!("name:{name}")).expect("victim listed");
            assert!(hit.subtitle.contains(&format!("pid {}", victim.id())), "{}", hit.subtitle);

            // The test is the victim's parent, so it only becomes a zombie until reaped here;
            // reap it while the plugin waits for it.
            let reaper = std::thread::spawn(move || wait_exit(&mut victim, 3000));
            let out = p.execute(&hit.id, "terminate", &name).unwrap();
            let st = reaper.join().unwrap().expect("victim should exit");
            assert_eq!(st.signal(), Some(libc::SIGTERM), "SIGTERM expected, got {st:?}");
            assert_eq!(out, ExecOutcome::Close, "it exited, so the launcher closes");
            assert!(wait_exit(&mut bystander, 300).is_none(), "differently named process must survive");

            // Gone now: a second execute reports it instead of silently succeeding.
            assert!(p.execute(&hit.id, "terminate", &name).is_err());
            let _ = bystander.kill();
            let _ = bystander.wait();
        }

        #[test]
        fn a_process_that_ignores_terminate_is_offered_force_kill() {
            let p = provider();
            let name = format!("zzkf{}", std::process::id() % 100_000);
            let Some((mut victim, _dir)) = spawn_named(&name, "sh", &["-c", STUBBORN]) else { return };

            let id = format!("name:{name}");
            assert!(find(&p, &name).iter().any(|i| i.id == id), "victim listed");
            let out = p.execute(&id, "terminate", &name).unwrap();
            assert_eq!(out.query(), Some(format!("{TRIGGER}{name}").as_str()), "stays open on the same search");
            assert!(wait_exit(&mut victim, 100).is_none(), "process ignores SIGTERM");

            let row = find(&p, &name).into_iter().find(|i| i.id == id).expect("still listed");
            assert_eq!(row.subtitle, STILL_RUNNING);
            assert_eq!(row.default_action().unwrap().id, "force");
            assert_eq!(p.execute(&id, "force", &name).unwrap(), ExecOutcome::Close);
            let st = wait_exit(&mut victim, 3000).expect("force kill must end it");
            assert_eq!(st.signal(), Some(libc::SIGKILL));
            assert!(lock(&p.pending).is_none());
        }

        /// A parent that starts a child under its own name running `child`, then waits for it.
        fn spawn_family(name: &str, child: &str) -> Option<(Child, tempfile::TempDir)> {
            spawn_named(name, "sh", &["-c", &format!("\"$0\" -c \"{child}\" & wait")])
        }

        #[test]
        fn terminate_stops_the_top_process_then_what_outlived_it() {
            let p = provider();
            let name = format!("zzkr{}", std::process::id() % 100_000);
            let Some((mut parent, _dir)) = spawn_family(&name, "while :; do sleep 1; done") else { return };
            let id = format!("name:{name}");
            let row = find(&p, &name).into_iter().find(|i| i.id == id).expect("listed");
            assert!(row.subtitle.starts_with("2 processes"), "{}", row.subtitle);

            let reaper = std::thread::spawn(move || wait_exit(&mut parent, 5000));
            assert_eq!(p.execute(&id, "terminate", &name).unwrap(), ExecOutcome::Close);
            assert_eq!(reaper.join().unwrap().and_then(|s| s.signal()), Some(libc::SIGTERM));
            assert!(find(&p, &name).iter().all(|i| i.id != id), "the child, left behind by its parent, is stopped too");
        }

        #[test]
        fn a_leftover_that_ignores_terminate_is_offered_force_kill() {
            let p = provider();
            let name = format!("zzkl{}", std::process::id() % 100_000);
            let Some((mut parent, _dir)) = spawn_family(&name, STUBBORN) else { return };
            let id = format!("name:{name}");
            assert!(find(&p, &name).iter().any(|i| i.id == id), "listed");

            let reaper = std::thread::spawn(move || wait_exit(&mut parent, 5000));
            let out = p.execute(&id, "terminate", &name).unwrap();
            assert!(reaper.join().unwrap().is_some(), "the parent exits");
            assert_eq!(out.query(), Some(format!("{TRIGGER}{name}").as_str()), "the child is still there");
            let row = find(&p, &name).into_iter().find(|i| i.id == id).expect("child listed");
            assert_eq!(row.default_action().unwrap().id, "force");
            assert_eq!(p.execute(&id, "force", &name).unwrap(), ExecOutcome::Close);
        }

        #[test]
        fn never_lists_pid_one_or_the_launcher_itself() {
            let p = provider();
            assert!(find(&p, "1").iter().all(|i| i.id != "pid:1"));
            assert!(find(&p, &std::process::id().to_string()).is_empty(), "the launcher must not be offered");
            assert!(find(&p, "zz-no-such-process-zz").is_empty());
        }
    }
}
