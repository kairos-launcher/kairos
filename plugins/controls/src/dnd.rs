//! Do Not Disturb, with an optional end time.
//!
//! Each desktop has its own switch (see [`Backend`]). A time limit is kept in `dnd.json`, and a
//! detached copy of this binary (`--dnd-off <dir> <until>`) turns it off when the time comes, as
//! the plugin itself is stopped when idle. The sleeper acts only while `dnd.json` still holds its
//! `until`: turning it off by hand or starting a new limit rewrites the file, and an older sleeper
//! then exits without touching anything.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::run;

/// Where Do Not Disturb is switched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// `org.gnome.desktop.notifications show-banners`.
    Gnome,
    /// `plasmanotifyrc` `[DoNotDisturb] Until`: Plasma ends it on its own, too.
    Kde,
    Swaync,
    Dunst,
    Mako,
    /// The per-user "show notifications" switch.
    Windows,
    /// Shortcuts named `Kairos DND On` / `Kairos DND Off` (macOS has no Focus API).
    MacShortcuts,
}

impl Backend {
    pub fn name(self) -> &'static str {
        match self {
            Self::Gnome => "GNOME",
            Self::Kde => "Plasma",
            Self::Swaync => "SwayNC",
            Self::Dunst => "dunst",
            Self::Mako => "mako",
            Self::Windows => "Windows",
            Self::MacShortcuts => "Shortcuts",
        }
    }
}

const MAC_ON: &str = "Kairos DND On";
const MAC_OFF: &str = "Kairos DND Off";
/// Plasma's `Until` for "until turned off".
const KDE_FOREVER: &str = "2099-12-31T23:59:59";

/// The switch for this desktop, or why there is none.
pub fn detect() -> Result<Backend, String> {
    if cfg!(windows) {
        return Ok(Backend::Windows);
    }
    if cfg!(target_os = "macos") {
        let list = run::output(&["shortcuts", "list"]).unwrap_or_default();
        return if list.lines().any(|l| l == MAC_ON) && list.lines().any(|l| l == MAC_OFF) {
            Ok(Backend::MacShortcuts)
        } else {
            Err(format!("create Shortcuts named “{MAC_ON}” and “{MAC_OFF}” that set Focus"))
        };
    }
    if run::desktop_is("KDE") && (run::has("kwriteconfig6") || run::has("kwriteconfig5")) {
        return Ok(Backend::Kde);
    }
    if ["GNOME", "Unity", "ubuntu", "Pantheon"].iter().any(|d| run::desktop_is(d)) {
        return Ok(Backend::Gnome);
    }
    // wlroots and X11 setups: whichever notification daemon answers.
    if run::output(&["swaync-client", "-D"]).is_some() {
        return Ok(Backend::Swaync);
    }
    if run::output(&["dunstctl", "is-paused"]).is_some() {
        return Ok(Backend::Dunst);
    }
    if run::output(&["makoctl", "mode"]).is_some() {
        return Ok(Backend::Mako);
    }
    if run::has("gsettings")
        && run::output(&["gsettings", "get", "org.gnome.desktop.notifications", "show-banners"]).is_some()
    {
        return Ok(Backend::Gnome);
    }
    Err("no supported notification daemon (GNOME, Plasma, SwayNC, dunst or mako)".into())
}

fn kwriteconfig() -> &'static str {
    if run::has("kwriteconfig6") { "kwriteconfig6" } else { "kwriteconfig5" }
}

fn kreadconfig() -> &'static str {
    if run::has("kreadconfig6") { "kreadconfig6" } else { "kreadconfig5" }
}

#[cfg_attr(not(windows), allow(dead_code))]
const WIN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Notifications\Settings";
#[cfg_attr(not(windows), allow(dead_code))]
const WIN_VALUE: &str = "NOC_GLOBAL_SETTING_TOASTS_ENABLED";

