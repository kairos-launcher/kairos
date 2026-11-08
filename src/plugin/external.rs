//! Out-of-process plugin: JSON-RPC 2.0, one message per line over stdio.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::error::{Error, Result};
use super::manifest::{Manifest, Mode};
use super::{
    Action, ExecCtx, ExecOutcome, Icon, OutputEvent, OutputInput, OutputSink, OutputStart, Provider, Query, ResultItem,
};
use crate::core::sync::lock;

const API: &str = "0.1";
const STARTUP_TIMEOUT: Duration = Duration::from_secs(3);
const EXEC_TIMEOUT: Duration = Duration::from_secs(10);
const ICON_TIMEOUT: Duration = Duration::from_secs(1);
/// Longest reply line accepted from a plugin (one JSON message).
const MAX_LINE: u64 = 1 << 20;

/// What the reader thread (or a newer query) hands to the thread waiting on a reply.
enum Msg {
    Line(Value),
    /// A newer query started: re-check whether the current wait is still wanted.
    Wake,
    /// The plugin closed stdout (or sent an oversized line); no more replies will come.
    Closed,
}

struct Running {
    /// Which process this is (counts up per spawn); output listeners are tied to it.
    generation: u64,
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Msg>,
    next_id: u64,
    last_used: Instant,
    /// Set by the reader thread once stdout closed: the process is gone even if it is not
    /// reaped yet, so the next call starts a new one instead of writing into a dead pipe.
    closed: Arc<AtomicBool>,
}

pub struct ExternalPlugin {
    manifest: Manifest,
    dir: PathBuf,
    /// Effective settings, sent in `initialize`. A change makes the engine build a new plugin.
    settings: Value,
    running: Mutex<Option<Running>>,
    /// Latest query id; a query whose id is no longer the latest is cancelled.
    qid: AtomicU64,
    /// Wakes a query waiting on the live process so it notices it was superseded.
    wake: Mutex<Option<Sender<Msg>>>,
    /// The execution whose `output` notifications the reader thread delivers right away.
    listener: Arc<Mutex<Option<Listener>>>,
    generations: AtomicU64,
    /// Sent as `data_dir` in `initialize`, created on start (`config::plugin_data_dir`).
    data_dir: PathBuf,
}

/// Where the reader thread sends `output` notifications for execution `exec`.
struct Listener {
    exec: u64,
    generation: u64,
    sink: OutputSink,
}

impl ExternalPlugin {
    pub fn new(manifest: Manifest, dir: PathBuf, settings: Value) -> Self {
        let data_dir = crate::config::plugin_data_dir(&manifest.id);
        Self::with_data_dir(manifest, dir, settings, data_dir)
    }

