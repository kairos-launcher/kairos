//! `> command` runs a shell command in a terminal (or silently, from the actions menu).
//! Recent commands are kept in `<state>/shell_history.json` (private to the user) and offered
//! as you type.

use std::path::PathBuf;
use std::sync::Mutex;

use serde_json::Value;

use crate::core::sync::lock;
use crate::core::{fuzzy, icons, process};
use crate::plugin;
use crate::plugin::{Error, ExecOutcome, Icon, Provider, Query, ResultItem};

const ID: &str = "builtin-shell";
const TRIGGER: &str = ">";
/// Result id of the row for what is typed; other rows are `h:<command>` from history.
const TYPED: &str = "run";

pub struct ShellProvider {
    terminal: String,
    keep_open: bool,
    max_history: usize,
    history_file: Option<PathBuf>,
    /// Most recent first. Loaded on first use.
    history: Mutex<Option<Vec<String>>>,
}

fn icon() -> Icon {
    icons::themed(&["utilities-terminal", "terminal", "org.gnome.Terminal"], ">")
}

impl ShellProvider {
    pub fn new(settings: &Value) -> Self {
        Self::with_history_file(settings, Some(crate::config::state_file("shell_history.json")))
    }

    pub fn with_history_file(settings: &Value, history_file: Option<PathBuf>) -> Self {
        Self {
            terminal: settings.get("terminal").and_then(Value::as_str).unwrap_or("").to_string(),
            keep_open: settings.get("keep_open").and_then(Value::as_bool).unwrap_or(true),
            max_history: settings.get("history").and_then(Value::as_u64).unwrap_or(50) as usize,
            history_file,
            history: Mutex::new(None),
        }
    }

    fn with_history<T>(&self, f: impl FnOnce(&mut Vec<String>) -> T) -> T {
        let mut g = lock(&self.history);
        let h = g.get_or_insert_with(|| {
            self.history_file
                .as_ref()
                .and_then(|p| std::fs::read_to_string(p).ok())
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or_default()
        });
        f(h)
    }

    fn remember(&self, cmd: &str) {
        if self.max_history == 0 {
            return;
        }
        let saved = self.with_history(|h| {
            h.retain(|c| c != cmd);
            h.insert(0, cmd.to_string());
            h.truncate(self.max_history);
            serde_json::to_string_pretty(h).ok()
        });
        // Commands can contain secrets, so the file is private to the user.
        if let (Some(json), Some(path)) = (saved, &self.history_file)
            && let Err(e) = crate::core::fs::write_private(path, json.as_bytes())
        {
            log::warn!("shell history: {}: {e}", path.display());
        }
    }

    /// What runs in the terminal: the command, then (optionally) an interactive shell so the
    /// window stays open to read the output.
    fn terminal_command(&self, cmd: &str) -> Vec<String> {
        if cfg!(windows) {
            let flag = if self.keep_open { "/K" } else { "/C" };
            return vec!["cmd".into(), flag.into(), cmd.into()];
        }
        let line = if self.keep_open { format!("{cmd}; exec \"${{SHELL:-sh}}\"") } else { cmd.to_string() };
        process::shell_argv(&line)
    }

    fn row(id: String, cmd: &str, subtitle: &str, score: f64) -> ResultItem {
        ResultItem::new(id, cmd, subtitle, score)
            .with_icon(icon())
            .with_action("terminal", "Run in terminal")
            .with_action("silent", "Run without a terminal")
            .with_action("copy", "Copy command")
    }
}

impl Provider for ShellProvider {
    fn id(&self) -> &str {
        ID
    }

    fn query(&self, q: &Query) -> Vec<ResultItem> {
        let text = q.text.trim();
        let mut out = vec![];
        if !text.is_empty() {
            out.push(Self::row(TYPED.into(), text, "Run in a terminal · Tab for more", 1.0));
        }
        let history = self.with_history(|h| h.clone());
        for (i, cmd) in history.iter().enumerate() {
            if cmd == text {
                continue;
            }
            let score =
                if text.is_empty() { Some(0.9 - i as f64 * 0.001) } else { fuzzy::score(text, cmd).map(|s| s * 0.8) };
            if let Some(score) = score {
                out.push(Self::row(format!("h:{cmd}"), cmd, "Recent", score).with_complete(format!("{TRIGGER}{cmd}")));
            }
        }
        out
    }

    fn execute(&self, result_id: &str, action_id: &str, query: &str) -> plugin::Result<ExecOutcome> {
        let cmd = match result_id.strip_prefix("h:") {
            Some(c) => c.to_string(),
            None if result_id == TYPED => query.trim().to_string(),
            None => return Err(Error::UnknownResult(result_id.into())),
        };
        if cmd.is_empty() {
            return Err("empty command".into());
        }
        match action_id {
            "terminal" => process::run_in_terminal(&self.terminal, &self.terminal_command(&cmd))?,
            "silent" => process::spawn(&process::shell_argv(&cmd))?,
            "copy" => {
                crate::core::clipboard::copy(&cmd)?;
                return Ok(ExecOutcome::Close);
            }
            _ => return Err(Error::UnknownAction(action_id.into())),
        }
        self.remember(&cmd);
        Ok(ExecOutcome::Close)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(settings: &Value) -> (tempfile::TempDir, ShellProvider, PathBuf) {
        let tmp = crate::test_support::tmp();
        let path = tmp.path().join("history.json");
        (tmp, ShellProvider::with_history_file(settings, Some(path.clone())), path)
    }

    #[test]
    fn silent_run_is_remembered_most_recent_first() {
        let (_tmp, p, path) = provider(&serde_json::json!({ "history": 2 }));
        for cmd in ["true", "echo one", "true"] {
            p.execute(TYPED, "silent", cmd).unwrap();
        }
        p.execute(TYPED, "silent", "echo two").unwrap();
        let saved: Vec<String> = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(saved, ["echo two", "true"], "deduplicated, capped at 2");

        let again = ShellProvider::with_history_file(&serde_json::json!({}), Some(path.clone()));
        let rows = again.query(&Query { text: "", trigger: Some(TRIGGER), limit: 8 });
        assert_eq!(rows.iter().map(|r| r.title.as_str()).collect::<Vec<_>>(), ["echo two", "true"], "loaded from disk");
        let rows = again.query(&Query { text: "ech", trigger: Some(TRIGGER), limit: 8 });
        assert_eq!(rows[0].id, TYPED);
        assert_eq!(rows[1].id, "h:echo two");
        assert_eq!(rows[1].complete.as_deref(), Some(">echo two"));
    }

    #[test]
    fn no_history_when_disabled() {
        let (_tmp, p, path) = provider(&serde_json::json!({ "history": 0 }));
        p.execute(TYPED, "silent", "true").unwrap();
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn terminal_keeps_a_shell_open_when_asked() {
        let (_tmp, p, _) = provider(&serde_json::json!({}));
        let argv = p.terminal_command("ls");
        assert_eq!(argv[1], "-c");
        assert!(argv[2].starts_with("ls; exec "), "{argv:?}");
        let (_tmp, p, _) = provider(&serde_json::json!({ "keep_open": false }));
        assert_eq!(p.terminal_command("ls")[2], "ls");
    }
}
