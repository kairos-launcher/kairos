//! Script commands: scripts in a folder become launcher commands, described by comments in
//! their first lines.
//!
//! ```sh
//! #!/bin/bash
//! # @kairos.title Restart audio
//! # @kairos.mode silent          (silent | output | terminal)
//! # @kairos.description Restarts PipeWire
//! systemctl --user restart pipewire
//! ```
//!
//! Raycast's `@raycast.*` keys are read too, so many of its script commands work unchanged.
//! The folders are `<config>/scripts` plus the `folders` setting. They are read again when a file
//! in them changed, the next time the launcher opens. Typed text reaches the script as separate
//! arguments, never through a shell.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime};

use serde_json::Value;

use crate::core::sync::{lock, read, write};
use crate::core::{clipboard, fuzzy, icons, paths::expand_home, process};
use crate::plugin;
use crate::plugin::{Error, ExecCtx, ExecOutcome, Icon, OutputEvent, OutputStart, Provider, Query, ResultItem};

const ID: &str = "builtin-scripts";
/// How much of a file is read to find its header.
const HEADER_BYTES: u64 = 8 * 1024;
const HEADER_LINES: usize = 50;
/// Raycast allows three arguments.
const MAX_ARGS: usize = 3;
/// Result id of the "Open scripts folder" row; never a path.
const FOLDER_ID: &str = "\u{0}folder";
const FOLDER_TITLE: &str = "Open scripts folder";
/// Result id prefix of the "Create script" row; the rest is the file name.
const CREATE_ID: &str = "\u{0}create:";
/// What starts a new script: `scripts create restart audio`.
const CREATE_PREFIX: &str = "scripts create";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Run in the background; the launcher closes.
    Silent,
    /// Stream stdout and stderr into the output panel.
    Output,
    /// Run in a terminal window.
    Terminal,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArgSpec {
    pub placeholder: String,
    pub optional: bool,
}

#[derive(Clone, Debug)]
pub struct Script {
    pub path: PathBuf,
    pub title: String,
    /// Typed alone (or followed by the argument) it runs the script.
    pub keyword: String,
    pub subtitle: String,
    pub icon: Option<Icon>,
    pub mode: Mode,
    pub markdown: bool,
    pub args: Vec<ArgSpec>,
    pub cwd: Option<PathBuf>,
}

impl Script {
    fn takes_args(&self) -> bool {
        !self.args.is_empty()
    }

    /// The arguments for `typed`, or why it cannot run yet.
    fn argv_args(&self, typed: &str) -> Result<Vec<String>, String> {
        let typed = typed.trim();
        let mut got: Vec<String> = match self.args.len() {
            0 => vec![],
            1 if typed.is_empty() => vec![],
            1 => vec![typed.to_string()],
            n => process::split_args(typed).into_iter().take(n).collect(),
        };
        if let Some(missing) = self.args.iter().skip(got.len()).find(|a| !a.optional) {
            return Err(format!("type {}", missing.placeholder));
        }
        // Raycast passes empty strings for optional arguments that were left out.
        while got.len() < self.args.len() {
            got.push(String::new());
        }
        Ok(got)
    }
}

/// Strip a comment marker (`#`, `//`, `--`, `;`, `::`, `'`, `REM`) from a line.
fn comment_body(line: &str) -> Option<&str> {
    let l = line.trim_start();
    for p in ["//", "--", "::", "#", ";", "'"] {
        if let Some(rest) = l.strip_prefix(p) {
            return Some(rest);
        }
    }
    let lower = l.get(..4).map(str::to_ascii_lowercase);
    (lower.as_deref() == Some("rem ")).then(|| &l[4..])
}

/// `@kairos.title Foo` / `@raycast.title Foo` -> (`title`, `Foo`).
fn meta(line: &str) -> Option<(&str, &str)> {
    let body = comment_body(line)?.trim();
    let rest = body.strip_prefix("@kairos.").or_else(|| body.strip_prefix("@raycast."))?;
    let (key, value) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    Some((key, value.trim()))
}

fn parse_mode(v: &str) -> Mode {
    match v {
        "output" | "fullOutput" | "inline" | "compact" => Mode::Output,
        "terminal" => Mode::Terminal,
        _ => Mode::Silent,
    }
}

/// Raycast's `argument1 { "type": "text", "placeholder": "Query", "optional": true }`.
fn raycast_arg(v: &str) -> ArgSpec {
    let json: Value = serde_json::from_str(v).unwrap_or(Value::Null);
    ArgSpec {
        placeholder: json["placeholder"].as_str().filter(|p| !p.is_empty()).unwrap_or("an argument").to_string(),
        optional: json["optional"].as_bool().unwrap_or(false),
    }
}

/// The keyword made from a file name: `Search Jira.sh` -> `search-jira`.
fn keyword_from(path: &Path) -> String {
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
    stem.split_whitespace().collect::<Vec<_>>().join("-")
}

