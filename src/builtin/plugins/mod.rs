//! Plugin manager: list, enable/disable, configure, install and uninstall plugins from the
//! launcher itself. Every change is written to `config.toml` or the plugins folder and then
//! applied with `Engine::reload`, so no restart is needed.
//!
//! Commands (after the `plugin ` trigger):
//! - *(empty)* or `<filter>`: every plugin, Enter toggles it
//! - `config <id>`: its settings; `set <id> <key> <value>`: change one
//! - `install <folder | git url>`, `uninstall <id>`, `reload`

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};

use serde_json::Value;

use crate::core::paths::expand_home;
use crate::core::sync::lock;
use crate::core::{fuzzy, icons, process};
use crate::engine::{Engine, Kind, PluginInfo};
use crate::plugin;
use crate::plugin::manifest::{Manifest, Setting, SettingKind};
use crate::plugin::{Error, ExecOutcome, Icon, Provider, Query, ResultItem};

const ID: &str = "builtin-plugins";
const TRIGGER: &str = "plugin ";
const COMMANDS: [(&str, &str); 4] = [
    ("install ", "Install a plugin from a folder or a git URL"),
    ("uninstall ", "Remove an installed plugin"),
    ("config ", "Show and change a plugin's settings"),
    ("reload", "Rescan plugins, config.toml and indexes"),
];

/// What a row stands for. Rendered into `ResultItem::id` and parsed back in `execute`, so the two
/// always agree on the format. The text form is stable: frecency remembers rows by it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RowId {
    /// `p:<plugin>`: a plugin in the list.
    Plugin(String),
    /// `cmd:<word>`: a sub-command suggestion.
    Command(String),
    /// `pick:<plugin>`: a plugin offered as the argument of a command.
    Pick(String),
    /// `s:<plugin>:<key>`: a setting in a plugin's settings list.
    Setting {
        plugin: String,
        key: String,
    },
    /// `set:<plugin>:<key>`: the confirmation row of `set`.
    Save {
        plugin: String,
        key: String,
    },
    /// `key:<key>`: a setting key offered while typing `set <plugin> `.
    Key(String),
    /// `rm:<plugin>`: the confirmation row of `uninstall`.
    Remove(String),
    Install,
    Reload,
    /// Rows without actions: hints, errors, notices.
    Info(&'static str),
}

impl std::fmt::Display for RowId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Plugin(id) => write!(f, "p:{id}"),
            Self::Command(word) => write!(f, "cmd:{word}"),
            Self::Pick(id) => write!(f, "pick:{id}"),
            Self::Setting { plugin, key } => write!(f, "s:{plugin}:{key}"),
            Self::Save { plugin, key } => write!(f, "set:{plugin}:{key}"),
            Self::Key(key) => write!(f, "key:{key}"),
            Self::Remove(id) => write!(f, "rm:{id}"),
            Self::Install => f.write_str("install"),
            Self::Reload => f.write_str("reload"),
            Self::Info(what) => f.write_str(what),
        }
    }
}

impl std::str::FromStr for RowId {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Error> {
        let pair = |rest: &str| {
            rest.split_once(':')
                .map(|(plugin, key)| (plugin.to_string(), key.to_string()))
                .ok_or_else(|| Error::UnknownResult(s.to_string()))
        };
        Ok(match s.split_once(':').unwrap_or((s, "")) {
            ("p", id) => Self::Plugin(id.into()),
            ("cmd", word) => Self::Command(word.into()),
            ("pick", id) => Self::Pick(id.into()),
            ("s", rest) => pair(rest).map(|(plugin, key)| Self::Setting { plugin, key })?,
            ("set", rest) => pair(rest).map(|(plugin, key)| Self::Save { plugin, key })?,
            ("key", key) => Self::Key(key.into()),
            ("rm", id) => Self::Remove(id.into()),
            ("install", "") => Self::Install,
            ("reload", "") => Self::Reload,
            _ => return Err(Error::UnknownResult(s.to_string())),
        })
    }
}

pub struct PluginManager {
    engine: Weak<Engine>,
    /// Message to show at the top while the search box still holds `.0` (result of the last action).
    notice: Mutex<Option<(String, String)>>,
}

fn icon() -> Icon {
    icons::themed(&["system-software-install", "application-x-addon", "preferences-system"], "P")
}

fn row(id: &RowId, title: impl Into<String>, subtitle: impl Into<String>, score: f64) -> ResultItem {
    ResultItem::new(id.to_string(), title, subtitle, score).with_icon(icon())
}

/// `s` if it is a git source this launcher clones: `https://…` or SSH (`ssh://…`, `git@host:…`).
/// Never plain `http://` (a network attacker could swap the code you are about to run), local
/// paths, `ext::`/`file://` transports, or anything git could read as an option.
fn git_source(s: &str) -> Option<&str> {
    let allowed = ["https://", "ssh://", "git@"].iter().any(|p| s.starts_with(p));
    (allowed && !s.chars().any(|c| c.is_whitespace() || c.is_control())).then_some(s)
}

/// Where cloned plugins wait for review before they are installed.
fn staging_root() -> PathBuf {
    crate::config::portable_dir()
        .map(|d| d.join("cache"))
        .or_else(dirs::cache_dir)
        .unwrap_or_else(std::env::temp_dir)
        .join("kairos")
        .join("staging")
}

/// Creates folders only the user can read (mode 0700 on Unix).
#[cfg(unix)]
fn private_dir_builder() -> std::fs::DirBuilder {
    use std::os::unix::fs::DirBuilderExt;
    let mut b = std::fs::DirBuilder::new();
    b.mode(0o700);
    b
}

#[cfg(not(unix))]
fn private_dir_builder() -> std::fs::DirBuilder {
    std::fs::DirBuilder::new()
}

/// Clone `url` into a new private staging folder and return it.
fn clone_to_staging(url: &str) -> Result<PathBuf, String> {
    let root = staging_root();
    std::fs::create_dir_all(&root).map_err(|e| format!("{}: {e}", root.display()))?;
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let dir = root.join(format!("clone-{}-{stamp}", std::process::id()));
    private_dir_builder().create(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let status = std::process::Command::new("git")
        .args(["clone", "--depth", "1", "--quiet", "--no-recurse-submodules", "--", url])
        .arg(&dir)
        // Only these transports, also for anything the clone pulls in.
        .env("GIT_ALLOW_PROTOCOL", "https:ssh")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .status();
    match status {
        Ok(st) if st.success() => Ok(dir),
        other => {
            let _ = std::fs::remove_dir_all(&dir);
            Err(match other {
                Ok(st) => format!("git clone failed ({st})"),
                Err(e) => format!("git: {e}"),
            })
        }
    }
}

/// A setting's current value for display (`"x"`, `5`, `true`, `{3 entries}`).
fn show_value(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => "(unset)".into(),
        Some(Value::Object(m)) => format!("{{{} entries}}", m.len()),
        Some(Value::Array(a)) if a.iter().any(Value::is_object) => format!("[{} items]", a.len()),
        Some(v) => v.to_string(),
    }
}

