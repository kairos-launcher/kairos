use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

// Metadata fields are read by the plugin manager / settings UI (not built yet).
#[allow(dead_code)]
#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default = "default_api")]
    pub api: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub run: Run,
    #[serde(default)]
    pub query: QueryCfg,
    /// Only for plugins with `run.url`.
    #[serde(default)]
    pub remote: Remote,
    #[serde(default)]
    pub settings: Vec<Setting>,
}

/// Id prefix of bundled plugins, reserved so an external plugin cannot pose as one.
pub const BUILTIN_PREFIX: &str = "builtin-";

fn default_api() -> String {
    "0.1".into()
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct Run {
    /// Required for external plugins; bundled (in-process) plugins have none.
    #[serde(default)]
    pub exec: Option<Exec>,
    /// A remote plugin: the JSON-RPC protocol over HTTPS POST to this URL instead of stdio.
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub mode: Mode,
    #[serde(default = "default_idle")]
    pub idle_timeout_s: u64,
}

impl Default for Run {
    fn default() -> Self {
        Self { exec: None, url: None, mode: Mode::default(), idle_timeout_s: default_idle() }
    }
}

impl Run {
    /// argv for the current OS, if this is an external plugin.
    pub fn argv(&self) -> Option<&[String]> {
        self.exec.as_ref().and_then(Exec::argv)
    }
}

/// How a remote plugin authenticates. The token itself is never in the manifest or
/// `config.toml`; see `remote::token`.
#[derive(Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default)]
pub struct Remote {
    pub auth: Auth,
    /// Header carrying the token for `auth = "header"`, e.g. `X-Api-Key`.
    pub header: Option<String>,
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Auth {
    #[default]
    Bearer,
    Header,
    None,
}

fn default_idle() -> u64 {
    30
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    Resident,
    #[default]
    OnDemand,
}

/// Either `exec = ["./bin", "arg"]` or `[run.exec]` with per-OS arrays.
#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(untagged)]
pub enum Exec {
    Argv(Vec<String>),
    PerOs { windows: Option<Vec<String>>, linux: Option<Vec<String>>, macos: Option<Vec<String>> },
}

impl Exec {
    pub fn argv(&self) -> Option<&[String]> {
        match self {
            Exec::Argv(v) => Some(v),
            Exec::PerOs { windows, linux, macos } => {
                let v = if cfg!(windows) {
                    windows
                } else if cfg!(target_os = "macos") {
                    macos
                } else {
                    linux
                };
                v.as_deref()
            }
        }
    }
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct QueryCfg {
    pub triggers: Vec<String>,
    pub global: bool,
    /// Only send a global search here if the text contains one of these characters
    /// (e.g. `"0123456789"`), so an on-demand plugin is not started for every search.
    pub global_requires: Option<String>,
    pub timeout_ms: u64,
    /// Do not query this plugin for fewer characters (saves requests to remote plugins).
    pub min_chars: usize,
}

impl Default for QueryCfg {
    fn default() -> Self {
        Self { triggers: vec![], global: true, global_requires: None, timeout_ms: 150, min_chars: 0 }
    }
}

/// One user setting a plugin declares. Values live in `config.toml` under `[plugins.<id>]`.
#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct Setting {
    pub key: String,
    #[serde(rename = "type")]
    pub kind: SettingKind,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub description: String,
    pub default: Option<toml::Value>,
    /// Allowed values of a `choice`.
    #[serde(default)]
    pub options: Vec<String>,
    /// TOML shown (commented out) in the generated config section instead of `key = default`,
    /// for settings whose default says little, such as an empty list of tables.
    #[serde(default)]
    pub example: String,
    /// Hint shown in an empty field of the settings page.
    #[serde(default)]
    pub placeholder: String,
    /// The plugin cannot work without a value: while it is empty the plugin is not started
    /// and its trigger offers to open the settings page instead.
    #[serde(default)]
    pub required: bool,
    /// For a `table`: its values are entries (tables) with these keys, edited one by one on
    /// the settings page (`[[settings.fields]]`, one level deep, no secrets).
    #[serde(default)]
    pub fields: Vec<Setting>,
    /// For a `table` with `fields`: pasted text in this format (`mcp`: MCP server JSON as
    /// Claude or VS Code write it) is converted into entries (`crate::plugin::import`).
    #[serde(default)]
    pub import: Option<String>,
}

/// The `type` of a setting.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(from = "String")]
pub enum SettingKind {
    String,
    Path,
    Number,
    Bool,
    /// One of the setting's `options`.
    Choice,
    List,
    /// Keys to values.
    Table,
    /// A string kept out of `config.toml` (API keys): stored privately by `plugin set` and
    /// passed to the plugin with its other settings (`crate::plugin::secrets`).
    Secret,
    /// Any number of named secrets (tokens used by name, e.g. `${github_token}` in the AI
    /// plugin's MCP servers): stored privately by `plugin set <id> <key> <name> <value>` and
    /// passed to the plugin as an object of names to values.
    Secrets,
    /// A type this launcher does not know (a newer plugin); any value is accepted.
    Other(String),
}

impl From<String> for SettingKind {
    fn from(s: String) -> Self {
        match s.as_str() {
            "string" => Self::String,
            "path" => Self::Path,
            "number" => Self::Number,
            "bool" => Self::Bool,
            "choice" => Self::Choice,
            "list" => Self::List,
            "table" => Self::Table,
            "secret" => Self::Secret,
            "secrets" => Self::Secrets,
            _ => Self::Other(s),
        }
    }
}

impl std::fmt::Display for SettingKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::String => "string",
            Self::Path => "path",
            Self::Number => "number",
            Self::Bool => "bool",
            Self::Choice => "choice",
            Self::List => "list",
            Self::Table => "table",
            Self::Secret => "secret",
            Self::Secrets => "secrets",
            Self::Other(s) => s,
        })
    }
}

