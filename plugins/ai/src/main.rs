//! AI plugin: `ai <question>` streams an answer into the launcher's output panel, and the
//! panel's input takes follow-up questions. Talks to the Anthropic Messages API or any
//! OpenAI-compatible chat API, as configured. JSON-RPC 2.0 over stdio (see docs/develop/protocol.md).
//!
//! With MCP servers configured (`mcp_servers`), the model can call their tools. A tool runs
//! at once when the config allows it, otherwise the answer pauses and the panel asks first.
//!
//! The main loop only reads stdin and answers at once; each answer streams from its own
//! thread, so `$/cancel` and new queries are never stuck behind a slow network or tool.

mod api;
mod config;
mod mcp;
mod oauth;

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};
use ureq::tls::{RootCerts, TlsConfig};

use api::{Event, Request, Stream, ToolCall, ToolResult, Turn};
use config::{Api, Config};
use mcp::{Mcp, Tools, display_name};
use oauth::OAuth;

/// Conversations kept for follow-ups (oldest dropped first).
const MAX_CONVERSATIONS: usize = 20;
/// Model requests per question before the plugin stops a chain of tool calls.
const MAX_ROUNDS: usize = 10;
const RETRY_CAP: Duration = Duration::from_secs(10);
const FOLLOW_UP: &str = "Ask a follow-up…";
const DECLINED: &str = "The user declined to run this tool.";

/// A failed request: HTTP status (`None` for network errors), body and `retry-after` seconds.
#[derive(Debug)]
pub struct HttpError {
    status: Option<u16>,
    body: String,
    retry_after: Option<u64>,
}

type Reader = Box<dyn BufRead + Send>;
/// Sends a request and returns the streamed body; swapped out in tests.
type Transport = Arc<dyn Fn(&Request) -> Result<Reader, HttpError> + Send + Sync>;
/// Writes one protocol message (stdout, shared by the main loop and the workers).
type Out = Arc<dyn Fn(Value) + Send + Sync>;

struct Chat {
    /// The `exec` that started it.
    id: u64,
    turns: Vec<Turn>,
    /// Tool calls waiting for the user's yes or no (the last turn's calls).
    pending: Vec<ToolCall>,
    /// Held by the answer that is changing `turns`; the next one waits for it.
    gate: Arc<Mutex<()>>,
    /// The stop flag of the latest answer, so a follow-up can stop the one before.
    busy: Option<Arc<AtomicBool>>,
}

impl Chat {
    fn new(id: u64) -> Self {
        Self { id, turns: vec![], pending: vec![], gate: Arc::default(), busy: None }
    }
}

/// Add a question; tool calls still waiting for approval are answered with no.
fn add_question(turns: &mut Vec<Turn>, pending: &mut Vec<ToolCall>, question: &str) {
    let declined: Vec<ToolResult> = std::mem::take(pending)
        .into_iter()
        .map(|call| ToolResult { id: call.id, text: DECLINED.into(), is_error: true })
        .collect();
    if !declined.is_empty() {
        turns.push(Turn::ToolResults(declined));
    }
    turns.push(Turn::User(question.to_string()));
}

/// Conversations, oldest first.
type Chats = Arc<Mutex<VecDeque<Chat>>>;

pub fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What the user chose for tool calls that wait for approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    Run,
    Deny,
}

struct Plugin {
    cfg: Arc<Config>,
    chats: Chats,
    /// Running answers by `exec`, so `$/cancel` can stop them.
    running: Arc<Mutex<HashMap<u64, Arc<AtomicBool>>>>,
    transport: Transport,
    out: Out,
    tools: Arc<dyn Tools>,
    mcp_http: mcp::HttpFn,
    /// Sign-ins to remote MCP servers, kept in the launcher's data folder for this plugin.
    oauth: Arc<OAuth>,
    /// Tools the user chose "Always allow" for, until the plugin stops.
    allowed: Arc<Mutex<HashSet<String>>>,
}

impl Plugin {
    fn new(transport: Transport, out: Out, mcp_http: mcp::HttpFn) -> Self {
        Self {
            cfg: Arc::default(),
            chats: Arc::default(),
            running: Arc::default(),
            transport,
            out,
            tools: Arc::new(Mcp::new(vec![], vec![], mcp_http.clone())),
            oauth: Arc::new(OAuth::new(mcp_http.clone(), None)),
            mcp_http,
            allowed: Arc::default(),
        }
    }

    fn handle(&mut self, method: &str, p: &Value) -> Option<Value> {
        let s = |k: &str| p[k].as_str().unwrap_or("");
        Some(match method {
            "initialize" => {
                let cfg = Config::from_settings(&p["settings"]);
                let data_dir = p["data_dir"].as_str().filter(|d| !d.is_empty()).map(std::path::Path::new);
                self.oauth = Arc::new(OAuth::new(self.mcp_http.clone(), data_dir));
                self.tools.stop();
                self.tools = Arc::new(
                    Mcp::new(cfg.servers.clone(), cfg.server_problems.clone(), self.mcp_http.clone())
                        .with_oauth(self.oauth.clone()),
                );
                self.cfg = Arc::new(cfg);
                json!({ "api": "0.1", "capabilities": { "streaming": false, "actions": true } })
            }
            "query" => json!({ "results": self.query(s("text"), !p["trigger"].is_null()) }),
            "execute" => self.execute(s("action_id"), s("query"), p["input"].as_str(), p["exec"].as_u64().unwrap_or(0)),
            "$/cancel" => {
                if let Some(flag) = p["exec"].as_u64().and_then(|e| lock(&self.running).get(&e).cloned()) {
                    flag.store(true, Ordering::SeqCst);
                }
                return None;
            }
            // Reply at once: the launcher kills the process 200 ms after asking. The servers
            // are stopped after the reply (see `main`); local ones also exit when their input
            // closes with this process.
            "shutdown" => json!({}),
            _ => return None,
        })
    }