/// Parse what the user typed for `setting` into a TOML value (the text rows edit one line;
/// lists and tables are edited on the settings page or in `config.toml`).
fn parse_value(setting: &Setting, raw: &str) -> Result<toml_edit::Item, String> {
    match setting.kind {
        SettingKind::Table | SettingKind::List => Err("edit this one on the settings page or in config.toml".into()),
        // Saved to the secret store, never to config.toml (an empty value removes it).
        SettingKind::Secret => Ok(toml_edit::value(raw.trim())),
        SettingKind::Secrets => Err("type a name, then the value".into()),
        _ => setting.parse_input(raw).map(Option::unwrap_or_default),
    }
}

/// `set <id> <key> <pasted text>` for a setting with `import`: the text converted and merged
/// into the current table, as the value to save, and the imported names.
fn import_value(p: &PluginInfo, s: &Setting, raw: &str) -> Result<(toml_edit::Item, Vec<String>), String> {
    let current = p.settings.get(&s.key).and_then(|v| toml::Value::try_from(v).ok());
    let (table, names) = s.import_entries(current.as_ref(), raw)?;
    let text = toml::to_string(&table).map_err(|e| e.to_string())?;
    Ok((s.parse_input(&text)?.unwrap_or_default(), names))
}

/// Copy a plugin folder, without its `.git`. Symlinks are refused: one could point anywhere,
/// and copying the link target would install files the user never saw.
fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        let ft = entry.file_type()?;
        if ft.is_dir() {
            if entry.file_name() != ".git" {
                copy_dir(&src, &dst)?;
            }
        } else if ft.is_file() {
            std::fs::copy(&src, &dst)?; // keeps the executable bit on Unix
        } else {
            return Err(std::io::Error::other(format!("{} is a symlink or special file", src.display())));
        }
    }
    Ok(())
}

impl PluginManager {
    pub fn new(engine: Weak<Engine>) -> Self {
        Self { engine, notice: Mutex::new(None) }
    }

    fn engine(&self) -> Result<Arc<Engine>, String> {
        self.engine.upgrade().ok_or_else(|| "launcher is shutting down".into())
    }

    fn set_notice(&self, query: &str, msg: impl Into<String>) {
        *lock(&self.notice) = Some((query.to_string(), msg.into()));
    }

    fn notice_for(&self, text: &str) -> Option<String> {
        lock(&self.notice).as_ref().filter(|(q, _)| q == text).map(|(_, m)| m.clone())
    }

    /// Show `msg` and put `next` (without the trigger) in the search box.
    fn report(&self, next: &str, msg: impl Into<String>) -> ExecOutcome {
        self.set_notice(next, msg);
        ExecOutcome::complete(format!("{TRIGGER}{next}"))
    }

    // ---------------------------------------------------------------- listing

    fn list(&self, plugins: &[PluginInfo], filter: &str) -> Vec<ResultItem> {
        let mut out: Vec<ResultItem> = plugins
            .iter()
            .enumerate()
            .filter_map(|(i, p)| {
                let m = &p.manifest;
                let score = if filter.is_empty() {
                    // Stable alphabetical order, enabled first.
                    0.6 - i as f64 * 0.001 - if p.enabled { 0.0 } else { 0.1 }
                } else {
                    [fuzzy::score(filter, &m.name), fuzzy::score(filter, &m.id).map(|s| s * 0.8)]
                        .into_iter()
                        .flatten()
                        .reduce(f64::max)?
                };
                let kind = if p.kind == Kind::Bundled { "bundled" } else { "external" };
                let toggles = m.id != ID;
                let state = match (p.enabled, toggles) {
                    (true, true) => "Enabled · ↵ to disable",
                    (false, true) => "Disabled · ↵ to enable",
                    (true, false) => "Enabled",
                    (false, false) => "Disabled",
                };
                let triggers = m.query.triggers.iter().map(|t| t.trim_end()).collect::<Vec<_>>().join(" ");
                let mut sub = format!("{state} · {kind} · v{}", m.version);
                if !triggers.is_empty() {
                    sub += &format!(" · {triggers}");
                }
                if !m.description.is_empty() {
                    sub += &format!(" — {}", m.description);
                }
                let title = if p.enabled { m.name.clone() } else { format!("{} (disabled)", m.name) };
                let mut r = row(&RowId::Plugin(m.id.clone()), title, sub, score);
                if toggles {
                    let label = if p.enabled {
                        "Disable plugin (currently enabled)"
                    } else {
                        "Enable plugin (currently disabled)"
                    };
                    r = r.with_action("toggle", label);
                }
                if !m.settings.is_empty() {
                    r = r.with_action("config", "Settings");
                }
                if let Some(dir) = &p.dir {
                    r = r.with_action("folder", "Open folder");
                    if self.removable(dir) {
                        r = r.with_action("uninstall", "Uninstall…");
                    }
                }
                Some(r)
            })
            .collect();
        // Sub-commands, offered while typing them.
        for (i, (cmd, about)) in COMMANDS.iter().enumerate() {
            let word = cmd.trim_end();
            if filter.is_empty() || (word.starts_with(filter) && filter.len() >= 2) {
                let score = if filter.is_empty() { 0.3 - i as f64 * 0.01 } else { 0.9 };
                out.push(
                    row(&RowId::Command(word.into()), word, *about, score)
                        .with_action("complete", "Use command")
                        .with_complete(format!("{TRIGGER}{cmd}")),
                );
            }
        }
        out
    }

    fn removable(&self, dir: &Path) -> bool {
        self.engine().is_ok_and(|e| dir.starts_with(&e.paths().plugins))
    }