/// Read a script's header. `None` if it has no title (not a script command).
pub fn parse(path: &Path, text: &str) -> Option<Script> {
    let mut title = None;
    let (mut keyword, mut description, mut package, mut icon, mut cwd) = (None, None, None, None, None);
    let (mut mode, mut markdown) = (Mode::Silent, false);
    let mut args: Vec<(usize, ArgSpec)> = vec![];
    for line in text.lines().take(HEADER_LINES) {
        let Some((key, value)) = meta(line) else { continue };
        let value = value.to_string();
        match key {
            "title" if !value.is_empty() => title = Some(value),
            "keyword" if !value.is_empty() && !value.contains(char::is_whitespace) => keyword = Some(value),
            "description" => description = Some(value),
            "packageName" => package = Some(value),
            "icon" if !value.is_empty() => icon = Some(value),
            "mode" => mode = parse_mode(&value),
            "format" => markdown = value == "markdown",
            "currentDirectoryPath" if !value.is_empty() => cwd = Some(expand_home(&value)),
            "argument" => args.push((1, ArgSpec { placeholder: value, optional: false })),
            k => {
                if let Some(n) = k.strip_prefix("argument").and_then(|n| n.parse::<usize>().ok())
                    && (1..=MAX_ARGS).contains(&n)
                {
                    args.push((n, raycast_arg(&value)));
                }
            }
        }
    }
    args.sort_by_key(|(n, _)| *n);
    args.dedup_by_key(|(n, _)| *n);
    let dir = path.parent().unwrap_or(Path::new("."));
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let keyword = keyword.unwrap_or_else(|| keyword_from(path));
    let subtitle = description
        .filter(|d| !d.is_empty())
        .or_else(|| package.filter(|p| !p.is_empty()).map(|p| format!("{p} · {name}")))
        .unwrap_or_else(|| format!("{keyword} · {name}"));
    Some(Script {
        path: path.to_path_buf(),
        title: title?,
        keyword,
        subtitle,
        icon: icon.map(|i| script_icon(&i, dir)),
        mode,
        markdown,
        args: args.into_iter().map(|(_, a)| a).collect(),
        cwd: cwd.map(|c| if c.is_absolute() { c } else { dir.join(c) }),
    })
}

/// `@kairos.icon`: an image next to the script, one character, a theme icon name or the name
/// of an installed app (`firefox`).
fn script_icon(value: &str, dir: &Path) -> Icon {
    let file = dir.join(value);
    if file.is_file() {
        return Icon { path: Some(file.to_string_lossy().into_owned()), ..Default::default() };
    }
    if value.chars().count() <= 2 {
        return Icon { glyph: Some(value.to_string()), ..Default::default() };
    }
    crate::builtin::apps::named_icon(value).unwrap_or_else(|| Icon { glyph: Some("›".into()), ..Default::default() })
}

fn default_icon() -> Icon {
    icons::themed(&["application-x-executable", "system-run", "utilities-terminal"], "›")
}