    fn with_data_dir(manifest: Manifest, dir: PathBuf, settings: Value, data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            manifest,
            dir,
            settings,
            running: Mutex::new(None),
            qid: AtomicU64::new(0),
            wake: Mutex::new(None),
            listener: Arc::new(Mutex::new(None)),
            generations: AtomicU64::new(0),
        }
    }

    fn spawn(&self) -> Result<Running> {
        let (program, args) =
            self.manifest.run.argv().and_then(|a| a.split_first()).ok_or("no run.exec for this OS")?;
        let mut prog = PathBuf::from(program);
        // Relative programs are resolved against the plugin dir on every OS.
        if prog.components().count() > 1 && prog.is_relative() {
            prog = self.dir.join(prog);
        }
        let mut cmd = Command::new(prog);
        cmd.args(args).current_dir(&self.dir).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit());
        crate::core::process::scrub_env(&mut cmd);
        crate::core::proxy::apply_env(&mut cmd);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut child = cmd.spawn().map_err(|e| Error::Message(format!("spawn {}: {e}", self.manifest.id)))?;
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = child.stdout.take().ok_or("no stdout")?;

        // Reader thread blocks on the pipe; no polling, exits when the child closes stdout.
        let (tx, rx) = channel();
        *lock(&self.wake) = Some(tx.clone());
        let id = self.manifest.id.clone();
        let generation = self.generations.fetch_add(1, Ordering::SeqCst) + 1;
        let listener = self.listener.clone();
        let closed = Arc::new(AtomicBool::new(false));
        let closed_by_reader = closed.clone();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = Vec::new();
            loop {
                line.clear();
                match (&mut reader).take(MAX_LINE + 1).read_until(b'\n', &mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(n) if n as u64 > MAX_LINE => {
                        // `Closed` makes the pending request fail; the plugin restarts.
                        log::warn!("plugin {id}: a reply line is over {MAX_LINE} bytes; stopping it");
                        break;
                    }
                    Ok(_) => {
                        let Ok(v) = serde_json::from_slice::<Value>(&line) else { continue };
                        // Output goes straight to its listener: nobody may be waiting on `rx`
                        // while an answer streams in.
                        if v.get("method").and_then(Value::as_str) == Some("output") {
                            deliver_output(&listener, generation, v.get("params").unwrap_or(&Value::Null));
                        } else if tx.send(Msg::Line(v)).is_err() {
                            return;
                        }
                    }
                }
            }
            // Before ending the stream: whoever sees it end may call again right away.
            closed_by_reader.store(true, Ordering::SeqCst);
            end_output(&listener, generation, "the plugin stopped");
            // The `wake` sender keeps the channel open, so say explicitly that it ended.
            let _ = tx.send(Msg::Closed);
        });

        let mut r = Running { generation, child, stdin, rx, next_id: 1, last_used: Instant::now(), closed };
        let mut params = json!({
            "api": API,
            "host": "kairos",
            "settings": self.settings,
            "locale": "en",
        });
        // Plugin ids are bare keys, so the folder name is safe. Without it the plugin still starts.
        let data = &self.data_dir;
        match crate::core::fs::create_private_dir(data) {
            Ok(()) => params["data_dir"] = json!(data.to_string_lossy()),
            Err(e) => log::warn!("plugin {}: no data folder {}: {e}", self.manifest.id, data.display()),
        }
        request(&mut r, "initialize", &params, STARTUP_TIMEOUT)?;
        Ok(r)
    }

    /// Run `f` against a live process, (re)starting it if needed.
    fn with_running<T>(&self, f: impl FnOnce(&mut Running) -> Result<T>) -> Result<T> {
        let mut guard = lock(&self.running);
        let gone = guard
            .as_mut()
            .is_some_and(|r| r.closed.load(Ordering::SeqCst) || matches!(r.child.try_wait(), Ok(Some(_))));
        if gone && let Some(mut dead) = guard.take() {
            let _ = dead.child.kill();
            let _ = dead.child.wait();
        }
        let r = match &mut *guard {
            Some(r) => r,
            empty @ None => empty.insert(self.spawn()?),
        };
        r.last_used = Instant::now();
        let out = f(r);
        if out.as_ref().is_err_and(|e| !e.is_timeout() && !e.is_cancelled()) {
            // Crash or protocol failure: drop the process; it restarts on the next query.
            // A timeout or a cancel keeps it alive: replies are matched by id, so a late one
            // is simply skipped by the next request.
            if let Some(mut dead) = guard.take() {
                let _ = dead.child.kill();
                let _ = dead.child.wait();
            }
        }
        out
    }
}

/// Send a request and wait for the response with the matching id.
/// Notifications received meanwhile are returned in the second slot.
fn request(r: &mut Running, method: &'static str, params: &Value, timeout: Duration) -> Result<Value> {
    request_collect(r, method, params, timeout, &|| false).map(|(v, _)| v)
}

/// Like `request`, but gives up with `Cancelled` as soon as `cancelled()` turns true.
fn request_collect(
    r: &mut Running,
    method: &'static str,
    params: &Value,
    timeout: Duration,
    cancelled: &dyn Fn() -> bool,
) -> Result<(Value, Vec<Value>)> {
    let id = r.next_id;
    r.next_id += 1;
    let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
    writeln!(r.stdin, "{msg}")
        .and_then(|()| r.stdin.flush())
        .map_err(|e| Error::Message(format!("write: {e}")).during(method))?;

    let deadline = Instant::now() + timeout;
    let mut notes = vec![];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match r.rx.recv_timeout(left) {
            Ok(Msg::Closed) | Err(RecvTimeoutError::Disconnected) => return Err(Error::Exited.during(method)),
            Err(RecvTimeoutError::Timeout) => return Err(Error::Timeout.during(method)),
            Ok(_) if cancelled() => return Err(Error::Cancelled.during(method)),
            Ok(Msg::Line(v)) if v.get("id").and_then(Value::as_u64) == Some(id) => {
                if let Some(err) = v.get("error") {
                    return Err(Error::Rpc(err.clone()).during(method));
                }
                return Ok((v.get("result").cloned().unwrap_or(Value::Null), notes));
            }
            Ok(Msg::Line(v)) => notes.push(v),
            Ok(Msg::Wake) => {}
        }
    }
}