    fn query(&self, text: &str, prefixed: bool) -> Vec<Value> {
        let text = text.trim();
        let problems = self.cfg.problems();
        let icon = json!({ "glyph": "✦" });
        if !prefixed {
            // A last-resort row for any search, only once the plugin can actually answer.
            if text.is_empty() || !problems.is_empty() {
                return vec![];
            }
            return vec![json!({
                "id": "ai:fallback", "title": format!("Ask AI: {text}"), "subtitle": self.cfg.model,
                "icon": icon, "score": 0.0, "fallback": true,
                "actions": [{ "id": "ask", "title": "Ask", "default": true, "output": true }],
            })];
        }
        if !problems.is_empty() {
            return vec![json!({
                "id": "ai:setup", "title": "AI is not set up",
                "subtitle": format!("Missing: {}", problems.join(", ")), "icon": icon, "score": 1.0,
                "actions": [{ "id": "setup", "title": "Open AI settings", "default": true,
                              "kind": "settings" }],
            })];
        }
        let mut rows = self.sign_in_rows(text);
        if text.is_empty() {
            return vec![json!({
                "id": "ai:empty", "title": "Ask AI anything…",
                "subtitle": format!("Type a question · {}", self.cfg.model), "icon": icon, "score": 1.0,
            })];
        }
        rows.push(json!({
            // Constant on purpose: frecency keys on the id (see the calculator).
            "id": "ai:ask", "title": text, "subtitle": format!("Ask {}", self.cfg.model), "icon": icon, "score": 1.0,
            "actions": [
                { "id": "ask", "title": "Ask", "default": true, "output": true },
                { "id": "copy", "title": "Copy question", "kind": "copy", "value": text },
            ],
        }));
        rows
    }

    /// The remote MCP servers the user signs in to, by name and url.
    fn sign_in_servers(&self) -> Vec<(&str, &str)> {
        self.cfg.servers.iter().filter_map(|c| Some((c.name.as_str(), Mcp::sign_in_url(c)?))).collect()
    }

    /// `ai sign in` / `ai sign out`: a row per server that uses OAuth, above the question.
    fn sign_in_rows(&self, text: &str) -> Vec<Value> {
        let t = text.to_lowercase();
        if !["sign", "sign in", "sign out", "login", "logout"].iter().any(|w| w.starts_with(&t) && t.len() >= 4) {
            return vec![];
        }
        self.sign_in_servers()
            .into_iter()
            .map(|(name, url)| {
                let signin = json!({ "id": format!("signin:{name}"), "title": "Sign in", "default": true });
                if self.oauth.signed_in(url) {
                    json!({
                        "id": format!("ai:signout:{name}"), "title": format!("Sign out of {name}"),
                        "subtitle": format!("Signed in · {url}"), "icon": { "glyph": "✦" }, "score": 1.1,
                        "actions": [
                            { "id": format!("signout:{name}"), "title": "Sign out", "default": true },
                            { "id": format!("signin:{name}"), "title": "Sign in again" },
                        ],
                    })
                } else {
                    json!({
                        "id": format!("ai:signin:{name}"), "title": format!("Sign in to {name}"),
                        "subtitle": format!("Opens the browser · {url}"), "icon": { "glyph": "✦" }, "score": 1.1,
                        "actions": [signin],
                    })
                }
            })
            .collect()
    }

    /// `signin:<name>`: prepare the sign-in and let the launcher open its page.
    fn sign_in(&self, name: &str) -> Value {
        let Some((_, url)) = self.sign_in_servers().into_iter().find(|(n, _)| *n == name) else {
            return json!({ "ok": false, "message": format!("{name} is not a remote MCP server to sign in to") });
        };
        // Signed in: reconnect, so the next question uses the new token.
        let tools = self.tools.clone();
        let who = name.to_string();
        let done: oauth::Done = Box::new(move |r| match r {
            Ok(()) => tools.stop(),
            Err(e) => eprintln!("ai: sign-in to {who}: {e}"),
        });
        match self.oauth.begin(url, done) {
            Ok(page) => json!({ "ok": true, "open_url": page }),
            Err(e) => json!({ "ok": false, "message": format!("Sign-in to {name}: {e}") }),
        }
    }

    fn execute(&self, action: &str, query: &str, input: Option<&str>, exec: u64) -> Value {
        let problems = self.cfg.problems();
        if !problems.is_empty() {
            return json!({ "ok": false, "message": format!("AI is not set up. Missing: {}", problems.join(", ")) });
        }
        if let Some(name) = action.strip_prefix("signin:") {
            return self.sign_in(name);
        }
        if let Some(name) = action.strip_prefix("signout:") {
            if let Some((_, url)) = self.sign_in_servers().into_iter().find(|(n, _)| *n == name) {
                self.oauth.sign_out(url);
                self.tools.stop();
            }
            return json!({ "ok": true });
        }
        let id = |prefix: &str| action.strip_prefix(prefix).and_then(|c| c.parse::<u64>().ok());
        if let Some(conv) = id("copy-answer:") {
            let answer = self.with_chat(conv, |c| {
                c.turns.iter().rev().find_map(|t| match t {
                    Turn::Assistant { text, .. } if !text.is_empty() => Some(text.clone()),
                    _ => None,
                })
            });
            return match answer.flatten() {
                Some(t) => json!({ "ok": true, "copy": t }),
                None => json!({ "ok": false, "message": "no answer to copy" }),
            };
        }
        let decision = [
            ("run-tools:", Decision::Run, false),
            ("always-tools:", Decision::Run, true),
            ("deny-tools:", Decision::Deny, false),
        ]
        .into_iter()
        .find_map(|(prefix, d, always)| id(prefix).map(|conv| (conv, d, always)));
        if let Some((conv, decision, always)) = decision {
            let Some(pending) = self.with_chat(conv, |c| std::mem::take(&mut c.pending)) else {
                return json!({ "ok": false, "message": "This conversation has expired; ask again." });
            };
            if pending.is_empty() {
                return json!({ "ok": false, "message": "Nothing is waiting to run." });
            }
            if always {
                lock(&self.allowed).extend(pending.iter().map(|c| c.name.clone()));
            }
            return self.answer(exec, conv, Some(decision), None);
        }
        // A follow-up continues its conversation; one that expired (the plugin was restarted)
        // starts a new one.
        let follow = id("followup:");
        let question = match follow {
            Some(_) => input.unwrap_or("").trim(),
            None if action == "ask" => query.trim(),
            None => return json!({ "ok": false, "message": format!("unknown action {action}") }),
        };
        if question.is_empty() {
            return json!({ "ok": false, "message": "nothing to ask" });
        }
        let conv = follow.filter(|c| self.with_chat(*c, |_| ()).is_some()).unwrap_or(exec);
        if conv == exec {
            let mut chats = lock(&self.chats);
            chats.push_back(Chat::new(exec));
            while chats.len() > MAX_CONVERSATIONS {
                chats.pop_front();
            }
        }
        self.answer(exec, conv, None, Some(question.to_string()))
    }

