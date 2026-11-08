//! Bundled plugins: compiled into the launcher and run in-process, but described by the
//! same `plugin.toml` manifest schema as external plugins so they list, configure and
//! disable the same way. See docs/develop/bundled-plugins.md.

pub mod actions;
pub mod apps;
pub mod files;
pub mod plugins;
pub mod processes;
pub mod scripts;
pub mod shell;
pub mod ssh;
pub mod system;
#[cfg(feature = "remote")]
pub mod update;
pub mod web;
pub mod windows;

use std::sync::{Arc, Weak};

use serde_json::Value;

use crate::config::Config;
use crate::engine::Engine;
use crate::plugin::Provider;
use crate::plugin::manifest::Manifest;

pub struct Bundled {
    pub manifest: Manifest,
    pub provider: Arc<dyn Provider>,
    /// Effective settings (manifest defaults + `config.toml`) the provider was built with.
    pub settings: Value,
}

/// What bundled plugins are built from.
pub struct Ctx<'a> {
    pub cfg: &'a Config,
    /// The engine itself, for plugins that manage it. Dangling in tests.
    pub engine: Weak<Engine>,
    /// Where `secret` settings are stored (empty in tests: none).
    pub secrets: &'a std::path::Path,
}

/// Every bundled plugin. To add one: create `builtin/<name>/` with a `plugin.toml` and a
/// `Provider` impl, then register it here. The constructor gets the plugin's effective
/// settings and may adjust its manifest (e.g. triggers that come from settings).
pub fn all(ctx: &Ctx) -> Vec<Bundled> {
    vec![
        bundle(ctx, include_str!("apps/plugin.toml"), "builtin/apps/plugin.toml", |_, _| {
            Arc::new(apps::AppsProvider::new())
        }),
        bundle(ctx, include_str!("files/plugin.toml"), "builtin/files/plugin.toml", |_, s| {
            Arc::new(files::FilesProvider::new(s))
        }),
        bundle(ctx, include_str!("shell/plugin.toml"), "builtin/shell/plugin.toml", |_, s| {
            Arc::new(shell::ShellProvider::new(s))
        }),
        bundle(ctx, include_str!("ssh/plugin.toml"), "builtin/ssh/plugin.toml", |m, s| {
            // Without an ssh client the plugin offers nothing, not even its command.
            if !ssh::available() {
                m.query.triggers.clear();
            }
            Arc::new(ssh::SshProvider::new(s))
        }),
        bundle(ctx, include_str!("system/plugin.toml"), "builtin/system/plugin.toml", |_, s| {
            Arc::new(system::SystemProvider::new(s))
        }),
        bundle(ctx, include_str!("processes/plugin.toml"), "builtin/processes/plugin.toml", |_, _| {
            Arc::new(processes::ProcessesProvider::new())
        }),
        bundle(ctx, include_str!("windows/plugin.toml"), "builtin/windows/plugin.toml", |_, _| {
            Arc::new(windows::WindowsProvider::new())
        }),
        // Before web, so a user keyword like `g` wins over the web engine's.
        bundle(ctx, include_str!("actions/plugin.toml"), "builtin/actions/plugin.toml", |m, s| {
            let p = actions::ActionsProvider::new(s);
            m.query.triggers = p.triggers();
            Arc::new(p)
        }),
        bundle(ctx, include_str!("scripts/plugin.toml"), "builtin/scripts/plugin.toml", |_, s| {
            Arc::new(scripts::ScriptsProvider::new(s))
        }),
        bundle(ctx, include_str!("web/plugin.toml"), "builtin/web/plugin.toml", |m, s| {
            let p = web::WebProvider::new(s);
            m.query.triggers = p.triggers();
            Arc::new(p)
        }),
        bundle(ctx, include_str!("plugins/plugin.toml"), "builtin/plugins/plugin.toml", |_, _| {
            Arc::new(plugins::PluginManager::new(ctx.engine.clone()))
        }),
        #[cfg(feature = "remote")]
        bundle(ctx, include_str!("update/plugin.toml"), "builtin/update/plugin.toml", |_, s| {
            Arc::new(update::UpdateProvider::new(s))
        }),
    ]
}

fn bundle(
    ctx: &Ctx,
    manifest: &str,
    origin: &str,
    make: impl FnOnce(&mut Manifest, &Value) -> Arc<dyn Provider>,
) -> Bundled {
    // Embedded at compile time, so a parse failure is a programming error caught by tests.
    let mut manifest = Manifest::parse(manifest, origin).unwrap_or_else(|e| panic!("bundled manifest: {e}"));
    let settings = manifest.effective_settings_with(
        ctx.cfg.plugin_settings(&manifest.id),
        &|k| crate::plugin::secrets::setting_in(ctx.secrets, &manifest.id, k),
        &|k| crate::plugin::secrets::table_in(ctx.secrets, &manifest.id, k),
    );
    let provider = make(&mut manifest, &settings);
    debug_assert_eq!(manifest.id, provider.id(), "manifest id must match Provider::id");
    Bundled { manifest, provider, settings }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_manifests_parse_and_match_providers() {
        let all = all(&Ctx { cfg: &Config::default(), engine: Weak::new(), secrets: std::path::Path::new("") });
        assert!(!all.is_empty());
        for b in &all {
            assert_eq!(b.manifest.id, b.provider.id());
            assert!(b.manifest.id.starts_with(crate::plugin::manifest::BUILTIN_PREFIX), "{}", b.manifest.id);
            assert!(b.manifest.run.argv().is_none(), "bundled plugins run in-process");
        }
        let mut ids: Vec<_> = all.iter().map(|b| b.manifest.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), all.len(), "duplicate bundled ids");
    }
}