    fn find<'a>(plugins: &'a [PluginInfo], id: &str) -> Option<&'a PluginInfo> {
        plugins.iter().find(|p| p.manifest.id == id)
    }

    /// Plugins whose id or name starts like `partial`, as completions for `<cmd> `.
    fn complete_ids(
        plugins: &[PluginInfo],
        cmd: &str,
        partial: &str,
        only: impl Fn(&PluginInfo) -> bool,
    ) -> Vec<ResultItem> {
        plugins
            .iter()
            .filter(|p| only(p))
            .filter_map(|p| {
                let score = if partial.is_empty() {
                    Some(0.5)
                } else {
                    [fuzzy::score(partial, &p.manifest.id), fuzzy::score(partial, &p.manifest.name)]
                        .into_iter()
                        .flatten()
                        .reduce(f64::max)
                }?;
                Some(
                    row(&RowId::Pick(p.manifest.id.clone()), &p.manifest.name, &p.manifest.id, score)
                        .with_action("complete", "Choose")
                        .with_complete(format!("{TRIGGER}{cmd} {}", p.manifest.id)),
                )
            })
            .collect()
    }

    // ---------------------------------------------------------------- config

    fn config_rows(p: &PluginInfo) -> Vec<ResultItem> {
        let id = &p.manifest.id;
        let mut out: Vec<ResultItem> = p
            .manifest
            .settings
            .iter()
            .enumerate()
            .map(|(i, s)| {
                if s.kind.is_secret() {
                    return Self::secret_row(p, s, 0.9 - i as f64 * 0.001);
                }
                let cur = p.settings.get(&s.key);
                let default = s.default.as_ref().and_then(|d| serde_json::to_value(d).ok());
                let mut sub = [s.label.as_str(), s.description.as_str()]
                    .into_iter()
                    .filter(|t| !t.is_empty())
                    .collect::<Vec<_>>()
                    .join(": ");
                if !s.options.is_empty() {
                    sub += &format!(" ({})", s.options.join(" | "));
                }
                if cur != default.as_ref() {
                    sub += &format!(" · default {}", show_value(default.as_ref()));
                }
                let mut r = row(
                    &RowId::Setting { plugin: id.clone(), key: s.key.clone() },
                    format!("{} = {}", s.key, show_value(cur)),
                    sub,
                    0.9 - i as f64 * 0.001,
                );
                let raw = match cur {
                    Some(Value::String(v)) => v.clone(),
                    Some(v) => v.to_string(),
                    None => String::new(),
                };
                r = match s.kind {
                    SettingKind::Bool => r.with_action("flip", "Toggle"),
                    SettingKind::Choice if !s.options.is_empty() => {
                        r.with_action("next", "Next option").with_complete(format!("{TRIGGER}set {id} {} {raw}", s.key))
                    }
                    SettingKind::Table | SettingKind::List => r.with_action("page", "Edit on the settings page"),
                    _ => r.with_action("edit", "Change…").with_complete(format!("{TRIGGER}set {id} {} {raw}", s.key)),
                };
                if cur != default.as_ref() {
                    r = r.with_action("reset", "Reset to default");
                }
                r.with_action("file", "Edit config.toml")
            })
            .collect();
        if out.is_empty() {
            out.push(row(&RowId::Info("none"), format!("{} has no settings", p.manifest.name), id.clone(), 0.9));
        } else {
            out.insert(
                0,
                row(&RowId::Plugin(id.clone()), format!("{} settings", p.manifest.name), "Open the settings page", 1.0)
                    .with_action("config", "Open settings page"),
            );
        }
        out
    }

    /// A secret's row shows only whether it is set (for named secrets: their names), never a
    /// value.
    fn secret_row(p: &PluginInfo, s: &Setting, score: f64) -> ResultItem {
        let id = &p.manifest.id;
        let cur = p.settings.get(&s.key);
        let named = s.kind == SettingKind::Secrets;
        let shown = match cur.and_then(Value::as_object) {
            Some(m) if named => m.keys().cloned().collect::<Vec<_>>().join(", "),
            _ if named => "(none)".into(),
            _ if cur.is_some() => "(set)".into(),
            _ => "(not set)".into(),
        };
        let set = cur.is_some() && !named;
        let about =
            [s.label.as_str(), s.description.as_str()].into_iter().filter(|t| !t.is_empty()).collect::<Vec<_>>();
        let mut r = row(
            &RowId::Setting { plugin: id.clone(), key: s.key.clone() },
            format!("{} = {shown}", s.key),
            format!("{} · stored privately, not in config.toml", about.join(": "))
                .trim_start_matches(" · ")
                .to_string(),
            score,
        )
        .with_action("edit", "Change…")
        .with_complete(format!("{TRIGGER}set {id} {} ", s.key));
        if set {
            r = r.with_action("reset", "Remove");
        }
        r
    }

    fn set_row(p: &PluginInfo, rest: &str) -> Vec<ResultItem> {
        let (key, raw) = rest.split_once(' ').unwrap_or((rest, ""));
        let Some(s) = p.manifest.settings.iter().find(|s| s.key == key) else {
            return p
                .manifest
                .settings
                .iter()
                .filter(|s| s.key.starts_with(key))
                .map(|s| {
                    row(&RowId::Key(s.key.clone()), &s.key, &s.label, 0.8)
                        .with_action("complete", "Choose")
                        .with_complete(format!("{TRIGGER}set {} {} ", p.manifest.id, s.key))
                })
                .collect();
        };
        if s.kind == SettingKind::Secrets {
            return Self::secrets_rows(p, s, raw);
        }
        if s.kind == SettingKind::Secret {
            let (title, action) = if raw.trim().is_empty() {
                (format!("Remove {key}"), "Remove")
            } else {
                (format!("Save {key} (hidden)"), "Save")
            };
            return vec![
                row(
                    &RowId::Save { plugin: p.manifest.id.clone(), key: key.to_string() },
                    title,
                    format!(
                        "{} · stored privately, not in config.toml · Enter to {}",
                        p.manifest.name,
                        action.to_lowercase()
                    ),
                    1.0,
                )
                .with_action("save", action),
            ];
        }
        if s.imports(raw) {
            return vec![match import_value(p, s, raw) {
                Ok((_, names)) => row(
                    &RowId::Save { plugin: p.manifest.id.clone(), key: key.to_string() },
                    format!("Add {} to {key}", names.join(", ")),
                    format!("{} · replaces entries with the same name · Enter to save", p.manifest.name),
                    1.0,
                )
                .with_action("save", "Save"),
                Err(e) => row(&RowId::Info("invalid"), format!("{key}: {e}"), &s.label, 1.0),
            }];
        }
        match parse_value(s, raw) {
            Ok(v) => vec![
                row(
                    &RowId::Save { plugin: p.manifest.id.clone(), key: key.to_string() },
                    format!("Set {key} = {}", v.to_string().trim()),
                    format!("{} · Enter to save", p.manifest.name),
                    1.0,
                )
                .with_action("save", "Save"),
            ],
            Err(e) => vec![row(&RowId::Info("invalid"), format!("{key}: {e}"), &s.label, 1.0)],
        }
    }

    /// `set <id> <key> <name> <value>` for a `secrets` setting: the stored names while the name
    /// is typed, then the row that saves (or, without a value, removes) that entry.
    fn secrets_rows(p: &PluginInfo, s: &Setting, raw: &str) -> Vec<ResultItem> {
        let id = &p.manifest.id;
        let key = &s.key;
        let names: Vec<String> =
            p.settings.get(key).and_then(Value::as_object).map(|m| m.keys().cloned().collect()).unwrap_or_default();
        let raw = raw.trim();
        let (name, value) = raw.split_once(' ').map_or((raw, ""), |(n, v)| (n, v.trim()));
        if !name.is_empty() && !crate::plugin::secrets::valid_name(name) {
            return vec![row(
                &RowId::Info("invalid"),
                format!("{key}: a name may only use letters, digits, - and _"),
                &s.label,
                1.0,
            )];
        }
        let save = |title: String, action: &str| {
            row(
                &RowId::Save { plugin: id.clone(), key: key.clone() },
                title,
                format!(
                    "{} · stored privately, not in config.toml · Enter to {}",
                    p.manifest.name,
                    action.to_lowercase()
                ),
                1.0,
            )
            .with_action("save", action)
        };
        if !value.is_empty() {
            return vec![save(format!("Save {key} {name} (hidden)"), "Save")];
        }
        // Only a name so far (the query arrives trimmed, so a stored name also offers removal).
        let mut out = vec![];
        if names.iter().any(|n| n == name) {
            out.push(save(format!("Remove {key} {name}"), "Remove"));
        }
        out.extend(names.iter().filter(|n| n.starts_with(name) && *n != name).map(|n| {
            row(&RowId::Key(format!("{key} {n}")), n, "stored · type a new value, or Enter on it to remove", 0.8)
                .with_action("complete", "Choose")
                .with_complete(format!("{TRIGGER}set {id} {key} {n} "))
        }));
        out.push(row(
            &RowId::Info("hint"),
            format!("{key}: type a name, a space, then the value"),
            format!("{} · stored privately, not in config.toml", p.manifest.name),
            0.9,
        ));
        out
    }

    // ---------------------------------------------------------------- install

    fn install_preview(src: &str) -> ResultItem {
        if src.is_empty() {
            return row(
                &RowId::Info("hint"),
                "Install from a folder or git URL",
                "plugin install ~/my-plugin  ·  plugin install https://host/repo",
                1.0,
            );
        }
        if git_source(src).is_some() {
            if process::which("git").is_none() {
                return row(
                    &RowId::Info("invalid"),
                    "git is not installed",
                    "Clone the repository yourself, then install its folder",
                    1.0,
                );
            }
            return row(
                &RowId::Install,
                format!("Fetch {src}"),
                "Clones it for review; nothing is installed until you confirm",
                1.0,
            )
            .with_action("install", "Fetch");
        }
        match Manifest::load(&expand_home(src)) {
            Ok(m) => {
                let runs = match &m.run.url {
                    Some(url) => format!("sends your searches to {url}"),
                    None => {
                        format!("runs `{}` unsandboxed, as you", m.run.argv().map(|a| a.join(" ")).unwrap_or_default())
                    }
                };
                row(
                    &RowId::Install,
                    format!("Install {} {}", m.name, m.version),
                    format!("{} · {runs}", if m.description.is_empty() { &m.id } else { &m.description }),
                    1.0,
                )
                .with_action("install", "Install")
            }
            Err(_) if src.starts_with("http://") => row(
                &RowId::Info("invalid"),
                "Use https://",
                "Plain http lets anyone on the network change the code",
                1.0,
            ),
            Err(e) => row(&RowId::Info("invalid"), "Not a plugin folder", e, 1.0),
        }
    }

    /// Install the plugin in `folder`: copied to `<plugins>/<id>`, then loaded. A folder that
    /// was staged by a clone is removed afterwards.
    fn install(&self, folder: &Path) -> Result<String, String> {
        let engine = self.engine()?;
        let m = Manifest::load(folder)?;
        if let Some(p) = Self::find(&engine.plugins().plugins, &m.id) {
            return Err(format!(
                "{} is already installed ({})",
                m.id,
                p.dir.as_deref().map_or("bundled".into(), |d| d.display().to_string())
            ));
        }
        // The id is validated by the manifest parser (letters, digits, `-`, `_`), so it is a
        // plain folder name that cannot point outside the plugins folder.
        let dest = engine.paths().plugins.join(&m.id);
        if dest.exists() {
            return Err(format!("{} already exists", dest.display()));
        }
        if let Err(e) = copy_dir(folder, &dest) {
            let _ = std::fs::remove_dir_all(&dest);
            return Err(format!("copy to {}: {e}", dest.display()));
        }
        if folder.canonicalize().is_ok_and(|f| staging_root().canonicalize().is_ok_and(|r| f.starts_with(r))) {
            let _ = std::fs::remove_dir_all(folder);
        }
        engine.reload();
        engine.write_default_sections();
        Ok(format!("Installed {} {} into {}", m.name, m.version, dest.display()))
    }

    fn uninstall_rows(&self, plugins: &[PluginInfo], id: &str) -> Vec<ResultItem> {
        let Some(p) = Self::find(plugins, id) else {
            return Self::complete_ids(plugins, "uninstall", id, |p| {
                p.dir.as_deref().is_some_and(|d| self.removable(d))
            });
        };
        match &p.dir {
            Some(dir) if self.removable(dir) => vec![
                row(
                    &RowId::Remove(id.to_string()),
                    format!("Remove {} permanently", p.manifest.name),
                    format!("Deletes {}", dir.display()),
                    1.0,
                )
                .with_action("remove", "Remove"),
            ],
            Some(dir) => vec![row(
                &RowId::Info("invalid"),
                format!("{} was not installed here", p.manifest.name),
                format!("Delete {} yourself, or disable it", dir.display()),
                1.0,
            )],
            None => vec![row(
                &RowId::Info("invalid"),
                format!("{} is bundled", p.manifest.name),
                "Bundled plugins can be disabled, not removed",
                1.0,
            )],
        }
    }
}