/// The command that runs `path`: directly when it is executable (Unix, shebang), else through
/// the interpreter its extension names.
fn command_for(path: &Path, executable: bool) -> Vec<String> {
    let p = path.to_string_lossy().into_owned();
    let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    if cfg!(windows) {
        return match ext.as_str() {
            "ps1" => ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File"]
                .map(String::from)
                .into_iter()
                .chain([p])
                .collect(),
            "py" => vec!["python".into(), p],
            "js" => vec!["node".into(), p],
            // `.exe`, and `.bat`/`.cmd` started directly: Rust then quotes the arguments for cmd
            // safely (or refuses), where `cmd /C script.bat <text>` would let `&` in typed text
            // start other commands.
            _ => vec![p],
        };
    }
    if executable {
        return vec![p];
    }
    let interpreter = match ext.as_str() {
        "bash" => "bash",
        "zsh" => "zsh",
        "fish" => "fish",
        "py" => "python3",
        "js" | "mjs" => "node",
        "rb" => "ruby",
        "pl" => "perl",
        "ps1" => "pwsh",
        _ => "sh",
    };
    vec![interpreter.into(), p]
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(_path: &Path) -> bool {
    false
}

/// Every folder and file the last scan looked at, with its modification time.
type Seen = Vec<(PathBuf, Option<SystemTime>)>;

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// Read the script commands in `folders` (not recursive).
pub fn scan(folders: &[PathBuf]) -> (Vec<Script>, Seen) {
    let mut scripts = vec![];
    let mut seen: Seen = vec![];
    for dir in folders {
        seen.push((dir.clone(), mtime(dir)));
        if crate::core::fs::world_writable(dir) {
            log::warn!("scripts: {} can be changed by every user; ignored", dir.display());
            continue;
        }
        let Ok(rd) = std::fs::read_dir(dir) else { continue };
        let mut files: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect();
        files.sort();
        for path in files {
            let hidden = path.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.'));
            if hidden {
                continue;
            }
            seen.push((path.clone(), mtime(&path)));
            if crate::core::fs::world_writable(&path) {
                log::warn!("scripts: {} can be changed by every user; ignored", path.display());
                continue;
            }
            let mut head = Vec::new();
            let Ok(f) = std::fs::File::open(&path) else { continue };
            if f.take(HEADER_BYTES).read_to_end(&mut head).is_err() {
                continue;
            }
            if let Some(s) = parse(&path, &String::from_utf8_lossy(&head)) {
                scripts.push(s);
            }
        }
    }
    (scripts, seen)
}

/// `restart audio` -> `restart-audio.sh` (`.ps1` on Windows); a name with a known extension
/// keeps it. `None` for an empty name or one that would leave the folder.
fn file_name_for(name: &str) -> Option<String> {
    let name = name.trim();
    if name.is_empty() || name.contains(['/', '\\']) || name.starts_with('.') {
        return None;
    }
    let slug: String = name
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-")
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .collect();
    let slug = slug.trim_matches(['.', '-']).to_string();
    if slug.is_empty() {
        return None;
    }
    let known = ["sh", "bash", "zsh", "fish", "py", "js", "mjs", "rb", "pl", "ps1", "bat", "cmd"];
    let has_ext = Path::new(&slug).extension().is_some_and(|e| known.contains(&e.to_string_lossy().as_ref()));
    Some(if has_ext { slug } else { format!("{slug}.{}", if cfg!(windows) { "ps1" } else { "sh" }) })
}

/// `restart audio.sh` -> `Restart audio`.
fn title_for(name: &str) -> String {
    let stem =
        Path::new(name.trim()).file_stem().map_or_else(|| name.to_string(), |s| s.to_string_lossy().into_owned());
    let words = stem.replace(['-', '_'], " ");
    let mut c = words.trim().chars();
    c.next().map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
}

/// A starting point for a new script, in the language its extension names.
fn template(path: &Path, title: &str) -> String {
    let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    let (shebang, c, body) = match ext.as_str() {
        "py" => ("#!/usr/bin/env python3\n", "#", format!("print(\"Hello from {title}\")\n")),
        "js" | "mjs" => ("#!/usr/bin/env node\n", "//", format!("console.log(\"Hello from {title}\");\n")),
        "rb" => ("#!/usr/bin/env ruby\n", "#", format!("puts \"Hello from {title}\"\n")),
        "pl" => ("#!/usr/bin/env perl\n", "#", format!("print \"Hello from {title}\\n\";\n")),
        "ps1" => ("", "#", format!("Write-Output \"Hello from {title}\"\n")),
        "bat" | "cmd" => ("@echo off\n", "REM", format!("echo Hello from {title}\n")),
        "zsh" => ("#!/usr/bin/env zsh\n", "#", format!("echo \"Hello from {title}\"\n")),
        "fish" => ("#!/usr/bin/env fish\n", "#", format!("echo \"Hello from {title}\"\n")),
        _ => ("#!/bin/bash\n", "#", format!("echo \"Hello from {title}\"\n")),
    };
    format!(
        "{shebang}{c} @kairos.title {title}\n\
         {c} @kairos.mode output\n\
         {c} @kairos.description What it does\n\
         {c}\n\
         {c} Modes: silent (runs in the background), output (shown in the launcher), terminal.\n\
         {c} To take typed text as the first argument, add a line: @kairos.argument Placeholder\n\
         {c} More keys: https://kairos-launcher.github.io/kairos/plugins/scripts\n\n{body}"
    )
}

/// Remove terminal colour and cursor codes (`ESC [ … m`, `ESC ] … BEL`) from a line.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            if c != '\r' {
                out.push(c);
            }
            continue;
        }
        match chars.next() {
            // CSI: parameters, then a final byte in @..~
            Some('[') => {
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            // OSC: up to BEL or ESC \
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\u{7}' || (c == '\u{1b}' && chars.next_if_eq(&'\\').is_some()) {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

#[derive(Default)]
struct Index {
    scripts: Vec<Script>,
    seen: Seen,
    built: bool,
}

/// A running output script, shared by its waiter thread and `cancel_output`.
type Running = Arc<Mutex<Option<Child>>>;

pub struct ScriptsProvider {
    folders: Vec<PathBuf>,
    terminal: String,
    /// Command that edits a script; empty: the default app (Notepad on Windows).
    editor: String,
    index: RwLock<Index>,
    /// Output scripts still running, by execution id.
    running: Arc<Mutex<HashMap<u64, Running>>>,
}

impl ScriptsProvider {
    pub fn new(settings: &Value) -> Self {
        let mut folders = vec![crate::config::config_dir().join("scripts")];
        for f in settings.get("folders").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
            let p = expand_home(f);
            if !folders.contains(&p) {
                folders.push(p);
            }
        }
        let text = |k: &str| settings.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let mut p = Self::with_folders(folders, text("terminal"));
        p.editor = text("editor");
        p
    }

    pub fn with_folders(folders: Vec<PathBuf>, terminal: String) -> Self {
        Self { folders, terminal, editor: String::new(), index: RwLock::default(), running: Arc::default() }
    }

    /// Open a script in the editor. Never the default app for `.bat`/`.cmd` on Windows: that runs it.
    fn edit(&self, file: &Path) -> Result<(), String> {
        let f = file.to_string_lossy();
        if !self.editor.trim().is_empty() {
            return process::open_with(&self.editor, &f);
        }
        if cfg!(windows) { process::spawn(&["notepad".to_string(), f.into_owned()]) } else { process::open(&f) }
    }

    fn reindex(&self) {
        let (scripts, seen) = scan(&self.folders);
        *write(&self.index) = Index { scripts, seen, built: true };
    }

    fn ensure_index(&self) {
        if !read(&self.index).built {
            self.reindex();
        }
    }

    fn find(&self, path: &str) -> Option<Script> {
        read(&self.index).scripts.iter().find(|s| s.path.to_string_lossy() == path).cloned()
    }

    fn row(s: &Script, typed: &str, score: f64) -> ResultItem {
        let mut r = ResultItem::new(s.path.to_string_lossy().into_owned(), &s.title, &s.subtitle, score)
            .with_icon(s.icon.clone().unwrap_or_else(default_icon));
        match s.argv_args(typed) {
            Ok(args) => {
                if s.takes_args() && !typed.trim().is_empty() {
                    r.subtitle = format!("{} {}", s.keyword, args.join(" ").trim_end());
                }
                r = r.with_action("run", "Run");
            }
            Err(e) => {
                r.subtitle = format!("{}: {e} after `{}`", s.title, s.keyword);
                r.complete = Some(format!("{} ", s.keyword));
            }
        }
        r.with_action("edit", "Edit script").with_action("copy", "Copy path")
    }

    /// The folder new scripts go in: `<config>/scripts`.
    fn main_folder(&self) -> Option<&Path> {
        self.folders.first().map(PathBuf::as_path)
    }

    /// "Open scripts folder", for `scripts`, `script folder`, `open scr…` and the like.
    fn folder_row(&self, lower: &str) -> Option<ResultItem> {
        let dir = self.main_folder()?;
        let score = if lower == "scripts" || lower == "script" {
            1.0
        } else if lower.chars().count() >= 3 {
            [fuzzy::score(lower, FOLDER_TITLE), fuzzy::score(lower, "scripts folder")]
                .into_iter()
                .flatten()
                .filter(|s| *s >= 0.7)
                .reduce(f64::max)?
                * 0.8
        } else {
            return None;
        };
        let subtitle = format!("Script commands · {}", dir.display());
        Some(
            ResultItem::new(FOLDER_ID, FOLDER_TITLE, subtitle, score)
                .with_icon(icons::themed(&["folder-scripts", "folder"], "›"))
                .with_action("open", "Open")
                .with_action("copy", "Copy path"),
        )
    }

    /// `scripts create <name>`: the row that makes the script (or, if it exists, edits it).
    fn create_row(&self, text: &str) -> Option<ResultItem> {
        let rest = text.strip_prefix(CREATE_PREFIX).or_else(|| text.strip_prefix("Scripts create"))?;
        if !(rest.is_empty() || rest.starts_with(' ')) {
            return None;
        }
        let dir = self.main_folder()?;
        let icon = icons::themed(&["document-new", "text-x-script"], "+");
        let Some(file) = file_name_for(rest.trim()) else {
            let mut r =
                ResultItem::new(CREATE_ID, "Create script", "Type a name: scripts create <name>", 1.1).with_icon(icon);
            r.complete = Some(format!("{CREATE_PREFIX} "));
            return Some(r);
        };
        let path = dir.join(&file);
        let (title, sub) = if path.exists() {
            (format!("Edit {file}"), format!("Already exists · {}", path.display()))
        } else {
            (format!("Create {file}"), format!("Opens in the editor · {}", path.display()))
        };
        Some(
            ResultItem::new(format!("{CREATE_ID}{}", rest.trim()), title, sub, 1.1)
                .with_icon(icon)
                .with_action("create", "Create and edit"),
        )
    }

    /// While typing `scripts`, also offer to create one (Tab fills in the command).
    fn create_hint(lower: &str) -> Option<ResultItem> {
        (lower.len() >= 3 && CREATE_PREFIX.starts_with(lower) && lower != CREATE_PREFIX).then(|| {
            let mut r = ResultItem::new(CREATE_ID, "Create script", format!("{CREATE_PREFIX} <name>"), 0.95)
                .with_icon(icons::themed(&["document-new", "text-x-script"], "+"));
            r.complete = Some(format!("{CREATE_PREFIX} "));
            r
        })
    }

    /// Write a new script from a template (unless it exists), then open it in the editor.
    fn create(&self, name: &str) -> plugin::Result<ExecOutcome> {
        let dir = self.main_folder().ok_or("no scripts folder")?;
        let file = file_name_for(name).ok_or("type a name for the script")?;
        let path = dir.join(&file);
        if !path.exists() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            std::fs::write(&path, template(&path, &title_for(name))).map_err(|e| format!("{}: {e}", path.display()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
            }
            self.reindex();
        }
        self.edit(&path)?;
        Ok(ExecOutcome::Close)
    }

    /// Create the scripts folder if needed and open it in the file manager.
    fn open_folder(&self, action_id: &str) -> plugin::Result<ExecOutcome> {
        let dir = self.main_folder().ok_or("no scripts folder")?;
        let path = dir.to_string_lossy();
        match action_id {
            "open" => {
                std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
                process::open(&path)?;
            }
            "copy" => clipboard::copy(&path)?,
            other => return Err(Error::UnknownAction(other.into())),
        }
        Ok(ExecOutcome::Close)
    }

    /// The script and what was typed for its arguments, if `text` is its keyword (+ text).
    fn by_keyword<'a>(s: &Script, text: &'a str) -> Option<&'a str> {
        if text == s.keyword {
            return Some("");
        }
        let rest = text.strip_prefix(&s.keyword)?.strip_prefix(' ')?;
        s.takes_args().then_some(rest)
    }

    /// Start an output script and stream what it prints into the panel.
    fn run_output(&self, s: &Script, argv: &[String], cwd: &Path, ctx: &ExecCtx) -> plugin::Result<ExecOutcome> {
        let (prog, args) = argv.split_first().ok_or("empty command")?;
        let mut cmd = Command::new(prog);
        cmd.args(args).current_dir(cwd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        process::scrub_env(&mut cmd);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut child = cmd.spawn().map_err(|e| format!("{}: {e}", s.path.display()))?;
        let readers = Arc::new(AtomicUsize::new(0));
        let pipes: [Option<Box<dyn Read + Send>>; 2] = [
            child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>),
            child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>),
        ];
        for pipe in pipes.into_iter().flatten() {
            let (sink, readers) = (ctx.sink.clone(), readers.clone());
            readers.fetch_add(1, Ordering::SeqCst);
            std::thread::spawn(move || {
                let mut r = BufReader::new(pipe);
                let mut line = Vec::new();
                while r.read_until(b'\n', &mut line).is_ok_and(|n| n > 0) {
                    sink(OutputEvent::Append(strip_ansi(&String::from_utf8_lossy(&line))));
                    line.clear();
                }
                readers.fetch_sub(1, Ordering::SeqCst);
            });
        }
        let running: Running = Arc::new(Mutex::new(Some(child)));
        lock(&self.running).insert(ctx.exec, running.clone());
        let (sink, all, exec) = (ctx.sink.clone(), self.running.clone(), ctx.exec);
        std::thread::spawn(move || {
            // Polls only while a script runs; `cancel_output` needs the child too, to kill it.
            let status = loop {
                let done = match lock(&running).as_mut() {
                    Some(c) => c.try_wait().ok().flatten(),
                    None => break None,
                };
                if done.is_some() {
                    break done;
                }
                std::thread::sleep(Duration::from_millis(50));
            };
            // Let the readers pass on the last lines (a child of the script may keep a pipe open).
            for _ in 0..20 {
                if readers.load(Ordering::SeqCst) == 0 {
                    break;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            let failed = match status {
                Some(s) if s.success() => None,
                Some(s) => Some(s.code().map_or_else(|| "stopped".to_string(), |c| format!("exit code {c}"))),
                None => Some("stopped".into()),
            };
            lock(&all).remove(&exec);
            sink(OutputEvent::Done(failed));
        });
        Ok(ExecOutcome::Output(Box::new(OutputStart {
            title: s.title.clone(),
            format: if s.markdown { "markdown" } else { "text" }.into(),
            streaming: true,
            ..OutputStart::default()
        })))
    }
}

