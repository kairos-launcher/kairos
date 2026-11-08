//! Routes queries to providers, merges and ranks results, records launches.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError, RwLock, Weak};
use std::time::{Duration, Instant, SystemTime};

use crate::builtin::{self, Bundled};
use crate::config::Config;
use crate::core::frecency::Frecency;
use crate::core::sync;
use crate::icon_store::{IconStore, LazyIcon};
use crate::plugin;
use crate::plugin::Action;
use crate::plugin::external::ExternalPlugin;
use crate::plugin::manifest::{self, Manifest};
use crate::plugin::{ExecCtx, ExecOutcome, Provider, Query, ResultItem};

/// Weight of the frecency boost relative to a 0..1 match score.
const FRECENCY_WEIGHT: f64 = 0.3;
/// Weight of the boost for the row last picked for the same typed text: enough to put it on top.
const PICK_WEIGHT: f64 = 1.0;
/// Half-life of a remembered pick, in days.
const PICK_HALF_LIFE_DAYS: f64 = 30.0;
/// Delay between hiding the launcher and stopping idle on-demand plugins.
const REAP_DELAY: Duration = Duration::from_secs(35);
/// Delay between hiding the launcher and handing memory back (`crate::memory`). Short: what it
/// pages out comes back from the page cache in milliseconds if the launcher reopens.
const RELEASE_DELAY: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Bundled,
    External,
}

/// A plugin known to the engine, whether or not it is enabled.
pub struct PluginInfo {
    pub manifest: Manifest,
    pub kind: Kind,
    pub enabled: bool,
    pub provider: Arc<dyn Provider>,
    /// External plugins: the folder holding `plugin.toml`.
    pub dir: Option<PathBuf>,
    /// The effective settings the provider was built with.
    pub settings: serde_json::Value,
}

/// One edit from a plugin's settings page (`Engine::save_settings`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingChange {
    /// The text in the field, parsed with `Setting::parse_input`; for a `secret`, its new
    /// value (empty removes it).
    Set { key: String, text: String },
    /// Back to the default; a `secret` is removed.
    Reset { key: String },
    /// One entry of a `secrets` setting; an empty value removes it.
    SecretEntry { key: String, name: String, value: String },
}

impl SettingChange {
    pub fn key(&self) -> &str {
        match self {
            Self::Set { key, .. } | Self::Reset { key } | Self::SecretEntry { key, .. } => key,
        }
    }
}

/// Routing data taken from a plugin's manifest.
struct Route {
    provider: Arc<dyn Provider>,
    triggers: Vec<String>,
    global: bool,
    global_requires: Option<String>,
    name: String,
    description: String,
    /// Titles of required settings that are empty: the plugin is not started and its trigger
    /// offers its settings page instead.
    setup: Vec<String>,
}

impl Route {
    fn wants_global(&self, text: &str) -> bool {
        self.setup.is_empty() && self.global && requires_ok(self.global_requires.as_deref(), text)
    }

    /// The one row a plugin that still needs setup answers its trigger with.
    fn setup_row(&self) -> ResultItem {
        ResultItem {
            id: format!("setup:{}", self.provider.id()),
            title: format!("{} needs setup", self.name),
            subtitle: format!("Missing: {} · Enter opens its settings", self.setup.join(", ")),
            icon: Some(crate::plugin::Icon { glyph: Some("⚙".into()), ..Default::default() }),
            score: 1.0,
            actions: vec![Action {
                id: "settings".into(),
                title: "Open settings".into(),
                default: true,
                kind: crate::plugin::ActionKind::Settings,
                ..Default::default()
            }],
            complete: None,
            // Not a launch worth remembering.
            fallback: true,
            provider: self.provider.id().to_string(),
            query: String::new(),
        }
    }
}

/// `global_requires`: the text must contain one of these characters (no filter if unset).
fn requires_ok(chars: Option<&str>, text: &str) -> bool {
    chars.is_none_or(|cs| text.chars().any(|c| cs.contains(c)))
}

/// Provider id of the engine's own "command" results (trigger suggestions).
const SUGGEST_PROVIDER: &str = "engine.commands";

/// Every known plugin and the routes of the enabled ones. Replaced as a whole on reload;
/// readers keep using the snapshot they took.
pub struct Registry {
    pub plugins: Vec<PluginInfo>,
    providers: Vec<Route>,
}

impl Registry {
    /// The provider of an enabled plugin.
    fn enabled(&self, id: &str) -> Option<&Arc<dyn Provider>> {
        self.providers.iter().map(|r| &r.provider).find(|p| p.id() == id)
    }

    fn new(plugins: Vec<PluginInfo>) -> Self {
        let providers = plugins
            .iter()
            .filter(|p| p.enabled)
            .map(|p| Route {
                provider: p.provider.clone(),
                triggers: p.manifest.query.triggers.clone(),
                // A remote plugin sees every keystroke, so the user decides whether it is global.
                global: match p.settings.get("global").and_then(serde_json::Value::as_bool) {
                    Some(g) if p.manifest.is_remote() => g,
                    _ => p.manifest.query.global,
                },
                global_requires: p.manifest.query.global_requires.clone(),
                name: p.manifest.name.clone(),
                description: p.manifest.description.clone(),
                setup: p.manifest.missing_required(&p.settings).iter().map(|s| s.title().to_string()).collect(),
            })
            .collect();
        Self { plugins, providers }
    }
}

/// Files the engine reads and writes. Tests point these at temporary folders; the default is
/// empty paths, which read nothing.
#[derive(Clone, Debug, Default)]
pub struct Paths {
    pub config: PathBuf,
    /// Where `plugin install` puts plugins; also scanned for plugins.
    pub plugins: PathBuf,
    /// Launch history used for ranking.
    pub frecency: PathBuf,
    /// Which row was picked for which typed text.
    pub picks: PathBuf,
    /// Private values of `secret` settings (`crate::plugin::secrets`); empty: none.
    pub secrets: PathBuf,
}