impl SettingKind {
    /// Kept in the private store, never in `config.toml`.
    pub fn is_secret(&self) -> bool {
        matches!(self, Self::Secret | Self::Secrets)
    }
}

impl Setting {
    /// Whether `v` is a valid value for this setting.
    pub fn accepts(&self, v: &toml::Value) -> bool {
        use toml::Value as V;
        match self.kind {
            SettingKind::String | SettingKind::Path | SettingKind::Secret => matches!(v, V::String(_)),
            SettingKind::Number => matches!(v, V::Integer(_) | V::Float(_)),
            SettingKind::Bool => matches!(v, V::Boolean(_)),
            SettingKind::Choice => {
                v.as_str().is_some_and(|s| self.options.is_empty() || self.options.iter().any(|o| o == s))
            }
            SettingKind::Table if !self.fields.is_empty() => v.as_table().is_some_and(|t| {
                t.values().all(|entry| {
                    entry
                        .as_table()
                        .is_some_and(|e| self.fields.iter().all(|f| e.get(&f.key).is_none_or(|x| f.accepts(x))))
                })
            }),
            SettingKind::Table | SettingKind::Secrets => matches!(v, V::Table(_)),
            SettingKind::List => matches!(v, V::Array(_)),
            SettingKind::Other(_) => true,
        }
    }
}

impl Setting {
    /// What the settings page and the text rows show as this setting's name.
    pub fn title(&self) -> &str {
        if self.label.is_empty() { &self.key } else { &self.label }
    }

    /// Whether `v` (an effective value) counts as empty for `required`.
    pub fn is_empty_value(v: Option<&serde_json::Value>) -> bool {
        use serde_json::Value as J;
        match v {
            None | Some(J::Null) => true,
            Some(J::String(s)) => s.trim().is_empty(),
            Some(J::Array(a)) => a.is_empty(),
            Some(J::Object(o)) => o.is_empty(),
            Some(J::Bool(_) | J::Number(_)) => false,
        }
    }

    /// A list edited as one item per line: every item is a string (and no example shows tables).
    pub fn is_line_list(&self, current: Option<&toml::Value>) -> bool {
        let strings = |v: Option<&toml::Value>| {
            v.and_then(toml::Value::as_array).is_none_or(|a| a.iter().all(toml::Value::is_str))
        };
        self.kind == SettingKind::List
            && strings(current)
            && strings(self.default.as_ref())
            && !self.example.contains("[[")
    }

    /// A table edited as key/value rows: no `fields`, and every value is a string.
    pub fn is_pair_table(&self, current: Option<&toml::Value>) -> bool {
        let strings = |v: Option<&toml::Value>| {
            v.and_then(toml::Value::as_table).is_none_or(|t| t.values().all(toml::Value::is_str))
        };
        self.kind == SettingKind::Table
            && self.fields.is_empty()
            && strings(current)
            && strings(self.default.as_ref())
            && !self.example.contains('[')
    }

    /// Parse what the user typed for this setting. `Ok(None)` removes the user's value (the
    /// default applies again). Lists take one item per line, or TOML (`[[key]]` blocks) for
    /// lists of tables; tables take the TOML of their contents (`name = value`, `[sub]`).
    /// Secrets are not parsed here: they never go to `config.toml`.
    pub fn parse_input(&self, raw: &str) -> Result<Option<toml_edit::Item>, String> {
        use toml_edit::value;
        let trimmed = raw.trim();
        Ok(Some(match self.kind {
            SettingKind::Bool => match trimmed.to_lowercase().as_str() {
                "true" | "yes" | "on" | "1" => value(true),
                "false" | "no" | "off" | "0" => value(false),
                _ => return Err("expected true or false".into()),
            },
            SettingKind::Number => trimmed
                .parse::<i64>()
                .map(value)
                .or_else(|_| trimmed.parse::<f64>().map(value))
                .map_err(|_| "expected a number".to_string())?,
            SettingKind::Choice if !self.options.is_empty() && !self.options.iter().any(|o| o == trimmed) => {
                return Err(format!("expected one of: {}", self.options.join(", ")));
            }
            SettingKind::List if self.is_line_list(None) => {
                let mut a = toml_edit::Array::new();
                for line in raw.lines().map(str::trim).filter(|l| !l.is_empty()) {
                    a.push(line);
                }
                value(a)
            }
            SettingKind::List => {
                let doc = parse_toml(raw)?;
                match doc.get(&self.key) {
                    None => value(toml_edit::Array::new()),
                    Some(
                        item @ (toml_edit::Item::ArrayOfTables(_) | toml_edit::Item::Value(toml_edit::Value::Array(_))),
                    ) => item.clone(),
                    Some(_) => return Err(format!("expected [[{}]] blocks", self.key)),
                }
            }
            SettingKind::Table => {
                let doc = parse_toml(raw)?;
                let mut t = doc.as_table().clone();
                t.set_implicit(false);
                toml_edit::Item::Table(t)
            }
            SettingKind::Secret | SettingKind::Secrets => return Err("secrets are stored privately".into()),
            SettingKind::String | SettingKind::Path | SettingKind::Choice | SettingKind::Other(_) => {
                value(trimmed.trim_matches('"'))
            }
        }))
    }

