//! Custom actions: keyword → command, configured in `config.toml`.
//!
//! ```toml
//! [plugins.builtin-actions.actions.gh]    # the keyword
//! command = "xdg-open https://github.com/{}"
//! ```
//!
//! Placeholders: `{}` (or `{0}`) what is typed after the keyword, `{url}` the same URL-encoded,
//! `{1}`..`{9}` single words (quotes group words), `{{` / `}}` literal braces.
//!
//! The command is split into arguments *before* placeholders are filled, so typed text never
//! reaches a shell. With `shell = true` it runs through `sh -c` and the parameter is quoted.

use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

use crate::core::url::encode;
use crate::core::{clipboard, fuzzy, icons, paths::expand_home, process};
use crate::plugin;
use crate::plugin::{Error, ExecOutcome, Icon, Provider, Query, ResultItem};

const ID: &str = "builtin-actions";

#[derive(Deserialize, Debug, Clone)]
pub struct ActionCfg {
    /// The entry's name in the `actions` table.
    #[serde(skip)]
    pub keyword: String,
    pub command: String,
    #[serde(default)]
    pub title: String,
    /// Parameter used when nothing is typed after the keyword.
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default)]
    pub terminal: bool,
    #[serde(default)]
    pub shell: bool,
    #[serde(default)]
    pub icon: Option<String>,
}

struct Action {
    cfg: ActionCfg,
    /// The command has placeholders, so the action takes a parameter.
    takes_param: bool,
}

pub struct ActionsProvider {
    actions: Vec<Action>,
    terminal: String,
}

/// Placeholder names that exist: `` (or `0`), `url`, `1`..`9`.
fn known(name: &str) -> bool {
    name.is_empty() || name == "url" || (name.len() == 1 && name.as_bytes()[0].is_ascii_digit())
}

enum Piece {
    Text(char),
    Placeholder(String),
}

/// Split `template` into literal characters and placeholders.
fn scan(template: &str) -> Result<Vec<Piece>, String> {
    let mut out = vec![];
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' | '}' if chars.peek() == Some(&c) => {
                chars.next();
                out.push(Piece::Text(c));
            }
            '{' => {
                let mut name = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some(c) => name.push(c),
                        None => return Err(format!("unclosed `{{` in {template:?}")),
                    }
                }
                if !known(&name) {
                    return Err(format!(
                        "unknown placeholder {{{name}}} in {template:?} (use {{}}, {{0}}, {{url}} or {{1}}..{{9}})"
                    ));
                }
                out.push(Piece::Placeholder(name));
            }
            c => out.push(Piece::Text(c)),
        }
    }
    Ok(out)
}

/// Whether `template` has any placeholder; an error names a malformed one.
pub fn has_placeholders(template: &str) -> Result<bool, String> {
    Ok(scan(template)?.iter().any(|p| matches!(p, Piece::Placeholder(_))))
}

/// Fill the placeholders of `template` with `param`; `quote` wraps each inserted value
/// (identity for arguments, shell quoting for `shell = true`). Templates are validated when
/// the plugin loads, so a malformed one here is used as is.
pub fn fill(template: &str, param: &str, quote: &dyn Fn(&str) -> String) -> String {
    let Ok(pieces) = scan(template) else { return template.to_string() };
    let words = process::split_args(param);
    let mut out = String::new();
    for piece in pieces {
        match piece {
            Piece::Text(c) => out.push(c),
            Piece::Placeholder(name) => {
                let value = match name.as_str() {
                    "" | "0" => param.to_string(),
                    "url" => encode(param),
                    n => n.parse::<usize>().ok().and_then(|i| words.get(i - 1)).cloned().unwrap_or_default(),
                };
                out.push_str(&quote(&value));
            }
        }
    }
    out
}

/// Quote a value for the platform shell so it stays one word and nothing in it is interpreted.
pub fn shell_quote(s: &str) -> String {
    if cfg!(windows) { format!("\"{}\"", s.replace('"', "\"\"")) } else { format!("'{}'", s.replace('\'', r"'\''")) }
}

/// How to start a filled, non-shell command.
#[derive(Debug, PartialEq)]
enum Launch {
    /// Run the program directly.
    Spawn(Vec<String>),
    /// Windows: let the shell resolve the program (`msedge`, a URL, a document) and hand it
    /// this command line. No cmd is involved, so typed text is never interpreted.
    ShellOpen(String, String),
}