impl Provider for ScriptsProvider {
    fn id(&self) -> &str {
        ID
    }

    fn warm_up(&self) {
        self.reindex();
    }

    fn refresh(&self) {
        self.reindex();
    }

    fn needs_refresh(&self) -> bool {
        let g = read(&self.index);
        // A file added to a folder changes the folder's time, which is in `seen` too.
        !g.built || g.seen.iter().any(|(p, t)| mtime(p) != *t)
    }

    fn query(&self, q: &Query) -> Vec<ResultItem> {
        self.ensure_index();
        let text = q.text.trim();
        if text.is_empty() {
            return vec![];
        }
        let lower = text.to_lowercase();
        let g = read(&self.index);
        if let Some(row) = self.create_row(text) {
            return vec![row];
        }
        let folder = self.folder_row(&lower).into_iter().chain(Self::create_hint(&lower));
        g.scripts
            .iter()
            .filter_map(|s| {
                if let Some(rest) = Self::by_keyword(s, text) {
                    // The keyword itself: above everything, Enter runs it.
                    return Some(Self::row(s, rest, 1.1));
                }
                if text.chars().count() < 2 {
                    return None;
                }
                let score = if s.keyword.starts_with(&lower) {
                    Some(0.75)
                } else {
                    fuzzy::score(text, &s.title).map(|x| x * 0.9)
                }?;
                Some(Self::row(s, "", score))
            })
            .chain(folder)
            .collect()
    }