    /// The text a settings page shows for `current` (the effective value), the inverse of
    /// `parse_input`.
    pub fn input_text(&self, current: Option<&toml::Value>) -> String {
        use toml::Value as V;
        match (current, &self.kind) {
            (None, _) => String::new(),
            (Some(V::String(s)), _) => s.clone(),
            (Some(V::Array(a)), SettingKind::List) if self.is_line_list(current) => {
                a.iter().filter_map(V::as_str).collect::<Vec<_>>().join("\n")
            }
            (Some(v @ V::Array(_)), _) => {
                let mut t = toml::Table::new();
                t.insert(self.key.clone(), v.clone());
                toml::to_string(&t).unwrap_or_default().trim_end().to_string()
            }
            (Some(V::Table(t)), _) => toml::to_string(t).unwrap_or_default().trim_end().to_string(),
            (Some(v), _) => v.to_string(),
        }
    }

    /// Whether `raw` is text to import (see `import`) rather than a name or a value.
    pub fn imports(&self, raw: &str) -> bool {
        self.import.is_some() && raw.trim_start().starts_with('{')
    }

    /// `raw` imported (see `import`) and merged into `current`: entries with the same name are
    /// replaced, the others kept. The merged table and the imported names.
    pub fn import_entries(
        &self,
        current: Option<&toml::Value>,
        raw: &str,
    ) -> Result<(toml::Table, Vec<String>), String> {
        let format = self.import.as_deref().ok_or("this setting imports nothing")?;
        let new = super::import::entries(format, raw)?;
        let names = new.keys().cloned().collect();
        let mut table = current.and_then(toml::Value::as_table).cloned().unwrap_or_default();
        table.extend(new);
        Ok((table, names))
    }

    /// `example` rewritten for the settings page, where a table or list is edited on its own:
    /// `[plugins.<id>.<key>.x]` becomes `[x]`, `[[plugins.<id>.<key>]]` becomes `[[<key>]]`.
    pub fn input_example(&self, plugin: &str) -> String {
        let full = format!("plugins.{}.{}", quote_key(plugin), quote_key(&self.key));
        self.example
            .trim()
            .replace(&format!("[[{full}]]"), &format!("[[{}]]", quote_key(&self.key)))
            .replace(&format!("[{full}."), "[")
    }
}

fn parse_toml(raw: &str) -> Result<toml_edit::DocumentMut, String> {
    raw.parse::<toml_edit::DocumentMut>().map_err(|e| {
        let msg = e.message().trim().to_string();
        match e.span().map(|s| raw[..s.start.min(raw.len())].lines().count().max(1)) {
            Some(line) => format!("line {line}: {msg}"),
            None => msg,
        }
    })
}

impl Manifest {
    /// Required settings that are empty in `effective` (the plugin's effective settings).
    pub fn missing_required(&self, effective: &serde_json::Value) -> Vec<&Setting> {
        self.settings.iter().filter(|s| s.required && Setting::is_empty_value(effective.get(&s.key))).collect()
    }

    /// Load an external plugin's `plugin.toml`; it must say how to run it.
    pub fn load(dir: &Path) -> Result<Self, String> {
        let file = dir.join("plugin.toml");
        let text = std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        let m = Self::parse(&text, &file.display().to_string())?;
        if m.id.starts_with(BUILTIN_PREFIX) {
            return Err(format!(
                "{}: id {:?}: `{BUILTIN_PREFIX}` is reserved for bundled plugins",
                file.display(),
                m.id
            ));
        }
        if !m.is_remote() && m.run.argv().is_none_or(<[std::string::String]>::is_empty) {
            return Err(format!("{}: no run.exec for this OS", file.display()));
        }
        Ok(m)
    }

