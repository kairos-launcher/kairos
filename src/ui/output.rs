//! The output panel: an action's result text, possibly streaming, shown in place of the list.
//!
//! Output events arrive on plugin reader threads. They queue in [`Inbox`] (in `Shared`) and are
//! applied on the UI thread at most ~30 times a second, so a fast token stream cannot flood the
//! event loop. Events that come before the reply that opened the panel wait in the inbox.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use slint::{ModelRc, VecModel};

use super::{ActionItem, App, MainWindow, complete_search, with_state};
use crate::core::sync::lock;
use crate::plugin::{Action, ActionKind, OutputEvent, OutputInput, OutputSink, OutputStart, ResultItem};

/// Most text kept for one output; the rest is cut off and the stream stopped.
const MAX_OUTPUT: usize = 256 * 1024;
const FLUSH_EVERY: Duration = Duration::from_millis(33);
/// A stream with no news for this long, or running this long in total, is stopped.
const IDLE_LIMIT: Duration = Duration::from_secs(60);
const TOTAL_LIMIT: Duration = Duration::from_secs(600);
const WATCHDOG_EVERY: Duration = Duration::from_secs(5);
/// The panel's own Copy action (copies everything shown).
const COPY_ID: &str = "\u{0}copy";
/// The panel's own "Open in editor" action: the whole answer in the default app, where any
/// part can be selected (markdown answers cannot be selected in the panel).
const EDIT_ID: &str = "\u{0}edit";

/// Events for the current execution, waiting for the UI thread.
#[derive(Default)]
pub(super) struct Inbox {
    exec: u64,
    events: Vec<OutputEvent>,
    /// A flush is already scheduled on the UI thread.
    posted: bool,
}

/// What the panel shows (UI thread only).
pub(super) struct OutputView {
    exec: u64,
    item: ResultItem,
    actions: Vec<Action>,
    text: String,
    /// The reply with the first part arrived; until then events wait in the inbox.
    started: bool,
    done: bool,
    began: Instant,
    last: Instant,
    /// The search box takes follow-up text for the plugin.
    input: Option<OutputInput>,
    /// `text` is markdown (`format: "markdown"`).
    markdown: bool,
    /// The search text from before the panel opened, put back on close.
    query: String,
}

/// UI-thread timers for the panel.
#[derive(Default)]
pub(super) struct OutputTimers {
    flush: slint::Timer,
    watchdog: slint::Timer,
    last_flush: Option<Instant>,
}

impl App {
    /// Where the plugin's events for execution `exec` go. Callable from any thread.
    pub(super) fn output_sink(&self, exec: u64) -> OutputSink {
        let app = Mutex::new(self.clone());
        let shared = self.shared.clone();
        Arc::new(move |e| {
            let mut inbox = lock(&shared.output);
            if inbox.exec != exec {
                return; // the panel moved on
            }
            inbox.events.push(e);
            if !std::mem::replace(&mut inbox.posted, true) {
                let app = lock(&app).clone();
                let _ = slint::invoke_from_event_loop(move || app.flush_output());
            }
        })
    }

    /// Start listening for execution `exec` (before it runs, so no event is missed).
    pub(super) fn expect_output(&self, exec: u64) {
        *lock(&self.shared.output) = Inbox { exec, ..Inbox::default() };
    }

    /// Open the panel for a new execution of `item`, replacing (and cancelling) any old one.
    /// A run started from the open panel keeps showing the old text until its reply comes.
    pub(super) fn open_output(&self, ui: &MainWindow, item: &ResultItem, exec: u64) {
        self.cancel_stream();
        let now = Instant::now();
        let reused = with_state(|s| {
            if let Some(v) = &mut s.output {
                (v.exec, v.started, v.done, v.began, v.last) = (exec, false, false, now, now);
                true
            } else {
                s.output = Some(OutputView {
                    exec,
                    item: item.clone(),
                    actions: vec![],
                    text: String::new(),
                    started: false,
                    done: false,
                    began: now,
                    last: now,
                    input: None,
                    markdown: false,
                    query: String::new(),
                });
                false
            }
        });
        if !reused {
            let query = ui.get_query().to_string();
            with_state(|s| s.output.as_mut().map(|v| v.query = query));
            ui.set_output_title(item.title.clone().into());
            Self::show_output_text(ui, "", false);
            ui.set_output_input(false);
            ui.set_output_enter("Copy".into());
        }
        ui.set_menu_open(false);
        ui.set_output_status(0);
        ui.set_output_message("".into());
        ui.set_output_open(true);
        ui.invoke_follow_output();
        self.start_watchdog();
    }