/// Send a notification (no reply expected).
fn notify(r: &mut Running, method: &str, params: &Value) -> Result<()> {
    let msg = json!({ "jsonrpc": "2.0", "method": method, "params": params });
    writeln!(r.stdin, "{msg}").and_then(|()| r.stdin.flush()).map_err(|e| Error::Message(format!("write: {e}")))
}

/// Hand one `output` notification to the listener for its `exec`; others are stale.
fn deliver_output(listener: &Mutex<Option<Listener>>, generation: u64, params: &Value) {
    let exec = params.get("exec").and_then(Value::as_u64);
    // Held while calling the sink, so events reach it in order and never after a cancel.
    let mut guard = lock(listener);
    let Some(l) = guard.as_ref().filter(|l| Some(l.exec) == exec && l.generation == generation) else { return };
    let events = output_events(params);
    let done = events.iter().any(|e| matches!(e, OutputEvent::Done(_)));
    for e in events {
        (l.sink)(e);
    }
    if done {
        *guard = None;
    }
}

/// The process behind `generation` is gone: fail its stream, if one is open.
fn end_output(listener: &Mutex<Option<Listener>>, generation: u64, why: &str) {
    let mut guard = lock(listener);
    if guard.as_ref().is_some_and(|l| l.generation == generation)
        && let Some(l) = guard.take()
    {
        (l.sink)(OutputEvent::Done(Some(why.into())));
    }
}

/// `{"exec":N, "append"|"text", "actions", "done", "error"}` -> events, in that order.
pub(super) fn output_events(p: &Value) -> Vec<OutputEvent> {
    let mut out = vec![];
    if let Some(t) = p.get("text").and_then(Value::as_str) {
        out.push(OutputEvent::Replace(t.into()));
    }
    if let Some(t) = p.get("append").and_then(Value::as_str) {
        out.push(OutputEvent::Append(t.into()));
    }
    if let Some(a) = p.get("actions") {
        out.push(OutputEvent::Actions(parse_actions(a)));
    }
    if let Some(i) = p.get("input") {
        out.push(OutputEvent::Input(parse_input(i)));
    }
    let error = p.get("error").and_then(Value::as_str).map(String::from);
    if error.is_some() || p.get("done").and_then(Value::as_bool) == Some(true) {
        out.push(OutputEvent::Done(error));
    }
    out
}

/// `{"placeholder": "…", "action": "id"}`; anything without an action means no input.
fn parse_input(v: &Value) -> Option<OutputInput> {
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(|t| t.chars().take(200).collect::<String>());
    let action = s("action").filter(|a| !a.is_empty())?;
    Some(OutputInput { placeholder: s("placeholder").unwrap_or_default(), action })
}

fn parse_actions(v: &Value) -> Vec<Action> {
    let mut actions: Vec<Action> = serde_json::from_value(v.clone()).unwrap_or_default();
    super::clamp_actions(&mut actions);
    actions
}

/// Interpret an `execute` reply (shared by stdio and remote plugins).
pub(super) fn outcome_from_reply(v: &Value) -> Result<ExecOutcome> {
    if v.get("ok").and_then(Value::as_bool) == Some(false) {
        return Err(v.get("message").and_then(Value::as_str).unwrap_or("action failed").into());
    }
    if let Some(o) = v.get("output").filter(|o| o.is_object()) {
        let s = |k: &str| o.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
        return Ok(ExecOutcome::Output(Box::new(OutputStart {
            title: s("title"),
            text: s("text"),
            format: s("format"),
            streaming: o.get("streaming").and_then(Value::as_bool).unwrap_or(false),
            actions: o.get("actions").map(parse_actions).unwrap_or_default(),
            input: o.get("input").and_then(parse_input),
        })));
    }
    // The launcher owns the clipboard: an on-demand plugin may be stopped before a paste.
    if let Some(text) = v.get("copy").and_then(Value::as_str) {
        crate::core::clipboard::copy(text)?;
    }
    // `open_url` is checked by the host (https/http/mailto only), not trusted blindly.
    if let Some(url) = v.get("open_url").and_then(Value::as_str) {
        crate::core::process::open_url(url)?;
    }
    Ok(if v.get("hide").and_then(Value::as_bool) == Some(false) { ExecOutcome::KeepOpen } else { ExecOutcome::Close })
}