    fn execute(&self, result_id: &str, action_id: &str, query: &str) -> plugin::Result<ExecOutcome> {
        self.execute_with(result_id, action_id, query, &ExecCtx::none())
    }

    fn execute_with(
        &self,
        result_id: &str,
        action_id: &str,
        query: &str,
        ctx: &ExecCtx,
    ) -> plugin::Result<ExecOutcome> {
        if result_id == FOLDER_ID {
            return self.open_folder(action_id);
        }
        if let Some(name) = result_id.strip_prefix(CREATE_ID) {
            return match action_id {
                "create" => self.create(name),
                other => Err(Error::UnknownAction(other.into())),
            };
        }
        let s = self.find(result_id).ok_or("the script is gone")?;
        match action_id {
            "edit" => {
                self.edit(&s.path)?;
                return Ok(ExecOutcome::Close);
            }
            "copy" => {
                clipboard::copy(result_id)?;
                return Ok(ExecOutcome::Close);
            }
            "run" => {}
            other => return Err(Error::UnknownAction(other.into())),
        }
        let typed = Self::by_keyword(&s, query.trim()).unwrap_or("");
        let mut argv = command_for(&s.path, is_executable(&s.path));
        argv.extend(s.argv_args(typed)?);
        let cwd = s.cwd.clone().unwrap_or_else(|| s.path.parent().unwrap_or(Path::new(".")).to_path_buf());
        match s.mode {
            Mode::Output => self.run_output(&s, &argv, &cwd, ctx),
            Mode::Terminal => {
                process::run_in_terminal(&self.terminal, &argv)?;
                Ok(ExecOutcome::Close)
            }
            Mode::Silent => {
                process::spawn_in(&argv, &cwd)?;
                Ok(ExecOutcome::Close)
            }
        }
    }

