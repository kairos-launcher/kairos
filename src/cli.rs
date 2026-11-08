//! Command-line flags. One table drives both parsing and `--help`, so they cannot drift.

use crate::config::Config;
use crate::engine::{self, Engine};
use crate::{autostart, ipc, shortcut_install};

/// What the process was asked to do.
#[derive(Debug, PartialEq, Eq)]
pub enum Cli {
    /// Start the resident launcher (hidden with `--background`).
    Run {
        background: bool,
    },
    /// Tell the running instance to toggle/show/hide/quit (show may carry search text).
    Send(ipc::Request),
    ListPlugins,
    /// Store a remote plugin's API token, read from stdin.
    SetToken(String),
    InstallAutostart,
    UninstallAutostart,
    InstallShortcut,
    UninstallShortcut,
    Help,
}

struct Flag {
    name: &'static str,
    /// Name of the flag's value, if it takes one.
    value: Option<&'static str>,
    help: &'static str,
}

const FLAGS: &[Flag] = &[
    Flag { name: "--background", value: None, help: "start hidden (used by autostart)" },
    Flag { name: "--toggle", value: None, help: "show or hide the running launcher" },
    Flag { name: "--show", value: None, help: "show the running launcher" },
    Flag { name: "--hide", value: None, help: "hide the running launcher" },
    Flag { name: "--quit", value: None, help: "stop the running launcher" },
    Flag { name: "--query", value: Some("<text>"), help: "show the running launcher with <text> in the search box" },
    Flag { name: "--list-plugins", value: None, help: "list plugins with their state and triggers" },
    Flag { name: "--set-token", value: Some("<plugin>"), help: "store a remote plugin's API token (read from stdin)" },
    Flag { name: "--install-shortcut", value: None, help: "bind the hotkey to `kairos --toggle` in the desktop" },
    Flag { name: "--uninstall-shortcut", value: None, help: "remove that binding" },
    Flag { name: "--install-autostart", value: None, help: "start hidden at sign-in" },
    Flag { name: "--uninstall-autostart", value: None, help: "do not start at sign-in" },
    Flag { name: "--help", value: None, help: "show this help" },
];

/// Parse the arguments after the program name.
pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Cli, String> {
    let mut args = args.into_iter();
    let Some(flag) = args.next() else { return Ok(Cli::Run { background: false }) };
    let mut value = || args.next().ok_or_else(|| format!("{flag} needs a value"));
    let cli = match flag.as_str() {
        "--background" => Cli::Run { background: true },
        "--toggle" => Cli::Send(ipc::Request::new(ipc::Command::Toggle)),
        "--show" => Cli::Send(ipc::Request::new(ipc::Command::Show)),
        "--hide" => Cli::Send(ipc::Request::new(ipc::Command::Hide)),
        "--quit" => Cli::Send(ipc::Request::new(ipc::Command::Quit)),
        "--query" => Cli::Send(ipc::Request { query: Some(value()?), ..ipc::Request::new(ipc::Command::Show) }),
        "--list-plugins" => Cli::ListPlugins,
        "--set-token" => Cli::SetToken(value()?),
        "--install-shortcut" => Cli::InstallShortcut,
        "--uninstall-shortcut" => Cli::UninstallShortcut,
        "--install-autostart" => Cli::InstallAutostart,
        "--uninstall-autostart" => Cli::UninstallAutostart,
        "-h" | "--help" => Cli::Help,
        other => return Err(format!("unknown argument {other:?}")),
    };
    match args.next() {
        Some(extra) => Err(format!("unexpected argument {extra:?} after {flag}")),
        None => Ok(cli),
    }
}

pub fn help() -> String {
    let usage = |f: &Flag| match f.value {
        Some(v) => format!("{} {v}", f.name),
        None => f.name.to_string(),
    };
    let width = FLAGS.iter().map(|f| usage(f).len()).max().unwrap_or(0);
    let mut out = String::from(
        "Usage: kairos [FLAG]\n\nWith no flag, starts the resident launcher with its window open.\n\nFlags:\n",
    );
    for f in FLAGS {
        out += &format!("  {:<width$}  {}\n", usage(f), f.help);
    }
    out
}