    /// Start answering `conv` from a thread and reply with the panel showing it so far. The
    /// question is added by the thread, once an earlier answer still running has finished
    /// writing its turns (it is stopped first), so the turns stay in order.
    fn answer(&self, exec: u64, conv: u64, resolve: Option<Decision>, question: Option<String>) -> Value {
        let cancel = Arc::new(AtomicBool::new(false));
        let Some((shown, gate)) = self.with_chat(conv, |c| {
            if let Some(old) = c.busy.replace(cancel.clone()) {
                old.store(true, Ordering::SeqCst);
            }
            let shown = match &question {
                Some(q) => {
                    let (mut turns, mut pending) = (c.turns.clone(), c.pending.clone());
                    add_question(&mut turns, &mut pending, q);
                    transcript(&turns)
                }
                None => transcript(&c.turns),
            };
            (shown, c.gate.clone())
        }) else {
            return json!({ "ok": false, "message": "This conversation has expired; ask again." });
        };
        lock(&self.running).insert(exec, cancel.clone());
        let job = Job {
            cfg: self.cfg.clone(),
            transport: self.transport.clone(),
            out: self.out.clone(),
            chats: self.chats.clone(),
            running: self.running.clone(),
            tools: self.tools.clone(),
            allowed: self.allowed.clone(),
            exec,
            conv,
            cancel,
        };
        std::thread::spawn(move || {
            let _turn = lock(&gate);
            if let Some(q) = question {
                with_chat(&job.chats, conv, |c| add_question(&mut c.turns, &mut c.pending, &q));
            }
            job.run(resolve);
        });
        json!({ "ok": true, "output": {
            "title": self.cfg.model, "text": shown, "streaming": true, "format": "markdown",
            "input": { "placeholder": FOLLOW_UP, "action": format!("followup:{conv}") },
        }})
    }

    fn with_chat<R>(&self, conv: u64, f: impl FnOnce(&mut Chat) -> R) -> Option<R> {
        with_chat(&self.chats, conv, f)
    }
}

fn with_chat<R>(chats: &Chats, conv: u64, f: impl FnOnce(&mut Chat) -> R) -> Option<R> {
    lock(chats).iter_mut().find(|c| c.id == conv).map(f)
}

/// How an answer (one `exec`) ended.
enum End {
    Answered,
    /// Paused: these calls need the user's approval.
    Ask(Vec<ToolCall>),
    Failed(String),
    Stopped,
}

/// One answer, streamed from its own thread: model requests and tool calls until the model
/// is done, a tool needs approval, or the user stops it.
struct Job {
    cfg: Arc<Config>,
    transport: Transport,
    out: Out,
    chats: Chats,
    running: Arc<Mutex<HashMap<u64, Arc<AtomicBool>>>>,
    tools: Arc<dyn Tools>,
    allowed: Arc<Mutex<HashSet<String>>>,
    exec: u64,
    conv: u64,
    cancel: Arc<AtomicBool>,
}

impl Job {
    fn send(&self, mut params: Value) {
        params["exec"] = json!(self.exec);
        (self.out)(json!({ "jsonrpc": "2.0", "method": "output", "params": params }));
    }

    fn emit(&self, text: &str) {
        self.send(json!({ "append": text }));
    }

