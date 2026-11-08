//! SSH hosts from `~/.ssh/config` (and files it `Include`s), connected in a terminal. Only
//! active when an `ssh` program is installed.

use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::SystemTime;

use serde_json::Value;

use crate::core::paths::resolve;
use crate::core::sync::{read, write};
use crate::core::{clipboard, fuzzy, icons, process};
use crate::plugin;
use crate::plugin::{Error, ExecOutcome, Icon, Provider, Query, ResultItem};

const ID: &str = "builtin-ssh";
/// Result id of the "connect to what I typed" row.
const TYPED: &str = "typed";

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Host {
    pub alias: String,
    pub hostname: String,
    pub user: String,
    pub port: String,
}

pub struct SshProvider {
    terminal: String,
    config: PathBuf,
    /// Hosts and the config's modification time they were read at.
    hosts: RwLock<(Vec<Host>, Option<SystemTime>)>,
}

/// Is an `ssh` client installed? Decides whether the plugin offers anything at all.
pub fn available() -> bool {
    process::which("ssh").is_some()
}

/// Files an `Include` pattern names; `*` and `?` are supported in the last path component.
fn include_paths(pattern: &str, base: &Path) -> Vec<PathBuf> {
    let p = resolve(pattern, base);
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if !name.contains(['*', '?']) {
        return vec![p];
    }
    let Some(dir) = p.parent() else { return vec![] };
    let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
    let mut out: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|f| f.file_name().is_some_and(|n| glob(&name, &n.to_string_lossy())))
        .collect();
    out.sort();
    out
}

/// `[user@]host[:port]` made of hostname characters only. The target is passed to a terminal
/// command line (through `cmd` on Windows), so nothing else may get in, and no leading `-`
/// that ssh would read as an option.
fn is_host(target: &str) -> bool {
    !target.is_empty()
        && !target.starts_with('-')
        && target.chars().all(|c| c.is_alphanumeric() || "._-@:[]%".contains(c))
}

/// Shell-style `*` / `?` matching.
fn glob(pat: &str, s: &str) -> bool {
    fn m(p: &[char], s: &[char]) -> bool {
        match p.first() {
            None => s.is_empty(),
            Some('*') => (0..=s.len()).any(|i| m(&p[1..], &s[i..])),
            Some('?') => !s.is_empty() && m(&p[1..], &s[1..]),
            Some(c) => s.first() == Some(c) && m(&p[1..], &s[1..]),
        }
    }
    let (p, s): (Vec<char>, Vec<char>) = (pat.chars().collect(), s.chars().collect());
    m(&p, &s)
}

/// Parse an ssh config. `base` resolves relative `Include`s (`~/.ssh`); `depth` stops loops.
pub fn parse(text: &str, base: &Path, depth: usize) -> Vec<Host> {
    let mut hosts: Vec<Host> = vec![];
    // Indexes into `hosts` of the aliases the current `Host` block applies to.
    let mut current: Vec<usize> = vec![];
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = match line.split_once(|c: char| c.is_whitespace() || c == '=') {
            Some((k, v)) => (
                k.to_lowercase(),
                v.trim_start_matches(|c: char| c.is_whitespace() || c == '=').trim().trim_matches('"'),
            ),
            None => continue,
        };
        match key.as_str() {
            "host" => {
                current.clear();
                for alias in value.split_whitespace() {
                    // Patterns (`*`, `?`, negations) are defaults, not hosts you can pick.
                    if alias.contains(['*', '?', '!']) {
                        continue;
                    }
                    let idx = if let Some(i) = hosts.iter().position(|h| h.alias == alias) {
                        i
                    } else {
                        hosts.push(Host { alias: alias.into(), ..Default::default() });
                        hosts.len() - 1
                    };
                    current.push(idx);
                }
            }
            "match" => current.clear(),
            "include" if depth < 8 => {
                for pattern in value.split_whitespace() {
                    for file in include_paths(pattern, base) {
                        if let Ok(t) = std::fs::read_to_string(&file) {
                            for h in parse(&t, base, depth + 1) {
                                if !hosts.iter().any(|x| x.alias == h.alias) {
                                    hosts.push(h);
                                }
                            }
                        }
                    }
                }
            }
            // ssh uses the first value it sees for each option.
            "hostname" | "user" | "port" => {
                for &i in &current {
                    let h = &mut hosts[i];
                    let field = match key.as_str() {
                        "hostname" => &mut h.hostname,
                        "user" => &mut h.user,
                        _ => &mut h.port,
                    };
                    if field.is_empty() {
                        *field = value.to_string();
                    }
                }
            }
            _ => {}
        }
    }
    hosts
}

fn icon() -> Icon {
    icons::themed(&["network-server", "utilities-terminal", "network-workgroup"], "S")
}

impl SshProvider {
    pub fn new(settings: &Value) -> Self {
        let config = settings.get("config").and_then(Value::as_str).unwrap_or("~/.ssh/config");
        let home = dirs::home_dir().unwrap_or_default();
        Self {
            terminal: settings.get("terminal").and_then(Value::as_str).unwrap_or("").to_string(),
            config: resolve(config, &home),
            hosts: RwLock::new((vec![], None)),
        }
    }

    fn mtime(&self) -> Option<SystemTime> {
        std::fs::metadata(&self.config).and_then(|m| m.modified()).ok()
    }

    fn load(&self) {
        let base = self.config.parent().map(Path::to_path_buf).unwrap_or_default();
        let hosts = std::fs::read_to_string(&self.config).map(|t| parse(&t, &base, 0)).unwrap_or_default();
        *write(&self.hosts) = (hosts, self.mtime());
    }