impl Provider for PluginManager {
    fn id(&self) -> &str {
        ID
    }

    fn query(&self, q: &Query) -> Vec<ResultItem> {
        let Ok(engine) = self.engine() else { return vec![] };
        let reg = engine.plugins();
        let plugins = &reg.plugins;
        let text = q.text.trim_start();
        let (cmd, rest) = text.split_once(' ').map_or((text, ""), |(c, r)| (c, r.trim()));
        let mut out = match cmd {
            "install" if text.len() > cmd.len() => vec![Self::install_preview(rest)],
            "uninstall" if text.len() > cmd.len() => self.uninstall_rows(plugins, rest),
            "config" if text.len() > cmd.len() => match Self::find(plugins, rest) {
                Some(p) => Self::config_rows(p),
                None => Self::complete_ids(plugins, "config", rest, |p| !p.manifest.settings.is_empty()),
            },
            "set" if text.len() > cmd.len() => {
                let (id, kv) = rest.split_once(' ').unwrap_or((rest, ""));
                match Self::find(plugins, id) {
                    Some(p) => Self::set_row(p, kv),
                    None => Self::complete_ids(plugins, "config", id, |p| !p.manifest.settings.is_empty()),
                }
            }
            "reload" => vec![
                row(&RowId::Reload, "Reload plugins", "Rescan plugins, config.toml and indexes", 1.0)
                    .with_action("reload", "Reload"),
            ],
            _ => self.list(plugins, text),
        };
        if let Some(msg) = self.notice_for(q.text) {
            out.push(row(&RowId::Info("notice"), msg, "", 2.0));
        }
        out
    }