    fn cancel_output(&self, exec: u64) {
        if let Some(r) = lock(&self.running).remove(&exec)
            && let Some(mut child) = lock(&r).take()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RESTART: &str = "#!/bin/bash\n# @kairos.title Restart audio\n# @kairos.mode output\n# @kairos.description Restarts PipeWire\necho hi\n";

    #[test]
    fn header_keys_and_comment_styles() {
        let s = parse(Path::new("/s/restart-audio.sh"), RESTART).unwrap();
        assert_eq!(
            (s.title.as_str(), s.keyword.as_str(), s.subtitle.as_str()),
            ("Restart audio", "restart-audio", "Restarts PipeWire")
        );
        assert_eq!(s.mode, Mode::Output);
        for (prefix, file) in
            [("//", "a.js"), ("--", "a.lua"), (";", "a.ini"), ("::", "a.bat"), ("REM", "a.cmd"), ("'", "a.vbs")]
        {
            let s =
                parse(Path::new(file), &format!("{prefix} @kairos.title T\n")).unwrap_or_else(|| panic!("{prefix}"));
            assert_eq!(s.title, "T");
        }
        assert!(parse(Path::new("x.sh"), "#!/bin/sh\necho no title\n").is_none());
        assert!(parse(Path::new("x.sh"), "echo '@kairos.title not a comment'\n").is_none());
    }

    #[test]
    fn raycast_metadata_is_understood() {
        let text = "#!/bin/bash\n# Required parameters:\n# @raycast.schemaVersion 1\n# @raycast.title Search Jira\n# @raycast.mode fullOutput\n# @raycast.packageName Dev Tools\n# @raycast.icon 🔎\n# @raycast.argument1 { \"type\": \"text\", \"placeholder\": \"Issue\" }\n# @raycast.argument2 { \"type\": \"text\", \"placeholder\": \"Project\", \"optional\": true }\n# @raycast.currentDirectoryPath ~\n";
        let s = parse(Path::new("/s/Search Jira.sh"), text).unwrap();
        assert_eq!(s.keyword, "search-jira");
        assert_eq!(s.mode, Mode::Output);
        assert_eq!(s.subtitle, "Dev Tools · Search Jira.sh");
        assert_eq!(s.icon.as_ref().and_then(|i| i.glyph.as_deref()), Some("🔎"));
        assert_eq!(
            s.args,
            [
                ArgSpec { placeholder: "Issue".into(), optional: false },
                ArgSpec { placeholder: "Project".into(), optional: true }
            ]
        );
        assert_eq!(s.cwd, Some(expand_home("~")));
        assert_eq!(s.argv_args("PROJ-1"), Ok(vec!["PROJ-1".to_string(), String::new()]));
        assert_eq!(s.argv_args("PROJ-1 'my team'"), Ok(vec!["PROJ-1".to_string(), "my team".into()]));
        assert_eq!(s.argv_args(""), Err("type Issue".into()));
        for (m, want) in [
            ("silent", Mode::Silent),
            ("compact", Mode::Output),
            ("inline", Mode::Output),
            ("terminal", Mode::Terminal),
            ("bogus", Mode::Silent),
        ] {
            assert_eq!(parse_mode(m), want);
        }
    }

    #[test]
    fn one_kairos_argument_takes_the_whole_text() {
        let s = parse(Path::new("j.sh"), "# @kairos.title J\n# @kairos.argument Issue key\n# @kairos.keyword jira\n")
            .unwrap();
        assert_eq!(s.keyword, "jira");
        assert_eq!(s.argv_args("a b \"c\""), Ok(vec!["a b \"c\"".to_string()]));
    }

    #[test]
    fn interpreters() {
        let p = Path::new("/s/x.py");
        if cfg!(unix) {
            assert_eq!(command_for(p, false), ["python3", "/s/x.py"]);
            assert_eq!(command_for(p, true), ["/s/x.py"], "executable: its shebang decides");
            assert_eq!(command_for(Path::new("/s/x"), false), ["sh", "/s/x"]);
        }
        if cfg!(windows) {
            assert_eq!(command_for(Path::new(r"C:\s\x.bat"), false), [r"C:\s\x.bat"], "never through cmd /C");
            assert_eq!(command_for(Path::new(r"C:\s\x.ps1"), false)[0], "powershell");
            assert_eq!(command_for(p, false), ["python", "/s/x.py"]);
        }
    }

    #[test]
    fn ansi_codes_are_removed() {
        assert_eq!(strip_ansi("\u{1b}[1;31mred\u{1b}[0m plain\r\n"), "red plain\n");
        assert_eq!(strip_ansi("\u{1b}]0;title\u{7}after"), "after");
        assert_eq!(strip_ansi("\u{1b}]8;;https://x\u{1b}\\link\u{1b}]8;;\u{1b}\\"), "link");
    }

    fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        p
    }