/// Icon paths in manifests and replies are relative to the plugin folder.
fn absolutize(icon: &mut Icon, dir: &std::path::Path) {
    if let Some(p) = &icon.path
        && std::path::Path::new(p).is_relative()
    {
        icon.path = Some(dir.join(p).to_string_lossy().into_owned());
    }
}

pub(super) fn parse_results(v: &Value) -> Vec<ResultItem> {
    v.get("results").cloned().and_then(|r| serde_json::from_value(r).ok()).unwrap_or_default()
}

pub(super) fn collect_notes(notes: &[Value], qid: u64, out: &mut Vec<ResultItem>) -> bool {
    let mut done = false;
    for n in notes {
        if n.get("method").and_then(Value::as_str) != Some("results") {
            continue;
        }
        let Some(p) = n.get("params") else { continue };
        if p.get("qid").and_then(Value::as_u64) != Some(qid) {
            continue; // stale
        }
        out.extend(parse_results(p));
        done |= p.get("done").and_then(Value::as_bool).unwrap_or(false);
    }
    done
}

impl Provider for ExternalPlugin {
    fn id(&self) -> &str {
        &self.manifest.id
    }

    fn query(&self, q: &Query) -> Vec<ResultItem> {
        let mut all = vec![];
        self.query_stream(q, &mut |items| all.extend(items));
        all
    }

    fn query_stream(&self, q: &Query, emit: &mut dyn FnMut(Vec<ResultItem>)) {
        let qid = self.qid.fetch_add(1, Ordering::SeqCst) + 1;
        // Wake an older query still waiting on the process so it gives way to this one.
        if let Some(wake) = lock(&self.wake).as_ref() {
            let _ = wake.send(Msg::Wake);
        }
        let cancelled = || self.qid.load(Ordering::SeqCst) != qid;
        let budget = Duration::from_millis(self.manifest.query.timeout_ms);
        let params = json!({ "qid": qid, "text": q.text, "trigger": q.trigger, "limit": q.limit });
        let started = Instant::now();
        let mut emit_batch = |mut items: Vec<ResultItem>| {
            for i in &mut items {
                if let Some(icon) = &mut i.icon {
                    absolutize(icon, &self.dir);
                }
            }
            if !items.is_empty() {
                emit(items);
            }
        };

        let res = self.with_running(|r| {
            // Superseded while waiting for the previous query: never send it.
            if cancelled() {
                return Err(Error::Cancelled);
            }
            let res = (|| {
                let (reply, notes) = request_collect(r, "query", &params, budget, &cancelled)?;
                let mut items = parse_results(&reply);
                let mut done = !reply.get("streaming").and_then(Value::as_bool).unwrap_or(false);
                done |= collect_notes(&notes, qid, &mut items);
                emit_batch(std::mem::take(&mut items));
                // Streaming: keep draining `results` notifications until done, out of budget
                // or superseded, reporting each batch as it arrives.
                while !done {
                    let left = budget.saturating_sub(started.elapsed());
                    match r.rx.recv_timeout(left) {
                        Ok(_) if cancelled() => return Err(Error::Cancelled),
                        Ok(Msg::Line(v)) => {
                            done = collect_notes(&[v], qid, &mut items);
                            emit_batch(std::mem::take(&mut items));
                        }
                        Ok(Msg::Wake) => {}
                        Ok(Msg::Closed) | Err(RecvTimeoutError::Disconnected) => return Err(Error::Exited),
                        Err(RecvTimeoutError::Timeout) => break,
                    }
                }
                Ok(())
            })();
            if res.as_ref().is_err_and(Error::is_cancelled) {
                // Let the plugin drop the stale work; it stays running.
                notify(r, "$/cancel", &json!({ "qid": qid }))?;
            }
            res
        });
        if let Err(e) = res
            && !e.is_cancelled()
        {
            log::warn!("plugin {}: {e}", self.manifest.id);
        }
    }