impl Paths {
    /// The user's real files.
    pub fn user() -> Self {
        Self {
            config: crate::config::config_path(),
            plugins: crate::config::config_dir().join("plugins"),
            frecency: crate::config::state_file("frecency.json"),
            picks: crate::config::state_file("query_picks.json"),
            secrets: crate::plugin::secrets::user_dir(),
        }
    }
}

pub struct Engine {
    registry: RwLock<Arc<Registry>>,
    frecency: Mutex<Frecency>,
    /// The row last picked per typed text (`pick_key`): one pick counts fully, and a new pick for
    /// the same text replaces the old one.
    picks: Mutex<Frecency>,
    max_results: AtomicUsize,
    /// `keep_search_s` from config.toml.
    keep_search_s: AtomicU64,
    icons: IconStore,
    paths: Paths,
    /// Handed to bundled plugins that manage the engine (the plugin manager).
    me: Weak<Engine>,
    /// Modification time of `config.toml` at the last (re)load.
    config_seen: Mutex<Option<SystemTime>>,
    /// Serializes reloads.
    reloading: Mutex<()>,
    reaper: Arc<Reaper>,
}

fn external_provider(m: &Manifest, dir: PathBuf, settings: serde_json::Value) -> Arc<dyn Provider> {
    #[cfg(feature = "remote")]
    if m.is_remote() {
        return Arc::new(crate::plugin::remote::RemotePlugin::new(m.clone(), settings));
    }
    Arc::new(ExternalPlugin::new(m.clone(), dir, settings))
}

/// A remote plugin without its own `ca_file` gets the global one.
fn with_global_ca(settings: &mut serde_json::Value, remote: bool, ca: Option<&std::path::Path>) {
    if let (true, Some(ca), Some(map)) = (remote, ca, settings.as_object_mut()) {
        map.entry("ca_file").or_insert_with(|| ca.to_string_lossy().into_owned().into());
    }
}

/// Merge bundled and discovered external plugins. Bundled plugins win id clashes; failures
/// are reported and skipped. A plugin whose manifest, folder and settings are unchanged since
/// `prev` keeps its provider, so reloading does not restart processes or drop indexes.
fn assemble(
    bundled: Vec<Bundled>,
    external: Vec<(PathBuf, Result<Manifest, String>)>,
    cfg: &Config,
    secrets: &std::path::Path,
    prev: Option<&Registry>,
) -> Vec<PluginInfo> {
    let reuse = |m: &Manifest, dir: Option<&std::path::Path>, settings: &serde_json::Value| {
        prev?
            .plugins
            .iter()
            .find(|p| p.manifest == *m && p.dir.as_deref() == dir && p.settings == *settings)
            .map(|p| p.provider.clone())
    };
    let mut out: Vec<PluginInfo> = bundled
        .into_iter()
        .map(|b| PluginInfo {
            enabled: cfg.plugin_enabled(&b.manifest.id),
            provider: reuse(&b.manifest, None, &b.settings).unwrap_or(b.provider),
            manifest: b.manifest,
            kind: Kind::Bundled,
            dir: None,
            settings: b.settings,
        })
        .collect();

    for (dir, m) in external {
        match m {
            Ok(m) if out.iter().any(|p| p.manifest.id == m.id) => {
                log::warn!("plugin {}: duplicate id, skipped ({})", m.id, dir.display());
            }
            Ok(m) => {
                let mut settings = m.effective_settings_with(
                    cfg.plugin_settings(&m.id),
                    &|k| crate::plugin::secrets::setting_in(secrets, &m.id, k),
                    &|k| crate::plugin::secrets::table_in(secrets, &m.id, k),
                );
                with_global_ca(&mut settings, m.is_remote(), cfg.ca_file.as_deref());
                let provider = reuse(&m, Some(&dir), &settings)
                    .unwrap_or_else(|| external_provider(&m, dir.clone(), settings.clone()));
                out.push(PluginInfo {
                    enabled: cfg.plugin_enabled(&m.id),
                    provider,
                    manifest: m,
                    kind: Kind::External,
                    dir: Some(dir),
                    settings,
                });
            }
            Err(e) => log::warn!("plugin load failed: {e}"),
        }
    }
    out
}

impl Engine {
    pub fn new(cfg: &Config) -> Arc<Self> {
        Self::with_paths(cfg, Paths::user())
    }

    pub fn with_paths(cfg: &Config, paths: Paths) -> Arc<Self> {
        let frecency = Frecency::load(file(&paths.frecency));
        let icons = IconStore::new(IconStore::default_dir());
        std::thread::spawn(|| IconStore::prune(&IconStore::default_dir()));
        let config_seen = crate::config::mtime(&paths.config);
        crate::core::proxy::configure(&cfg.proxy, &cfg.no_proxy);
        Arc::new_cyclic(|me: &Weak<Engine>| {
            let plugins = load_plugins(cfg, &paths, me.clone(), None);
            let e = Self::build(plugins, frecency, cfg.max_results, icons, paths, me.clone(), config_seen);
            e.keep_search_s.store(cfg.keep_search_s, Ordering::Relaxed);
            e
        })
    }

    /// How long an unfinished search is kept for the next open (`keep_search_s`).
    pub fn keep_search(&self) -> Duration {
        Duration::from_secs(self.keep_search_s.load(Ordering::Relaxed))
    }

    fn build(
        plugins: Vec<PluginInfo>,
        frecency: Frecency,
        max_results: usize,
        icons: IconStore,
        paths: Paths,
        me: Weak<Engine>,
        config_seen: Option<SystemTime>,
    ) -> Self {
        Self {
            registry: RwLock::new(Arc::new(Registry::new(plugins))),
            frecency: Mutex::new(frecency),
            picks: Mutex::new(Frecency::load_with(file(&paths.picks), 1, PICK_HALF_LIFE_DAYS)),
            max_results: AtomicUsize::new(max_results),
            keep_search_s: AtomicU64::new(crate::config::Config::default().keep_search_s),
            icons,
            paths,
            me,
            config_seen: Mutex::new(config_seen),
            reloading: Mutex::new(()),
            reaper: Arc::default(),
        }
    }