    fn stopped(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    fn allowed(&self, name: &str) -> bool {
        self.tools.allowed(name) || lock(&self.allowed).contains(name)
    }

    fn run(self, resolve: Option<Decision>) {
        let end = self.rounds(resolve);
        lock(&self.running).remove(&self.exec);
        with_chat(&self.chats, self.conv, |c| {
            if c.busy.as_ref().is_some_and(|b| Arc::ptr_eq(b, &self.cancel)) {
                c.busy = None;
            }
        });
        if self.stopped() {
            return; // the launcher no longer listens
        }
        let conv = self.conv;
        let sign_in: Vec<Value> = self
            .tools
            .sign_in_needed()
            .into_iter()
            .map(|n| json!({ "id": format!("signin:{n}"), "title": format!("Sign in to {n}") }))
            .collect();
        match end {
            End::Answered => {
                let mut actions =
                    vec![json!({ "id": format!("copy-answer:{conv}"), "title": "Copy answer", "default": true })];
                actions.extend(sign_in);
                self.send(json!({ "actions": actions, "done": true }));
            }
            End::Ask(calls) => {
                let mut names: Vec<String> = calls.iter().map(|c| display_name(&c.name)).collect();
                names.sort();
                names.dedup();
                let always = match &names[..] {
                    [one] => format!("Always allow {one}"),
                    _ => "Always allow these tools".into(),
                };
                let mut actions = vec![
                    json!({ "id": format!("run-tools:{conv}"), "title": "Run", "default": true }),
                    json!({ "id": format!("always-tools:{conv}"), "title": always }),
                    json!({ "id": format!("deny-tools:{conv}"), "title": "Don't run" }),
                ];
                actions.extend(sign_in);
                self.send(json!({ "actions": actions, "done": true }));
            }
            End::Failed(e) => self.send(json!({ "error": e })),
            End::Stopped => {}
        }
    }

    fn rounds(&self, resolve: Option<Decision>) -> End {
        let (tools, problems) = self.tools.list();
        for p in problems {
            self.emit(&format!("⚠ {p}\n\n"));
        }
        let mut resolve = resolve;
        for round in 0.. {
            if let Some(d) = resolve.take() {
                self.run_calls(d);
                if self.stopped() {
                    return End::Stopped;
                }
            }
            if round == MAX_ROUNDS {
                self.emit(&format!("(stopped after {MAX_ROUNDS} rounds of tool calls)"));
                return End::Answered;
            }
            let Some(turns) = with_chat(&self.chats, self.conv, |c| c.turns.clone()) else {
                return End::Stopped;
            };
            let req = api::request(&self.cfg, &turns, &tools);
            let api = self.cfg.api.unwrap_or(Api::OpenAi);
            let (stream, error) = stream_answer(api, &self.transport, &req, &self.cancel, &|t| self.emit(t));
            let Turn::Assistant { text, calls, raw } = stream.into_turn() else { unreachable!() };
            if error.is_some() || self.stopped() || (text.is_empty() && calls.is_empty()) {
                // Even a partial or failed answer keeps the turns alternating.
                let text = if text.is_empty() { "(no answer)".to_string() } else { text };
                self.push(Turn::answer(text));
                return match error {
                    Some(e) => End::Failed(e),
                    None if self.stopped() => End::Stopped,
                    None => End::Answered,
                };
            }
            let has_text = !text.is_empty();
            self.push(Turn::Assistant { text, calls: calls.clone(), raw });
            if calls.is_empty() {
                return End::Answered;
            }
            if has_text {
                self.emit("\n\n");
            }
            if calls.iter().all(|c| self.allowed(&c.name)) {
                resolve = Some(Decision::Run);
                continue;
            }
            let ask: Vec<ToolCall> = calls.iter().filter(|c| !self.allowed(&c.name)).cloned().collect();
            for c in &ask {
                self.emit(&call_line(c));
            }
            self.emit("Run it? Enter runs, Tab shows the choices.\n");
            with_chat(&self.chats, self.conv, |c| c.pending = calls);
            return End::Ask(ask);
        }
        unreachable!()
    }

    /// Run (or decline) the calls of the conversation's last turn and add their results.
    fn run_calls(&self, decision: Decision) {
        let calls = with_chat(&self.chats, self.conv, |c| match c.turns.last() {
            Some(Turn::Assistant { calls, .. }) => calls.clone(),
            _ => vec![],
        })
        .unwrap_or_default();
        if calls.is_empty() {
            return;
        }
        let mut results = vec![];
        for c in &calls {
            if !self.stopped() {
                self.emit(&call_line(c));
            }
            let outcome = if self.stopped() {
                Err("Stopped by the user.".to_string())
            } else if decision == Decision::Deny && !self.allowed(&c.name) {
                Err(DECLINED.to_string())
            } else if c.input.is_null() {
                Err("The arguments were not valid JSON.".to_string())
            } else {
                self.tools.call(&c.name, &c.input, &self.cancel)
            };
            let r = match outcome {
                Ok(text) => ToolResult { id: c.id.clone(), text, is_error: false },
                Err(text) => ToolResult { id: c.id.clone(), text, is_error: true },
            };
            if !self.stopped() {
                self.emit(&result_line(&r));
            }
            results.push(r);
        }
        if !self.stopped() {
            self.emit("\n");
        }
        self.push(Turn::ToolResults(results));
    }

    fn push(&self, turn: Turn) {
        with_chat(&self.chats, self.conv, |c| c.turns.push(turn));
    }
}

/// Send `req` and pass the answer's text to `emit` as it arrives, until done, failed or
/// cancelled. Returns what arrived (text and tool calls) and the error, if any. Rate limits
/// and server errors get one retry before any text arrived.
fn stream_answer(
    api: Api,
    transport: &Transport,
    req: &Request,
    cancel: &AtomicBool,
    emit: &dyn Fn(&str),
) -> (Stream, Option<String>) {
    let mut stream = Stream::new(api);
    let mut retried = false;
    let reader = loop {
        match transport(req) {
            Ok(r) => break r,
            Err(e) if !retried && e.status.is_some_and(api::retryable) => {
                retried = true;
                std::thread::sleep(Duration::from_secs(e.retry_after.unwrap_or(2)).min(RETRY_CAP));
                if cancel.load(Ordering::SeqCst) {
                    return (stream, None);
                }
            }
            Err(e) => {
                let msg = match e.status {
                    Some(s) => api::http_error(s, &e.body),
                    None => format!("Could not reach the API: {}", e.body),
                };
                return (stream, Some(msg));
            }
        }
    };
    let mut done = false;
    for line in reader.lines() {
        if cancel.load(Ordering::SeqCst) {
            return (stream, None); // dropping the reader closes the connection
        }
        let line = match line {
            Ok(l) => l,
            Err(e) => return (stream, Some(format!("The answer stopped: {e}"))),
        };
        match stream.line(&line) {
            Some(Event::Text(t)) => emit(&t),
            Some(Event::Note(n)) => emit(&format!("\n\n{n}")),
            Some(Event::Error(e)) => return (stream, Some(e)),
            Some(Event::Done) => {
                done = true;
                break;
            }
            None => {}
        }
    }
    if stream.is_empty() && !done {
        return (stream, Some("The API closed the connection without an answer".into()));
    }
    (stream, None)
}

/// What the panel shows for a conversation: every question marked with `›`, then its answer,
/// with the tool calls made on the way.
fn transcript(turns: &[Turn]) -> String {
    let mut out = String::new();
    let mut calls: &[ToolCall] = &[];
    for t in turns {
        match t {
            Turn::User(q) => out += &format!("› {}\n\n", md_text(q)),
            Turn::Assistant { text, calls: c, .. } => {
                if !text.is_empty() {
                    out += &format!("{text}\n\n");
                }
                calls = c;
            }
            Turn::ToolResults(rs) => {
                for (c, r) in calls.iter().zip(rs) {
                    out += &call_line(c);
                    out += &result_line(r);
                }
                out += "\n";
            }
        }
    }
    out
}

fn shorten(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

/// The panel shows markdown: text from elsewhere (a question, an error) is escaped so it
/// shows as typed. Only characters that can start markup inside a line need it; every line
/// here starts with `›`, `⚙` or `→`.
fn md_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '~' | '|') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Inline code, with a fence longer than any backtick run inside.
fn md_code(s: &str) -> String {
    let longest = s.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest + 1);
    let pad = if s.starts_with('`') || s.ends_with('`') { " " } else { "" };
    format!("{fence}{pad}{s}{pad}{fence}")
}