/// Run a one-shot command (everything except `Run`).
pub fn run(cli: Cli, cfg: &Config) -> Result<(), String> {
    match cli {
        Cli::Run { .. } => Ok(()),
        Cli::Send(req) => {
            ipc::send(&req).map_err(|e| format!("cannot reach a running launcher ({e}); start `kairos` first"))
        }
        Cli::ListPlugins => {
            list_plugins(cfg);
            Ok(())
        }
        Cli::SetToken(id) => set_token(&id),
        Cli::InstallAutostart => autostart::install(),
        Cli::UninstallAutostart => autostart::uninstall(),
        Cli::InstallShortcut => shortcut_install::install(&cfg.hotkey),
        Cli::UninstallShortcut => shortcut_install::uninstall(),
        Cli::Help => {
            print!("{}", help());
            Ok(())
        }
    }
}

/// Read the API token from stdin and store it privately. Piped in so it never lands in shell
/// history: `read -rs T; printf %s "$T" | kairos --set-token jira`.
fn set_token(id: &str) -> Result<(), String> {
    let mut token = String::new();
    std::io::stdin().read_line(&mut token).map_err(|e| e.to_string())?;
    if token.trim().is_empty() {
        return Err("no token on stdin".into());
    }
    let path = crate::plugin::secrets::write(id, None, &token)?;
    println!("token saved to {}", path.display());
    Ok(())
}

fn list_plugins(cfg: &Config) {
    let engine = Engine::new(cfg);
    println!("{:<28} {:<10} {:<9} {:<8} TRIGGERS", "ID", "KIND", "STATE", "VERSION");
    for p in &engine.plugins().plugins {
        let kind = match p.kind {
            engine::Kind::Bundled => "bundled",
            engine::Kind::External => "external",
        };
        let triggers =
            if p.manifest.query.triggers.is_empty() { "-".to_string() } else { p.manifest.query.triggers.join(" ") };
        let state = if p.enabled { "enabled" } else { "disabled" };
        println!("{:<28} {kind:<10} {state:<9} {:<8} {triggers}", p.manifest.id, p.manifest.version);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_args(args: &[&str]) -> Result<Cli, String> {
        parse(args.iter().map(ToString::to_string))
    }

    #[test]
    fn every_flag_parses() {
        assert_eq!(parse_args(&[]), Ok(Cli::Run { background: false }));
        assert_eq!(parse_args(&["--background"]), Ok(Cli::Run { background: true }));
        assert_eq!(parse_args(&["--toggle"]), Ok(Cli::Send(ipc::Request::new(ipc::Command::Toggle))));
        assert_eq!(parse_args(&["--quit"]), Ok(Cli::Send(ipc::Request::new(ipc::Command::Quit))));
        let show = ipc::Request { query: Some("f report".into()), ..ipc::Request::new(ipc::Command::Show) };
        assert_eq!(parse_args(&["--query", "f report"]), Ok(Cli::Send(show)));
        assert_eq!(parse_args(&["--set-token", "jira"]), Ok(Cli::SetToken("jira".into())));
        assert_eq!(parse_args(&["-h"]), Ok(Cli::Help));
        for f in FLAGS {
            let mut args = vec![f.name];
            if f.value.is_some() {
                args.push("x");
            }
            assert!(parse_args(&args).is_ok(), "{} is in the help but not parsed", f.name);
        }
    }

    #[test]
    fn mistakes_are_errors() {
        assert!(parse_args(&["--bogus"]).unwrap_err().contains("--bogus"));
        assert!(parse_args(&["--set-token"]).unwrap_err().contains("needs a value"));
        assert!(parse_args(&["--toggle", "junk"]).unwrap_err().contains("junk"));
    }

    #[test]
    fn help_lists_every_flag() {
        let h = help();
        assert!(FLAGS.iter().all(|f| h.contains(f.name)));
        assert!(h.contains("--set-token <plugin>"));
    }
}