    /// An engine over the given plugins, with no files behind it.
    #[cfg(test)]
    fn from_plugins(plugins: Vec<PluginInfo>, frecency: Frecency, max_results: usize, icons: IconStore) -> Self {
        Self::build(plugins, frecency, max_results, icons, Paths::default(), Weak::new(), None)
    }

    /// The current plugins. Cheap; the snapshot stays valid across reloads.
    pub fn plugins(&self) -> Arc<Registry> {
        sync::read(&self.registry).clone()
    }

    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    /// Re-read `config.toml` and the plugin folders and swap in the result. Unchanged plugins
    /// keep running; new ones are warmed up in the background; removed ones are stopped when the
    /// last running query lets go of them.
    pub fn reload(&self) {
        let _one_at_a_time = sync::lock(&self.reloading);
        let seen = crate::config::mtime(&self.paths.config);
        let cfg = Config::load_from(&self.paths.config);
        let prev = self.plugins();
        // Plugin processes got the old proxy in their environment: start every plugin afresh.
        let reuse = (!crate::core::proxy::configure(&cfg.proxy, &cfg.no_proxy)).then_some(&*prev);
        let next = Arc::new(Registry::new(load_plugins(&cfg, &self.paths, self.me.clone(), reuse)));
        let fresh: Vec<Arc<dyn Provider>> = next
            .providers
            .iter()
            .filter(|r| !prev.providers.iter().any(|o| Arc::ptr_eq(&o.provider, &r.provider)))
            .map(|r| r.provider.clone())
            .collect();
        *sync::write(&self.registry) = next;
        self.max_results.store(cfg.max_results, Ordering::Relaxed);
        self.keep_search_s.store(cfg.keep_search_s, Ordering::Relaxed);
        *sync::lock(&self.config_seen) = seen;
        if !fresh.is_empty() {
            std::thread::spawn(move || fresh.iter().for_each(|p| p.warm_up()));
        }
    }

    /// Reload if `config.toml` changed since the last load. Returns whether it did.
    pub fn reload_if_config_changed(&self) -> bool {
        let now = crate::config::mtime(&self.paths.config);
        let changed = *sync::lock(&self.config_seen) != now;
        if changed {
            self.reload();
        }
        changed
    }

    /// Persist `[plugins.<id>] <key> = value` and reload.
    pub fn set_plugin_value(&self, id: &str, key: &str, value: toml_edit::Item) -> Result<(), String> {
        crate::config::set_plugin_value(&self.paths.config, id, key, value)?;
        self.reload();
        Ok(())
    }

