use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Deserialize)]
#[serde(default)]
pub struct Config {
    /// Global hotkey, e.g. "alt+space". Windows reserves Win+Space.
    pub hotkey: String,
    pub max_results: usize,
    /// Extra plugin directory, besides `<config>/plugins`.
    pub plugins_dir: Option<PathBuf>,
    /// Per-plugin settings, keyed by plugin id: `[plugins.builtin-apps] enabled = false`.
    pub plugins: HashMap<String, PluginCfg>,
    /// Show a system tray icon (double-click shows the launcher; menu: Show, Settings, Quit).
    pub tray: bool,
    /// Colour theme.
    pub theme: ThemeMode,
    /// `true`/`false` registers/removes autostart each time the launcher starts; unset leaves it alone.
    pub start_on_boot: Option<bool>,
    /// Monitor to open on (X11/Windows).
    pub position: Placement,
    /// Fade in results that arrive late and show a progress bar while a plugin is still
    /// answering. Typing and selection are never animated.
    pub animations: bool,
    /// Closing without running anything and reopening within this many seconds brings back
    /// the search (or answer panel). 0: always start empty.
    pub keep_search_s: u64,
    /// PEM file with the CA certificates trusted by remote plugins (replacing the public
    /// roots). A plugin's own `ca_file` in `[plugins."<id>"]` wins. User config only.
    pub ca_file: Option<PathBuf>,
    /// HTTP proxy for plugins: "auto" (environment, then system settings), "none", or a URL.
    pub proxy: String,
    /// Extra hosts that bypass the proxy (comma-separated, `NO_PROXY` syntax).
    pub no_proxy: String,
}

/// Colour theme (`theme`): follow the desktop, or force one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(from = "String")]
pub enum ThemeMode {
    #[default]
    Auto,
    Dark,
    Light,
}

/// Which monitor the launcher opens on (`position`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(from = "String")]
pub enum Placement {
    /// The monitor under the mouse cursor.
    #[default]
    Mouse,
    /// The monitor of the focused window.
    Active,
    Primary,
}

/// Parse a case-insensitive keyword. An unknown one is reported and the default used, so a
/// typo in one setting does not throw away the whole config file.
fn keyword<T: Copy + Default>(setting: &str, value: &str, known: &[(&str, T)]) -> T {
    let value = value.trim().to_ascii_lowercase();
    known.iter().find(|(k, _)| *k == value).map_or_else(
        || {
            let names: Vec<&str> = known.iter().map(|(k, _)| *k).collect();
            log::warn!("config: {setting} = {value:?} is not one of {}; using the default", names.join(", "));
            T::default()
        },
        |(_, v)| *v,
    )
}

impl From<String> for ThemeMode {
    fn from(s: String) -> Self {
        keyword("theme", &s, &[("auto", Self::Auto), ("dark", Self::Dark), ("light", Self::Light)])
    }
}

impl From<String> for Placement {
    fn from(s: String) -> Self {
        keyword("position", &s, &[("mouse", Self::Mouse), ("active", Self::Active), ("primary", Self::Primary)])
    }
}

#[derive(Deserialize)]
#[serde(default)]
pub struct PluginCfg {
    pub enabled: bool,
    /// Every other key: the plugin's own settings (see its `[[settings]]`).
    #[serde(flatten)]
    pub settings: toml::Table,
}

impl Default for PluginCfg {
    fn default() -> Self {
        Self { enabled: true, settings: toml::Table::new() }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            hotkey: "alt+space".into(),
            max_results: 8,
            plugins_dir: None,
            plugins: HashMap::new(),
            tray: true,
            theme: ThemeMode::Auto,
            start_on_boot: None,
            position: Placement::Mouse,
            animations: true,
            keep_search_s: 90,
            ca_file: None,
            proxy: "auto".into(),
            no_proxy: String::new(),
        }
    }
}

/// Folder the executable lives in (symlinks resolved).
pub fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe().ok()?.canonicalize().ok()?.parent().map(Path::to_path_buf)
}