    /// Offer or withdraw the follow-up input. Offering it empties the search box.
    fn set_output_input(ui: &MainWindow, input: Option<OutputInput>) {
        let shown = input.clone();
        let was = with_state(|s| s.output.as_mut().map(|v| std::mem::replace(&mut v.input, input).is_some()));
        match &shown {
            Some(i) => {
                ui.set_output_placeholder(i.placeholder.clone().into());
                if was == Some(false) {
                    ui.set_query("".into());
                }
                ui.set_output_input(true);
            }
            None => ui.set_output_input(false),
        }
    }

    /// Enter in the follow-up input: run the plugin's input action with the typed text.
    pub(super) fn send_output_input(&self, text: &str) {
        let Some(ui) = self.weak.upgrade() else { return };
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let Some((item, input)) =
            with_state(|s| s.output.as_ref().and_then(|v| Some((v.item.clone(), v.input.clone()?))))
        else {
            return;
        };
        let action = Action { id: input.action, title: String::new(), ..Action::default() };
        if self.run_with_input(item, action, Some(text.to_string())) {
            ui.set_query("".into());
        }
    }

    /// The execute reply: fill the panel (opening it if the action had no `output` hint).
    pub(super) fn apply_output_start(&self, ui: &MainWindow, item: &ResultItem, exec: u64, start: OutputStart) {
        if lock(&self.shared.output).exec != exec {
            // The user went back or closed the launcher before the reply came: drop it.
            if start.streaming {
                let (engine, provider) = (self.engine.clone(), item.provider.clone());
                std::thread::spawn(move || engine.cancel_output(&provider, exec));
            }
            return;
        }
        let open = with_state(|s| s.output.as_ref().map(|v| v.exec));
        if open != Some(exec) {
            self.open_output(ui, item, exec);
        }
        with_state(|s| {
            if let Some(v) = &mut s.output {
                v.started = true;
                v.done = !start.streaming;
                v.actions = start.actions;
                v.markdown = start.format == "markdown";
                v.text = start.text;
                cap(&mut v.text);
                v.last = Instant::now();
            }
        });
        if !start.title.is_empty() {
            ui.set_output_title(start.title.into());
        }
        Self::set_output_input(ui, start.input);
        self.flush_output_now();
    }

    /// The action finished without output although the panel opened for it: close it again.
    pub(super) fn output_not_needed(&self, exec: u64) {
        if with_state(|s| s.output.as_ref().is_some_and(|v| v.exec == exec)) {
            self.close_output();
        }
    }

    /// The action failed: show why in the panel, if it is open for it.
    pub(super) fn output_failed(&self, exec: u64, message: &str) -> bool {
        let mine = with_state(|s| match &mut s.output {
            Some(v) if v.exec == exec => {
                v.done = true;
                true
            }
            _ => false,
        });
        if mine && let Some(ui) = self.weak.upgrade() {
            ui.set_output_status(2);
            ui.set_output_message(message.into());
        }
        mine
    }

    fn flush_output(&self) {
        let wait = with_state(|s| {
            let since = s.output_timers.last_flush.map(|t| t.elapsed());
            since.and_then(|d| FLUSH_EVERY.checked_sub(d)).filter(|w| !w.is_zero())
        });
        match wait {
            Some(wait) => {
                let app = self.clone();
                with_state(|s| {
                    s.output_timers.flush.start(slint::TimerMode::SingleShot, wait, move || app.flush_output_now());
                });
            }
            None => self.flush_output_now(),
        }
    }