/// Whether it is on, read from the desktop; `None` where it cannot be read (macOS: `dnd.json`).
pub fn is_on(b: Backend) -> Option<bool> {
    match b {
        Backend::Gnome => {
            Some(run::output(&["gsettings", "get", "org.gnome.desktop.notifications", "show-banners"])? == "false")
        }
        Backend::Kde => {
            let until =
                run::output(&[kreadconfig(), "--file", "plasmanotifyrc", "--group", "DoNotDisturb", "--key", "Until"])?;
            Some(kde_active(&until, &jiff::Zoned::now()))
        }
        Backend::Swaync => Some(run::output(&["swaync-client", "-D"])? == "true"),
        Backend::Dunst => Some(run::output(&["dunstctl", "is-paused"])? == "true"),
        Backend::Mako => Some(run::output(&["makoctl", "mode"])?.lines().any(|l| l.trim() == "do-not-disturb")),
        Backend::Windows => win_is_on(),
        Backend::MacShortcuts => None,
    }
}

#[cfg(windows)]
fn win_is_on() -> Option<bool> {
    Some(crate::win::get_dword(WIN_KEY, WIN_VALUE).unwrap_or(1) == 0)
}

#[cfg(not(windows))]
fn win_is_on() -> Option<bool> {
    None
}

/// Plasma's `Until` is a local date-time; DND is on while it lies ahead.
fn kde_active(until: &str, now: &jiff::Zoned) -> bool {
    let Ok(dt) = until.trim().parse::<jiff::civil::DateTime>() else { return false };
    dt.to_zoned(now.time_zone().clone()).is_ok_and(|z| z > *now)
}

/// Switch it; `until` (Unix seconds) only matters to Plasma, which keeps the end time itself.
pub fn switch(b: Backend, on: bool, until: Option<i64>) -> Result<(), String> {
    let flag = if on { "true" } else { "false" };
    match b {
        Backend::Gnome => run::status(&[
            "gsettings",
            "set",
            "org.gnome.desktop.notifications",
            "show-banners",
            if on { "false" } else { "true" },
        ]),
        Backend::Kde => {
            let base =
                [kwriteconfig(), "--file", "plasmanotifyrc", "--group", "DoNotDisturb", "--key", "Until", "--notify"];
            if !on {
                return run::status(&[&base[..], &["--delete"]].concat());
            }
            let at = until.and_then(|t| kde_time(t, &jiff::Zoned::now())).unwrap_or_else(|| KDE_FOREVER.to_string());
            run::status(&[&base[..], &[at.as_str()]].concat())
        }
        Backend::Swaync => run::status(&["swaync-client", if on { "-dn" } else { "-df" }]),
        Backend::Dunst => run::status(&["dunstctl", "set-paused", flag]),
        Backend::Mako => run::status(&["makoctl", "mode", if on { "-a" } else { "-r" }, "do-not-disturb"]),
        Backend::Windows => win_switch(on),
        Backend::MacShortcuts => run::status(&["shortcuts", "run", if on { MAC_ON } else { MAC_OFF }]),
    }
}

/// Unix seconds as Plasma's local `2026-11-15T15:30:00`.
fn kde_time(t: i64, now: &jiff::Zoned) -> Option<String> {
    let ts = jiff::Timestamp::from_second(t).ok()?;
    Some(ts.to_zoned(now.time_zone().clone()).datetime().strftime("%Y-%m-%dT%H:%M:%S").to_string())
}

#[cfg(windows)]
fn win_switch(on: bool) -> Result<(), String> {
    crate::win::set_dword(WIN_KEY, WIN_VALUE, u32::from(!on))?;
    crate::win::setting_changed(None);
    Ok(())
}

#[cfg(not(windows))]
fn win_switch(_: bool) -> Result<(), String> {
    Err("not supported on this system".into())
}

/// `dnd.json`: whether this plugin turned it on, and until when.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Saved {
    #[serde(default)]
    pub on: bool,
    /// Unix seconds; `None`: until turned off.
    #[serde(default)]
    pub until: Option<i64>,
}

pub fn file(dir: &Path) -> PathBuf {
    dir.join("dnd.json")
}

pub fn load(dir: &Path) -> Saved {
    std::fs::read(file(dir)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

pub fn save(dir: &Path, s: &Saved) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let tmp = dir.join("dnd.json.tmp");
    let body = serde_json::to_vec(s).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, body)
        .and_then(|()| std::fs::rename(&tmp, file(dir)))
        .map_err(|e| format!("{}: {e}", tmp.display()))
}

/// What the row shows: on/off and the end time, if this plugin set one that is still ahead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    pub on: bool,
    pub until: Option<i64>,
}