    fn search(p: &ScriptsProvider, text: &str) -> Vec<ResultItem> {
        p.query(&Query { text, trigger: None, limit: 8 })
    }

    #[test]
    fn index_follows_the_folder_and_queries_match() {
        let tmp = crate::test_support::tmp();
        let dir = tmp.path().to_path_buf();
        write_script(&dir, "restart-audio.sh", RESTART);
        write_script(&dir, "jira.sh", "#!/bin/sh\n# @kairos.title Open Jira issue\n# @kairos.argument Issue key\n");
        write_script(&dir, "notes.txt", "no header");
        let p = ScriptsProvider::with_folders(vec![dir.clone(), tmp.path().join("missing")], String::new());
        p.warm_up();
        assert!(!p.needs_refresh());
        assert_eq!(read(&p.index).scripts.len(), 2);

        let rows = search(&p, "restart-audio");
        assert!((rows[0].score - 1.1).abs() < 1e-9, "keyword: top");
        assert_eq!(search(&p, "restart")[0].title, "Restart audio", "title / keyword prefix");
        let rows = search(&p, "jira");
        assert_eq!(rows[0].complete.as_deref(), Some("jira "), "argument missing: Tab puts the keyword in");
        assert_eq!(rows[0].actions[0].id, "edit", "not runnable yet");
        let rows = search(&p, "jira PROJ-7");
        assert_eq!(rows[0].actions[0].id, "run");
        assert_eq!(rows[0].subtitle, "jira PROJ-7");
        assert!(search(&p, "restart-audio extra").is_empty(), "no arguments: keyword + text is no match");
        for q in ["scripts", "script folder", "open scr"] {
            assert!(search(&p, q).iter().any(|r| r.id == FOLDER_ID), "{q}");
        }
        assert!(search(&p, "fi").iter().all(|r| r.id != FOLDER_ID), "not for unrelated text");

        std::thread::sleep(Duration::from_millis(20));
        write_script(&dir, "new.sh", "# @kairos.title New one\n");
        assert!(p.needs_refresh(), "file added");
        p.refresh();
        assert_eq!(read(&p.index).scripts.len(), 3);
    }

    #[test]
    fn new_script_names_titles_and_templates() {
        let ext = if cfg!(windows) { "ps1" } else { "sh" };
        assert_eq!(file_name_for("Restart Audio").as_deref(), Some(format!("restart-audio.{ext}").as_str()));
        assert_eq!(file_name_for("clean.py").as_deref(), Some("clean.py"));
        assert_eq!(file_name_for("a.b c").as_deref(), Some(format!("a.b-c.{ext}").as_str()), "unknown extension");
        for bad in ["", "  ", "../x", "a/b", ".hidden", "!!!"] {
            assert_eq!(file_name_for(bad), None, "{bad:?}");
        }
        assert_eq!(title_for("restart-audio"), "Restart audio");
        assert_eq!(title_for("clean.py"), "Clean");
        // Every template is a script command with exactly the title and mode set.
        for ext in ["sh", "py", "js", "rb", "pl", "ps1", "bat", "zsh", "fish"] {
            let path = PathBuf::from(format!("/s/x.{ext}"));
            let s = parse(&path, &template(&path, "My thing")).unwrap_or_else(|| panic!("{ext}"));
            assert_eq!((s.title.as_str(), s.mode, s.args.len()), ("My thing", Mode::Output, 0), "{ext}");
        }
    }