    /// Apply the waiting events and show the result.
    fn flush_output_now(&self) {
        let Some(ui) = self.weak.upgrade() else { return };
        let ready = with_state(|s| s.output.as_ref().map(|v| (v.exec, v.started)));
        let Some((exec, true)) = ready else {
            lock(&self.shared.output).posted = false;
            return; // not open, or the reply has not come yet: keep the events
        };
        let events = {
            let mut inbox = lock(&self.shared.output);
            inbox.posted = false;
            if inbox.exec == exec { std::mem::take(&mut inbox.events) } else { vec![] }
        };
        let mut input_change = None;
        let (text, markdown, status, message, enter, truncated) = with_state(|s| {
            s.output_timers.last_flush = Some(Instant::now());
            let v = s.output.as_mut()?;
            let mut message = None;
            let mut truncated = false;
            for e in events {
                v.last = Instant::now();
                match e {
                    OutputEvent::Append(t) => v.text.push_str(&t),
                    OutputEvent::Replace(t) => v.text = t,
                    OutputEvent::Actions(a) => v.actions = a,
                    OutputEvent::Input(i) => input_change = Some(i),
                    OutputEvent::Done(err) => {
                        v.done = true;
                        message = err;
                    }
                }
                if cap(&mut v.text) {
                    truncated = !v.done;
                    v.done = true;
                }
            }
            let status = match (&message, v.done) {
                (Some(_), _) => 2,
                (None, true) => 1,
                (None, false) => 0,
            };
            let enter = default_action(&panel_actions(v)).map(|a| a.title).unwrap_or_default();
            Some((v.text.clone(), v.markdown, status, message, enter, truncated))
        })
        .unwrap_or_default();
        if truncated {
            self.cancel_exec(exec);
        }
        if let Some(i) = input_change {
            Self::set_output_input(&ui, i);
        }
        Self::show_output_text(&ui, &text, markdown);
        if ui.get_output_status() != 2 {
            ui.set_output_status(status);
            ui.set_output_message(message.unwrap_or_default().into());
        }
        ui.set_output_enter(enter.into());
    }

    /// Put the text in the panel: as styled markdown, or plain (selectable). The other view is
    /// emptied so only one copy of a long answer is kept.
    fn show_output_text(ui: &MainWindow, text: &str, markdown: bool) {
        if markdown {
            ui.set_output_styled(super::markdown::to_styled(text));
            ui.set_output_text("".into());
        } else {
            ui.set_output_text(text.into());
            ui.set_output_styled(slint::StyledText::default());
        }
        ui.set_output_markdown(markdown);
    }

    /// A link in a markdown answer: open it in the browser and step aside (the answer is kept
    /// for a moment, as after any close).
    pub(super) fn open_output_link(&self, url: &str) {
        match crate::core::process::open_url(url) {
            Ok(()) => self.hide(),
            Err(e) => log::warn!("output link: {e}"),
        }
    }

    /// Ctrl+C while an answer streams: stop it and keep what arrived.
    pub(super) fn stop_output(&self) {
        let Some(exec) = with_state(|s| s.output.as_ref().filter(|v| !v.done).map(|v| v.exec)) else { return };
        // Show what already arrived before the stop drops the rest.
        self.flush_output_now();
        self.cancel_exec(exec);
        if let Some(ui) = self.weak.upgrade() {
            ui.set_output_status(2);
            ui.set_output_message("Stopped".into());
        }
    }

    /// Ctrl+C / ←: back to the results. Also used when the window hides for good.
    pub(super) fn close_output(&self) {
        self.cancel_stream();
        let view = with_state(|s| {
            s.output_timers.watchdog.stop();
            s.output_timers.flush.stop();
            s.output.take()
        });
        *lock(&self.shared.output) = Inbox::default();
        let Some(ui) = self.weak.upgrade() else { return };
        if ui.get_output_open() {
            ui.set_menu_open(false);
            ui.set_output_open(false);
            ui.set_output_input(false);
            // Back to the search that led here; the results are still the ones for it.
            if let Some(v) = view {
                ui.set_query(v.query.into());
            }
            // Free the text now; a large answer should not outlive the panel.
            Self::show_output_text(&ui, "", false);
            ui.invoke_focus_input();
        }
    }