/// Combine what the desktop says with `dnd.json`. A limit only counts while DND is still on (it
/// may have been turned off elsewhere).
pub fn status(live: Option<bool>, saved: &Saved, now: i64) -> Status {
    let on = live.unwrap_or(saved.on && saved.until.is_none_or(|u| u > now));
    Status { on, until: saved.until.filter(|u| on && *u > now) }
}

/// Turn it on (until `until`) or off, and record that, superseding any waiting sleeper.
pub fn set(b: Backend, dir: &Path, on: bool, until: Option<i64>) -> Result<(), String> {
    let until = until.filter(|_| on);
    switch(b, on, until)?;
    save(dir, &Saved { on, until })?;
    if let Some(t) = until {
        spawn_sleeper(dir, t).map_err(|e| format!("starting the timer for Do Not Disturb: {e}"))?;
    }
    Ok(())
}

/// Run this binary again, detached, to turn Do Not Disturb off at `until`. It must outlive this
/// process (the launcher stops idle plugins), so it gets its own process group and no pipes.
fn spawn_sleeper(dir: &Path, until: i64) -> std::io::Result<()> {
    let mut cmd = Command::new(std::env::current_exe()?);
    cmd.arg("--dnd-off").arg(dir).arg(until.to_string());
    // Not the plugin folder: on Windows that would keep it from being removed.
    cmd.current_dir(std::env::temp_dir()).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0000_0008 | 0x0000_0200); // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP
    }
    let mut child = cmd.spawn()?;
    // Reaped here while this process lives; afterwards by whoever inherits it.
    std::thread::spawn(move || child.wait());
    Ok(())
}

/// How often the sleeper looks at the wall clock: a monotonic sleep does not count time
/// suspended, and a superseded sleeper should exit soon.
const CHECK_S: i64 = 30;

/// The sleeper: wait until `until`, then turn Do Not Disturb off if this limit still stands.
pub fn sleeper(dir: &Path, until: i64) {
    loop {
        if load(dir).until != Some(until) {
            return; // turned off by hand, or a newer limit
        }
        let left = until - now();
        if left <= 0 {
            break;
        }
        std::thread::sleep(Duration::from_secs(u64::try_from(left.min(CHECK_S)).unwrap_or(1)));
    }
    if let Ok(b) = detect()
        && let Err(e) = switch(b, false, None)
    {
        eprintln!("controls: turning Do Not Disturb off: {e}");
    }
    if load(dir).until == Some(until) {
        let _ = save(dir, &Saved::default());
    }
}

pub fn now() -> i64 {
    jiff::Timestamp::now().as_second()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("kairos-controls-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn status_prefers_the_desktop() {
        let saved = Saved { on: true, until: Some(1000) };
        assert_eq!(status(Some(true), &saved, 500), Status { on: true, until: Some(1000) });
        assert_eq!(status(Some(false), &saved, 500), Status { on: false, until: None }, "turned off elsewhere");
        assert_eq!(status(Some(true), &saved, 2000), Status { on: true, until: None }, "limit passed");
        // macOS: only the file knows.
        assert_eq!(status(None, &saved, 500), Status { on: true, until: Some(1000) });
        assert_eq!(status(None, &saved, 2000), Status { on: false, until: None });
        assert_eq!(status(None, &Saved { on: true, until: None }, 2000), Status { on: true, until: None });
    }

    #[test]
    fn superseded_sleeper_exits_without_switching() {
        let d = dir("sleeper");
        save(&d, &Saved { on: true, until: Some(now() + 3600) }).unwrap();
        // A sleeper for an older limit returns at once and leaves the file alone.
        sleeper(&d, 42);
        assert_eq!(load(&d).until.map(|u| u > now()), Some(true));
        save(&d, &Saved::default()).unwrap();
        sleeper(&d, now() + 3600);
        assert_eq!(load(&d), Saved::default());
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn plasma_times() {
        let now: jiff::Zoned = "2026-11-15T10:00:00+01:00[Europe/Zurich]".parse().unwrap();
        assert!(kde_active("2026-11-15T15:30:00", &now));
        assert!(!kde_active("2026-11-15T09:30:00", &now));
        assert!(!kde_active("", &now));
        assert!(kde_active(KDE_FOREVER, &now));
        let t = now.timestamp().as_second() + 5400;
        assert_eq!(kde_time(t, &now).as_deref(), Some("2026-11-15T11:30:00"));
    }
}