/// ``⚙ fs.read_file `{"path":"notes.md"}` ``
fn call_line(c: &ToolCall) -> String {
    let args = match &c.input {
        Value::Object(m) if m.is_empty() => String::new(),
        Value::Null => " (invalid arguments)".into(),
        v => format!(" {}", md_code(&shorten(&v.to_string(), 200))),
    };
    format!("⚙ {}{args}\n", md_text(&display_name(&c.name)))
}

/// `→ ok, 1.2 KB` or `→ error: …`
fn result_line(r: &ToolResult) -> String {
    if r.is_error {
        let first = r.text.lines().next().unwrap_or("failed");
        return format!("→ {}\n", md_text(&shorten(first, 160)));
    }
    let n = r.text.len();
    let size = if n < 1024 { format!("{n} B") } else { format!("{:.1} KB", n as f64 / 1024.0) };
    format!("→ ok, {size}\n")
}

fn agent() -> &'static ureq::Agent {
    // One agent for the process: keeps connections alive and loads the OS roots once.
    static AGENT: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            // Trust the OS store, so internal servers with a company CA work.
            .tls_config(TlsConfig::builder().root_certs(RootCerts::PlatformVerifier).build())
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_recv_response(Some(Duration::from_secs(120)))
            .timeout_recv_body(Some(Duration::from_secs(600)))
            .user_agent(concat!("kairos-ai/", env!("CARGO_PKG_VERSION")))
            .build()
            .into()
    })
}

fn http(req: &Request) -> Result<Reader, HttpError> {
    let net = |e: &dyn std::fmt::Display| HttpError { status: None, body: e.to_string(), retry_after: None };
    let mut r = agent().post(&req.url);
    for (k, v) in &req.headers {
        r = r.header(k, v);
    }
    let resp = r.send(req.body.to_string()).map_err(|e| net(&e))?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        let retry_after =
            resp.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|s| s.trim().parse().ok());
        let body = resp.into_body().with_config().limit(64 * 1024).read_to_string().unwrap_or_default();
        return Err(HttpError { status: Some(status), body, retry_after });
    }
    Ok(Box::new(BufReader::new(resp.into_body().into_reader())))
}

/// HTTP for remote MCP servers.
fn mcp_http(req: &mcp::HttpReq) -> Result<mcp::HttpResp, String> {
    let mut r = ureq::http::Request::builder().method(req.method).uri(&req.url);
    for (k, v) in &req.headers {
        r = r.header(k, v);
    }
    let r = r.body(req.body.clone().unwrap_or_default()).map_err(|e| format!("bad request: {e}"))?;
    let r = agent().configure_request(r).timeout_global(Some(req.timeout)).build();
    let resp = agent().run(r).map_err(|e| format!("could not reach the server: {e}"))?;
    let header = |k: &str| resp.headers().get(k).and_then(|v| v.to_str().ok()).map(str::to_string);
    let (session, content_type) = (header("mcp-session-id"), header("content-type").unwrap_or_default());
    Ok(mcp::HttpResp {
        status: resp.status().as_u16(),
        session,
        content_type,
        www_authenticate: header("www-authenticate"),
        body: Box::new(BufReader::new(resp.into_body().into_reader())),
    })
}