/// Portable mode keeps all user data in `<exe_dir>/data`: switched on by a `portable` file next to the
/// executable, or by `KAIROS_HOME` (which names the folder). Ignored if that folder can't be written.
pub fn portable_dir() -> Option<PathBuf> {
    if let Some(h) = std::env::var_os("KAIROS_HOME").filter(|h| !h.is_empty()) {
        return Some(PathBuf::from(h));
    }
    let data = exe_dir().filter(|d| d.join("portable").exists())?.join("data");
    std::fs::create_dir_all(&data).ok()?;
    let writable = std::fs::metadata(&data).is_ok_and(|m| !m.permissions().readonly());
    writable.then_some(data)
}

pub fn config_dir() -> PathBuf {
    portable_dir().unwrap_or_else(|| dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("kairos"))
}

fn state_dir() -> PathBuf {
    portable_dir()
        .or_else(|| dirs::state_dir().or_else(dirs::data_local_dir).map(|d| d.join("kairos")))
        .unwrap_or_else(config_dir)
}

/// A plugin's own folder for what it keeps between runs (`data_dir` in `initialize`), e.g.
/// `~/.local/state/kairos/plugins/<id>`. Outside the plugin folder, so updates keep it.
pub fn plugin_data_dir(id: &str) -> PathBuf {
    state_dir().join("plugins").join(id)
}

/// Where the launcher keeps what it learns (launch and command history): the XDG state folder
/// (`~/.local/state/kairos`), the local app-data folder on Windows, or the data folder in
/// portable mode. Older versions kept these files in the config folder; they are moved over.
pub fn state_file(name: &str) -> PathBuf {
    let dir = state_dir();
    let path = dir.join(name);
    let legacy = config_dir().join(name);
    if legacy != path && legacy.is_file() && !path.exists() {
        let moved = std::fs::create_dir_all(&dir).and_then(|()| std::fs::rename(&legacy, &path));
        if moved.is_err() && std::fs::copy(&legacy, &path).is_ok() {
            let _ = std::fs::remove_file(&legacy);
        }
    }
    path
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

/// What a fresh `config.toml` contains: every option, commented out at its default.
pub fn default_file_contents() -> &'static str {
    r#"# Launcher settings. Changes apply the next time the launcher opens.

# Global hotkey. On Wayland, bind `kairos --toggle` in your desktop instead
# (see `kairos --install-shortcut`).
# hotkey = "alt+space"

# Results shown at most.
# max_results = 8

# System tray icon (double-click shows the launcher).
# tray = true

# Colour theme: "auto" follows the system, or force "dark" / "light".
# theme = "auto"

# Monitor to open on (X11/Windows): "mouse" (under the cursor), "active" (the focused
# window's) or "primary". On Wayland the desktop decides. Needs a restart.
# position = "mouse"

# Fade in late results and show a progress bar while a plugin is still answering.
# animations = true

# Reopening within this many seconds brings back an unfinished search or answer, if you closed
# the launcher without running anything (0: always start empty).
# keep_search_s = 90

# Proxy for plugins that go online (exchange rates, AI, remote plugins): "auto" uses the
# HTTPS_PROXY/HTTP_PROXY variables, else the system settings (Windows Internet Options including
# setup scripts, GNOME, KDE); "none" connects directly; or give one, e.g. "http://proxy:8080",
# "http://user:password@proxy:8080", "socks5://127.0.0.1:1080". Plugins get it as HTTPS_PROXY etc.
# proxy = "auto"
# Hosts that skip the proxy, besides the system's list, e.g. "localhost,.corp.example".
# no_proxy = ""

# Start the launcher (hidden) when you sign in. Unset leaves autostart as it is.
# start_on_boot = true

# Extra folder with plugins, besides <config>/plugins and the `plugins` folder next to the executable.
# Relative paths are relative to this file; `~` is your home folder.
# plugins_dir = "/path/to/plugins"

# Turn a plugin off by its id (see `kairos --list-plugins`), or in the launcher: `plugin `.
# Plugin settings go in the same section; plugins add theirs below, commented out.
# [plugins.calc]
# enabled = false
"#
}