/// Windows has no `start` program and keeps browsers off PATH, so `start msedge <url>` or
/// `msedge <url>` go through ShellExecute; `on_path` tells whether a program can be spawned.
fn windows_launch(mut argv: Vec<String>, on_path: &dyn Fn(&str) -> bool) -> Launch {
    if argv.first().is_some_and(|p| p.eq_ignore_ascii_case("start")) {
        argv.remove(0);
        // `start "" prog ...`: an empty window title.
        if argv.first().is_some_and(String::is_empty) {
            argv.remove(0);
        }
    }
    match argv.split_first() {
        Some((prog, args)) if !on_path(prog) => Launch::ShellOpen(prog.clone(), process::join_win_args(args)),
        _ => Launch::Spawn(argv),
    }
}

fn launch(argv: Vec<String>) -> Result<(), String> {
    if !cfg!(windows) {
        return process::spawn(&argv);
    }
    match windows_launch(argv, &|p| process::which(p).is_some()) {
        Launch::Spawn(mut argv) => {
            // `which` also finds `foo.cmd` / `foo.bat`, but spawning a bare `foo` only finds `.exe`.
            if let Some(path) = argv.first().and_then(|p| process::which(p)) {
                argv[0] = path.to_string_lossy().into_owned();
            }
            process::spawn(&argv)
        }
        #[cfg(windows)]
        Launch::ShellOpen(prog, params) => {
            process::open_with_params(&prog, (!params.is_empty()).then_some(params.as_str()))
        }
        #[cfg(not(windows))]
        Launch::ShellOpen(..) => unreachable!("only chosen on Windows"),
    }
}

/// How a command line looks to the user (arguments with spaces in quotes).
fn display(argv: &[String]) -> String {
    argv.iter()
        .map(|a| if a.is_empty() || a.contains(char::is_whitespace) { format!("\"{a}\"") } else { a.clone() })
        .collect::<Vec<_>>()
        .join(" ")
}

impl Action {
    /// The argv to run for `param`.
    fn argv(&self, param: &str) -> Vec<String> {
        if self.cfg.shell {
            process::shell_argv(&fill(&self.cfg.command, param, &shell_quote))
        } else {
            process::split_args(&self.cfg.command)
                .iter()
                .map(|a| fill(&expand_home(a).to_string_lossy(), param, &|v| v.to_string()))
                .collect()
        }
    }

    /// The command line as shown and copied.
    fn shown(&self, param: &str) -> String {
        if self.cfg.shell { fill(&self.cfg.command, param, &shell_quote) } else { display(&self.argv(param)) }
    }

    fn title(&self) -> &str {
        if self.cfg.title.is_empty() { &self.cfg.keyword } else { &self.cfg.title }
    }

    fn icon(&self) -> Icon {
        match self.cfg.icon.as_deref() {
            Some(p) if Path::new(p).is_absolute() && Path::new(p).is_file() => {
                Icon { path: Some(p.into()), ..Default::default() }
            }
            Some(name) => crate::builtin::apps::named_icon(name)
                .unwrap_or_else(|| icons::themed(&["system-run", "utilities-terminal"], "!")),
            None => icons::themed(&["system-run", "utilities-terminal"], "!"),
        }
    }

    /// The parameter to use: what was typed, else the configured default.
    fn param<'a>(&'a self, typed: &'a str) -> Option<&'a str> {
        let typed = typed.trim();
        if !typed.is_empty() {
            return Some(typed);
        }
        self.cfg.default.as_deref().filter(|d| !d.is_empty())
    }

    fn row(&self, typed: &str, score: f64) -> ResultItem {
        let r = ResultItem::new(&self.cfg.keyword, self.title(), "", score).with_icon(self.icon());
        if !self.takes_param {
            let mut r = r.with_action("run", "Run").with_action("copy", "Copy command");
            r.subtitle = format!("{} · {}", self.cfg.keyword, self.shown(""));
            return r;
        }
        if let Some(p) = self.param(typed) {
            let mut r = r.with_action("run", "Run").with_action("copy", "Copy command");
            r.subtitle = self.shown(p);
            r
        } else {
            let mut r = r;
            r.subtitle = format!("Type what goes into: {}", self.cfg.command);
            r
        }
    }
}

impl ActionsProvider {
    pub fn new(settings: &Value) -> Self {
        let mut actions: Vec<Action> = vec![];
        for (keyword, v) in settings.get("actions").and_then(Value::as_object).into_iter().flatten() {
            let what = format!("actions.{keyword}");
            let mut cfg: ActionCfg = match serde_json::from_value(v.clone()) {
                Ok(c) => c,
                Err(e) => {
                    log::warn!("{what}: {e}; skipped");
                    continue;
                }
            };
            cfg.keyword.clone_from(keyword);
            let problem = if cfg.keyword.trim().is_empty() || cfg.keyword.contains(char::is_whitespace) {
                Some("keyword must be one word".to_string())
            } else if cfg.command.trim().is_empty() {
                Some("command is empty".into())
            } else {
                has_placeholders(&cfg.command).err()
            };
            if let Some(p) = problem {
                log::warn!("{what}: {p}; skipped");
                continue;
            }
            let takes_param = has_placeholders(&cfg.command).unwrap_or(false);
            actions.push(Action { cfg, takes_param });
        }
        Self { actions, terminal: settings.get("terminal").and_then(Value::as_str).unwrap_or("").to_string() }
    }