    /// Parse manifest text without requiring `run.exec` (used for bundled plugins).
    pub fn parse(text: &str, origin: &str) -> Result<Self, String> {
        let mut m: Self = toml::from_str(text).map_err(|e| format!("{origin}: {e}"))?;
        if !is_bare_key(&m.id) {
            return Err(format!(
                "{origin}: id {:?} may only use letters, digits, `-` and `_` (it is a key in config.toml)",
                m.id
            ));
        }
        for s in &m.settings {
            check_fields(s).map_err(|e| format!("{origin}: setting {:?}: {e}", s.key))?;
        }
        if let Some(url) = &m.run.url {
            if m.run.exec.is_some() {
                return Err(format!("{origin}: run.exec and run.url are mutually exclusive"));
            }
            check_remote_url(url).map_err(|e| format!("{origin}: run.url: {e}"))?;
            if m.remote.auth == Auth::Header && m.remote.header.as_deref().is_none_or(|h| h.trim().is_empty()) {
                return Err(format!("{origin}: remote.auth = \"header\" needs remote.header"));
            }
            // A network round trip is slower and shares the user's keystrokes with a third
            // party, so remote plugins are opt-in for global search and get a longer budget.
            let raw: toml::Table = toml::from_str(text).unwrap_or_default();
            let q = raw.get("query").and_then(toml::Value::as_table);
            if q.is_none_or(|q| !q.contains_key("global")) {
                m.query.global = false;
            }
            if q.is_none_or(|q| !q.contains_key("timeout_ms")) {
                m.query.timeout_ms = 800;
            }
        }
        Ok(m)
    }

    pub fn is_remote(&self) -> bool {
        self.run.url.is_some()
    }

    /// The settings a plugin runs with: its declared defaults, overridden by the user's values
    /// from `config.toml`. A value of the wrong type is reported and the default kept; keys the
    /// plugin does not declare are passed through. `secret` settings come from the private
    /// store instead (`crate::plugin::secrets`).
    #[cfg(test)]
    pub fn effective_settings(&self, user: Option<&toml::Table>) -> serde_json::Value {
        self.effective_settings_with(user, &|_| None, &|_| BTreeMap::new())
    }

    /// `effective_settings` with `secret` settings looked up by `secret(key)` and `secrets`
    /// settings by `secrets(key)` (name to value).
    pub fn effective_settings_with(
        &self,
        user: Option<&toml::Table>,
        secret: &dyn Fn(&str) -> Option<String>,
        secrets: &dyn Fn(&str) -> BTreeMap<String, String>,
    ) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        for s in &self.settings {
            if s.kind.is_secret() {
                if user.is_some_and(|u| u.contains_key(&s.key)) {
                    log::warn!(
                        "config: plugins.{id}.{key} is a secret and is not read from config.toml; \
                         remove it and run `plugin set {id} {key} <value>` in the launcher",
                        id = self.id,
                        key = s.key
                    );
                }
                if s.kind == SettingKind::Secrets {
                    let all = secrets(&s.key);
                    if !all.is_empty() {
                        map.insert(s.key.clone(), serde_json::json!(all));
                    }
                } else if let Some(v) = secret(&s.key) {
                    map.insert(s.key.clone(), serde_json::Value::String(v));
                }
                continue;
            }
            let mut v = s.default.clone();
            if let Some(u) = user.and_then(|u| u.get(&s.key)) {
                if s.accepts(u) {
                    v = Some(u.clone());
                } else {
                    log::warn!(
                        "config: plugins.{}.{}: expected {}, got {u}; using the default",
                        self.id,
                        s.key,
                        s.kind
                    );
                }
            }
            if let Some(v) = v.and_then(|v| serde_json::to_value(v).ok()) {
                map.insert(s.key.clone(), v);
            }
        }
        for (k, v) in user.into_iter().flatten() {
            let secret = self.settings.iter().any(|s| s.key == *k && s.kind.is_secret());
            if !secret
                && !map.contains_key(k)
                && let Ok(v) = serde_json::to_value(v)
            {
                map.insert(k.clone(), v);
            }
        }
        serde_json::Value::Object(map)
    }

    /// The `config.toml` section listing every setting at its default, all commented out, or
    /// `None` if the plugin has no settings.
    pub fn default_section(&self) -> Option<String> {
        if self.settings.is_empty() {
            return None;
        }
        let header = format!("[plugins.{}]", quote_key(&self.id));
        let mut out =
            format!("# {} ({}). Uncomment and edit to change; defaults shown.\n# {header}\n", self.name, self.id);
        let mut tables = String::new();
        for s in &self.settings {
            let mut about = [s.label.as_str(), s.description.as_str()]
                .iter()
                .filter(|t| !t.is_empty())
                .copied()
                .collect::<Vec<_>>()
                .join(": ");
            if !s.options.is_empty() {
                about = format!("{about} ({})", s.options.join(" | "));
            }
            if s.kind.is_secret() {
                let about = if about.is_empty() { String::new() } else { format!("{about}. ") };
                let (what, args) = match s.kind {
                    SettingKind::Secrets => ("Named secrets", "<name> <value>"),
                    _ => ("A secret", "<value>"),
                };
                out += &format!(
                    "# {key}: {about}{what}, not stored in this file: set in the launcher with \
                     `plugin set {id} {key} {args}`.\n",
                    key = s.key,
                    id = self.id,
                );
                continue;
            }
            if !s.example.is_empty() {
                tables += &format!("#\n# {about}. Example:\n");
                for line in s.example.trim().lines() {
                    tables += &if line.is_empty() { "#\n".to_string() } else { format!("# {line}\n") };
                }
                continue;
            }
            match &s.default {
                Some(toml::Value::Table(t)) => {
                    tables += &format!("#\n# {about}\n# [plugins.{}.{}]\n", quote_key(&self.id), quote_key(&s.key));
                    for (k, v) in t {
                        tables += &format!("# {} = {v}\n", quote_key(k));
                    }
                }
                Some(v) => out += &format!("# {} = {v}{}\n", quote_key(&s.key), comment(&about)),
                None => out += &format!("# {} = {}\n", quote_key(&s.key), comment(&about).trim_start()),
            }
        }
        Some(out + &tables)
    }
}