    #[test]
    fn scripts_create_writes_the_file_once() {
        let tmp = crate::test_support::tmp();
        let dir = tmp.path().join("scripts");
        let mut p = ScriptsProvider::with_folders(vec![dir.clone()], String::new());
        // An "editor" that does nothing, so no window opens.
        p.editor = "true".into();
        let row = &search(&p, "scripts create Restart audio")[0];
        assert!(row.title.starts_with("Create restart-audio."), "{}", row.title);
        assert_eq!(row.actions[0].id, "create");
        p.execute(&row.id, "create", "").unwrap();
        let file = dir.join(file_name_for("Restart audio").unwrap());
        let body = std::fs::read_to_string(&file).unwrap();
        assert!(body.contains("@kairos.title Restart audio"));
        assert_eq!(search(&p, "Restart audio")[0].title, "Restart audio", "indexed right away");
        std::fs::write(&file, "# @kairos.title Mine\n").unwrap();
        p.execute(&row.id, "create", "").unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "# @kairos.title Mine\n", "never overwritten");
        assert!(search(&p, "scripts create Restart audio")[0].title.starts_with("Edit "));
        let empty = &search(&p, "scripts create")[0];
        assert_eq!(empty.complete.as_deref(), Some("scripts create "));
        assert!(empty.actions.is_empty(), "nothing to run without a name");
        assert!(
            search(&p, "scri").iter().any(|r| r.complete.as_deref() == Some("scripts create ")),
            "offered while typing"
        );
    }

    #[test]
    fn folder_row_shows_the_path_and_rejects_unknown_actions() {
        let tmp = crate::test_support::tmp();
        let dir = tmp.path().join("scripts");
        let p = ScriptsProvider::with_folders(vec![dir.clone()], String::new());
        assert!(matches!(p.execute(FOLDER_ID, "bogus", ""), Err(Error::UnknownAction(_))));
        assert!(search(&p, "scripts").iter().any(|r| r.subtitle.ends_with(&dir.display().to_string())));
        // `open` (create + file manager) is not run here: it would open a window.
    }

    #[cfg(unix)]
    #[test]
    fn world_writable_scripts_are_ignored() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = crate::test_support::tmp();
        let p = write_script(tmp.path(), "open.sh", "# @kairos.title Open\n");
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(scan(&[tmp.path().to_path_buf()]).0.is_empty());
    }

    /// Collect a streamed output until `Done`.
    #[cfg(unix)]
    fn run_collect(p: &ScriptsProvider, id: &str, query: &str) -> (String, Option<String>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = ExecCtx { exec: 7, sink: Arc::new(move |e| drop(tx.send(e))), input: None };
        let out = p.execute_with(id, "run", query, &ctx).unwrap();
        assert!(matches!(out, ExecOutcome::Output(ref o) if o.streaming));
        let mut text = String::new();
        loop {
            match rx.recv_timeout(Duration::from_secs(5)).expect("output ends") {
                OutputEvent::Append(t) => text.push_str(&t),
                OutputEvent::Done(e) => return (text, e),
                _ => {}
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn output_scripts_stream_and_report_the_exit_code() {
        let tmp = crate::test_support::tmp();
        let s = write_script(
            tmp.path(),
            "fail.sh",
            "#!/bin/sh\n# @kairos.title Fail\n# @kairos.mode output\nprintf 'a\\nb\\n'\nprintf '\\033[31mred\\033[0m\\n'\nexit 3\n",
        );
        let arg = write_script(
            tmp.path(),
            "echo.sh",
            "#!/bin/sh\n# @kairos.title Echo\n# @kairos.mode output\n# @kairos.argument Text\nprintf '[%s]' \"$1\"\n",
        );
        let p = ScriptsProvider::with_folders(vec![tmp.path().to_path_buf()], String::new());
        p.warm_up();
        let (text, err) = run_collect(&p, &s.to_string_lossy(), "fail");
        assert_eq!(text, "a\nb\nred\n");
        assert_eq!(err.as_deref(), Some("exit code 3"));
        let (text, err) = run_collect(&p, &arg.to_string_lossy(), "echo it's \"quoted\" $HOME; ls");
        assert_eq!(text, "[it's \"quoted\" $HOME; ls]", "one argument, never through a shell");
        assert_eq!(err, None);
    }

    #[cfg(unix)]
    #[test]
    fn closing_the_panel_stops_the_script() {
        let tmp = crate::test_support::tmp();
        let s = write_script(
            tmp.path(),
            "slow.sh",
            "#!/bin/sh\n# @kairos.title Slow\n# @kairos.mode output\necho start\nexec sleep 30\n",
        );
        let p = ScriptsProvider::with_folders(vec![tmp.path().to_path_buf()], String::new());
        p.warm_up();
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = ExecCtx { exec: 9, sink: Arc::new(move |e| drop(tx.send(e))), input: None };
        p.execute_with(&s.to_string_lossy(), "run", "slow", &ctx).unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), OutputEvent::Append("start\n".into()));
        let started = std::time::Instant::now();
        p.cancel_output(9);
        loop {
            if let OutputEvent::Done(e) = rx.recv_timeout(Duration::from_secs(5)).unwrap() {
                assert_eq!(e.as_deref(), Some("stopped"));
                break;
            }
        }
        assert!(started.elapsed() < Duration::from_secs(3));
    }
}