    /// `"<keyword> "` for every action that takes a parameter.
    pub fn triggers(&self) -> Vec<String> {
        self.actions.iter().filter(|a| a.takes_param).map(|a| format!("{} ", a.cfg.keyword)).collect()
    }

    fn find(&self, keyword: &str) -> Option<&Action> {
        self.actions.iter().find(|a| a.cfg.keyword == keyword)
    }
}

impl Provider for ActionsProvider {
    fn id(&self) -> &str {
        ID
    }

    fn query(&self, q: &Query) -> Vec<ResultItem> {
        if let Some(trigger) = q.trigger {
            return self.find(trigger.trim_end()).map(|a| vec![a.row(q.text, 1.0)]).unwrap_or_default();
        }
        // Plain search: actions without a parameter, found by keyword or title.
        let text = q.text.trim();
        if text.chars().count() < 2 {
            return vec![];
        }
        let lower = text.to_lowercase();
        self.actions
            .iter()
            .filter(|a| !a.takes_param)
            .filter_map(|a| {
                let kw = a.cfg.keyword.to_lowercase();
                let score = if kw == lower {
                    1.1 // the keyword itself: above everything, Enter runs it
                } else if kw.starts_with(&lower) {
                    0.75
                } else {
                    fuzzy::score(text, &a.cfg.title).map(|s| s * 0.6)?
                };
                Some(a.row("", score))
            })
            .collect()
    }