    /// Tab in the panel: its actions in the usual menu.
    pub(super) fn open_output_menu(&self) {
        let Some(ui) = self.weak.upgrade() else { return };
        let Some(actions) = with_state(|s| s.output.as_ref().map(panel_actions)) else { return };
        let default = default_action(&actions).map(|a| a.id);
        let rows: Vec<ActionItem> = actions
            .iter()
            .map(|a| ActionItem { title: a.title.clone().into(), is_default: Some(&a.id) == default.as_ref() })
            .collect();
        ui.set_actions(ModelRc::new(VecModel::from(rows)));
        ui.set_action_selected(0);
        ui.set_menu_open(true);
    }

    /// Enter in the panel (`index` -1) or a chosen entry of its menu.
    pub(super) fn run_output_action(&self, index: i32) {
        let Some(ui) = self.weak.upgrade() else { return };
        let Some((actions, text, markdown, item)) =
            with_state(|s| s.output.as_ref().map(|v| (panel_actions(v), v.text.clone(), v.markdown, v.item.clone())))
        else {
            return;
        };
        let chosen = match usize::try_from(index) {
            Ok(i) => actions.get(i).cloned(),
            Err(_) => default_action(&actions),
        };
        let Some(action) = chosen else { return };
        ui.set_menu_open(false);
        if action.id == COPY_ID {
            match crate::core::clipboard::copy(&text) {
                Ok(()) => self.hide_done(),
                Err(e) => log::warn!("copy: {e}"),
            }
            return;
        }
        if action.id == EDIT_ID {
            match open_in_editor(&text, markdown) {
                Ok(()) => self.hide_done(),
                Err(e) => log::warn!("open in editor: {e}"),
            }
            return;
        }
        match action.run_host() {
            Some(Ok(out)) => {
                if let Some(q) = out.query() {
                    self.close_output();
                    complete_search(&ui, q);
                } else if !out.keeps_open() {
                    self.hide_done();
                }
            }
            Some(Err(e)) => log::warn!("action failed: {e}"),
            // A plugin action: runs again in the panel (e.g. "Regenerate").
            None => self.run(item, action),
        }
    }

    /// Cancel the running stream, if any (the panel stays as it is).
    fn cancel_stream(&self) {
        let running = with_state(|s| s.output.as_ref().filter(|v| !v.done).map(|v| v.exec));
        if let Some(exec) = running {
            self.cancel_exec(exec);
        }
    }

    fn cancel_exec(&self, exec: u64) {
        let Some(provider) = with_state(|s| {
            let v = s.output.as_mut().filter(|v| v.exec == exec)?;
            v.done = true;
            Some(v.item.provider.clone())
        }) else {
            return;
        };
        let mut inbox = lock(&self.shared.output);
        if inbox.exec == exec {
            inbox.exec = 0;
            inbox.events.clear();
        }
        drop(inbox);
        // Telling the plugin may wait on it briefly: not on the UI thread.
        let engine = self.engine.clone();
        std::thread::spawn(move || engine.cancel_output(&provider, exec));
    }

    /// Stop streams that went quiet or ran too long. Runs only while the panel is open.
    fn start_watchdog(&self) {
        let app = self.clone();
        with_state(|s| {
            s.output_timers.watchdog.start(slint::TimerMode::Repeated, WATCHDOG_EVERY, move || {
                let stuck = with_state(|s| {
                    s.output
                        .as_ref()
                        .filter(|v| !v.done && (v.last.elapsed() > IDLE_LIMIT || v.began.elapsed() > TOTAL_LIMIT))
                        .map(|v| v.exec)
                });
                if let Some(exec) = stuck {
                    app.cancel_exec(exec);
                    app.output_failed(exec, "Stopped: no answer from the plugin");
                }
            });
        });
    }
}

