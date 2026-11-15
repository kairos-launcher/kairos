//! The launcher's own options applied while it runs: at start, after the settings page saves,
//! and when `config.toml` changes on disk. Each takes effect at once; only what changed is
//! redone (re-grabbing the hotkey, adding or removing the tray icon, autostart).

use std::sync::atomic::Ordering;

use global_hotkey::GlobalHotKeyManager;
use global_hotkey::hotkey::HotKey;
use slint::ComponentHandle;

use super::{App, MainWindow, Theme, is_wayland, theme_mode_index, with_state};
use crate::config::Config;

/// What was applied last, to redo only what changed.
pub(super) struct Applied {
    hotkey: String,
    pub(super) tray: bool,
    start_on_boot: Option<bool>,
}

/// The global hotkey: its manager (kept for the whole run) and the key it holds.
pub(super) struct Hotkey {
    manager: GlobalHotKeyManager,
    key: Option<HotKey>,
}

impl App {
    /// Apply `cfg`'s options that live in the UI. Returns what did not work, worded for the
    /// user (e.g. a hotkey another app holds).
    pub(super) fn apply_config(&self, ui: &MainWindow, cfg: &Config) -> Vec<String> {
        ui.global::<Theme>().set_mode(theme_mode_index(cfg.theme));
        let before = with_state(|s| {
            s.placement = cfg.position;
            s.animations = cfg.animations;
            s.applied.replace(Applied { hotkey: cfg.hotkey.clone(), tray: cfg.tray, start_on_boot: cfg.start_on_boot })
        });
        let mut problems = vec![];
        if before.as_ref().is_none_or(|b| b.hotkey != cfg.hotkey)
            && let Err(e) = self.set_hotkey(&cfg.hotkey)
        {
            problems.push(e);
        }
        if before.as_ref().is_none_or(|b| b.tray != cfg.tray) {
            if cfg.tray {
                self.spawn_tray();
            } else {
                with_state(|s| s.tray = None);
            }
        }
        // At start `main` has applied it already.
        if before.is_some_and(|b| b.start_on_boot != cfg.start_on_boot) {
            crate::autostart::apply(cfg.start_on_boot);
        }
        problems
    }

    /// Grab `spec` as the global hotkey instead of the current one. Wayland forbids app-level
    /// key grabs; there the control socket is the trigger (`kairos --install-shortcut` binds
    /// it in the desktop).
    fn set_hotkey(&self, spec: &str) -> Result<(), String> {
        if is_wayland() {
            log::info!("Wayland: bind `kairos --toggle` to {spec} (see `kairos --install-shortcut`).");
            return Ok(());
        }
        let key: HotKey = spec.parse().map_err(|e| format!("hotkey {spec:?}: {e}"))?;
        with_state(|s| -> Result<(), String> {
            if s.hotkey.is_none() {
                let manager = GlobalHotKeyManager::new().map_err(|e| format!("hotkey: {e}"))?;
                s.hotkey = Some(Hotkey { manager, key: None });
            }
            let Some(h) = s.hotkey.as_mut() else { return Ok(()) };
            if h.key == Some(key) {
                return Ok(());
            }
            // The new one first: if it cannot be had, the old one keeps working.
            h.manager.register(key).map_err(|e| {
                format!("cannot grab {spec:?} (another app may own it): {e}; `kairos --toggle` still works")
            })?;
            if let Some(old) = h.key.replace(key) {
                let _ = h.manager.unregister(old);
            }
            Ok(())
        })?;
        self.shared.hotkey_id.store(key.id(), Ordering::SeqCst);
        Ok(())
    }
}
