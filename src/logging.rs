//! A minimal stderr logger for the `log` facade.
//!
//! The level comes from `KAIROS_LOG` (`error`, `warn`, `info`, `debug`, `trace`, `off`);
//! `KAIROS_DEBUG` (the older switch) means `debug`. The default is `info`.
//!
//! The clipboard library warns on every start under GNOME (no Wayland clipboard protocol, so it
//! falls back to X11, which works); below `debug` only its errors are shown.

use std::io::Write;

use log::{LevelFilter, Log, Metadata, Record};

struct StderrLogger;

static LOGGER: StderrLogger = StderrLogger;

impl Log for StderrLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        shown(metadata.target(), metadata.level(), log::max_level())
    }

    fn log(&self, record: &Record<'_>) {
        if self.enabled(record.metadata()) {
            let _ = writeln!(std::io::stderr().lock(), "{:<5} {}", record.level(), record.args());
        }
    }

    fn flush(&self) {}
}

pub fn init() {
    let level = level(std::env::var("KAIROS_LOG").ok().as_deref(), std::env::var_os("KAIROS_DEBUG").is_some());
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(level);
    }
}

fn shown(target: &str, level: log::Level, max: LevelFilter) -> bool {
    let quiet = target.starts_with("arboard") && max < LevelFilter::Debug;
    level <= if quiet { LevelFilter::Error } else { max }
}

fn level(setting: Option<&str>, debug: bool) -> LevelFilter {
    match setting.and_then(|s| s.trim().parse().ok()) {
        Some(level) => level,
        None if debug => LevelFilter::Debug,
        None => LevelFilter::Info,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipboard_warnings_need_debug() {
        use log::Level;
        assert!(!shown("arboard::platform::linux", Level::Warn, LevelFilter::Info));
        assert!(shown("arboard::platform::linux", Level::Error, LevelFilter::Info));
        assert!(shown("arboard::platform::linux", Level::Warn, LevelFilter::Debug));
        assert!(shown("kairos::ui", Level::Warn, LevelFilter::Info));
        assert!(!shown("kairos::ui", Level::Debug, LevelFilter::Info));
    }

    #[test]
    fn level_comes_from_the_environment() {
        assert_eq!(level(None, false), LevelFilter::Info);
        assert_eq!(level(None, true), LevelFilter::Debug);
        assert_eq!(level(Some("warn"), true), LevelFilter::Warn, "KAIROS_LOG wins");
        assert_eq!(level(Some("OFF"), false), LevelFilter::Off);
        assert_eq!(level(Some("nonsense"), false), LevelFilter::Info);
    }
}
