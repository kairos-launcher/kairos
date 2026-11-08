//! Session and power commands. Destructive ones ask first: Enter completes the search box
//! to `sys <command>!`, whose only row runs it.

use std::path::Path;
#[cfg(not(windows))]
use std::path::PathBuf;

use serde_json::Value;

use crate::core::{fuzzy, icons, process};
use crate::plugin;
use crate::plugin::{Error, ExecOutcome, Provider, Query, ResultItem};

const ID: &str = "builtin-system";
const TRIGGER: &str = "sys ";

struct Cmd {
    id: &'static str,
    title: &'static str,
    /// Extra words it is found by.
    keywords: &'static str,
    icons: &'static [&'static str],
    glyph: &'static str,
    destructive: bool,
}

const CMDS: [Cmd; 7] = [
    Cmd {
        id: "lock",
        title: "Lock screen",
        keywords: "lock screen",
        icons: &["system-lock-screen"],
        glyph: "L",
        destructive: false,
    },
    Cmd {
        id: "sleep",
        title: "Sleep",
        keywords: "suspend standby",
        icons: &["system-suspend", "weather-clear-night"],
        glyph: "S",
        destructive: false,
    },
    Cmd {
        id: "hibernate",
        title: "Hibernate",
        keywords: "hibernate",
        icons: &["system-hibernate", "system-suspend-hibernate"],
        glyph: "H",
        destructive: false,
    },
    Cmd {
        id: "logout",
        title: "Log out",
        keywords: "logout sign out",
        icons: &["system-log-out", "application-exit"],
        glyph: "O",
        destructive: true,
    },
    Cmd {
        id: "restart",
        title: "Restart",
        keywords: "reboot",
        icons: &["system-reboot", "system-restart"],
        glyph: "R",
        destructive: true,
    },
    Cmd {
        id: "shutdown",
        title: "Shut down",
        keywords: "shutdown power off poweroff",
        icons: &["system-shutdown"],
        glyph: "P",
        destructive: true,
    },
    Cmd {
        id: "trash",
        title: "Empty trash",
        keywords: "empty trash bin recycle",
        icons: &["user-trash-full", "user-trash"],
        glyph: "T",
        destructive: true,
    },
];

pub struct SystemProvider {
    confirm: bool,
}

fn find(id: &str) -> Option<&'static Cmd> {
    CMDS.iter().find(|c| c.id == id)
}

#[cfg(not(windows))]
fn trash_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("Trash"))
}

/// Items in the trash (Linux), for the subtitle.
fn trash_count() -> Option<usize> {
    #[cfg(not(windows))]
    {
        std::fs::read_dir(trash_dir()?.join("files")).ok().map(std::iter::Iterator::count)
    }
    #[cfg(windows)]
    {
        None
    }
}

/// Delete everything inside `dir`, keeping `dir`.
#[cfg_attr(windows, allow(dead_code))]
fn clear_dir(dir: &Path) -> Result<(), String> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Ok(()) };
    for e in rd.flatten() {
        let p = e.path();
        let r = if e.file_type().is_ok_and(|t| t.is_dir()) {
            std::fs::remove_dir_all(&p)
        } else {
            std::fs::remove_file(&p)
        };
        r.map_err(|err| format!("{}: {err}", p.display()))?;
    }
    Ok(())
}

fn args(v: &[&str]) -> Vec<String> {
    v.iter().map(std::string::ToString::to_string).collect()
}

/// The command line for `id` on this OS, or `None` where it is handled in-process / unsupported.
fn argv(id: &str) -> Option<Vec<String>> {
    if cfg!(windows) {
        return Some(args(match id {
            "lock" => &["rundll32.exe", "user32.dll,LockWorkStation"],
            "sleep" => &["rundll32.exe", "powrprof.dll,SetSuspendState", "0,1,0"],
            "hibernate" => &["shutdown", "/h"],
            "logout" => &["shutdown", "/l"],
            "restart" => &["shutdown", "/r", "/t", "0"],
            "shutdown" => &["shutdown", "/s", "/t", "0"],
            "trash" => &["powershell", "-NoProfile", "-Command", "Clear-RecycleBin -Force"],
            _ => return None,
        }));
    }
    if cfg!(target_os = "macos") {
        return Some(args(match id {
            "lock" => &["pmset", "displaysleepnow"],
            "sleep" => &["pmset", "sleepnow"],
            "logout" => &["osascript", "-e", "tell app \"System Events\" to log out"],
            "restart" => &["osascript", "-e", "tell app \"System Events\" to restart"],
            "shutdown" => &["osascript", "-e", "tell app \"System Events\" to shut down"],
            _ => return None,
        }));
    }
    match id {
        "lock" => Some(args(&["loginctl", "lock-session"])),
        "sleep" => Some(args(&["systemctl", "suspend"])),
        "hibernate" => Some(args(&["systemctl", "hibernate"])),
        "logout" => Some(match std::env::var("XDG_SESSION_ID") {
            Ok(s) if !s.is_empty() => vec!["loginctl".into(), "terminate-session".into(), s],
            _ => vec!["loginctl".into(), "terminate-user".into(), std::env::var("USER").unwrap_or_default()],
        }),
        "restart" => Some(args(&["systemctl", "reboot"])),
        "shutdown" => Some(args(&["systemctl", "poweroff"])),
        _ => None,
    }
}

