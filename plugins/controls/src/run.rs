//! Running the helper programs the backends use (`wpctl`, `gsettings`, `osascript`, …).
//! Arguments are passed as an argv, never through a shell.

use std::process::{Command, Stdio};

fn command(argv: &[&str]) -> Option<Command> {
    let (prog, args) = argv.split_first()?;
    let mut cmd = Command::new(prog);
    cmd.args(args).stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no console flashing up
    }
    Some(cmd)
}

/// Standard output of a command that succeeded, trimmed.
pub fn output(argv: &[&str]) -> Option<String> {
    let out = command(argv)?.stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Run a command; the error names it and carries its stderr.
pub fn status(argv: &[&str]) -> Result<(), String> {
    let name = argv.first().copied().unwrap_or("");
    let out =
        command(argv).ok_or("empty command")?.stdout(Stdio::null()).output().map_err(|e| format!("{name}: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&out.stderr);
    let err = err.trim();
    Err(if err.is_empty() { format!("{name} failed ({})", out.status) } else { format!("{name}: {err}") })
}

/// Whether `prog` is on `PATH`.
pub fn has(prog: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else { return false };
    let exts: &[&str] = if cfg!(windows) { &["", ".exe", ".cmd", ".bat"] } else { &[""] };
    std::env::split_paths(&path).any(|dir| exts.iter().any(|e| dir.join(format!("{prog}{e}")).is_file()))
}

/// `XDG_CURRENT_DESKTOP` contains `name` (`ubuntu:GNOME`, `KDE`), ignoring case.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn desktop_is(name: &str) -> bool {
    std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|d| d.split(':').any(|p| p.eq_ignore_ascii_case(name)))
}

/// PowerShell, without profile or prompts, for the few Windows settings that have no simpler API.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn powershell(script: &str) -> Option<String> {
    output(&["powershell", "-NoProfile", "-NonInteractive", "-Command", script])
}