    fn execute(&self, result_id: &str, action_id: &str, query: &str) -> plugin::Result<ExecOutcome> {
        let engine = self.engine()?;
        let reg = engine.plugins();
        let text = query.trim();
        let row: RowId = result_id.parse()?;
        let outcome = match (row, action_id) {
            (RowId::Plugin(id), "toggle") => {
                let p = Self::find(&reg.plugins, &id).ok_or("plugin gone")?;
                engine.set_plugin_value(&id, "enabled", toml_edit::value(!p.enabled))?;
                self.report(text, format!("{} {}", p.manifest.name, if p.enabled { "disabled" } else { "enabled" }))
            }
            (RowId::Plugin(id), "config") | (RowId::Setting { plugin: id, .. }, "page") => ExecOutcome::Settings(id),
            (RowId::Plugin(id), "uninstall") => ExecOutcome::complete(format!("{TRIGGER}uninstall {id}")),
            (RowId::Plugin(id), "folder") => {
                let p = Self::find(&reg.plugins, &id).ok_or("plugin gone")?;
                process::open(&p.dir.as_deref().ok_or("bundled plugin")?.to_string_lossy())?;
                ExecOutcome::Close
            }
            (RowId::Setting { plugin, key }, "flip" | "next" | "reset") => {
                let p = Self::find(&reg.plugins, &plugin).ok_or("plugin gone")?;
                let s = p.manifest.settings.iter().find(|s| s.key == key).ok_or("setting gone")?;
                if s.kind == SettingKind::Secret {
                    engine.set_plugin_secret(&plugin, &key, "")?;
                    return Ok(self.report(&format!("config {plugin}"), format!("Removed {key}")));
                }
                let cur = p.settings.get(&key);
                let item = match action_id {
                    "flip" => toml_edit::value(!cur.and_then(Value::as_bool).unwrap_or(false)),
                    "next" => {
                        let at = s.options.iter().position(|o| Some(o.as_str()) == cur.and_then(Value::as_str));
                        let next = at.map_or(0, |i| i + 1);
                        let option = s.options.get(next).or(s.options.first()).ok_or("no options")?;
                        toml_edit::value(option.clone())
                    }
                    _ => toml_edit::Item::None,
                };
                engine.set_plugin_value(&plugin, &key, item)?;
                ExecOutcome::complete(format!("{TRIGGER}{text}"))
            }
            (RowId::Setting { .. }, "file") => {
                crate::config::ensure_config_file_at(&engine.paths().config).map_err(|e| e.to_string())?;
                process::open(&engine.paths().config.to_string_lossy())?;
                ExecOutcome::Close
            }
            (RowId::Save { plugin, key }, "save") => {
                let p = Self::find(&reg.plugins, &plugin).ok_or("plugin gone")?;
                let s = p.manifest.settings.iter().find(|s| s.key == key).ok_or("setting gone")?;
                let raw = text.splitn(4, ' ').nth(3).unwrap_or("");
                if s.kind == SettingKind::Secrets {
                    let (name, value) = raw.trim().split_once(' ').map_or((raw.trim(), ""), |(n, v)| (n, v));
                    let done = if value.trim().is_empty() { "Removed" } else { "Saved" };
                    return Ok(match engine.set_plugin_secret_entry(&plugin, &key, name, value) {
                        Ok(()) => self.report(&format!("config {plugin}"), format!("{done} {key} {name}")),
                        Err(e) => self.report(&format!("config {plugin}"), e),
                    });
                }
                if s.kind == SettingKind::Secret {
                    let done = if raw.trim().is_empty() { "Removed" } else { "Saved" };
                    return Ok(match engine.set_plugin_secret(&plugin, &key, raw) {
                        Ok(()) => self.report(&format!("config {plugin}"), format!("{done} {key}")),
                        Err(e) => self.report(&format!("config {plugin}"), e),
                    });
                }
                if s.imports(raw) {
                    let saved = import_value(p, s, raw).and_then(|(v, _)| engine.set_plugin_value(&plugin, &key, v));
                    return Ok(match saved {
                        Ok(()) => self.report(&format!("config {plugin}"), format!("Saved {key}")),
                        Err(e) => self.report(text, e),
                    });
                }
                match parse_value(s, raw).and_then(|v| engine.set_plugin_value(&plugin, &key, v)) {
                    Ok(()) => self.report(&format!("config {plugin}"), format!("Saved {key}")),
                    Err(e) => self.report(text, e),
                }
            }
            (RowId::Install, "install") => {
                let src = text.strip_prefix("install").unwrap_or("").trim();
                match git_source(src) {
                    // Cloned code is shown for review first: the box now holds the staged
                    // folder, whose row says what the plugin runs.
                    Some(url) => match clone_to_staging(url) {
                        Ok(dir) => self.report(
                            &format!("install {}", dir.display()),
                            "Fetched. Check what it runs, then press Enter to install",
                        ),
                        Err(e) => self.report(text, format!("Fetch failed: {e}")),
                    },
                    None => match self.install(&expand_home(src)) {
                        Ok(msg) => self.report("", msg),
                        Err(e) => self.report(text, format!("Install failed: {e}")),
                    },
                }
            }
            (RowId::Remove(id), "remove") => {
                let p = Self::find(&reg.plugins, &id).ok_or("plugin gone")?;
                let dir = p.dir.clone().filter(|d| self.removable(d)).ok_or("not removable")?;
                let name = p.manifest.name.clone();
                drop(reg);
                match std::fs::remove_dir_all(&dir) {
                    Ok(()) => {
                        engine.reload();
                        self.report("", format!("Removed {name}"))
                    }
                    Err(e) => self.report(text, format!("Remove failed: {e}")),
                }
            }
            (RowId::Reload, _) => {
                engine.reload();
                // Also rebuild indexes (apps, ssh hosts, ...); can take a moment, so not inline.
                let e = engine.clone();
                std::thread::spawn(move || e.refresh_all());
                self.report("", format!("Reloaded: {} plugins", engine.plugins().plugins.len()))
            }
            // Enter on a row that completes: same as Tab.
            (RowId::Command(word), _) => {
                ExecOutcome::complete(format!("{TRIGGER}{word}{}", if word == "reload" { "" } else { " " }))
            }
            (RowId::Pick(id), _) => {
                let cmd = text.split_whitespace().next().filter(|c| *c != "set").unwrap_or("config");
                ExecOutcome::complete(format!("{TRIGGER}{cmd} {id}"))
            }
            (RowId::Key(key), _) => {
                let id = text.split_whitespace().nth(1).unwrap_or("");
                ExecOutcome::complete(format!("{TRIGGER}set {id} {key} "))
            }
            (RowId::Setting { plugin, key }, "edit") => {
                let p = Self::find(&reg.plugins, &plugin).ok_or("plugin gone")?;
                let secret = p.manifest.settings.iter().any(|s| s.key == key && s.kind.is_secret());
                let raw = match p.settings.get(&key) {
                    // Never put a stored secret in the search box.
                    _ if secret => String::new(),
                    Some(Value::String(v)) => v.clone(),
                    Some(v) => v.to_string(),
                    None => String::new(),
                };
                ExecOutcome::complete(format!("{TRIGGER}set {plugin} {key} {raw}"))
            }
            _ => return Err(Error::UnknownAction(format!("{action_id} on {result_id}"))),
        };
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::engine::Paths;

    fn sandbox() -> (tempfile::TempDir, PathBuf, Arc<Engine>) {
        let tmp = crate::test_support::tmp();
        let root = tmp.path().to_path_buf();
        std::fs::create_dir_all(root.join("plugins")).unwrap();
        let paths = Paths {
            config: root.join("config.toml"),
            plugins: root.join("plugins"),
            frecency: root.join("frecency.json"),
            picks: root.join("query_picks.json"),
            secrets: root.join("secrets"),
        };
        let engine = Engine::with_paths(&Config::default(), paths);
        (tmp, root, engine)
    }

    fn q(text: &str) -> Query<'_> {
        Query { text, trigger: Some(TRIGGER), limit: 8 }
    }