/// Remote plugins must use `https://host[:port]/...`, without credentials in the URL.
/// `fields` only on a table, one level deep, and not secret (secrets are stored per setting).
fn check_fields(s: &Setting) -> Result<(), String> {
    if s.fields.is_empty() {
        return Ok(());
    }
    if s.kind != SettingKind::Table {
        return Err("only a table can have fields".into());
    }
    for f in &s.fields {
        if !f.fields.is_empty() {
            return Err(format!("field {:?}: fields cannot have fields", f.key));
        }
        if f.kind.is_secret() {
            return Err(format!("field {:?}: a field cannot be a secret", f.key));
        }
    }
    Ok(())
}

fn check_remote_url(url: &str) -> Result<(), String> {
    let rest = url.strip_prefix("https://").ok_or("must start with https://")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() {
        return Err("missing host".into());
    }
    if authority.contains('@') {
        return Err("must not contain credentials".into());
    }
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("invalid characters".into());
    }
    Ok(())
}

fn comment(about: &str) -> String {
    if about.is_empty() { String::new() } else { format!("   # {about}") }
}

/// A TOML key, quoted unless it is a bare key.
/// Usable unquoted as a TOML key.
fn is_bare_key(k: &str) -> bool {
    !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn quote_key(k: &str) -> String {
    if is_bare_key(k) { k.to_string() } else { format!("{k:?}") }
}

/// Find `<root>/<plugin>/plugin.toml` for every subdirectory of `root`.
pub fn discover(root: &Path) -> Vec<(PathBuf, Result<Manifest, String>)> {
    if crate::core::fs::world_writable(root) {
        log::warn!("plugins: ignoring {}: anyone on this machine can write to it", root.display());
        return vec![];
    }
    let Ok(rd) = std::fs::read_dir(root) else { return vec![] };
    let mut out: Vec<_> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("plugin.toml").is_file())
        .filter(|p| {
            let unsafe_dir =
                crate::core::fs::world_writable(p) || crate::core::fs::world_writable(&p.join("plugin.toml"));
            if unsafe_dir {
                log::warn!("plugin {}: ignored: anyone on this machine can write to it", p.display());
            }
            !unsafe_dir
        })
        .map(|p| {
            let m = Manifest::load(&p);
            (p, m)
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_spec_example() {
        let m: Manifest = toml::from_str(
            r#"
            id = "calc"
            name = "Calc"
            version = "0.1.0"
            [run]
            exec = ["./calc"]
            mode = "resident"
            [query]
            triggers = ["="]
            global_requires = "0123456789"
            [[settings]]
            key = "precision"
            type = "number"
            default = 6
            "#,
        )
        .unwrap();
        assert_eq!(m.run.mode, Mode::Resident);
        assert_eq!(m.query.triggers, ["="]);
        assert_eq!(m.query.timeout_ms, 150);
        assert_eq!(m.query.global_requires.as_deref(), Some("0123456789"));
        assert_eq!(m.effective_settings(None)["precision"], 6);
    }

    #[test]
    fn per_os_exec() {
        let m: Manifest = toml::from_str(
            r#"
            id = "x"
            name = "x"
            version = "1"
            [run.exec]
            windows = ["x.exe"]
            linux = ["./x"]
            "#,
        )
        .unwrap();
        assert_eq!(m.run.argv().unwrap()[0], if cfg!(windows) { "x.exe" } else { "./x" });
    }
}

#[cfg(test)]
mod parse_tests {
    use super::*;

    const WITH_SETTINGS: &str = r#"
        id = "web"
        name = "Web"
        version = "1"
        [[settings]]
        key = "engine"
        type = "choice"
        label = "Default engine"
        options = ["ddg", "g"]
        default = "ddg"
        [[settings]]
        key = "browser"
        type = "string"
        label = "Browser"
        default = ""
        [[settings]]
        key = "engines"
        type = "table"
        [settings.default]
        ddg = "https://duckduckgo.com/?q={q}"
    "#;

    #[test]
    fn user_values_override_defaults_and_bad_types_are_ignored() {
        let m = Manifest::parse(WITH_SETTINGS, "t").unwrap();
        let user: toml::Table = toml::from_str("engine = \"g\"\nbrowser = 5\nextra = true\n").unwrap();
        let s = m.effective_settings(Some(&user));
        assert_eq!(s["engine"], "g");
        assert_eq!(s["browser"], "", "wrong type keeps the default");
        assert_eq!(s["extra"], true, "undeclared keys pass through");
        assert_eq!(s["engines"]["ddg"], "https://duckduckgo.com/?q={q}");
        let user: toml::Table = toml::from_str("engine = \"bing\"\n").unwrap();
        assert_eq!(m.effective_settings(Some(&user))["engine"], "ddg", "not one of the options");
    }

    #[test]
    fn default_section_is_commented_and_valid_once_uncommented() {
        let m = Manifest::parse(WITH_SETTINGS, "t").unwrap();
        let text = m.default_section().unwrap();
        assert!(text.lines().all(|l| l.starts_with('#')), "{text}");
        assert!(text.contains("# [plugins.web]"), "{text}");
        // What a user does: uncomment the setting lines, leave the prose.
        let uncommented: String = text
            .lines()
            .filter_map(|l| l.strip_prefix("# ").filter(|r| r.contains('=') || r.starts_with('[')))
            .collect::<Vec<_>>()
            .join("\n");
        let parsed: toml::Table = toml::from_str(&uncommented).unwrap_or_else(|e| panic!("{e}\n{uncommented}"));
        let web = &parsed["plugins"]["web"];
        assert_eq!(web["engine"].as_str(), Some("ddg"));
        assert_eq!(web["engines"]["ddg"].as_str(), Some("https://duckduckgo.com/?q={q}"));
        let with_example = Manifest::parse(
            r#"
            id = "x"
            name = "X"
            version = "1"
            [[settings]]
            key = "items"
            type = "list"
            default = []
            example = '''
[[plugins.x.items]]
name = "a"
'''
            "#,
            "t",
        )
        .unwrap();
        let text = with_example.default_section().unwrap();
        assert!(!text.contains("items = []"), "the example replaces the bare default: {text}");
        assert!(text.contains("# [[plugins.x.items]]\n# name = \"a\"\n"), "{text}");
        let none = Manifest::parse("id = \"b\"\nname = \"B\"\nversion = \"1\"\n", "t").unwrap();
        assert!(none.default_section().is_none());
    }

    fn temp_dir() -> (tempfile::TempDir, PathBuf) {
        let t = crate::test_support::tmp();
        let d = t.path().to_path_buf();
        (t, d)
    }

    fn setting(kind: &str, options: &[&str]) -> Setting {
        Setting {
            key: "k".into(),
            kind: SettingKind::from(kind.to_string()),
            label: String::new(),
            description: String::new(),
            default: None,
            options: options.iter().map(std::string::ToString::to_string).collect(),
            example: String::new(),
            placeholder: String::new(),
            required: false,
            fields: vec![],
            import: None,
        }
    }

    #[test]
    fn examples_are_shown_relative_to_the_setting_on_the_settings_page() {
        let mut s = setting("table", &[]);
        s.key = "mcp".into();
        s.example = "[plugins.ai.mcp.files]\ncommand = [\"x\"]\n".into();
        assert_eq!(s.input_example("ai"), "[files]\ncommand = [\"x\"]");
        s.kind = SettingKind::List;
        s.key = "items".into();
        s.example = "[[plugins.x.items]]\nname = \"a\"".into();
        assert_eq!(s.input_example("x"), "[[items]]\nname = \"a\"");
        assert!(s.parse_input(&s.input_example("x")).unwrap().unwrap().is_array_of_tables());
    }

    #[test]
    fn accepts_checks_each_type() {
        use toml::Value as V;
        let s = || V::String("a".into());
        let i = V::Integer(1);
        let f = V::Float(1.5);
        let b = V::Boolean(true);
        let l = V::Array(vec![]);
        let t = V::Table(toml::Table::default());
        let all = [s(), i.clone(), f.clone(), b.clone(), l.clone(), t.clone()];
        let cases: [(&str, &[&str], [bool; 6]); 8] = [
            //        kind      options     str    int    float  bool   list   table
            ("string", &[], [true, false, false, false, false, false]),
            ("path", &[], [true, false, false, false, false, false]),
            ("number", &[], [false, true, true, false, false, false]),
            ("bool", &[], [false, false, false, true, false, false]),
            ("choice", &[], [true, false, false, false, false, false]),
            ("choice", &["b"], [false, false, false, false, false, false]),
            ("list", &[], [false, false, false, false, true, false]),
            ("table", &[], [false, false, false, false, false, true]),
        ];
        for (kind, opts, want) in cases {
            let got: Vec<bool> = all.iter().map(|v| setting(kind, opts).accepts(v)).collect();
            assert_eq!(got, want, "{kind} {opts:?}");
        }
        assert!(setting("choice", &["a", "b"]).accepts(&s()));
        assert!(all.iter().all(|v| setting("future-type", &[]).accepts(v)), "unknown types accept anything");
    }

    #[test]
    fn effective_settings_without_user_values() {
        let m = Manifest::parse(
            "id = \"x\"\nname = \"X\"\nversion = \"1\"\n[[settings]]\nkey = \"a\"\ntype = \"string\"\n[[settings]]\nkey = \"b\"\ntype = \"bool\"\ndefault = true\n",
            "t",
        )
        .unwrap();
        assert_eq!(m.effective_settings(None), serde_json::json!({"b": true}), "no default and no value: key absent");
        let user: toml::Table = toml::from_str("a = \"set\"").unwrap();
        assert_eq!(m.effective_settings(Some(&user)), serde_json::json!({"a": "set", "b": true}));
    }

    #[test]
    fn secret_settings_come_from_the_store_never_from_config_toml() {
        let m = Manifest::parse(
            "id = \"ai\"\nname = \"AI\"\nversion = \"1\"\n[[settings]]\nkey = \"api_key\"\ntype = \"secret\"\nlabel = \"API key\"\n[[settings]]\nkey = \"model\"\ntype = \"string\"\n[[settings]]\nkey = \"tokens\"\ntype = \"secrets\"\n",
            "t",
        )
        .unwrap();
        assert_eq!(m.settings[0].kind, SettingKind::Secret);
        assert_eq!(m.settings[2].kind, SettingKind::Secrets);
        let user: toml::Table =
            toml::from_str("api_key = \"leaked\"\nmodel = \"m\"\ntokens = { gh = \"leaked\" }").unwrap();
        let none = |_: &str| None;
        let no_table = |_: &str| BTreeMap::new();
        assert_eq!(
            m.effective_settings_with(Some(&user), &none, &no_table),
            serde_json::json!({"model": "m"}),
            "a key typed into config.toml is not used"
        );
        let store = |k: &str| (k == "api_key").then(|| "sk-1".to_string());
        let table = |k: &str| match k {
            "tokens" => BTreeMap::from([("gh".to_string(), "t1".to_string())]),
            _ => BTreeMap::new(),
        };
        assert_eq!(
            m.effective_settings_with(None, &store, &table),
            serde_json::json!({"api_key": "sk-1", "tokens": {"gh": "t1"}})
        );
        let section = m.default_section().unwrap();
        assert!(section.contains("plugin set ai tokens <name> <value>"), "{section}");
        assert!(
            section.contains("# api_key: API key. A secret") && section.contains("plugin set ai api_key"),
            "{section}"
        );
        assert!(!section.contains("api_key ="), "no value line for a secret: {section}");
    }

    #[test]
    fn load_requires_exec_for_this_os() {
        let (_tmp, d) = temp_dir();
        let err = Manifest::load(&d).unwrap_err();
        assert!(err.contains("plugin.toml"), "{err}");

        let head = "id = \"x\"\nname = \"X\"\nversion = \"1\"\n";
        let other_os = if cfg!(windows) { "linux = [\"./x\"]" } else { "windows = [\"x.exe\"]" };
        for run in ["", "[run]\nexec = []\n", &format!("[run.exec]\n{other_os}\n")] {
            std::fs::write(d.join("plugin.toml"), format!("{head}{run}")).unwrap();
            let err = Manifest::load(&d).unwrap_err();
            assert!(err.contains("no run.exec for this OS"), "{run:?}: {err}");
        }
        std::fs::write(d.join("plugin.toml"), format!("{head}[run]\nexec = [\"./x\"]\nmode = \"resident\"\n")).unwrap();
        assert_eq!(Manifest::load(&d).unwrap().run.mode, Mode::Resident);
    }

    #[test]
    fn external_plugins_cannot_use_the_builtin_prefix() {
        let (_tmp, d) = temp_dir();
        std::fs::write(
            d.join("plugin.toml"),
            "id = \"builtin-web\"\nname = \"X\"\nversion = \"1\"\n[run]\nexec = [\"./x\"]\n",
        )
        .unwrap();
        assert!(Manifest::load(&d).unwrap_err().contains("reserved"));
    }

    #[test]
    fn parse_errors_name_the_origin() {
        let err = Manifest::parse("name = \"X\"\nversion = \"1\"\n", "here.toml").unwrap_err();
        assert!(err.starts_with("here.toml: ") && err.contains("id"), "{err}");
        let err = Manifest::parse("id = \"x\"\nname = \"X\"\nversion = \"1\"\n[run]\nmode = \"always\"\n", "here.toml")
            .unwrap_err();
        assert!(err.starts_with("here.toml: "), "{err}");
    }

    #[test]
    fn ids_must_be_bare_toml_keys() {
        let parse = |id: &str| Manifest::parse(&format!("id = \"{id}\"\nname = \"N\"\nversion = \"1\"\n"), "t");
        for ok in ["calc", "proc-kill", "my_plugin2"] {
            assert!(parse(ok).is_ok(), "{ok}");
        }
        for bad in ["dev.example.echo", "with space", "", "ümlaut"] {
            assert!(parse(bad).unwrap_err().contains("may only use"), "{bad}");
        }
    }

    #[test]
    fn discover_sorts_and_keeps_broken_manifests() {
        let (_tmp, root) = temp_dir();
        let good = |id: &str| format!("id = \"{id}\"\nname = \"N\"\nversion = \"1\"\n[run]\nexec = [\"./x\"]\n");
        for (dir, text) in [("b", good("b")), ("a", good("a")), ("broken", "not toml =".into())] {
            std::fs::create_dir(root.join(dir)).unwrap();
            std::fs::write(root.join(dir).join("plugin.toml"), text).unwrap();
        }
        std::fs::create_dir(root.join("empty")).unwrap();
        std::fs::write(root.join("plugin.toml"), good("file-at-root")).unwrap();

        let found = discover(&root);
        let names: Vec<_> = found.iter().map(|(p, _)| p.file_name().unwrap().to_str().unwrap()).collect();
        assert_eq!(names, ["a", "b", "broken"]);
        assert_eq!(found[0].1.as_ref().unwrap().id, "a");
        assert!(found[2].1.is_err());
        assert!(discover(&root.join("missing")).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn world_writable_plugin_folders_are_ignored() {
        use std::os::unix::fs::PermissionsExt;
        let (_tmp, root) = temp_dir();
        let dir = root.join("p");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("plugin.toml"), "id = \"p\"\nname = \"N\"\nversion = \"1\"\n[run]\nexec = [\"./x\"]\n")
            .unwrap();
        assert_eq!(discover(&root).len(), 1);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(discover(&root).is_empty(), "plugin folder writable by all");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(discover(&root).is_empty(), "plugins root writable by all");
    }

    #[test]
    fn default_section_quotes_keys_lists_options_and_handles_missing_default() {
        let m = Manifest::parse(
            r#"
            id = "plain-id"
            name = "P"
            version = "1"
            [[settings]]
            key = "odd key"
            type = "choice"
            label = "Mode"
            options = ["a", "b"]
            default = "a"
            [[settings]]
            key = "nodefault"
            type = "string"
            description = "anything"
            "#,
            "t",
        )
        .unwrap();
        let text = m.default_section().unwrap();
        assert!(text.contains("# [plugins.plain-id]\n"), "bare id stays unquoted: {text}");
        assert!(text.contains("# \"odd key\" = \"a\"   # Mode (a | b)\n"), "{text}");
        assert!(text.contains("# nodefault = # anything\n"), "{text}");
    }

    #[test]
    fn bundled_manifest_needs_no_exec() {
        let m = Manifest::parse("id = \"b\"\nname = \"B\"\nversion = \"1\"\n", "test").unwrap();
        assert!(m.run.argv().is_none());
        assert_eq!(m.run.mode, Mode::OnDemand);
        assert!(m.query.global);
    }

    fn with_settings(settings: &str) -> Result<Manifest, String> {
        Manifest::parse(&format!("id = \"p\"\nname = \"P\"\nversion = \"1\"\n{settings}"), "test")
    }

    #[test]
    fn tables_of_entries() {
        let m = with_settings(
            "[[settings]]\nkey = \"servers\"\ntype = \"table\"\n\
             [[settings.fields]]\nkey = \"command\"\ntype = \"list\"\n\
             [[settings.fields]]\nkey = \"env\"\ntype = \"table\"\n",
        )
        .unwrap();
        let s = &m.settings[0];
        assert_eq!(s.fields.iter().map(|f| f.key.as_str()).collect::<Vec<_>>(), ["command", "env"]);
        let v = |t: &str| toml::Value::Table(toml::from_str(t).unwrap());
        assert!(s.accepts(&v("[a]\ncommand = [\"x\"]\nother = 1\n[b]\nenv = { A = \"b\" }")));
        assert!(!s.accepts(&v("[a]\ncommand = \"x\"")), "a field of the wrong type");
        assert!(!s.accepts(&v("a = 1")), "an entry that is not a table");
        assert!(!s.is_pair_table(None));
        assert!(s.fields[1].is_pair_table(Some(&v("A = \"b\""))));
        assert!(!s.fields[1].is_pair_table(Some(&v("A = 1"))));

        for (bad, why) in [
            ("type = \"list\"\n[[settings.fields]]\nkey = \"a\"\ntype = \"string\"", "only a table"),
            ("type = \"table\"\n[[settings.fields]]\nkey = \"a\"\ntype = \"secret\"", "cannot be a secret"),
            (
                "type = \"table\"\n[[settings.fields]]\nkey = \"a\"\ntype = \"table\"\n\
                 [[settings.fields.fields]]\nkey = \"b\"\ntype = \"string\"",
                "cannot have fields",
            ),
        ] {
            let e = with_settings(&format!("[[settings]]\nkey = \"s\"\n{bad}")).unwrap_err();
            assert!(e.contains(why), "{e}");
        }
    }
}