impl SystemProvider {
    pub fn new(settings: &Value) -> Self {
        Self { confirm: settings.get("confirm").and_then(Value::as_bool).unwrap_or(true) }
    }

    fn row(c: &Cmd, score: f64) -> ResultItem {
        let mut sub = String::from("System");
        if c.id == "trash"
            && let Some(n) = trash_count()
        {
            sub = format!("{n} item{} in the trash", if n == 1 { "" } else { "s" });
        }
        ResultItem::new(c.id, c.title, sub, score).with_icon(icons::themed(c.icons, c.glyph))
    }

    fn run(id: &str) -> Result<(), String> {
        if id == "trash" && !cfg!(windows) {
            #[cfg(not(windows))]
            {
                let t = trash_dir().ok_or("no trash folder")?;
                for sub in ["files", "info", "expunged"] {
                    clear_dir(&t.join(sub))?;
                }
            }
            return Ok(());
        }
        process::spawn(&argv(id).ok_or("not supported on this system")?)
    }
}

impl Provider for SystemProvider {
    fn id(&self) -> &str {
        ID
    }

    fn query(&self, q: &Query) -> Vec<ResultItem> {
        let text = q.text.trim();
        if let Some(id) = text.strip_suffix('!').filter(|_| q.trigger.is_some()) {
            return find(id).map_or(vec![], |c| {
                let mut r = Self::row(c, 1.0).with_action("run", c.title);
                r.title = format!("Confirm: {}", c.title);
                r.subtitle = "Enter to continue, Esc to cancel".into();
                vec![r]
            });
        }
        let global = q.trigger.is_none();
        // Plain searches need a few letters, so typing an app name never offers "Shut down".
        if global && text.chars().count() < 3 {
            return vec![];
        }
        CMDS.iter()
            .filter_map(|c| {
                let score = if text.is_empty() {
                    0.5
                } else {
                    let s = [
                        fuzzy::score(text, c.title),
                        fuzzy::score(text, c.id),
                        fuzzy::score(text, c.keywords).map(|s| s * 0.9),
                    ]
                    .into_iter()
                    .flatten()
                    .reduce(f64::max)?;
                    if global && s < 0.7 {
                        return None; // substring matches only
                    }
                    if global { s * 0.85 } else { s }
                };
                let r = Self::row(c, score);
                Some(if c.destructive && self.confirm {
                    r.with_action("confirm", &format!("{}…", c.title))
                } else {
                    r.with_action("run", c.title)
                })
            })
            .collect()
    }

    fn execute(&self, result_id: &str, action_id: &str, _query: &str) -> plugin::Result<ExecOutcome> {
        let c = find(result_id).ok_or_else(|| format!("unknown command {result_id}"))?;
        match action_id {
            "confirm" => Ok(ExecOutcome::complete(format!("{TRIGGER}{}!", c.id))),
            "run" => {
                Self::run(c.id)?;
                Ok(ExecOutcome::Close)
            }
            _ => Err(Error::UnknownAction(action_id.into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q<'a>(text: &'a str, trigger: Option<&'a str>) -> Query<'a> {
        Query { text, trigger, limit: 8 }
    }

    #[test]
    fn destructive_commands_ask_first() {
        let p = SystemProvider::new(&serde_json::json!({}));
        let r = p.query(&q("reboot", Some(TRIGGER)));
        assert_eq!(r[0].id, "restart");
        assert_eq!(r[0].default_action().unwrap().id, "confirm");
        let out = p.execute("restart", "confirm", "reboot").unwrap();
        assert_eq!(out.query(), Some("sys restart!"), "nothing runs yet");
        let confirm = p.query(&q("restart!", Some(TRIGGER)));
        assert_eq!(confirm.len(), 1);
        assert!(confirm[0].title.starts_with("Confirm"));
        assert_eq!(confirm[0].default_action().unwrap().id, "run");
        assert!(p.query(&q("restart!", None)).is_empty(), "confirmation only through the trigger");
    }

    #[test]
    fn harmless_commands_run_directly_and_confirm_can_be_off() {
        let p = SystemProvider::new(&serde_json::json!({}));
        assert_eq!(p.query(&q("lock", Some(TRIGGER)))[0].default_action().unwrap().id, "run");
        let p = SystemProvider::new(&serde_json::json!({ "confirm": false }));
        assert_eq!(p.query(&q("shut", Some(TRIGGER)))[0].default_action().unwrap().id, "run");
    }

    #[test]
    fn plain_searches_stay_quiet_for_short_or_loose_matches() {
        let p = SystemProvider::new(&serde_json::json!({}));
        assert!(p.query(&q("sh", None)).is_empty());
        assert!(p.query(&q("firefox", None)).is_empty());
        let r = p.query(&q("shut", None));
        assert_eq!(r[0].id, "shutdown");
        assert!(r[0].score < 1.0);
        assert_eq!(p.query(&q("", Some(TRIGGER))).len(), CMDS.len());
    }

    #[test]
    fn clearing_a_dir_keeps_the_dir() {
        let tmp_d = crate::test_support::tmp();
        let d = tmp_d.path().to_path_buf();
        std::fs::create_dir_all(d.join("sub/deeper")).unwrap();
        std::fs::write(d.join("a"), "x").unwrap();
        clear_dir(&d).unwrap();
        assert!(d.is_dir() && std::fs::read_dir(&d).unwrap().count() == 0);
    }
}
