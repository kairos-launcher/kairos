#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
//! A fast, low-memory application launcher. See README.md and docs/.

mod app_icon;
mod autostart;
mod builtin;
mod cli;
mod config;
mod core;
mod drag;
mod engine;
mod icon_store;
mod ipc;
mod logging;
mod memory;
mod plugin;
mod screen;
mod shortcut_install;
#[cfg(test)]
mod test_support;
mod theme;
mod tray;
mod ui;

use std::process::ExitCode;

use cli::Cli;

fn main() -> ExitCode {
    logging::init();
    let cli = match cli::parse(std::env::args().skip(1)) {
        Ok(cli) => cli,
        Err(e) => {
            eprintln!("{e} (try --help)");
            return ExitCode::FAILURE;
        }
    };
    let cfg = config::Config::load();
    let result = match cli {
        Cli::Run { background } => {
            apply_start_on_boot(&cfg);
            ui::run(&cfg, background)
        }
        other => cli::run(other, &cfg),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

/// `start_on_boot` in config.toml registers or removes autostart on every start.
fn apply_start_on_boot(cfg: &config::Config) {
    let Some(on) = cfg.start_on_boot else { return };
    let result = if on { autostart::install() } else { autostart::uninstall() };
    if let Err(e) = result {
        log::warn!("start_on_boot: {e}");
    }
}
