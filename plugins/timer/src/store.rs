//! `timers.json` in the launcher's state folder: the running timers and the recently started ones.
//! The plugin and every timer's sleeper process edit it, so changes go through [`Store::update`],
//! which holds a lock file and writes atomically.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

/// How many recently started timers are offered again.
pub const RECENT: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timer {
    pub id: String,
    pub label: String,
    /// When it ends, Unix seconds.
    pub due: i64,
    /// How long it ran when started.
    pub seconds: u64,
    #[serde(default)]
    pub alarm: bool,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub timers: Vec<Timer>,
    /// What was typed to start them, newest first (`10m tea`).
    #[serde(default)]
    pub recent: Vec<String>,
}

impl State {
    /// Remember `text` as the newest recent timer (once, ignoring case).
    pub fn remember(&mut self, text: &str) {
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        self.recent.retain(|r| !r.eq_ignore_ascii_case(&text));
        self.recent.insert(0, text);
        self.recent.truncate(RECENT);
    }
}

pub struct Store {
    path: PathBuf,
}

impl Store {
    pub fn new(dir: &Path) -> Self {
        Self { path: dir.join("timers.json") }
    }

    /// The launcher's state folder, as `config::state_file` in the launcher picks it.
    pub fn default_dir() -> PathBuf {
        let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        if let Some(h) = env("KAIROS_HOME") {
            return h;
        }
        // Portable: this binary is in `<exe dir>/plugins/timer/` next to a `portable` file.
        if let Some(root) = std::env::current_exe()
            .ok()
            .and_then(|e| e.canonicalize().ok())
            .and_then(|e| Some(e.parent()?.parent()?.parent()?.to_path_buf()))
            .filter(|r| r.join("portable").exists())
        {
            return root.join("data");
        }
        let base = if cfg!(windows) {
            env("LOCALAPPDATA")
        } else if cfg!(target_os = "macos") {
            env("HOME").map(|h| h.join("Library/Application Support"))
        } else {
            env("XDG_STATE_HOME").or_else(|| env("HOME").map(|h| h.join(".local/state")))
        };
        base.unwrap_or_else(std::env::temp_dir).join("kairos")
    }

    /// The state as it is now; empty if the file is missing or unreadable.
    pub fn load(&self) -> State {
        fs::read(&self.path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    /// Change the state under the lock and save it; returns what `f` returns.
    pub fn update<R>(&self, f: impl FnOnce(&mut State) -> R) -> std::io::Result<R> {
        let _lock = Lock::take(&self.path.with_extension("json.lock"))?;
        let mut state = self.load();
        let r = f(&mut state);
        self.save(&state)?;
        Ok(r)
    }

    fn save(&self, state: &State) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension(format!("json.{}.tmp", std::process::id()));
        let mut f = private(OpenOptions::new().write(true).create(true).truncate(true)).open(&tmp)?;
        f.write_all(&serde_json::to_vec_pretty(state).map_err(std::io::Error::other)?)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, &self.path).inspect_err(|_| {
            let _ = fs::remove_file(&tmp);
        })
    }
}

/// Files only the user can read (on Unix).
fn private(o: &mut OpenOptions) -> &mut OpenOptions {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o
}

/// A lock file, created exclusively and removed on drop. One left behind by a crashed process is
/// taken over after a few seconds (every holder is done in milliseconds).
struct Lock(PathBuf);

impl Lock {
    fn take(path: &Path) -> std::io::Result<Self> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        for _ in 0..200 {
            match private(OpenOptions::new().write(true).create_new(true)).open(path) {
                Ok(_) => return Ok(Self(path.to_path_buf())),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let age = fs::metadata(path).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok());
                    if age.is_some_and(|a| a > Duration::from_secs(5)) {
                        let _ = fs::remove_file(path);
                    } else {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
                Err(e) => return Err(e),
            }
        }
        Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "timers.json is locked"))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// Unix seconds now.
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// A new timer id: unique across the plugin and earlier runs.
pub fn new_id() -> String {
    let nanos = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    format!("{:x}{:x}", nanos, std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("kairos-timer-test-{}", new_id()));
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn timer(id: &str) -> Timer {
        Timer { id: id.into(), label: "tea".into(), due: 100, seconds: 60, alarm: false }
    }

    #[test]
    fn update_round_trips() {
        let dir = temp();
        let store = Store::new(&dir);
        assert_eq!(store.load(), State::default(), "no file yet");
        store.update(|s| s.timers.push(timer("a"))).unwrap();
        store.update(|s| s.timers.push(timer("b"))).unwrap();
        store.update(|s| s.timers.retain(|t| t.id != "a")).unwrap();
        assert_eq!(store.load().timers, vec![timer("b")]);
        assert!(!dir.join("timers.json.lock").exists(), "lock released");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(dir.join("timers.json")).unwrap().permissions().mode() & 0o777, 0o600);
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn concurrent_updates_all_land() {
        let dir = temp();
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let dir = dir.clone();
                std::thread::spawn(move || Store::new(&dir).update(|s| s.timers.push(timer(&i.to_string()))).unwrap())
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(Store::new(&dir).load().timers.len(), 8);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn stale_lock_is_taken_over() {
        let dir = temp();
        let lock = dir.join("timers.json.lock");
        fs::write(&lock, "").unwrap();
        let old = SystemTime::now() - Duration::from_secs(60);
        fs::File::options().write(true).open(&lock).unwrap().set_modified(old).unwrap();
        Store::new(&dir).update(|s| s.timers.push(timer("a"))).unwrap();
        assert_eq!(Store::new(&dir).load().timers.len(), 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn recent_is_unique_and_short() {
        let mut s = State::default();
        for t in ["10m tea", "25", "1h  call", "10M Tea", "a", "b", "c"] {
            s.remember(t);
        }
        assert_eq!(s.recent, ["c", "b", "a", "10M Tea", "1h call"]);
    }
}