fn main() {
    let stdout = Arc::new(Mutex::new(std::io::stdout()));
    let out: Out = Arc::new(move |msg| {
        let mut o = lock(&stdout);
        let _ = writeln!(o, "{msg}");
        let _ = o.flush();
    });
    let mut plugin = Plugin::new(Arc::new(http), out.clone(), Arc::new(mcp_http));
    // stdin closing means: exit
    for line in std::io::stdin().lock().lines().map_while(Result::ok) {
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        let method = msg["method"].as_str().unwrap_or("");
        let reply = plugin.handle(method, &msg["params"]);
        if msg["id"].is_null() {
            continue; // a notification
        }
        out(match reply {
            Some(result) => json!({ "jsonrpc": "2.0", "id": msg["id"], "result": result }),
            None => {
                json!({ "jsonrpc": "2.0", "id": msg["id"], "error": { "code": -32601, "message": "unknown method" } })
            }
        });
        if method == "shutdown" {
            break;
        }
    }
    // Stop local MCP servers with us.
    plugin.tools.stop();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const ANTHROPIC_SSE: &str = "event: message_start\ndata: {\"type\":\"message_start\"}\n\n\
        data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\
        data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\" world\"}}\n\
        data: {\"type\":\"message_stop\"}\n";

    type Sent = Arc<Mutex<Vec<Value>>>;
    type Seen = Arc<Mutex<Vec<Request>>>;

    /// A configured plugin whose HTTP answers come from `reply`, recording what it sends.
    fn plugin(reply: impl Fn(usize) -> Result<Reader, HttpError> + Send + Sync + 'static) -> (Plugin, Sent, Seen) {
        let sent: Sent = Arc::default();
        let seen: Seen = Arc::default();
        let (s, r) = (sent.clone(), seen.clone());
        let out: Out = Arc::new(move |v| lock(&s).push(v));
        let transport: Transport = Arc::new(move |req| {
            let n = {
                let mut seen = lock(&r);
                seen.push(req.clone());
                seen.len()
            };
            reply(n)
        });
        let mut p = Plugin::new(transport, out, Arc::new(|_| Err("no http in tests".into())));
        p.handle(
            "initialize",
            &json!({"settings": {
                "api": "anthropic", "base_url": "https://api.anthropic.com", "model": "claude-x", "api_key": "sk"
            }}),
        );
        (p, sent, seen)
    }

    fn ok(body: &'static str) -> Result<Reader, HttpError> {
        Ok(Box::new(Cursor::new(body.as_bytes())))
    }

    /// The output notifications for `exec`, once one says `done` or `error` (3 s at most).
    fn outputs(sent: &Sent, exec: u64) -> Vec<Value> {
        for _ in 0..300 {
            let got: Vec<Value> =
                lock(sent).iter().filter(|m| m["params"]["exec"] == exec).map(|m| m["params"].clone()).collect();
            if got.iter().any(|p| p.get("done").is_some() || p.get("error").is_some()) {
                return got;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("no end of stream for exec {exec}: {:?}", lock(sent));
    }

    fn appended(o: &[Value]) -> String {
        o.iter().filter_map(|p| p["append"].as_str()).collect()
    }

    #[test]
    fn unconfigured_explains_and_stays_out_of_plain_searches() {
        let mut p = Plugin::new(Arc::new(|_| ok("")), Arc::new(|_| {}), Arc::new(|_| Err("no http".into())));
        p.handle("initialize", &json!({"settings": {"api": "openai"}}));
        let r = p.query("hi", true);
        assert_eq!(r[0]["title"], "AI is not set up");
        assert_eq!(r[0]["subtitle"], "Missing: base_url, model");
        assert_eq!(r[0]["actions"][0]["kind"], "settings");
        assert!(p.query("hi", false).is_empty());
        assert_eq!(p.execute("ask", "hi", None, 1)["ok"], false);
    }

    #[test]
    fn sign_in_rows_and_action() {
        let mut p = Plugin::new(Arc::new(|_| ok("")), Arc::new(|_| {}), Arc::new(|_| Err("no http".into())));
        p.handle(
            "initialize",
            &json!({"settings": {"api": "openai", "base_url": "http://localhost:1/v1", "model": "m", "mcp_servers": {
                "tracker": {"url": "https://tracker.example.com/mcp"},
                "gh": {"url": "https://h/mcp", "headers": {"Authorization": "Bearer t"}},
                "fs": {"command": ["x"]},
            }}}),
        );
        let r = p.query("sign in", true);
        assert_eq!(r.len(), 2, "one server signs in, then the question: {r:?}");
        assert_eq!(r[0]["title"], "Sign in to tracker");
        assert_eq!(r[0]["actions"][0]["id"], "signin:tracker");
        assert_eq!(r[1]["id"], "ai:ask", "asking stays possible");
        assert_eq!(p.query("what is a sign", true).len(), 1);
        // Without a data folder from the launcher there is nowhere to keep tokens.
        let reply = p.execute("signin:tracker", "", None, 1);
        assert!(reply["message"].as_str().unwrap().contains("update Kairos"), "{reply}");
        assert_eq!(p.execute("signin:gh", "", None, 1)["ok"], false, "gh has its own token");
    }

    #[test]
    fn rows_when_configured() {
        let (p, _, _) = plugin(|_| ok(""));
        let r = p.query(" what is rust ", true);
        assert_eq!((r[0]["id"].as_str(), r[0]["title"].as_str()), (Some("ai:ask"), Some("what is rust")));
        assert_eq!(r[0]["actions"][0]["output"], true);
        let f = p.query("firefox", false);
        assert_eq!((f[0]["title"].as_str(), f[0]["fallback"].as_bool()), (Some("Ask AI: firefox"), Some(true)));
        assert!(p.query("", false).is_empty());
        assert_eq!(p.query("", true)[0]["title"], "Ask AI anything…");
    }

    #[test]
    fn ask_streams_and_follow_ups_continue_the_conversation() {
        let (p, sent, seen) = plugin(|_| ok(ANTHROPIC_SSE));
        let reply = p.execute("ask", "first?", None, 7);
        let o = &reply["output"];
        assert_eq!(
            (o["title"].as_str(), o["text"].as_str(), o["streaming"].as_bool()),
            (Some("claude-x"), Some("› first?\n\n"), Some(true))
        );
        assert_eq!(o["input"]["action"], "followup:7");
        let got = outputs(&sent, 7);
        assert_eq!(appended(&got), "Hello world");
        let last = got.last().unwrap();
        assert_eq!(last["done"], true);
        assert_eq!(last["actions"][0]["id"], "copy-answer:7");

        let reply = p.execute("followup:7", "first?", Some("second?"), 8);
        assert_eq!(reply["output"]["text"], "› first?\n\nHello world\n\n› second?\n\n");
        assert_eq!(reply["output"]["input"]["action"], "followup:7", "same conversation");
        outputs(&sent, 8);
        let req = lock(&seen)[1].clone();
        let roles: Vec<&str> =
            req.body["messages"].as_array().unwrap().iter().map(|m| m["role"].as_str().unwrap()).collect();
        assert_eq!(roles, ["user", "assistant", "user"], "history goes along");
        assert_eq!(p.execute("copy-answer:7", "", None, 9), json!({"ok": true, "copy": "Hello world"}));
    }

    #[test]
    fn an_expired_conversation_starts_a_new_one() {
        let (p, sent, _) = plugin(|_| ok(ANTHROPIC_SSE));
        let reply = p.execute("followup:999", "", Some("again?"), 3);
        assert_eq!(reply["output"]["text"], "› again?\n\n");
        assert_eq!(reply["output"]["input"]["action"], "followup:3");
        outputs(&sent, 3);
    }

    #[test]
    fn http_errors_end_the_stream_with_a_message_after_one_retry() {
        let (p, sent, seen) = plugin(|_| {
            Err(HttpError {
                status: Some(529),
                body: r#"{"error":{"message":"Overloaded"}}"#.into(),
                retry_after: Some(0),
            })
        });
        p.execute("ask", "q", None, 1);
        let got = outputs(&sent, 1);
        assert_eq!(got.last().unwrap()["error"], "Overloaded (HTTP 529).");
        assert_eq!(lock(&seen).len(), 2, "retried once");

        let (p, sent, seen) = plugin(|_| Err(HttpError { status: Some(401), body: String::new(), retry_after: None }));
        p.execute("ask", "q", None, 2);
        assert!(outputs(&sent, 2).last().unwrap()["error"].as_str().unwrap().contains("API key"));
        assert_eq!(lock(&seen).len(), 1, "no retry for a bad key");

        let (p, sent, _) = plugin(|n| {
            if n == 1 {
                Err(HttpError { status: Some(503), body: String::new(), retry_after: Some(0) })
            } else {
                ok(ANTHROPIC_SSE)
            }
        });
        p.execute("ask", "q", None, 3);
        assert_eq!(appended(&outputs(&sent, 3)), "Hello world", "the retry succeeded");
    }

    /// A body that yields one line, then waits for the test to let it go on.
    struct Gate(Arc<Mutex<bool>>, usize);
    impl std::io::Read for Gate {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let line = b"data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"x\"}}\n";
            if self.1 > 0 {
                while !*lock(&self.0) {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            self.1 += 1;
            buf[..line.len()].copy_from_slice(line);
            Ok(line.len())
        }
    }

    #[test]
    fn cancel_stops_the_stream_and_keeps_the_partial_answer() {
        let open = Arc::new(Mutex::new(false));
        let o = open.clone();
        let (mut p, sent, _) = plugin(move |_| Ok(Box::new(BufReader::new(Gate(o.clone(), 0)))));
        p.execute("ask", "q", None, 5);
        for _ in 0..300 {
            if lock(&sent).iter().any(|m| m["params"]["append"] == "x") {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        p.handle("$/cancel", &json!({"exec": 5}));
        *lock(&open) = true;
        for _ in 0..300 {
            if lock(&p.running).is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(lock(&p.running).is_empty(), "the worker ended");
        let msgs = lock(&sent).clone();
        assert!(!msgs.iter().any(|m| m["params"].get("done").is_some()), "nothing after a cancel: {msgs:?}");
        let turns = p.with_chat(5, |c| c.turns.clone()).unwrap();
        assert!(matches!(turns.last().unwrap(), Turn::Assistant { .. }), "the partial answer is kept");
    }

    #[test]
    fn transcript_marks_questions() {
        let t = [Turn::User("q".into()), Turn::answer("a")];
        assert_eq!(transcript(&t), "› q\n\na\n\n");
        // The panel renders markdown: a question shows as typed.
        let q = vec![Turn::User("use *args or [x](y) in `py`".into())];
        assert_eq!(transcript(&q), "› use \\*args or \\[x\\](y) in \\`py\\`\n\n");
        assert_eq!(md_code("a`b"), "``a`b``");
        let call = ToolCall { id: "1".into(), name: "fs__read".into(), input: json!({"p": "a"}) };
        let t = [
            Turn::User("q".into()),
            Turn::Assistant { text: "Let me look.".into(), calls: vec![call.clone()], raw: None },
            Turn::ToolResults(vec![ToolResult { id: "1".into(), text: "x".repeat(2048), is_error: false }]),
            Turn::answer("Done."),
        ];
        assert_eq!(transcript(&t), "› q\n\nLet me look.\n\n⚙ fs.read `{\"p\":\"a\"}`\n→ ok, 2.0 KB\n\nDone.\n\n");
        let pending = [Turn::User("q".into()), Turn::Assistant { text: String::new(), calls: vec![call], raw: None }];
        assert_eq!(transcript(&pending), "› q\n\n", "waiting calls are asked about, not listed");
    }

    const TOOL_SSE: &str = "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tu1\",\"name\":\"fs__read\",\"input\":{}}}\n\
        data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"p\\\":\\\"a\\\"}\"}}\n\
        data: {\"type\":\"content_block_stop\",\"index\":0}\n\
        data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}\n\
        data: {\"type\":\"message_stop\"}\n";

    #[derive(Default)]
    struct FakeTools {
        allow: bool,
        /// Calls run until they are stopped.
        slow: bool,
        problems: Mutex<Vec<String>>,
        calls: Mutex<Vec<(String, Value)>>,
    }

    impl Tools for FakeTools {
        fn list(&self) -> (Vec<api::ToolDef>, Vec<String>) {
            let def =
                api::ToolDef { name: "fs__read".into(), description: "Read".into(), schema: json!({"type": "object"}) };
            (vec![def], std::mem::take(&mut *lock(&self.problems)))
        }
        fn allowed(&self, _: &str) -> bool {
            self.allow
        }
        fn call(&self, name: &str, input: &Value, cancel: &AtomicBool) -> Result<String, String> {
            lock(&self.calls).push((name.into(), input.clone()));
            if self.slow {
                while !cancel.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(5));
                }
                return Err("stopped".into());
            }
            Ok("file text".into())
        }
    }

    /// A plugin with fake tools whose model first calls `fs__read`, then answers.
    fn tool_plugin(allow: bool) -> (Plugin, Sent, Seen, Arc<FakeTools>) {
        let (mut p, sent, seen) = plugin(|n| ok(if n % 2 == 1 { TOOL_SSE } else { ANTHROPIC_SSE }));
        let tools = Arc::new(FakeTools { allow, ..Default::default() });
        p.tools = tools.clone();
        (p, sent, seen, tools)
    }

    /// The tool results the request carries (Anthropic shape).
    fn results_sent(req: &Request) -> Vec<Value> {
        let msgs = req.body["messages"].as_array().unwrap();
        msgs.iter()
            .flat_map(|m| m["content"].as_array().cloned().unwrap_or_default())
            .filter(|b| b["type"] == "tool_result")
            .collect()
    }

    #[test]
    fn allowed_tools_run_at_once() {
        let (p, sent, seen, tools) = tool_plugin(true);
        p.execute("ask", "read a", None, 7);
        let got = outputs(&sent, 7);
        assert_eq!(appended(&got), "⚙ fs.read `{\"p\":\"a\"}`\n→ ok, 9 B\n\nHello world");
        assert_eq!(got.last().unwrap()["actions"][0]["id"], "copy-answer:7");
        assert_eq!(*lock(&tools.calls), [("fs__read".to_string(), json!({"p": "a"}))]);
        let reqs = lock(&seen).clone();
        assert_eq!(reqs.len(), 2);
        assert_eq!(reqs[0].body["tools"][0]["name"], "fs__read");
        assert_eq!(
            results_sent(&reqs[1]),
            [json!({"type": "tool_result", "tool_use_id": "tu1", "content": "file text", "is_error": false})]
        );
        assert_eq!(p.execute("copy-answer:7", "", None, 8)["copy"], "Hello world");
        let shown = p.with_chat(7, |c| transcript(&c.turns)).unwrap();
        assert_eq!(shown, "› read a\n\n⚙ fs.read `{\"p\":\"a\"}`\n→ ok, 9 B\n\nHello world\n\n");
    }

    #[test]
    fn other_tools_wait_for_approval() {
        let (p, sent, seen, tools) = tool_plugin(false);
        p.execute("ask", "read a", None, 7);
        let got = outputs(&sent, 7);
        let last = got.last().unwrap();
        let ids: Vec<&str> = last["actions"].as_array().unwrap().iter().map(|a| a["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["run-tools:7", "always-tools:7", "deny-tools:7"]);
        assert_eq!(last["actions"][1]["title"], "Always allow fs.read");
        assert!(appended(&got).starts_with("⚙ fs.read `{\"p\":\"a\"}`\nRun it?"));
        assert!(lock(&tools.calls).is_empty(), "nothing ran yet");
        assert_eq!(lock(&seen).len(), 1);

        let reply = p.execute("run-tools:7", "read a", None, 8);
        assert_eq!(reply["output"]["text"], "› read a\n\n");
        assert_eq!(reply["output"]["input"]["action"], "followup:7");
        assert_eq!(appended(&outputs(&sent, 8)), "⚙ fs.read `{\"p\":\"a\"}`\n→ ok, 9 B\n\nHello world");
        assert_eq!(lock(&tools.calls).len(), 1);
        assert_eq!(results_sent(&lock(&seen)[1])[0]["is_error"], false);
        assert_eq!(p.execute("run-tools:7", "", None, 9)["ok"], false, "nothing waits any more");
        assert_eq!(p.execute("run-tools:999", "", None, 10)["ok"], false, "unknown conversation");
    }

    #[test]
    fn declined_tools_tell_the_model() {
        let (p, sent, seen, tools) = tool_plugin(false);
        p.execute("ask", "read a", None, 7);
        outputs(&sent, 7);
        p.execute("deny-tools:7", "", None, 8);
        assert_eq!(appended(&outputs(&sent, 8)), format!("⚙ fs.read `{{\"p\":\"a\"}}`\n→ {DECLINED}\n\nHello world"));
        assert!(lock(&tools.calls).is_empty());
        let r = &results_sent(&lock(&seen)[1])[0];
        assert_eq!((r["is_error"].as_bool(), r["content"].as_str()), (Some(true), Some(DECLINED)));
    }

    #[test]
    fn always_allow_lasts_for_the_session() {
        let (p, sent, _, tools) = tool_plugin(false);
        p.execute("ask", "read a", None, 7);
        outputs(&sent, 7);
        p.execute("always-tools:7", "", None, 8);
        outputs(&sent, 8);
        p.execute("ask", "read b", None, 9);
        let got = outputs(&sent, 9);
        assert_eq!(got.last().unwrap()["actions"][0]["id"], "copy-answer:9", "no question this time");
        assert_eq!(lock(&tools.calls).len(), 2);
    }

    #[test]
    fn a_follow_up_declines_waiting_tools() {
        let (p, sent, seen, _) = tool_plugin(false);
        p.execute("ask", "read a", None, 7);
        outputs(&sent, 7);
        p.execute("followup:7", "", Some("never mind"), 8);
        outputs(&sent, 8);
        let req = lock(&seen)[1].clone();
        let last = req.body["messages"].as_array().unwrap().last().unwrap().clone();
        assert_eq!(last["role"], "user");
        assert_eq!(last["content"][0]["content"], DECLINED);
        assert_eq!(last["content"][1], json!({"type": "text", "text": "never mind"}));
    }

    #[test]
    fn a_follow_up_during_a_tool_call_keeps_the_turns_in_order() {
        let (mut p, sent, seen) = plugin(|n| ok(if n == 1 { TOOL_SSE } else { ANTHROPIC_SSE }));
        let tools = Arc::new(FakeTools { allow: true, slow: true, ..Default::default() });
        p.tools = tools.clone();
        p.execute("ask", "read a", None, 7);
        for _ in 0..300 {
            if !lock(&tools.calls).is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let reply = p.execute("followup:7", "", Some("next"), 8);
        assert!(reply["output"]["text"].as_str().unwrap().ends_with("› next\n\n"));
        assert_eq!(appended(&outputs(&sent, 8)), "Hello world");
        let turns = p.with_chat(7, |c| c.turns.clone()).unwrap();
        let kinds: Vec<&str> = turns
            .iter()
            .map(|t| match t {
                Turn::User(_) => "user",
                Turn::Assistant { .. } => "assistant",
                Turn::ToolResults(_) => "results",
            })
            .collect();
        assert_eq!(kinds, ["user", "assistant", "results", "user", "assistant"]);
        let msgs = lock(&seen)[1].body["messages"].clone();
        let last = msgs.as_array().unwrap().last().unwrap();
        assert_eq!(last["content"][0]["type"], "tool_result", "the call's result comes before the question");
        assert_eq!(last["content"][1]["text"], "next");
    }

    #[test]
    fn tool_rounds_are_limited() {
        let (mut p, sent, seen) = plugin(|_| ok(TOOL_SSE));
        let tools = Arc::new(FakeTools { allow: true, ..Default::default() });
        *lock(&tools.problems) = vec!["MCP server x: could not start".into()];
        p.tools = tools.clone();
        p.execute("ask", "loop", None, 3);
        let got = outputs(&sent, 3);
        assert_eq!(lock(&seen).len(), MAX_ROUNDS);
        let text = appended(&got);
        assert!(text.starts_with("⚠ MCP server x: could not start\n\n"), "{text}");
        assert!(text.ends_with("(stopped after 10 rounds of tool calls)"), "{text}");
        let turns = p.with_chat(3, |c| c.turns.clone()).unwrap();
        assert!(matches!(turns.last(), Some(Turn::ToolResults(_))), "every call has its result");
    }
}