/// Create `path` from the template if it does not exist yet; never overwrites.
pub fn ensure_config_file_at(path: &std::path::Path) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    match std::fs::OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut f) => f.write_all(default_file_contents().as_bytes()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}

/// Open `config.toml` in the desktop's default editor, creating it first if needed.
/// Stand-in for a settings window.
pub fn open_config() -> Result<(), String> {
    let path = config_path();
    ensure_config_file_at(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    crate::core::process::open(&path.to_string_lossy()).map_err(|e| format!("cannot open {}: {e}", path.display()))
}

/// Last modification time of a file or folder, if it exists.
pub fn mtime(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn load_doc(path: &Path) -> Result<toml_edit::DocumentMut, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => default_file_contents().to_string(),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    text.parse().map_err(|e| format!("{}: {e}", path.display()))
}

fn save(path: &Path, text: &str) -> Result<(), String> {
    crate::core::fs::write_atomic(path, text.as_bytes()).map_err(|e| format!("{}: {e}", path.display()))
}

/// Set `[plugins.<id>] <key> = value` in the config file at `path`, keeping everything else
/// (comments, order, formatting) as it is. Creates the file from the template if missing.
pub fn set_plugin_value(path: &Path, id: &str, key: &str, value: toml_edit::Item) -> Result<(), String> {
    set_plugin_values(path, id, vec![(key.to_string(), value)])
}

/// `set_plugin_value` for several keys in one write. `Item::None` removes a key.
pub fn set_plugin_values(path: &Path, id: &str, values: Vec<(String, toml_edit::Item)>) -> Result<(), String> {
    let mut doc = load_doc(path)?;
    let plugins = doc
        .entry("plugins")
        .or_insert_with(toml_edit::table)
        .as_table_mut()
        .ok_or("config: `plugins` is not a table")?;
    plugins.set_implicit(true);
    let table = plugins
        .entry(id)
        .or_insert_with(toml_edit::table)
        .as_table_mut()
        .ok_or_else(|| format!("config: plugins.{id} is not a table"))?;
    for (key, mut value) in values {
        // Tables parsed elsewhere (the settings page) carry that text's positions; let them
        // go after the plugin's own section instead.
        if let Some(t) = value.as_table_mut() {
            clear_positions(t);
        }
        table[key.as_str()] = value;
    }
    save(path, &doc.to_string())
}

fn clear_positions(t: &mut toml_edit::Table) {
    t.set_position(None);
    for (_, item) in t.iter_mut() {
        match item {
            toml_edit::Item::Table(sub) => clear_positions(sub),
            toml_edit::Item::ArrayOfTables(a) => a.iter_mut().for_each(clear_positions),
            _ => {}
        }
    }
}

/// Append `section` (already formatted, normally all commented out) unless the file already
/// mentions `[plugins.<id>]`, commented or not. Returns whether it was added.
pub fn append_plugin_section(path: &Path, id: &str, section: &str) -> Result<bool, String> {
    ensure_config_file_at(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    // Present already, as a real table or commented out, quoted or bare: leave it alone.
    let is_header = |l: &str| {
        let l = l.trim_start_matches(['#', ' ']).trim_end();
        l.strip_prefix("[plugins.").and_then(|r| r.strip_suffix(']')).is_some_and(|k| k.trim_matches('"') == id)
    };
    let real = toml::from_str::<toml::Table>(&text).is_ok_and(|t| t.get("plugins").and_then(|p| p.get(id)).is_some());
    if real || text.lines().any(is_header) {
        return Ok(false);
    }
    let sep = if text.ends_with("\n\n") || text.is_empty() {
        ""
    } else if text.ends_with('\n') {
        "\n"
    } else {
        "\n\n"
    };
    save(path, &format!("{text}{sep}{section}"))?;
    Ok(true)
}

impl Config {
    pub fn load() -> Self {
        Self::load_from(&config_path())
    }

    pub fn load_from(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(s) => toml::from_str(&s).unwrap_or_else(|e| {
                log::warn!("config: {}: {e}; using defaults", path.display());
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn plugin_enabled(&self, id: &str) -> bool {
        self.plugins.get(id).is_none_or(|p| p.enabled)
    }

    /// The user's values for a plugin's settings (empty if it has no section).
    pub fn plugin_settings(&self, id: &str) -> Option<&toml::Table> {
        self.plugins.get(id).map(|p| &p.settings)
    }

    /// Directories scanned for plugins: `user_dir`, `plugins_dir` (relative paths resolve against `user_dir`'s parent, the
    /// config folder), `<exe_dir>/plugins` (bundled), and `KAIROS_PLUGINS` (a path list: `:` separated, `;` on Windows) adds folders for development.
    /// `user_dir` is `<config>/plugins` (or a test folder).
    pub fn plugin_dirs_with(&self, user_dir: &Path) -> Vec<PathBuf> {
        let mut dirs = vec![user_dir.to_path_buf()];
        dirs.extend(
            self.plugins_dir
                .as_deref()
                .map(|p| crate::core::paths::resolve(p, user_dir.parent().unwrap_or(Path::new(".")))),
        );
        dirs.extend(exe_dir().map(|d| d.join("plugins")));
        if let Some(v) = std::env::var_os("KAIROS_PLUGINS") {
            dirs.extend(std::env::split_paths(&v));
        }
        dirs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugins_dir_is_relative_to_config_folder() {
        let c = Config { plugins_dir: Some("extra".into()), ..Config::default() };
        let dirs = c.plugin_dirs_with(Path::new("/cfg/plugins"));
        assert_eq!(dirs[0], Path::new("/cfg/plugins"));
        assert_eq!(dirs[1], Path::new("/cfg/extra"));
        let abs = Config { plugins_dir: Some("/abs".into()), ..Config::default() };
        assert_eq!(abs.plugin_dirs_with(Path::new("/cfg/plugins"))[1], Path::new("/abs"));
    }

    #[test]
    fn ca_file_is_read_from_the_top_level_and_defaults_to_none() {
        assert!(Config::default().ca_file.is_none());
        let c: Config = toml::from_str("ca_file = \"/etc/corp-ca.pem\"\n").unwrap();
        assert_eq!(c.ca_file.as_deref(), Some(Path::new("/etc/corp-ca.pem")));
    }

    #[test]
    fn bundled_plugins_come_after_user_dir() {
        let dirs = Config::default().plugin_dirs_with(Path::new("/cfg/plugins"));
        assert_eq!(dirs[0], Path::new("/cfg/plugins"));
        assert!(dirs.get(1).is_none_or(|d| d.ends_with("plugins")));
    }

    #[test]
    fn template_is_valid_and_means_defaults() {
        let c: Config = toml::from_str(default_file_contents()).unwrap();
        let d = Config::default();
        assert_eq!((c.hotkey, c.max_results, c.tray), (d.hotkey, d.max_results, d.tray));
        assert!(c.plugins.is_empty());
    }

    #[test]
    fn uncommenting_every_template_option_still_parses() {
        let all: String = default_file_contents()
            .lines()
            .map(|l| l.strip_prefix("# ").filter(|r| r.contains('=') || r.starts_with('[')).unwrap_or(l))
            .collect::<Vec<_>>()
            .join("\n");
        let c: Config = toml::from_str(&all).unwrap_or_else(|e| panic!("{e}\n{all}"));
        assert!(!c.plugin_enabled("calc"));
        assert_eq!(c.theme, ThemeMode::Auto);
        assert_eq!(c.plugins_dir.as_deref(), Some(std::path::Path::new("/path/to/plugins")));
    }

    #[test]
    fn ensure_creates_once_and_never_overwrites() {
        let tmp = crate::test_support::tmp();
        let dir = tmp.path();
        let path = dir.join("nested").join("config.toml");
        ensure_config_file_at(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), default_file_contents());
        std::fs::write(&path, "tray = false\n").unwrap();
        ensure_config_file_at(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "tray = false\n", "user edits must survive");
    }

    fn tmp_config(text: &str) -> (tempfile::TempDir, PathBuf) {
        let tmp = crate::test_support::tmp();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, text).unwrap();
        (tmp, path)
    }

    #[test]
    fn plugin_sections_carry_settings_next_to_enabled() {
        let c: Config =
            toml::from_str("[plugins.builtin-web]\nbrowser = \"firefox\"\n[plugins.builtin-web.engines]\ng = \"x\"\n")
                .unwrap();
        assert!(c.plugin_enabled("builtin-web"));
        let s = c.plugin_settings("builtin-web").unwrap();
        assert_eq!(s["browser"].as_str(), Some("firefox"));
        assert_eq!(s["engines"]["g"].as_str(), Some("x"));
        assert!(!s.contains_key("enabled"));
    }

    #[test]
    fn writes_keep_comments_and_other_sections() {
        let (_tmp, path) = tmp_config("# my hotkey\nhotkey = \"ctrl+space\"\n\n[plugins.a]\nenabled = false # off\n");
        set_plugin_value(&path, "calc", "enabled", toml_edit::value(false)).unwrap();
        set_plugin_value(&path, "a", "enabled", toml_edit::value(true)).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# my hotkey\nhotkey = \"ctrl+space\"\n"), "{text}");
        assert!(!text.contains("[plugins]\n"), "parent table stays implicit: {text}");
        let c: Config = toml::from_str(&text).unwrap();
        assert!(!c.plugin_enabled("calc"));
        assert!(c.plugin_enabled("a"));
        assert_eq!(c.hotkey, "ctrl+space");
    }

    #[test]
    fn plugin_section_is_appended_once() {
        let (_tmp, path) = tmp_config("tray = false\n");
        let section = "# [plugins.\"x.y\"]\n# a = 1\n";
        assert!(append_plugin_section(&path, "x.y", section).unwrap());
        assert!(!append_plugin_section(&path, "x.y", section).unwrap(), "commented header counts");
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, "tray = false\n\n# [plugins.\"x.y\"]\n# a = 1\n");
        set_plugin_value(&path, "z", "enabled", toml_edit::value(false)).unwrap();
        assert!(!append_plugin_section(&path, "z", "# [plugins.z]\n").unwrap(), "real section counts");
    }

    #[test]
    fn theme_setting_maps_to_modes() {
        let mode = |t: &str| toml::from_str::<Config>(&format!("theme = \"{t}\"")).unwrap().theme;
        assert_eq!(
            (mode("auto"), mode("dark"), mode("Light"), mode("nonsense")),
            (ThemeMode::Auto, ThemeMode::Dark, ThemeMode::Light, ThemeMode::Auto)
        );
        // A bad keyword costs only that setting, not the rest of the file.
        assert_eq!(Config::default().keep_search_s, 90);
        let k: Config = toml::from_str("keep_search_s = 0\n").unwrap();
        assert_eq!(k.keep_search_s, 0);
        assert!(default_file_contents().contains("# keep_search_s = 90"));
        let c: Config = toml::from_str("theme = \"nonsense\"\nmax_results = 3\n").unwrap();
        assert_eq!(c.max_results, 3);
    }

    #[test]
    fn position_setting_maps_to_placements() {
        let p = |t: &str| toml::from_str::<Config>(&format!("position = \"{t}\"")).unwrap().position;
        assert_eq!(
            (p("mouse"), p("Active"), p("primary"), p("nonsense")),
            (Placement::Mouse, Placement::Active, Placement::Primary, Placement::Mouse)
        );
        assert_eq!(Config::default().position, Placement::Mouse);
    }
}