    /// Apply the changes of a plugin's settings page: everything is checked first, so either
    /// all of it is saved (one `config.toml` write, then one reload, so an external plugin
    /// restarts once) or nothing is and each bad field gets its message, by key.
    pub fn save_settings(&self, id: &str, changes: Vec<SettingChange>) -> Result<(), Vec<(String, String)>> {
        use manifest::SettingKind;
        let reg = self.plugins();
        let general = |msg: String| vec![(String::new(), msg)];
        let plugin = reg.plugins.iter().find(|p| p.manifest.id == id).ok_or_else(|| general("plugin gone".into()))?;
        let mut values = vec![];
        let mut secrets = vec![];
        let mut errors = vec![];
        for change in changes {
            let key = change.key().to_string();
            let Some(setting) = plugin.manifest.settings.iter().find(|s| s.key == key) else {
                errors.push((key, "not a setting of this plugin".into()));
                continue;
            };
            match (change, &setting.kind) {
                (SettingChange::Reset { .. }, SettingKind::Secret) => secrets.push((key, None, String::new())),
                (SettingChange::Reset { .. }, _) => values.push((key, toml_edit::Item::None)),
                (SettingChange::Set { text, .. }, SettingKind::Secret) => secrets.push((key, None, text)),
                (SettingChange::SecretEntry { name, value, .. }, SettingKind::Secrets) => {
                    if crate::plugin::secrets::valid_name(&name) {
                        secrets.push((key, Some(name), value));
                    } else {
                        errors.push((key, format!("{name:?}: use letters, digits, - and _")));
                    }
                }
                (SettingChange::Set { text, .. }, SettingKind::Number) if text.trim().is_empty() => {
                    values.push((key, toml_edit::Item::None));
                }
                (SettingChange::Set { text, .. }, _) => match setting.parse_input(&text) {
                    Ok(item) => values.push((key, item.unwrap_or_default())),
                    Err(e) => errors.push((key, e)),
                },
                (SettingChange::SecretEntry { .. }, _) => errors.push((key, "not a list of named secrets".into())),
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        let mut result = Ok(());
        if !values.is_empty() {
            result = crate::config::set_plugin_values(&self.paths.config, id, values);
        }
        for (key, name, value) in secrets {
            if result.is_err() {
                break;
            }
            result = match name {
                Some(name) => crate::plugin::secrets::write_entry_in(&self.paths.secrets, id, &key, &name, &value),
                None => crate::plugin::secrets::write_in(&self.paths.secrets, id, Some(&key), &value),
            }
            .map(|_| ());
        }
        self.reload();
        result.map_err(general)
    }

    /// Store (or, with an empty value, remove) a plugin's `secret` setting and reload so the
    /// plugin gets it. Never touches `config.toml`.
    pub fn set_plugin_secret(&self, id: &str, key: &str, value: &str) -> Result<(), String> {
        crate::plugin::secrets::write_in(&self.paths.secrets, id, Some(key), value)?;
        self.reload();
        Ok(())
    }

    /// Store (or remove) one named entry of a plugin's `secrets` setting and reload.
    pub fn set_plugin_secret_entry(&self, id: &str, key: &str, name: &str, value: &str) -> Result<(), String> {
        crate::plugin::secrets::write_entry_in(&self.paths.secrets, id, key, name, value)?;
        self.reload();
        Ok(())
    }

    /// Add a commented-out section with the default settings of every plugin that has settings
    /// and no section in `config.toml` yet. Never changes existing text.
    pub fn write_default_sections(&self) {
        let mut added = false;
        for p in &self.plugins().plugins {
            if let Some(section) = p.manifest.default_section() {
                match crate::config::append_plugin_section(&self.paths.config, &p.manifest.id, &section) {
                    Ok(a) => added |= a,
                    Err(e) => log::warn!("config: {e}"),
                }
            }
        }
        if added {
            // Only comments were added: nothing to reload.
            *sync::lock(&self.config_seen) = crate::config::mtime(&self.paths.config);
        }
    }

    /// Warm up enabled plugins (index data, start resident processes). Call from a
    /// background thread.
    pub fn warm_up(&self) {
        for p in self.plugins().providers.iter().filter(|p| p.setup.is_empty()) {
            p.provider.warm_up();
        }
        // Read the proxy settings now, not when the first plugin starts.
        let _ = crate::core::proxy::current();
    }

    pub fn refresh_if_stale(&self) {
        for p in self.plugins().providers.iter().filter(|p| p.setup.is_empty()) {
            if p.provider.needs_refresh() {
                p.provider.refresh();
            }
        }
    }

    /// Rebuild every plugin's data now, stale or not (`plugin reload`). Call off the UI thread.
    pub fn refresh_all(&self) {
        for p in self.plugins().providers.iter().filter(|p| p.setup.is_empty()) {
            p.provider.refresh();
        }
    }

    /// Results that advertise commands while the user is still typing one: `ki` or `kill`
    /// offers the `kill ` trigger. Choosing it completes the text so the plugin takes over.
    fn suggest_commands(providers: &[Route], text: &str) -> Vec<ResultItem> {
        let typed = text.trim_end().to_lowercase();
        if typed.is_empty() {
            return vec![];
        }
        providers
            .iter()
            .filter_map(|p| {
                let trigger = p.triggers.iter().find(|t| t.trim_end().to_lowercase().starts_with(&typed))?;
                let word = trigger.trim_end();
                let exact = word.to_lowercase() == typed;
                let progress = typed.chars().count() as f64 / word.chars().count().max(1) as f64;
                let detail =
                    if p.description.is_empty() { p.name.clone() } else { format!("{}: {}", p.name, p.description) };
                Some(ResultItem {
                    // The id carries the text to complete to, including any trailing space.
                    id: trigger.clone(),
                    title: word.to_string(),
                    subtitle: detail,
                    icon: Some(crate::plugin::Icon { glyph: Some("›".into()), ..Default::default() }),
                    // An exact command outranks everything; a partial one stays below good matches.
                    score: if exact { 1.2 } else { 0.5 + 0.3 * progress },
                    actions: vec![crate::plugin::Action {
                        id: "complete".into(),
                        title: "Use command".into(),
                        default: true,
                        ..Default::default()
                    }],
                    // Tab completes straight to the trigger; no one-entry menu.
                    complete: Some(trigger.clone()),
                    fallback: false,
                    provider: SUGGEST_PROVIDER.into(),
                    query: String::new(),
                })
            })
            .collect()
    }

    /// Blocking query: the final merged list (see `query_stream`).
    #[cfg(test)]
    pub fn query(&self, text: &str) -> Vec<ResultItem> {
        let mut last = vec![];
        self.query_stream(text, |items| {
            last = items;
            true
        });
        last
    }

    /// Progressive query. `on_update` gets the full ranked list after every provider batch;
    /// return false to stop early (the query was superseded).
    pub fn query_stream(&self, text: &str, mut on_update: impl FnMut(Vec<ResultItem>) -> bool) {
        let text = text.trim_start();
        if text.is_empty() {
            on_update(vec![]);
            return;
        }

        let reg = self.plugins();
        let max_results = self.max_results.load(Ordering::Relaxed);
        // A trigger prefix routes the query to exactly one plugin.
        let targeted = reg
            .providers
            .iter()
            .find_map(|p| p.triggers.iter().find(|t| text.starts_with(t.as_str())).map(|t| (p, t.as_str())));

        let mut jobs: Vec<(&Route, Query)> = vec![];
        let mut suggestions: Vec<ResultItem> = vec![];
        if let Some((p, _)) = targeted.filter(|(p, _)| !p.setup.is_empty()) {
            on_update(vec![p.setup_row()]);
            return;
        }
        if let Some((p, t)) = targeted {
            jobs.push((p, Query { text: text[t.len()..].trim_start(), trigger: Some(t), limit: max_results }));
        } else {
            suggestions = Self::suggest_commands(&reg.providers, text);
            for p in reg.providers.iter().filter(|p| p.wants_global(text)) {
                jobs.push((p, Query { text, trigger: None, limit: max_results }));
            }
        }

        // Fan out without a join barrier: every provider reports as soon as it has results, and
        // each arrival re-ranks and publishes, so a slow provider never delays the fast ones.
        let mut per_job: Vec<Vec<ResultItem>> = vec![vec![]; jobs.len()];
        if (jobs.is_empty() || !suggestions.is_empty()) && !on_update(self.rank(&per_job, &suggestions, max_results)) {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel::<(usize, Vec<ResultItem>)>();
        std::thread::scope(|s| {
            for (n, (p, q)) in jobs.iter().enumerate() {
                let tx = tx.clone();
                s.spawn(move || {
                    p.provider.query_stream(q, &mut |mut items| {
                        items.truncate(q.limit);
                        for i in &mut items {
                            i.clamp_sizes();
                            i.provider = p.provider.id().to_string();
                            i.query = q.text.to_string();
                        }
                        let _ = tx.send((n, items));
                    });
                });
            }
            drop(tx);
            while let Ok((n, mut items)) = rx.recv() {
                self.finalize_icons(&mut items);
                per_job[n].extend(items);
                if !on_update(self.rank(&per_job, &suggestions, max_results)) {
                    break; // superseded; dropping `rx` makes the remaining sends no-ops
                }
            }
        });
    }

    /// Merge the per-provider results, add frecency and cut to `max_results`.
    fn rank(&self, per_job: &[Vec<ResultItem>], suggestions: &[ResultItem], max_results: usize) -> Vec<ResultItem> {
        let mut all: Vec<ResultItem> = per_job.iter().flatten().cloned().collect();
        all.extend(suggestions.iter().cloned());
        {
            let f = sync::lock(&self.frecency);
            let picks = sync::lock(&self.picks);
            for i in all.iter_mut().filter(|i| i.provider != SUGGEST_PROVIDER && !i.fallback) {
                i.score += FRECENCY_WEIGHT * f.boost(&key(i)) + PICK_WEIGHT * picks.boost(&pick_key(i));
            }
        }
        // Fallback rows go last and keep their place: the other rows make room for them.
        let (mut all, mut fallbacks): (Vec<_>, Vec<_>) = all.into_iter().partition(|i| !i.fallback);
        all.sort_by(|a, b| b.score.total_cmp(&a.score));
        fallbacks.sort_by(|a, b| b.score.total_cmp(&a.score));
        fallbacks.truncate(max_results);
        all.truncate(max_results - fallbacks.len());
        all.extend(fallbacks);
        all
    }

    /// Turn inline `base64` icons into cached image files and apply already-fetched lazy ones,
    /// so the UI only ever deals with `path` / `glyph`.
    fn finalize_icons(&self, items: &mut [ResultItem]) {
        for item in items {
            let provider = item.provider.clone();
            let Some(icon) = &mut item.icon else { continue };
            if icon.path.is_none()
                && let Some(data) = icon.base64.take()
            {
                icon.path = self.icons.store_base64(&provider, &data).map(|p| p.to_string_lossy().into_owned());
            }
            if icon.path.is_none()
                && icon.glyph.is_none()
                && let Some(LazyIcon::File(p)) = icon.id.as_deref().and_then(|id| self.icons.lazy_get(&provider, id))
            {
                icon.path = Some(p.to_string_lossy().into_owned());
            }
        }
    }

    /// Fetch the lazy icons (`icon: {"id": ..}`) of the given, already displayed, results with
    /// one `icon` request per distinct id. Returns true if any result gained an icon, so the
    /// caller can refresh the list. Call after showing results, off the UI thread.
    pub fn fetch_lazy_icons(&self, items: &mut [ResultItem]) -> bool {
        let mut pending: Vec<(String, String)> = vec![];
        for item in items.iter() {
            let Some(icon) = &item.icon else { continue };
            let Some(id) = &icon.id else { continue };
            let key = (item.provider.clone(), id.clone());
            if icon.path.is_none()
                && icon.glyph.is_none()
                && self.icons.lazy_get(&key.0, &key.1).is_none()
                && !pending.contains(&key)
            {
                pending.push(key);
            }
        }
        if pending.is_empty() {
            return false;
        }
        let reg = self.plugins();
        for (plugin, id) in &pending {
            let fetched = reg.enabled(plugin).and_then(|p| p.icon(id));
            let path = fetched.and_then(|ic| {
                ic.path
                    .map(std::path::PathBuf::from)
                    .filter(|p| p.is_file())
                    .or_else(|| ic.base64.as_deref().and_then(|b| self.icons.store_base64(plugin, b)))
            });
            self.icons.lazy_put(plugin, id, path.map_or(LazyIcon::Missing, LazyIcon::File));
        }
        self.finalize_icons(items);
        true
    }

    /// Run an action of `item`. Streamed output (`ExecOutcome::Output`) goes to `ctx.sink`.
    pub fn execute(&self, item: &ResultItem, action_id: &str, ctx: &ExecCtx) -> plugin::Result<ExecOutcome> {
        if item.provider == SUGGEST_PROVIDER {
            // Completing a command: the item id is the trigger text to put in the search box.
            return Ok(ExecOutcome::complete(item.id.clone()));
        }
        let host = item.actions.iter().find(|a| a.id == action_id).and_then(Action::run_host);
        let out = if let Some(done) = host {
            match done? {
                // A plugin can only open its own settings page.
                ExecOutcome::Settings(_) => return Ok(ExecOutcome::Settings(item.provider.clone())),
                out => out,
            }
        } else {
            self.plugins().enabled(&item.provider).ok_or(plugin::Error::Gone)?.execute_with(
                &item.id,
                action_id,
                &item.query,
                ctx,
            )?
        };
        if !item.fallback {
            let saves = [
                sync::lock(&self.frecency).record(&key(item)),
                sync::lock(&self.picks).record_only(&pick_key(item), &pick_group(item)),
            ];
            persist(saves.into_iter().flatten().collect());
        }
        Ok(out)
    }

    /// Stop streaming the output of execution `exec` (its panel closed). May block briefly on
    /// the plugin, so call it off the UI thread.
    pub fn cancel_output(&self, provider: &str, exec: u64) {
        if let Some(p) = self.plugins().enabled(provider) {
            p.cancel_output(exec);
        }
    }

    /// After the launcher hides: hand memory back to the system soon (`crate::memory`), and
    /// stop on-demand plugins once they have been idle long enough (then hand back what they
    /// freed). One reaper thread serves every hide; a later hide pushes the deadlines back.
    pub fn schedule_reap(self: &Arc<Self>) {
        let r = &self.reaper;
        let now = Instant::now();
        *sync::lock(&r.due) = Deadlines { release: Some(now + RELEASE_DELAY), reap: Some(now + REAP_DELAY) };
        r.wake.notify_one();
        r.started.get_or_init(|| {
            let (r, me) = (r.clone(), Arc::downgrade(self));
            std::thread::spawn(move || {
                loop {
                    let due = r.wait_until_due();
                    let Some(me) = me.upgrade() else { return };
                    if due == Due::Reap {
                        for p in &me.plugins().providers {
                            p.provider.reap_idle();
                        }
                    }
                    crate::memory::release_while_hidden();
                }
            });
        });
    }

    /// The launcher is shown again: nothing to give back or stop until the next hide.
    pub fn cancel_reap(&self) {
        *sync::lock(&self.reaper.due) = Deadlines::default();
        self.reaper.wake.notify_one();
    }
}

#[derive(Default)]
struct Deadlines {
    release: Option<Instant>,
    reap: Option<Instant>,
}

#[derive(Debug, PartialEq, Eq)]
enum Due {
    Release,
    Reap,
}

#[derive(Default)]
struct Reaper {
    due: Mutex<Deadlines>,
    wake: Condvar,
    started: OnceLock<()>,
}

impl Reaper {
    /// Block until a deadline is set and has passed, clear it and say which one it was.
    fn wait_until_due(&self) -> Due {
        let mut due = sync::lock(&self.due);
        loop {
            let next = [(due.release, Due::Release), (due.reap, Due::Reap)]
                .into_iter()
                .filter_map(|(t, d)| Some((t?, d)))
                .min_by_key(|(t, _)| *t);
            match next {
                None => due = self.wake.wait(due).unwrap_or_else(PoisonError::into_inner),
                Some((t, d)) if Instant::now() >= t => {
                    match d {
                        Due::Release => due.release = None,
                        Due::Reap => due.reap = None,
                    }
                    return d;
                }
                Some((t, _)) => {
                    let left = t.saturating_duration_since(Instant::now());
                    due = self.wake.wait_timeout(due, left).unwrap_or_else(PoisonError::into_inner).0;
                }
            }
        }
    }
}

fn load_plugins(cfg: &Config, paths: &Paths, me: Weak<Engine>, prev: Option<&Registry>) -> Vec<PluginInfo> {
    let external = cfg.plugin_dirs_with(&paths.plugins).iter().flat_map(|root| manifest::discover(root)).collect();
    assemble(
        builtin::all(&builtin::Ctx { cfg, engine: me, secrets: &paths.secrets }),
        external,
        cfg,
        &paths.secrets,
        prev,
    )
}

pub fn key(i: &ResultItem) -> String {
    format!("{}:{}", i.provider, i.id)
}

/// Pick-memory key: `pick_group` plus the row's `key`.
fn pick_key(i: &ResultItem) -> String {
    pick_group(i) + &key(i)
}

/// The typed text of a pick: trimmed, lowercased, without the trigger (the provider id in
/// `pick_key` already implies it).
fn pick_group(i: &ResultItem) -> String {
    format!("{}\u{1f}", i.query.trim().to_lowercase())
}

/// `Some(path)` unless empty (tests: nothing read or written).
fn file(path: &std::path::Path) -> Option<PathBuf> {
    (!path.as_os_str().is_empty()).then(|| path.to_path_buf())
}

/// Write history files off the calling thread, so `execute` returns (and the window reacts)
/// without waiting for the disk. Writes run one at a time, and a snapshot older than the last
/// one written is dropped (the newer one already holds its launch).
fn persist(saves: Vec<crate::core::frecency::PendingSave>) {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    static WRITTEN: Mutex<u64> = Mutex::new(0);
    if saves.is_empty() {
        return;
    }
    let seq = NEXT.fetch_add(1, Ordering::SeqCst);
    std::thread::spawn(move || {
        let mut written = sync::lock(&WRITTEN);
        if seq < *written {
            return;
        }
        *written = seq;
        for save in saves {
            save.write();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    /// An icon store in a temporary folder that lives as long as the returned guard.
    fn test_store() -> (tempfile::TempDir, IconStore) {
        let tmp = crate::test_support::tmp();
        let store = IconStore::new(tmp.path().join("icons"));
        (tmp, store)
    }

    /// Records the `query` its `execute` receives.
    struct Recorder(Mutex<String>);
    impl Provider for Recorder {
        fn id(&self) -> &'static str {
            "rec"
        }
        fn query(&self, q: &Query) -> Vec<ResultItem> {
            let item = format!(r#"{{"id":"1","title":"{}","actions":[{{"id":"go","title":"Go"}}]}}"#, q.text);
            vec![serde_json::from_str(&item).unwrap()]
        }
        fn execute(&self, _: &str, _: &str, query: &str) -> plugin::Result<ExecOutcome> {
            *self.0.lock().unwrap() = query.to_string();
            Ok(ExecOutcome::Close)
        }
    }

    struct Fake(&'static str);
    impl Provider for Fake {
        fn id(&self) -> &str {
            self.0
        }
        /// Returns one result whose title is the query text it received.
        fn query(&self, q: &Query) -> Vec<ResultItem> {
            let item = format!(r#"{{"id":"1","title":"{}"}}"#, q.text);
            vec![serde_json::from_str(&item).unwrap()]
        }
        fn execute(&self, _: &str, _: &str, _: &str) -> plugin::Result<ExecOutcome> {
            Ok(ExecOutcome::Close)
        }
    }

    struct Slow(&'static str, u64);
    impl Provider for Slow {
        fn id(&self) -> &str {
            self.0
        }
        fn query(&self, q: &Query) -> Vec<ResultItem> {
            std::thread::sleep(std::time::Duration::from_millis(self.1));
            Fake(self.0).query(q)
        }
        fn execute(&self, _: &str, _: &str, _: &str) -> plugin::Result<ExecOutcome> {
            Ok(ExecOutcome::Close)
        }
    }

    fn manifest(id: &str, exec: bool) -> Manifest {
        let run = if exec { "[run]\nexec = [\"x\"]\n" } else { "" };
        Manifest::parse(&format!("id = \"{id}\"\nname = \"{id}\"\nversion = \"1\"\n{run}"), "test").unwrap()
    }

    fn bundled(id: &'static str) -> Bundled {
        Bundled { manifest: manifest(id, false), provider: Arc::new(Fake(id)), settings: serde_json::json!({}) }
    }

    #[test]
    fn fallback_rows_rank_last_without_boost_and_keep_their_place() {
        let e = Engine::with_paths(&Config::default(), Paths::default());
        let row = |id: &str, score: f64, fallback: bool| {
            let mut r = ResultItem::new(id, id, "", score);
            r.provider = "p".into();
            r.fallback = fallback;
            r
        };
        // Heavy use of the fallback row must not lift it above real results.
        for _ in 0..50 {
            let _ = sync::lock(&e.frecency).record("p:ask");
        }
        let jobs = vec![vec![row("ask", 0.0, true), row("a", 0.2, false), row("b", 0.9, false), row("c", 0.5, false)]];
        let ids = |v: Vec<ResultItem>| v.into_iter().map(|r| r.id).collect::<Vec<_>>();
        assert_eq!(ids(e.rank(&jobs, &[], 8)), ["b", "c", "a", "ask"]);
        assert_eq!(ids(e.rank(&jobs, &[], 3)), ["b", "c", "ask"], "normal rows make room");
        let ask = e.rank(&jobs, &[], 8).pop().unwrap();
        assert!(ask.score.abs() < f64::EPSILON, "no frecency boost: {}", ask.score);
    }

    #[test]
    fn the_row_last_picked_for_a_text_ranks_first_for_that_text() {
        let e = Engine::with_paths(&Config::default(), Paths::default());
        let row = |id: &str, score: f64, query: &str| {
            let mut r = ResultItem::new(id, id, "", score);
            r.provider = "p".into();
            r.query = query.into();
            r
        };
        let ids = |v: Vec<ResultItem>| v.into_iter().map(|r| r.id).collect::<Vec<_>>();
        let jobs = |q: &str| vec![vec![row("a", 0.9, q), row("b", 0.6, q), row("c", 0.3, q)]];
        let pick = |id: &str, q: &str| {
            let r = row(id, 0.0, q);
            let _ = sync::lock(&e.picks).record_only(&pick_key(&r), &pick_group(&r));
        };
        pick("c", "fi");
        assert_eq!(ids(e.rank(&jobs("fi"), &[], 8)), ["c", "a", "b"]);
        assert_eq!(ids(e.rank(&jobs(" FI "), &[], 8)), ["c", "a", "b"], "trimmed, case-insensitive");
        assert_eq!(ids(e.rank(&jobs("fir"), &[], 8)), ["a", "b", "c"], "other texts unaffected");
        pick("b", "fi");
        assert_eq!(ids(e.rank(&jobs("fi"), &[], 8)), ["b", "a", "c"], "the latest pick replaces the old one");
    }

    #[test]
    fn global_ca_file_applies_to_remote_plugins_unless_overridden() {
        let ca = std::path::Path::new("/ca/all.pem");
        let mut s = serde_json::json!({});
        with_global_ca(&mut s, true, Some(ca));
        assert_eq!(s["ca_file"], "/ca/all.pem");
        let mut own = serde_json::json!({"ca_file": "/ca/own.pem"});
        with_global_ca(&mut own, true, Some(ca));
        assert_eq!(own["ca_file"], "/ca/own.pem");
        let mut local = serde_json::json!({});
        with_global_ca(&mut local, false, Some(ca));
        assert!(local.get("ca_file").is_none());
    }

    #[test]
    fn global_requires_filters_plain_searches() {
        assert!(requires_ok(None, "firefox"));
        assert!(requires_ok(Some("0123456789"), "5 km to miles"));
        assert!(!requires_ok(Some("0123456789"), "firefox"));
        assert!(!requires_ok(Some(""), "anything"));
    }

    /// The echo example made global with `global_requires`: plain searches without a digit
    /// never reach it (so an on-demand plugin is never started for them).
    #[test]
    fn global_requires_skips_external_plugin() {
        if !crate::test_support::python3() {
            return;
        }
        let tmp_root = crate::test_support::tmp();
        let root = tmp_root.path().to_path_buf();
        let dir = root.join("plugins/echo-py");
        std::fs::create_dir_all(&dir).unwrap();
        let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/plugins/echo-py");
        std::fs::copy(src.join("echo.py"), dir.join("echo.py")).unwrap();
        let manifest = std::fs::read_to_string(src.join("plugin.toml")).unwrap();
        let manifest = manifest.replace("global = false", "global = true\nglobal_requires = \"0123456789\"");
        std::fs::write(dir.join("plugin.toml"), manifest).unwrap();
        let paths = Paths {
            config: root.join("config.toml"),
            plugins: root.join("plugins"),
            frecency: root.join("frecency.json"),
            picks: root.join("query_picks.json"),
            secrets: root.join("secrets"),
        };
        let e = Engine::with_paths(&Config::default(), paths);
        let echoed = |text: &str| e.query(text).iter().any(|r| r.provider == "echo-py");
        assert!(!echoed("hello"), "no digit: not asked");
        assert!(echoed("hello 5"), "digit: asked");
    }

    #[test]
    fn bundled_wins_duplicate_ids_and_external_is_added() {
        let ext = vec![
            (PathBuf::from("/p/a"), Ok(manifest("apps", true))),
            (PathBuf::from("/p/b"), Ok(manifest("ext", true))),
            (PathBuf::from("/p/c"), Err("bad manifest".to_string())),
        ];
        let list = assemble(vec![bundled("apps")], ext, &Config::default(), std::path::Path::new(""), None);
        let ids: Vec<_> = list.iter().map(|p| (p.manifest.id.as_str(), p.kind)).collect();
        assert_eq!(ids, [("apps", Kind::Bundled), ("ext", Kind::External)]);
    }

    #[test]
    fn assemble_applies_the_global_ca_to_remote_plugins_only() {
        let remote = |id: &str| {
            Manifest::parse(
                &format!("id = \"{id}\"\nname = \"{id}\"\nversion = \"1\"\n[run]\nurl = \"https://example.com/\"\n"),
                "test",
            )
            .unwrap()
        };
        let cfg: Config =
            toml::from_str("ca_file = \"/ca/all.pem\"\n[plugins.own]\nca_file = \"/ca/own.pem\"\n").unwrap();
        let ext = vec![
            (PathBuf::from("/p/r"), Ok(remote("r"))),
            (PathBuf::from("/p/own"), Ok(remote("own"))),
            (PathBuf::from("/p/l"), Ok(manifest("l", true))),
        ];
        let list = assemble(vec![], ext, &cfg, std::path::Path::new(""), None);
        let ca = |id: &str| list.iter().find(|p| p.manifest.id == id).unwrap().settings.get("ca_file").cloned();
        assert_eq!(ca("r"), Some("/ca/all.pem".into()));
        assert_eq!(ca("own"), Some("/ca/own.pem".into()));
        assert_eq!(ca("l"), None);
    }

    #[test]
    fn disabled_plugins_are_listed_but_flagged() {
        let cfg: Config = toml::from_str("[plugins.apps]\nenabled = false\n").unwrap();
        let list = assemble(vec![bundled("apps"), bundled("other")], vec![], &cfg, std::path::Path::new(""), None);
        assert_eq!(list.iter().map(|p| p.enabled).collect::<Vec<_>>(), [false, true]);
    }

    fn info(id: &'static str, extra: &str) -> PluginInfo {
        let m = Manifest::parse(&format!("id = \"{id}\"\nname = \"{id}\"\nversion = \"1\"\n{extra}"), "test").unwrap();
        PluginInfo {
            manifest: m,
            kind: Kind::Bundled,
            enabled: true,
            provider: Arc::new(Fake(id)),
            dir: None,
            settings: serde_json::json!({}),
        }
    }

    /// A slow provider must not delay the fast one: the first update has only the fast
    /// results, the second has both, and returning false stops further updates.
    #[test]
    fn results_are_published_progressively() {
        let mut slow = info("slow", "");
        slow.provider = Arc::new(Slow("slow", 300));
        let (_icons, icons) = test_store();
        let e = Engine::from_plugins(vec![info("fast", ""), slow], Frecency::load(None), 8, icons);

        let t = Instant::now();
        let mut updates: Vec<(Duration, Vec<String>)> = vec![];
        e.query_stream("hi", |items| {
            updates.push((t.elapsed(), items.into_iter().map(|r| r.provider).collect()));
            true
        });
        assert_eq!(updates.len(), 2);
        assert_eq!(updates[0].1, ["fast"]);
        assert!(updates[0].0 < Duration::from_millis(200));
        assert_eq!(updates[1].1.len(), 2);

        let mut n = 0;
        e.query_stream("hi", |_| {
            n += 1;
            false
        });
        assert_eq!(n, 1);
    }

    #[test]
    fn trigger_routes_to_one_plugin_global_queries_go_to_all_global_ones() {
        let plugins = vec![
            info("a-global", ""),
            info("b-trig", "[query]\ntriggers = [\"b \"]\nglobal = false\n"),
            info("c-global", ""),
        ];
        let (_icons, icons) = test_store();
        let e = Engine::from_plugins(plugins, Frecency::load(None), 8, icons);

        let mut hits: Vec<_> = e.query("b  hello").into_iter().map(|r| (r.provider, r.title)).collect();
        hits.sort();
        assert_eq!(hits, [("b-trig".to_string(), "hello".to_string())], "prefix stripped, one plugin only");

        let mut hits: Vec<_> = e.query("hello").into_iter().map(|r| r.provider).collect();
        hits.sort();
        assert_eq!(hits, ["a-global", "c-global"], "non-global plugin excluded from plain search");

        assert!(e.query("   ").is_empty());
    }

    #[test]
    fn execute_receives_the_trigger_stripped_query() {
        let rec = Arc::new(Recorder(Mutex::new(String::new())));
        let m = Manifest::parse("id = \"rec\"\nname = \"r\"\nversion = \"1\"\n[query]\ntriggers = [\"= \"]\n", "t")
            .unwrap();
        let plugins = vec![PluginInfo {
            manifest: m,
            kind: Kind::Bundled,
            enabled: true,
            provider: rec.clone(),
            dir: None,
            settings: serde_json::json!({}),
        }];
        let (_icons, icons) = test_store();
        let e = Engine::from_plugins(plugins, Frecency::load(None), 8, icons);
        let item = e.query("= 2+2").remove(0);
        assert_eq!(item.query, "2+2");
        e.execute(&item, "go", &ExecCtx::none()).unwrap();
        assert_eq!(*rec.0.lock().unwrap(), "2+2");
    }

    fn with_description(id: &'static str, triggers: &str, description: &str) -> PluginInfo {
        let m = Manifest::parse(
            &format!("id = \"{id}\"\nname = \"Name {id}\"\nversion = \"1\"\ndescription = \"{description}\"\n[query]\ntriggers = [{triggers}]\nglobal = false\n"),
            "test",
        )
        .unwrap();
        PluginInfo {
            manifest: m,
            kind: Kind::External,
            enabled: true,
            provider: Arc::new(Fake(id)),
            dir: None,
            settings: serde_json::json!({}),
        }
    }

    #[test]
    fn typing_a_command_word_suggests_it_before_the_space() {
        let (_icons, icons) = test_store();
        let e = Engine::from_plugins(
            vec![
                with_description("p-kill", "\"kill \"", "Stop processes"),
                with_description("p-echo", "\"echo \"", "Echo"),
            ],
            Frecency::load(None),
            8,
            icons,
        );
        for typed in ["kill", "ki", "K", "KILL"] {
            let r = e.query(typed);
            let s =
                r.iter().find(|r| r.provider == SUGGEST_PROVIDER).unwrap_or_else(|| panic!("{typed}: no suggestion"));
            assert_eq!((s.title.as_str(), s.id.as_str()), ("kill", "kill "), "{typed}");
            assert_eq!(s.complete.as_deref(), Some("kill "), "{typed}: Tab must complete, not open a menu");
            assert!(s.subtitle.contains("Stop processes"), "{}", s.subtitle);
            assert!(r.iter().all(|x| x.title != "echo"), "{typed}: unrelated command offered");
        }
        // Once the space is typed the plugin itself answers; no suggestion is shown.
        assert!(e.query("kill ").iter().all(|r| r.provider != SUGGEST_PROVIDER));
        assert!(e.query("zzz").is_empty());
        // The exact word ranks first.
        assert_eq!(e.query("kill")[0].provider, SUGGEST_PROVIDER);
    }

    #[test]
    fn choosing_a_suggestion_completes_the_text_and_keeps_the_window_open() {
        let (_icons, icons) = test_store();
        let e =
            Engine::from_plugins(vec![with_description("p-kill", "\"kill \"", "Stop")], Frecency::load(None), 8, icons);
        let item = e.query("ki").remove(0);
        let out = e.execute(&item, "complete", &ExecCtx::none()).unwrap();
        assert!(out.keeps_open());
        assert_eq!(out.query(), Some("kill "));
    }
}
