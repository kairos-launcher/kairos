//! Starting other programs: open files and URLs, find programs on PATH, run in a terminal.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Start `argv` detached from the launcher (no stdio) and reap it when it exits.
pub fn spawn(argv: &[String]) -> Result<(), String> {
    let (prog, args) = argv.split_first().ok_or("empty command")?;
    let mut cmd = Command::new(prog);
    cmd.args(args);
    run_detached(cmd, prog)
}

/// Like [`spawn`], with extra environment variables.
pub fn spawn_env(argv: &[String], env: &[(&str, &str)]) -> Result<(), String> {
    let (prog, args) = argv.split_first().ok_or("empty command")?;
    let mut cmd = Command::new(prog);
    cmd.args(args);
    run_detached_with(cmd, prog, env)
}

/// Like [`spawn`], in the folder `cwd`.
pub fn spawn_in(argv: &[String], cwd: &Path) -> Result<(), String> {
    let (prog, args) = argv.split_first().ok_or("empty command")?;
    let mut cmd = Command::new(prog);
    cmd.args(args).current_dir(cwd);
    run_detached(cmd, prog)
}

/// Run a shell command line (`sh -c`, or `cmd /C` on Windows) in the background.
pub fn spawn_shell(line: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Hand cmd the line as written: Rust's argument quoting would turn `"` into `\"`,
        // which cmd does not understand. `/S` makes cmd strip exactly the outer quotes.
        let mut cmd = Command::new("cmd");
        cmd.raw_arg(format!("/D /S /C \"{line}\""));
        run_detached(cmd, "cmd")
    }
    #[cfg(not(windows))]
    {
        spawn(&shell_argv(line))
    }
}

fn run_detached(cmd: Command, prog: &str) -> Result<(), String> {
    run_detached_with(cmd, prog, &[])
}

/// `env` is set after scrubbing, so it may name a scrubbed variable.
fn run_detached_with(mut cmd: Command, prog: &str, env: &[(&str, &str)]) -> Result<(), String> {
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    scrub_env(&mut cmd);
    cmd.envs(env.iter().copied());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = cmd.spawn().map_err(|e| format!("{prog}: {e}"))?;
    // Reap the child when it exits so it does not linger as a zombie.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Join arguments into a Windows command line that a program splits back into the same
/// arguments (the `CommandLineToArgvW` rules).
pub fn join_win_args(args: &[String]) -> String {
    args.iter().map(|a| win_quote(a)).collect::<Vec<_>>().join(" ")
}

fn win_quote(a: &str) -> String {
    if !a.is_empty() && !a.contains([' ', '\t', '"']) {
        return a.to_string();
    }
    let mut out = String::from('"');
    let mut slashes = 0;
    for c in a.chars() {
        match c {
            '\\' => slashes += 1,
            '"' => {
                // Backslashes before a quote are doubled, and the quote is escaped.
                out.push_str(&"\\".repeat(slashes * 2 + 1));
                out.push('"');
                slashes = 0;
            }
            c => {
                out.push_str(&"\\".repeat(slashes));
                out.push(c);
                slashes = 0;
            }
        }
    }
    // Trailing backslashes are doubled so they do not escape the closing quote.
    out.push_str(&"\\".repeat(slashes * 2));
    out.push('"');
    out
}

/// Open a link that came from outside (a remote plugin) in the browser. Only `https`, `http`
/// and `mailto` are allowed, so a server cannot launch `file:` paths or custom handlers.
/// On Windows the link bypasses `cmd`, where `&` and `^` would start other commands.
pub fn open_url(url: &str) -> Result<(), String> {
    check_url(url)?;
    open(url)
}

fn check_url(url: &str) -> Result<(), String> {
    let scheme = url.split_once(':').map(|(s, _)| s.to_ascii_lowercase()).unwrap_or_default();
    if !["https", "http", "mailto"].contains(&scheme.as_str()) {
        return Err(format!("refusing to open a {scheme:?} link"));
    }
    if url.len() > 4096 || url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("refusing to open a malformed link".into());
    }
    Ok(())
}

/// Open a file, folder or URL with the desktop's default application.
pub fn open(target: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        // Not `cmd /C start`: cmd would run whatever follows a `&` in a file name.
        win::shell_open(target, None)
    }
    #[cfg(not(windows))]
    {
        let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
        spawn(&[opener.into(), target.into()])
    }
}