    fn manager(e: &Arc<Engine>) -> Arc<dyn Provider> {
        e.plugins().plugins.iter().find(|p| p.manifest.id == ID).unwrap().provider.clone()
    }

    #[test]
    fn row_ids_round_trip_and_keep_their_text_form() {
        let ids = [
            (RowId::Plugin("web".into()), "p:web"),
            (RowId::Command("install".into()), "cmd:install"),
            (RowId::Pick("web".into()), "pick:web"),
            (RowId::Setting { plugin: "web".into(), key: "a:b".into() }, "s:web:a:b"),
            (RowId::Save { plugin: "web".into(), key: "browser".into() }, "set:web:browser"),
            (RowId::Key("browser".into()), "key:browser"),
            (RowId::Remove("echo".into()), "rm:echo"),
            (RowId::Install, "install"),
            (RowId::Reload, "reload"),
        ];
        for (id, text) in ids {
            assert_eq!(id.to_string(), text);
            assert_eq!(text.parse::<RowId>().unwrap(), id);
        }
        for bad in ["", "s:no-key", "bogus:x", "hint", "install:x"] {
            assert!(bad.parse::<RowId>().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn disable_and_enable_without_restart() {
        let (_tmp, root, e) = sandbox();
        let pm = manager(&e);
        let web = |e: &Engine| e.query("web rust").iter().any(|r| r.provider == "builtin-web");
        assert!(web(&e));
        let web_row =
            |pm: &Arc<dyn Provider>| pm.query(&q("web")).into_iter().find(|r| r.id == "p:builtin-web").unwrap();
        let toggle = |r: &ResultItem| r.actions.iter().find(|a| a.id == "toggle").unwrap().title.clone();
        let r = web_row(&pm);
        assert!(r.subtitle.starts_with("Enabled · ↵ to disable"), "{}", r.subtitle);
        assert!(toggle(&r).contains("currently enabled"));
        let out = pm.execute("p:builtin-web", "toggle", "web").unwrap();
        assert_eq!(out.query(), Some("plugin web"), "stays open on the same list");
        assert!(!web(&e), "web disabled at once");
        let text = std::fs::read_to_string(root.join("config.toml")).unwrap();
        assert!(text.contains("enabled = false"), "{text}");
        assert!(pm.query(&q("web")).iter().any(|r| r.title.contains("disabled")));
        let r = web_row(&pm);
        assert!(r.subtitle.starts_with("Disabled · ↵ to enable"), "{}", r.subtitle);
        assert!(toggle(&r).contains("currently disabled"));
        pm.execute("p:builtin-web", "toggle", "web").unwrap();
        assert!(web(&e));
    }

    #[test]
    fn manager_cannot_disable_itself() {
        let (_tmp, _, e) = sandbox();
        let row = manager(&e).query(&q("")).into_iter().find(|r| r.id == format!("p:{ID}")).unwrap();
        assert!(row.actions.iter().all(|a| a.id != "toggle"));
        assert!(!row.subtitle.contains("↵ to"), "{}", row.subtitle);
    }

    #[test]
    fn install_then_uninstall_a_folder() {
        let (_tmp, root, e) = sandbox();
        let pm = manager(&e);
        let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/plugins/echo-py");
        let text = format!("install {}", src.display());
        let preview = pm.query(&q(&text));
        assert_eq!(preview[0].id, "install", "{:?}", preview[0].title);
        assert!(preview[0].subtitle.contains("unsandboxed"));

        pm.execute("install", "install", &text).unwrap();
        assert!(root.join("plugins/echo-py/plugin.toml").is_file());
        assert!(e.plugins().plugins.iter().any(|p| p.manifest.id == "echo-py"), "loaded without restart");
        let again = pm.execute("install", "install", &text).unwrap();
        assert!(
            pm.query(&q(again.query().unwrap().strip_prefix(TRIGGER).unwrap()))
                .iter()
                .any(|r| r.title.contains("already installed"))
        );

        let rows = pm.query(&q("uninstall echo-py"));
        assert_eq!(rows[0].id, "rm:echo-py");
        pm.execute("rm:echo-py", "remove", "uninstall echo-py").unwrap();
        assert!(!root.join("plugins/echo-py").exists());
        assert!(e.plugins().plugins.iter().all(|p| p.manifest.id != "echo-py"));
    }

    #[test]
    fn only_https_and_ssh_sources_are_cloned() {
        for ok in ["https://github.com/a/b", "ssh://git@host/a/b.git", "git@github.com:a/b.git"] {
            assert_eq!(git_source(ok), Some(ok));
        }
        for bad in [
            "--upload-pack=touch /tmp/pwned.git",
            "-c core.sshCommand=x https://h/r",
            "ext::sh -c touch% /tmp/pwned",
            "file:///etc",
            "http://example.com/r.git",
            "/home/me/plugin.git",
            "https://host/repo with space",
            "",
        ] {
            assert_eq!(git_source(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn installed_folders_are_named_by_id_and_symlinks_are_refused() {
        let (_tmp, root, e) = sandbox();
        let pm = manager(&e);
        // A folder whose name says nothing about the id (as a clone in staging would be).
        let src = root.join("some-clone-123");
        copy_dir(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/plugins/echo-py"), &src).unwrap();
        pm.execute("install", "install", &format!("install {}", src.display())).unwrap();
        assert!(root.join("plugins/echo-py/plugin.toml").is_file());

        #[cfg(unix)]
        {
            let linked = root.join("linked");
            std::fs::create_dir_all(&linked).unwrap();
            std::fs::write(
                linked.join("plugin.toml"),
                "id = \"lnk\"\nname = \"L\"\nversion = \"1\"\n[run]\nexec = [\"./x\"]\n",
            )
            .unwrap();
            std::os::unix::fs::symlink("/etc/passwd", linked.join("x")).unwrap();
            pm.execute("install", "install", &format!("install {}", linked.display())).unwrap();
            assert!(!root.join("plugins/lnk").exists(), "nothing half-copied is left behind");
            assert!(e.plugins().plugins.iter().all(|p| p.manifest.id != "lnk"));
        }
    }

    #[test]
    fn bundled_plugins_cannot_be_uninstalled() {
        let (_tmp, _, e) = sandbox();
        let rows = manager(&e).query(&q("uninstall builtin-web"));
        assert!(rows[0].actions.is_empty() && rows[0].title.contains("bundled"));
    }

    #[test]
    fn set_imports_pasted_mcp_json_into_a_table() {
        let (_tmp, root, e) = sandbox();
        let dir = root.join("plugins/ai");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("plugin.toml"),
            "id = \"ai\"\nname = \"AI\"\nversion = \"1\"\n[run]\nexec = [\"./x\"]\n\
             [[settings]]\nkey = \"mcp_servers\"\ntype = \"table\"\nimport = \"mcp\"\ndefault = {}\n\
             [[settings.fields]]\nkey = \"command\"\ntype = \"list\"\n",
        )
        .unwrap();
        std::fs::write(root.join("config.toml"), "[plugins.ai.mcp_servers.old]\ncommand = [\"o\"]\n").unwrap();
        e.reload();
        let pm = manager(&e);
        let text = r#"set ai mcp_servers {"mcpServers": {"fs": {"command": "npx", "args": ["srv"]}}}"#;
        let rows = pm.query(&q(text));
        assert_eq!(rows[0].title, "Add fs to mcp_servers");
        pm.execute("set:ai:mcp_servers", "save", text).unwrap();
        let settings = e.plugins().plugins.iter().find(|p| p.manifest.id == "ai").unwrap().settings.clone();
        assert_eq!(settings["mcp_servers"]["fs"]["command"], serde_json::json!(["npx", "srv"]));
        assert_eq!(settings["mcp_servers"]["old"]["command"], serde_json::json!(["o"]), "others are kept");
        let bad = pm.query(&q(r#"set ai mcp_servers {"a": {"type": "sse", "url": "https://h"}}"#));
        assert!(bad[0].title.starts_with("mcp_servers: a: SSE") && bad[0].actions.is_empty(), "{:?}", bad[0]);
    }

    #[test]
    fn secret_settings_are_stored_privately_and_never_shown() {
        let (_tmp, root, e) = sandbox();
        let dir = root.join("plugins/sec");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("plugin.toml"),
            "id = \"sec\"\nname = \"Sec\"\nversion = \"1\"\n[run]\nexec = [\"./x\"]\n\
             [[settings]]\nkey = \"api_key\"\ntype = \"secret\"\nlabel = \"API key\"\n",
        )
        .unwrap();
        e.reload();
        let pm = manager(&e);
        let settings = || e.plugins().plugins.iter().find(|p| p.manifest.id == "sec").unwrap().settings.clone();
        let row = |text: &str| pm.query(&q(text)).into_iter().find(|r| r.id == "s:sec:api_key").unwrap();
        assert_eq!(row("config sec").title, "api_key = (not set)");
        assert_eq!(row("config sec").complete.as_deref(), Some("plugin set sec api_key "), "never pre-filled");

        let set = pm.query(&q("set sec api_key sk-secret-9"));
        assert_eq!(set[0].title, "Save api_key (hidden)");
        assert!(!format!("{set:?}").contains("sk-secret-9"), "the value is not echoed");
        pm.execute("set:sec:api_key", "save", "set sec api_key sk-secret-9").unwrap();
        assert_eq!(settings()["api_key"], "sk-secret-9", "the plugin gets it with its settings");
        let config = std::fs::read_to_string(root.join("config.toml")).unwrap_or_default();
        assert!(!config.contains("sk-secret-9"), "not in config.toml: {config}");
        let file = root.join("secrets/sec.api_key");
        assert!(file.is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let shown = row("config sec");
        assert_eq!(shown.title, "api_key = (set)");
        assert!(!format!("{shown:?}").contains("sk-secret-9"));

        pm.execute("s:sec:api_key", "reset", "config sec").unwrap();
        assert!(settings().get("api_key").is_none() && !file.exists(), "Remove deletes it");
        pm.execute("set:sec:api_key", "save", "set sec api_key sk-secret-9").unwrap();
        let edit = pm.execute("s:sec:api_key", "edit", "config sec").unwrap();
        assert!(!format!("{edit:?}").contains("sk-secret-9"), "Change… does not put it in the box: {edit:?}");
    }

    #[test]
    fn named_secrets_are_stored_privately_and_only_their_names_shown() {
        let (_tmp, root, e) = sandbox();
        let dir = root.join("plugins/sec");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("plugin.toml"),
            "id = \"sec\"\nname = \"Sec\"\nversion = \"1\"\n[run]\nexec = [\"./x\"]\n\
             [[settings]]\nkey = \"secrets\"\ntype = \"secrets\"\nlabel = \"Tokens\"\n",
        )
        .unwrap();
        e.reload();
        let pm = manager(&e);
        let settings = || e.plugins().plugins.iter().find(|p| p.manifest.id == "sec").unwrap().settings.clone();
        let row = |text: &str| pm.query(&q(text)).into_iter().find(|r| r.id == "s:sec:secrets").unwrap();
        assert_eq!(row("config sec").title, "secrets = (none)");

        assert!(pm.query(&q("set sec secrets gh"))[0].actions.is_empty(), "still typing the name");
        assert!(pm.query(&q("set sec secrets a.b x"))[0].actions.is_empty(), "invalid name");
        let set = pm.query(&q("set sec secrets gh tok-123"));
        assert_eq!(set[0].title, "Save secrets gh (hidden)");
        assert!(!format!("{set:?}").contains("tok-123"));
        pm.execute("set:sec:secrets", "save", "set sec secrets gh tok-123").unwrap();
        pm.execute("set:sec:secrets", "save", "set sec secrets jira tok-456").unwrap();
        assert_eq!(settings()["secrets"], serde_json::json!({"gh": "tok-123", "jira": "tok-456"}));
        assert!(root.join("secrets/sec.secrets.gh").is_file());
        let config = std::fs::read_to_string(root.join("config.toml")).unwrap_or_default();
        assert!(!config.contains("tok-"), "not in config.toml: {config}");

        let shown = row("config sec");
        assert_eq!(shown.title, "secrets = gh, jira");
        assert!(!format!("{shown:?}").contains("tok-"));
        let names = pm.query(&q("set sec secrets "));
        assert!(names.iter().any(|r| r.complete.as_deref() == Some("plugin set sec secrets gh ")));
        assert!(!format!("{names:?}").contains("tok-"));
        let edit = pm.execute("s:sec:secrets", "edit", "config sec").unwrap();
        assert!(!format!("{edit:?}").contains("tok-"), "{edit:?}");

        assert_eq!(pm.query(&q("set sec secrets gh "))[0].title, "Remove secrets gh");
        pm.execute("set:sec:secrets", "save", "set sec secrets gh ").unwrap();
        assert_eq!(settings()["secrets"], serde_json::json!({"jira": "tok-456"}));
    }

    #[test]
    fn settings_can_be_changed_from_the_launcher() {
        let (_tmp, _, e) = sandbox();
        let pm = manager(&e);
        let rows = pm.query(&q("config builtin-web"));
        let browser = rows.iter().find(|r| r.id == "s:builtin-web:browser").expect("browser setting listed");
        assert_eq!(browser.complete.as_deref(), Some("plugin set builtin-web browser "));
        let set = pm.query(&q("set builtin-web browser firefox --private-window"));
        assert_eq!(set[0].id, "set:builtin-web:browser");
        pm.execute("set:builtin-web:browser", "save", "set builtin-web browser firefox --private-window").unwrap();
        let web =
            e.plugins().plugins.iter().find(|p| p.manifest.id == "builtin-web").map(|p| p.settings.clone()).unwrap();
        assert_eq!(web["browser"], "firefox --private-window");
        let bad = pm.query(&q("set builtin-web fallback maybe"));
        assert!(bad[0].actions.is_empty(), "invalid values cannot be saved");
        pm.execute("s:builtin-web:fallback", "flip", "config builtin-web").unwrap();
        let web =
            e.plugins().plugins.iter().find(|p| p.manifest.id == "builtin-web").map(|p| p.settings.clone()).unwrap();
        assert_eq!(web["fallback"], false);
        pm.execute("s:builtin-web:fallback", "reset", "config builtin-web").unwrap();
        let web =
            e.plugins().plugins.iter().find(|p| p.manifest.id == "builtin-web").map(|p| p.settings.clone()).unwrap();
        assert_eq!(web["fallback"], true);
    }

    #[test]
    fn reload_keeps_unchanged_providers() {
        let (_tmp, _, e) = sandbox();
        let before = e.plugins();
        e.reload();
        let after = e.plugins();
        for p in &after.plugins {
            let old = before.plugins.iter().find(|o| o.manifest.id == p.manifest.id).unwrap();
            assert!(Arc::ptr_eq(&old.provider, &p.provider), "{} was rebuilt", p.manifest.id);
        }
    }

    fn install_test_plugin(root: &Path, id: &str, settings: &str) {
        let dir = root.join("plugins").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("plugin.toml"),
            format!(
                "id = \"{id}\"\nname = \"Test\"\nversion = \"1\"\n[run]\nexec = [\"./missing\"]\n\
                 [query]\ntriggers = [\"{id} \"]\nglobal = true\n{settings}"
            ),
        )
        .unwrap();
    }

    #[test]
    fn the_settings_action_opens_the_settings_page() {
        let (_tmp, _root, e) = sandbox();
        let out = manager(&e).execute("p:builtin-web", "config", "").unwrap();
        assert_eq!(out, ExecOutcome::Settings("builtin-web".into()));
        let rows = manager(&e).query(&q("config builtin-web"));
        assert_eq!(rows[0].id, "p:builtin-web", "first row opens the page: {:?}", rows[0]);
    }

    #[test]
    fn a_settings_page_saves_everything_at_once_or_nothing() {
        use crate::engine::SettingChange::{Reset, SecretEntry, Set};
        let (_tmp, root, e) = sandbox();
        install_test_plugin(
            &root,
            "pg",
            "[[settings]]\nkey = \"model\"\ntype = \"string\"\ndefault = \"\"\n\
             [[settings]]\nkey = \"max\"\ntype = \"number\"\ndefault = 5\n\
             [[settings]]\nkey = \"roots\"\ntype = \"list\"\ndefault = [\"~\"]\n\
             [[settings]]\nkey = \"servers\"\ntype = \"table\"\ndefault = {}\n\
             [[settings]]\nkey = \"key\"\ntype = \"secret\"\n\
             [[settings]]\nkey = \"tokens\"\ntype = \"secrets\"\n",
        );
        e.reload();
        let settings = || e.plugins().plugins.iter().find(|p| p.manifest.id == "pg").unwrap().settings.clone();
        let set = |key: &str, text: &str| Set { key: key.into(), text: text.into() };

        let bad = e.save_settings("pg", vec![set("model", "m1"), set("max", "lots"), set("servers", "[x")]);
        let keys: Vec<String> = bad.unwrap_err().into_iter().map(|(k, _)| k).collect();
        assert_eq!(keys, ["max", "servers"]);
        assert_eq!(settings()["model"], "", "nothing saved when a field is invalid");

        e.save_settings(
            "pg",
            vec![
                set("model", "m1"),
                set("max", "12"),
                set("roots", "~/a\n\n /b \n"),
                set("servers", "[files]\ncommand = [\"npx\", \"fs\"]\n"),
                set("key", "sk-9"),
                SecretEntry { key: "tokens".into(), name: "gh".into(), value: "tok-1".into() },
            ],
        )
        .unwrap();
        let s = settings();
        assert_eq!(
            (&s["model"], &s["max"], &s["roots"]),
            (&"m1".into(), &12.into(), &serde_json::json!(["~/a", "/b"]))
        );
        assert_eq!(s["servers"]["files"]["command"], serde_json::json!(["npx", "fs"]));
        assert_eq!((&s["key"], &s["tokens"]["gh"]), (&"sk-9".into(), &"tok-1".into()));
        let config = std::fs::read_to_string(root.join("config.toml")).unwrap();
        assert!(!config.contains("sk-9") && !config.contains("tok-1"), "secrets stay out: {config}");
        assert!(config.contains("[plugins.pg.servers.files]"), "tables stay tables: {config}");

        e.save_settings("pg", vec![Reset { key: "max".into() }, Reset { key: "key".into() }]).unwrap();
        let s = settings();
        assert_eq!(s["max"], 5);
        assert!(s.get("key").is_none(), "secret removed");
    }

    #[test]
    fn a_plugin_missing_a_required_setting_offers_its_settings_instead() {
        let (_tmp, root, e) = sandbox();
        install_test_plugin(
            &root,
            "rq",
            "[[settings]]\nkey = \"model\"\ntype = \"string\"\nlabel = \"Model\"\nrequired = true\n",
        );
        e.reload();
        let rows = e.query("rq hello");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "Test needs setup");
        assert!(rows[0].subtitle.contains("Model"), "{}", rows[0].subtitle);
        let out = e.execute(&rows[0], "settings", &crate::plugin::ExecCtx::none()).unwrap();
        assert_eq!(out, ExecOutcome::Settings("rq".into()));
        assert!(e.query("hello").iter().all(|r| r.provider != "rq"), "skipped in global search");

        e.save_settings("rq", vec![crate::engine::SettingChange::Set { key: "model".into(), text: "m".into() }])
            .unwrap();
        assert!(e.query("rq hello").iter().all(|r| !r.id.starts_with("setup:")));
    }
}