/// The panel's actions: the plugin's, plus Copy (the default when the plugin gave none) and
/// Open in editor.
fn panel_actions(v: &OutputView) -> Vec<Action> {
    let copy = Action {
        id: COPY_ID.into(),
        title: "Copy".into(),
        default: v.actions.is_empty(),
        kind: ActionKind::Call,
        ..Action::default()
    };
    let edit =
        Action { id: EDIT_ID.into(), title: "Open in editor".into(), kind: ActionKind::Call, ..Action::default() };
    v.actions.iter().cloned().chain([copy, edit]).collect()
}

/// Write the answer to the cache and open it with the default app.
fn open_in_editor(text: &str, markdown: bool) -> Result<(), String> {
    let dir = dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("kairos");
    let path = write_answer(&dir, text, markdown).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = path.to_string_lossy();
    let opened = crate::core::process::open(&path);
    // Windows has no default app for `.md` unless one was installed; Notepad always exists.
    #[cfg(windows)]
    let opened = opened.or_else(|e| {
        log::debug!("open in editor: {e}; trying Notepad");
        crate::core::process::spawn(&["notepad.exe".into(), path.to_string()])
    });
    opened
}

/// The answer as `answer.md` (or `.txt`) in `dir`, replacing the last one. Only the user may
/// read it: answers can be private.
fn write_answer(dir: &std::path::Path, text: &str, markdown: bool) -> std::io::Result<std::path::PathBuf> {
    use std::io::Write;
    std::fs::create_dir_all(dir)?;
    let path = dir.join(if markdown { "answer.md" } else { "answer.txt" });
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let mut file = opts.open(&path)?;
    // `mode` applies only to a new file; an older one may have been created looser.
    #[cfg(unix)]
    file.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    file.write_all(text.as_bytes())?;
    Ok(path)
}

fn default_action(actions: &[Action]) -> Option<Action> {
    actions.iter().find(|a| a.default).or(actions.first()).cloned()
}

/// Cut `text` to `MAX_OUTPUT` bytes (at a character boundary). True if it was cut.
fn cap(text: &mut String) -> bool {
    if text.len() <= MAX_OUTPUT {
        return false;
    }
    let mut end = MAX_OUTPUT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text.push_str("\n… (output truncated)");
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(actions: Vec<Action>) -> OutputView {
        let now = Instant::now();
        OutputView {
            exec: 1,
            item: ResultItem::new("i", "t", "", 0.5),
            actions,
            text: String::new(),
            started: true,
            done: false,
            began: now,
            last: now,
            input: None,
            markdown: false,
            query: String::new(),
        }
    }

    #[test]
    fn copy_is_the_default_only_without_plugin_actions() {
        let a = panel_actions(&view(vec![]));
        assert_eq!(a.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), [COPY_ID, EDIT_ID]);
        assert_eq!(default_action(&a).unwrap().id, COPY_ID);
        let mine = Action { id: "again".into(), title: "Again".into(), ..Action::default() };
        let a = panel_actions(&view(vec![mine]));
        assert_eq!(a.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), ["again", COPY_ID, EDIT_ID]);
        assert_eq!(default_action(&a).unwrap().id, "again", "first plugin action wins");
    }

    #[test]
    fn answer_is_written_private_and_replaced() {
        let dir = tempfile::tempdir().unwrap();
        write_answer(dir.path(), "# old", true).unwrap();
        let md = write_answer(dir.path(), "# hi", true).unwrap();
        assert_eq!(md.file_name().unwrap(), "answer.md");
        assert_eq!(std::fs::read_to_string(&md).unwrap(), "# hi");
        let txt = write_answer(dir.path(), "plain", false).unwrap();
        assert_eq!(txt.file_name().unwrap(), "answer.txt");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&md).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }

    #[test]
    fn long_output_is_cut_at_a_char_boundary() {
        let mut t = "é".repeat(MAX_OUTPUT);
        assert!(cap(&mut t));
        assert!(t.ends_with("(output truncated)") && t.len() <= MAX_OUTPUT + 30);
        let mut short = "hi".to_string();
        assert!(!cap(&mut short));
        assert_eq!(short, "hi");
    }
}