    fn execute(&self, result_id: &str, action_id: &str, query: &str) -> Result<ExecOutcome> {
        self.execute_with(result_id, action_id, query, &ExecCtx::none())
    }

    fn execute_with(&self, result_id: &str, action_id: &str, query: &str, ctx: &ExecCtx) -> Result<ExecOutcome> {
        let exec = ctx.exec;
        let mut params = json!({ "result_id": result_id, "action_id": action_id, "query": query, "exec": exec });
        if let Some(input) = &ctx.input {
            params["input"] = json!(input);
        }
        let out = self
            .with_running(|r| {
                // Listen before asking: output sent ahead of the reply must not be lost.
                *lock(&self.listener) = Some(Listener { exec, generation: r.generation, sink: ctx.sink.clone() });
                request(r, "execute", &params, EXEC_TIMEOUT)
            })
            .and_then(|v| outcome_from_reply(&v));
        if !matches!(&out, Ok(ExecOutcome::Output(o)) if o.streaming) {
            let mut l = lock(&self.listener);
            if l.as_ref().is_some_and(|l| l.exec == exec) {
                *l = None;
            }
        }
        out
    }

    fn cancel_output(&self, exec: u64) {
        {
            let mut l = lock(&self.listener);
            if l.as_ref().is_none_or(|l| l.exec != exec) {
                return; // already done or replaced
            }
            *l = None;
        }
        // Never start the plugin just to cancel; skip if it is gone.
        if let Some(r) = lock(&self.running).as_mut() {
            let _ = notify(r, "$/cancel", &json!({ "exec": exec }));
        }
    }

    fn icon(&self, icon_id: &str) -> Option<Icon> {
        let params = json!({ "icon_id": icon_id, "size": 64 });
        let v = self.with_running(|r| request(r, "icon", &params, ICON_TIMEOUT)).ok()?;
        let mut icon: Icon = serde_json::from_value(v).ok()?;
        absolutize(&mut icon, &self.dir);
        Some(icon)
    }

    fn reap_idle(&self) {
        if self.manifest.run.mode != Mode::OnDemand {
            return;
        }
        if lock(&self.listener).is_some() {
            return; // still streaming output
        }
        let mut guard = lock(&self.running);
        let idle = Duration::from_secs(self.manifest.run.idle_timeout_s);
        if guard.as_ref().is_some_and(|r| r.last_used.elapsed() >= idle)
            && let Some(mut r) = guard.take()
        {
            let _ = request(&mut r, "shutdown", &json!({}), Duration::from_millis(200));
            let _ = r.child.kill();
            let _ = r.child.wait();
        }
    }

    fn warm_up(&self) {
        if self.manifest.run.mode == Mode::Resident {
            let _ = self.with_running(|_| Ok(()));
        }
    }
}

impl Drop for ExternalPlugin {
    fn drop(&mut self) {
        if let Some(mut r) = lock(&self.running).take() {
            let _ = r.child.kill();
            let _ = r.child.wait();
        }
    }
}

#[cfg(test)]
mod protocol_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echo_plugin_roundtrip() {
        if !crate::test_support::python3() {
            return;
        }
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/plugins/echo-py");
        let manifest = Manifest::load(&dir).unwrap();
        let tmp = crate::test_support::tmp();
        let p = ExternalPlugin::with_data_dir(manifest, dir, json!({}), tmp.path().join("data"));
        assert_eq!(p.id(), "echo-py");
        let items = p.query(&Query { text: "hello", trigger: Some("echopy "), limit: 5 });
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "hello");
        let out = p.execute(&items[0].id, "noop", "hello").unwrap();
        assert!(!out.keeps_open());

        // "Stream back": the reference for output streaming.
        let got = Arc::new(Mutex::new(String::new()));
        let g = got.clone();
        let ctx = ExecCtx {
            input: None,
            exec: 1,
            sink: Arc::new(move |e| match e {
                OutputEvent::Append(t) => lock(&g).push_str(&t),
                OutputEvent::Done(_) => lock(&g).push('.'),
                _ => {}
            }),
        };
        let out = p.execute_with(&items[0].id, "stream", "hello world", &ctx).unwrap();
        assert!(matches!(out, ExecOutcome::Output(o) if o.streaming && o.title == "Echo"));
        let t = Instant::now();
        while !lock(&got).ends_with('.') && t.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(*lock(&got), "hello world .");
    }
}