/// Variables a launched program must not inherit: API tokens of remote plugins, the
/// activation token the desktop gave the launcher itself (stale, and not meant for others),
/// and the marker of a restart after an update.
pub fn scrub_env(cmd: &mut Command) {
    scrub(cmd, std::env::vars_os().map(|(k, _)| k));
}

fn scrub(cmd: &mut Command, keys: impl Iterator<Item = std::ffi::OsString>) {
    for key in keys {
        let k = key.to_string_lossy();
        if k.starts_with("KAIROS_TOKEN_")
            || k == "XDG_ACTIVATION_TOKEN"
            || k == "DESKTOP_STARTUP_ID"
            || k == "KAIROS_RESTARTED"
        {
            cmd.env_remove(&key);
        }
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod win {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    fn wide(s: &str) -> Vec<u16> {
        std::ffi::OsStr::new(s).encode_wide().chain(Some(0)).collect()
    }

    /// Open a file, folder, shortcut or URL with its default handler, without a shell;
    /// `params` is the command line passed to a program or shortcut.
    pub fn shell_open(target: &str, params: Option<&str>) -> Result<(), String> {
        let (verb, file) = (wide("open"), wide(target));
        let params = params.map(wide);
        // SAFETY: `verb`, `file` and `params` are NUL-terminated UTF-16 buffers that outlive
        // the call; the window, parameters and directory may be null.
        let r = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                file.as_ptr(),
                params.as_ref().map_or(std::ptr::null(), Vec::as_ptr),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        // ShellExecuteW reports success with a value above 32.
        let code = r as isize;
        if code > 32 { Ok(()) } else { Err(format!("cannot open {target} (error {code})")) }
    }
}

/// Open a program or shortcut with `params` as its command line (none: as [`open`]).
#[cfg(windows)]
pub fn open_with_params(target: &str, params: Option<&str>) -> Result<(), String> {
    win::shell_open(target, params)
}

/// Open `target` with a user-given command line such as `firefox --private-window`; an empty
/// command means the default application.
pub fn open_with(command: &str, target: &str) -> Result<(), String> {
    let mut argv = split_args(command);
    if argv.is_empty() {
        return open(target);
    }
    argv.push(target.into());
    spawn(&argv)
}

/// Split a command line on whitespace, honouring "double" and 'single' quotes.
pub fn split_args(s: &str) -> Vec<String> {
    let (mut out, mut cur, mut quote, mut has) = (vec![], String::new(), None, false);
    for c in s.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (None, '"' | '\'') => {
                quote = Some(c);
                has = true;
            }
            (None, c) if c.is_whitespace() => {
                if has || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                has = false;
            }
            // Inside quotes, or a plain character.
            (_, c) => cur.push(c),
        }
    }
    if has || !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Full path of `name` if it is an executable on PATH (or already a path to one).
pub fn which(name: &str) -> Option<PathBuf> {
    let p = Path::new(name);
    if p.components().count() > 1 {
        return p.is_file().then(|| p.to_path_buf());
    }
    let exts: &[&str] = if cfg!(windows) { &["", ".exe", ".cmd", ".bat"] } else { &[""] };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .flat_map(|dir| exts.iter().map(move |e| dir.join(format!("{name}{e}"))))
        .find(|c| c.is_file())
}

/// Terminal emulators tried in order, with the flag that precedes the command to run.
#[cfg(not(windows))]
const TERMINALS: [(&str, &str); 9] = [
    ("x-terminal-emulator", "-e"),
    ("kitty", ""),
    ("alacritty", "-e"),
    ("foot", ""),
    ("wezterm", "start --"),
    ("ghostty", "-e"),
    ("gnome-terminal", "--"),
    ("konsole", "-e"),
    ("xterm", "-e"),
];

/// The argv that runs `command` in a terminal window. `configured` overrides detection: a
/// terminal command line whose arguments come before the command, e.g. `kitty` or
/// `gnome-terminal --`.
pub fn terminal_argv(configured: &str, command: &[String]) -> Result<Vec<String>, String> {
    let mut argv = split_args(configured);
    if argv.is_empty() {
        argv = detect_terminal().ok_or("no terminal emulator found; set one in the plugin settings")?;
    }
    argv.extend(command.iter().cloned());
    Ok(argv)
}

#[cfg(not(windows))]
fn detect_terminal() -> Option<Vec<String>> {
    if let Some(t) = std::env::var("TERMINAL").ok().filter(|t| which(t).is_some()) {
        return Some(vec![t, "-e".into()]);
    }
    TERMINALS.iter().find(|(t, _)| which(t).is_some()).map(|(t, flag)| {
        let mut v = vec![t.to_string()];
        v.extend(flag.split_whitespace().map(String::from));
        v
    })
}

#[cfg(windows)]
#[allow(clippy::unnecessary_wraps, reason = "same signature as the Unix version")]
fn detect_terminal() -> Option<Vec<String>> {
    Some(match which("wt") {
        Some(_) => vec!["wt".into()],
        None => vec!["cmd".into(), "/C".into(), "start".into(), String::new()],
    })
}

/// Run `command` in a new terminal window.
pub fn run_in_terminal(configured: &str, command: &[String]) -> Result<(), String> {
    spawn(&terminal_argv(configured, command)?)
}

/// argv that runs a shell command line: `sh -c` (or `cmd /C`).
pub fn shell_argv(line: &str) -> Vec<String> {
    if cfg!(windows) {
        vec!["cmd".into(), "/C".into(), line.into()]
    } else {
        let sh = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "sh".into());
        vec![sh, "-c".into(), line.into()]
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_web_and_mail_links_may_be_opened() {
        for ok in ["https://example.com/a?b=1&c=2", "HTTP://x.org", "mailto:a@b.c"] {
            assert!(super::check_url(ok).is_ok(), "{ok}");
        }
        for bad in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "ms-msdt:/id",
            "-oProxy",
            "/usr/bin/x",
            "https://a b",
            "https://a\n.com",
            "",
        ] {
            assert!(super::check_url(bad).is_err(), "{bad:?}");
        }
    }

    use super::*;

    #[test]
    fn windows_arguments_round_trip() {
        let j = |a: &[&str]| join_win_args(&a.iter().map(ToString::to_string).collect::<Vec<_>>());
        assert_eq!(j(&["https://x.org?p=a%20b&c=1"]), "https://x.org?p=a%20b&c=1");
        assert_eq!(j(&["a b", ""]), r#""a b" """#);
        assert_eq!(j(&[r#"say "hi""#]), r#""say \"hi\"""#);
        assert_eq!(j(&[r"C:\dir with space\"]), r#""C:\dir with space\\""#);
        assert_eq!(j(&[r#"a\"b"#]), r#""a\\\"b""#);
        assert_eq!(j(&[r"C:\no\space"]), r"C:\no\space");
    }

    #[test]
    fn tokens_are_not_passed_to_launched_programs() {
        let mut cmd = Command::new("x");
        let keys = ["KAIROS_TOKEN_JIRA", "XDG_ACTIVATION_TOKEN", "HOME", "KAIROS_HOME"];
        scrub(&mut cmd, keys.iter().map(std::ffi::OsString::from));
        let removed: Vec<_> = cmd.get_envs().filter(|(_, v)| v.is_none()).map(|(k, _)| k.to_owned()).collect();
        assert_eq!(removed, ["KAIROS_TOKEN_JIRA", "XDG_ACTIVATION_TOKEN"]);
    }

    #[test]
    fn splits_quoted_arguments() {
        assert_eq!(split_args(r"firefox --private-window"), ["firefox", "--private-window"]);
        assert_eq!(split_args(r#"  "my browser" -a 'x y' """#), ["my browser", "-a", "x y", ""]);
        assert!(split_args("   ").is_empty());
    }

    #[test]
    fn configured_terminal_comes_first() {
        let argv = terminal_argv("gnome-terminal --", &["ssh".into(), "box".into()]).unwrap();
        assert_eq!(argv, ["gnome-terminal", "--", "ssh", "box"]);
    }

    #[test]
    fn which_finds_a_common_program() {
        let name = if cfg!(windows) { "cmd" } else { "sh" };
        assert!(which(name).is_some());
        assert!(which("surely-not-a-real-program-xyz").is_none());
    }
}