    fn execute(&self, result_id: &str, action_id: &str, query: &str) -> plugin::Result<ExecOutcome> {
        let a = self.find(result_id).ok_or_else(|| format!("no action {result_id:?}"))?;
        let param = if a.takes_param { a.param(query).ok_or("type a parameter after the keyword")? } else { "" };
        match action_id {
            "copy" => clipboard::copy(&a.shown(param))?,
            "run" => {
                if a.cfg.terminal {
                    process::run_in_terminal(&self.terminal, &a.argv(param))?;
                } else if a.cfg.shell {
                    process::spawn_shell(&fill(&a.cfg.command, param, &shell_quote))?;
                } else {
                    launch(a.argv(param))?;
                }
            }
            _ => return Err(Error::UnknownAction(action_id.into())),
        }
        Ok(ExecOutcome::Close)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn raw(v: &str) -> String {
        v.to_string()
    }

    /// `actions` as a list with `keyword`s, stored as the table config uses.
    fn provider(actions: &Value) -> ActionsProvider {
        let table: serde_json::Map<String, Value> = actions
            .as_array()
            .unwrap()
            .iter()
            .map(|a| (a["keyword"].as_str().unwrap().to_string(), a.clone()))
            .collect();
        ActionsProvider::new(&json!({ "actions": table }))
    }

    #[test]
    fn fills_placeholders() {
        assert_eq!(fill("https://x.org/{}", "a b", &raw), "https://x.org/a b");
        assert_eq!(fill("?q={url}", "a b&c", &raw), "?q=a%20b%26c");
        assert_eq!(fill("{2}-{1}-{3}", "\"new york\" 3", &raw), "3-new york-");
        assert_eq!(fill("{{literal}} {}", "x", &raw), "{literal} x");
        assert!(!has_placeholders("no params").unwrap());
        assert!(!has_placeholders("a {{b}}").unwrap(), "escaped braces are text");
        assert!(has_placeholders("{1}").unwrap());
        assert!(has_placeholders("{foo}").is_err());
        assert!(has_placeholders("{0}").unwrap(), "{{0}} is the whole parameter");
        assert_eq!(fill("?p={0}", "a b", &raw), "?p=a b");
        assert!(has_placeholders("open {").is_err());
    }

    #[test]
    fn windows_opens_start_and_off_path_programs_through_the_shell() {
        let p = provider(&json!([{ "keyword": "e", "command": "start msedge https://example.com?part={url}" }]));
        let argv = p.actions[0].argv("a b&c");
        let nothing_on_path = |_: &str| false;
        assert_eq!(
            windows_launch(argv, &nothing_on_path),
            Launch::ShellOpen("msedge".into(), "https://example.com?part=a%20b%26c".into())
        );
        let argv = ["START", "", "msedge", "https://x.org/a b"].map(String::from).to_vec();
        assert_eq!(
            windows_launch(argv, &nothing_on_path),
            Launch::ShellOpen("msedge".into(), r#""https://x.org/a b""#.into())
        );
        let argv = ["notepad", "x.txt"].map(String::from).to_vec();
        assert_eq!(windows_launch(argv.clone(), &|p: &str| p == "notepad"), Launch::Spawn(argv));
    }

    #[test]
    fn typed_text_never_reaches_a_shell() {
        let p = provider(&json!([{ "keyword": "gh", "command": "xdg-open https://github.com/{}" }]));
        let argv = p.actions[0].argv("foo; rm -rf ~");
        assert_eq!(argv, ["xdg-open", "https://github.com/foo; rm -rf ~"], "one argument");

        let p = provider(&json!([{ "keyword": "say", "command": "echo {} | wc -c", "shell": true }]));
        let line = p.actions[0].shown("it's $(rm x)");
        if cfg!(unix) {
            assert_eq!(line, r"echo 'it'\''s $(rm x)' | wc -c");
        }
    }

    #[test]
    fn invalid_actions_are_skipped() {
        let p = provider(&json!([
            { "keyword": "ok", "command": "true" },
            { "keyword": "two words", "command": "true" },
            { "keyword": "nocmd", "command": " " },
            { "keyword": "bad", "command": "x {foo}" },
            { "keyword": "", "command": "true" },
            { "keyword": "notext", "command": 1 },
        ]));
        assert_eq!(p.actions.len(), 1);
        assert_eq!(p.actions[0].cfg.keyword, "ok");
    }

    #[test]
    fn parameter_actions_get_triggers_others_answer_plain_searches() {
        let p = provider(&json!([
            { "keyword": "gh", "command": "xdg-open https://github.com/{}", "title": "GitHub" },
            { "keyword": "downloads", "command": "xdg-open ~/Downloads" },
        ]));
        assert_eq!(p.triggers(), ["gh "]);
        let r = p.query(&Query { text: "downloads", trigger: None, limit: 8 });
        assert_eq!(r.len(), 1);
        assert!(r[0].score > 1.0, "exact keyword outranks everything");
        assert_eq!(r[0].default_action().unwrap().id, "run");
        let downloads = format!("{}Downloads", std::path::MAIN_SEPARATOR);
        assert!(r[0].subtitle.ends_with(&downloads) && !r[0].subtitle.contains('~'), "{}", r[0].subtitle);
        assert!(
            p.query(&Query { text: "gh", trigger: None, limit: 8 }).is_empty(),
            "parameter actions go through their trigger"
        );
        let r = p.query(&Query { text: "rust-lang/rust", trigger: Some("gh "), limit: 8 });
        assert_eq!(r[0].subtitle, "xdg-open https://github.com/rust-lang/rust");
        assert_eq!(r[0].title, "GitHub");
    }

    #[test]
    fn empty_parameter_uses_the_default_or_waits() {
        let p = provider(&json!([
            { "keyword": "a", "command": "x {}", "default": "home" },
            { "keyword": "b", "command": "x {}" },
        ]));
        let r = p.query(&Query { text: "", trigger: Some("a "), limit: 8 });
        assert_eq!(r[0].subtitle, "x home");
        let r = p.query(&Query { text: " ", trigger: Some("b "), limit: 8 });
        assert!(r[0].actions.is_empty(), "nothing to run yet");
        assert!(p.execute("b", "run", "").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn runs_the_filled_command() {
        let tmp_dir = crate::test_support::tmp();
        let dir = tmp_dir.path().to_path_buf();
        std::fs::create_dir_all(&dir).unwrap();
        let p = provider(&json!([{ "keyword": "mk", "command": format!("touch {}/{{}}", dir.display()) }]));
        p.execute("mk", "run", "made by test").unwrap();
        let file = dir.join("made by test");
        let t = std::time::Instant::now();
        while !file.exists() && t.elapsed() < std::time::Duration::from_secs(5) {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(file.exists(), "one file, spaces and all");
    }

    #[test]
    fn manifest_example_is_valid_config() {
        let m = crate::plugin::manifest::Manifest::parse(include_str!("plugin.toml"), "actions").unwrap();
        let text = m.default_section().unwrap();
        let uncommented: String = text
            .lines()
            .filter_map(|l| l.strip_prefix("# ").filter(|r| r.contains('=') || r.starts_with('[')))
            .collect::<Vec<_>>()
            .join("\n");
        let cfg: crate::config::Config = toml::from_str(&uncommented).unwrap_or_else(|e| panic!("{e}\n{uncommented}"));
        let settings = m.effective_settings(cfg.plugin_settings(ID));
        let p = ActionsProvider::new(&settings);
        assert_eq!(p.actions.len(), 3, "{text}");
        assert_eq!(p.triggers(), ["gh ", "logs "]);
    }
}