    fn connect_row(id: String, title: &str, subtitle: String, score: f64) -> ResultItem {
        ResultItem::new(id, title, subtitle, score)
            .with_icon(icon())
            .with_action("connect", "Connect")
            .with_action("copy", "Copy ssh command")
    }
}

impl Provider for SshProvider {
    fn id(&self) -> &str {
        ID
    }

    fn warm_up(&self) {
        self.load();
    }

    fn refresh(&self) {
        self.load();
    }

    fn needs_refresh(&self) -> bool {
        read(&self.hosts).1 != self.mtime()
    }

    fn query(&self, q: &Query) -> Vec<ResultItem> {
        let text = q.text.trim();
        let g = read(&self.hosts);
        let mut out: Vec<ResultItem> =
            g.0.iter()
                .enumerate()
                .filter_map(|(i, h)| {
                    let score = if text.is_empty() {
                        0.8 - i as f64 * 0.001
                    } else {
                        [fuzzy::score(text, &h.alias), fuzzy::score(text, &h.hostname).map(|s| s * 0.8)]
                            .into_iter()
                            .flatten()
                            .reduce(f64::max)?
                    };
                    let mut target = if h.hostname.is_empty() { h.alias.clone() } else { h.hostname.clone() };
                    if !h.user.is_empty() {
                        target = format!("{}@{target}", h.user);
                    }
                    if !h.port.is_empty() {
                        target += &format!(":{}", h.port);
                    }
                    Some(Self::connect_row(format!("host:{}", h.alias), &h.alias, target, score))
                })
                .collect();
        if !text.is_empty() && !text.contains(char::is_whitespace) && !g.0.iter().any(|h| h.alias == text) {
            out.push(Self::connect_row(TYPED.into(), &format!("ssh {text}"), "Connect to this address".into(), 0.3));
        }
        if out.is_empty() {
            out.push(
                ResultItem::new(
                    "hint",
                    "No hosts in your SSH config",
                    format!("Type an address, or add hosts to {}", self.config.display()),
                    0.5,
                )
                .with_icon(icon()),
            );
        }
        out
    }

    fn execute(&self, result_id: &str, action_id: &str, query: &str) -> plugin::Result<ExecOutcome> {
        let target = match result_id.strip_prefix("host:") {
            Some(alias) => alias.to_string(),
            None if result_id == TYPED => query.trim().to_string(),
            None => return Err(Error::UnknownResult(result_id.into())),
        };
        // Never let a typed target be read as an ssh option.
        if !is_host(&target) {
            return Err("invalid host".into());
        }
        match action_id {
            "connect" => process::run_in_terminal(&self.terminal, &["ssh".to_string(), target])?,
            "copy" => clipboard::copy(&format!("ssh {target}"))?,
            _ => return Err(Error::UnknownAction(action_id.into())),
        }
        Ok(ExecOutcome::Close)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hosts_skips_patterns_and_keeps_first_values() {
        let text = "# comment\nHost *\n  User everyone\nHost web1 web2\n  HostName 10.0.0.1\n  User deploy\nHost db\n  Hostname=db.example.com\n  Port 2222\n  User first\n  User second\nHost !bad web?\nMatch host x\n  User ignored\n";
        let hosts = parse(text, Path::new("/nonexistent"), 0);
        assert_eq!(hosts.iter().map(|h| h.alias.as_str()).collect::<Vec<_>>(), ["web1", "web2", "db"]);
        assert_eq!((hosts[0].hostname.as_str(), hosts[0].user.as_str()), ("10.0.0.1", "deploy"));
        assert_eq!(hosts[1].hostname, "10.0.0.1", "one block, several aliases");
        assert_eq!(
            (hosts[2].hostname.as_str(), hosts[2].port.as_str(), hosts[2].user.as_str()),
            ("db.example.com", "2222", "first")
        );
    }

    #[test]
    fn follows_includes_with_globs() {
        let tmp_dir = crate::test_support::tmp();
        let dir = tmp_dir.path().to_path_buf();
        std::fs::create_dir_all(dir.join("conf.d")).unwrap();
        std::fs::write(dir.join("conf.d/a.conf"), "Host inc-a\n  HostName a\n").unwrap();
        std::fs::write(dir.join("conf.d/b.conf"), "Host inc-b\nInclude conf.d/*.conf\n").unwrap();
        std::fs::write(dir.join("conf.d/skip.txt"), "Host nope\n").unwrap();
        let hosts = parse("Include conf.d/*.conf\nHost main\n", &dir, 0);
        assert_eq!(
            hosts.iter().map(|h| h.alias.as_str()).collect::<Vec<_>>(),
            ["inc-a", "inc-b", "main"],
            "no loop, no duplicates"
        );
    }

    #[test]
    fn rows_and_typed_addresses() {
        let tmp_dir = crate::test_support::tmp();
        let dir = tmp_dir.path().to_path_buf();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config"), "Host web1\n  HostName 10.0.0.1\n  User deploy\n").unwrap();
        let p = SshProvider::new(&serde_json::json!({ "config": dir.join("config").to_string_lossy() }));
        assert!(p.needs_refresh());
        p.refresh();
        assert!(!p.needs_refresh());
        let rows = p.query(&Query { text: "web", trigger: Some("ssh "), limit: 8 });
        assert_eq!(rows[0].title, "web1");
        assert_eq!(rows[0].subtitle, "deploy@10.0.0.1");
        assert_eq!(rows.last().unwrap().id, TYPED);
        assert!(p.execute(TYPED, "connect", "-oProxyCommand=x").is_err(), "options are not hosts");
        assert!(p.execute(TYPED, "copy", "host&calc").is_err(), "shell characters are not hosts");
        assert!(is_host("deploy@10.0.0.1") && is_host("[::1]") && is_host("web1:2222"));
    }
}
