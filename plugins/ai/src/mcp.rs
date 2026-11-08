//! A small MCP client: the tools of the servers in `mcp_servers`, for the model to call.
//!
//! Servers start on the first question that needs them (never while typing: the plugin runs
//! on every search) and live as long as the plugin process. Local servers speak JSON-RPC over
//! stdin/stdout; remote ones Streamable HTTP. Only tools are used (no resources or prompts).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::api::ToolDef;
use crate::config::{ServerConfig, ServerKind};
use crate::lock;
use crate::oauth::OAuth;

pub const PROTOCOL: &str = "2025-06-18";
/// Starting a server and listing its tools, including `npx` fetching it the first time.
/// Well below the launcher's 60 s limit for a silent stream, as nothing is shown meanwhile.
const START_TIMEOUT: Duration = Duration::from_secs(30);
/// One tool call; below the launcher's 60 s limit for a silent stream.
const CALL_TIMEOUT: Duration = Duration::from_secs(45);
/// Longest tool result passed to the model.
const MAX_RESULT: usize = 32 * 1024;

/// The tools the model may call. `Mcp` in the plugin, a fake in tests.
pub trait Tools: Send + Sync {
    /// The tools (starting the servers on first use), and problems to show, each only once.
    fn list(&self) -> (Vec<ToolDef>, Vec<String>);
    /// Whether the configuration lets this tool run without asking.
    fn allowed(&self, name: &str) -> bool;
    /// Run a tool: its output, or the error to tell the model.
    fn call(&self, name: &str, input: &Value, cancel: &AtomicBool) -> Result<String, String>;
    /// Stop the servers (local ones are killed, remote sessions ended).
    fn stop(&self) {}
    /// Servers that answered that the user must sign in first.
    fn sign_in_needed(&self) -> Vec<String> {
        vec![]
    }
}

/// How a tool is shown in the panel: `fs__read_file` → `fs.read_file`.
pub fn display_name(name: &str) -> String {
    name.replacen("__", ".", 1)
}

/// The name the model knows a server's tool by (`[A-Za-z0-9_-]`, at most 64 characters).
fn tool_name(server: &str, tool: &str) -> String {
    format!("{server}__{tool}")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .take(64)
        .collect()
}

// --- Connections ---------------------------------------------------------------------------

/// A JSON-RPC connection to one server.
trait Conn: Send {
    /// Send a request and wait for its result.
    fn request(&mut self, method: &str, params: Value, timeout: Duration, cancel: &AtomicBool)
    -> Result<Value, String>;
    fn notify(&mut self, method: &str, params: Value);
}

/// What a message from the server means for the request `want`.
enum Incoming {
    Answer(Result<Value, String>),
    /// The server asked something; send this reply.
    Reply(Value),
    Other,
}

fn incoming(msg: &Value, want: u64) -> Incoming {
    if let Some(method) = msg.get("method").and_then(Value::as_str) {
        return match msg.get("id").filter(|i| !i.is_null()) {
            Some(id) if method == "ping" => Incoming::Reply(json!({"jsonrpc": "2.0", "id": id, "result": {}})),
            Some(id) => Incoming::Reply(json!({"jsonrpc": "2.0", "id": id,
                "error": {"code": -32601, "message": "not supported by this client"}})),
            None => Incoming::Other, // a notification (log, progress, list changed)
        };
    }
    if msg["id"].as_u64() != Some(want) {
        return Incoming::Other;
    }
    Incoming::Answer(match msg.get("error") {
        Some(e) => Err(e["message"].as_str().unwrap_or("the server reported an error").to_string()),
        None => Ok(msg["result"].clone()),
    })
}

fn cancelled(id: u64, reason: &str) -> Value {
    json!({"requestId": id, "reason": reason})
}

/// A local server: a child process speaking line-delimited JSON-RPC.
struct Local {
    child: Child,
    /// `None` once closed (on drop).
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    next: u64,
}

impl Local {
    fn spawn(command: &[String], env: &[(String, String)]) -> Result<Self, String> {
        let mut child = spawn(command, env).map_err(|e| format!("could not start {}: {e}", command[0]))?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return Err("no pipes to the server".into());
        };
        let (tx, lines) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Ok(Self { child, stdin: Some(stdin), lines, next: 1 })
    }

    fn send(&mut self, msg: &Value) -> Result<(), String> {
        let stdin = self.stdin.as_mut().ok_or("the server was stopped")?;
        writeln!(stdin, "{msg}").and_then(|()| stdin.flush()).map_err(|_| "the server exited".to_string())
    }
}

fn spawn(command: &[String], env: &[(String, String)]) -> std::io::Result<Child> {
    let build = |program: &str| {
        let mut c = Command::new(program);
        c.args(&command[1..])
            .envs(env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            c.creation_flags(CREATE_NO_WINDOW);
        }
        c
    };
    match build(&command[0]).spawn() {
        // `npx`, `uvx` and friends are `.cmd` scripts on Windows, which are not found by name.
        Err(e) if cfg!(windows) && e.kind() == std::io::ErrorKind::NotFound && !command[0].contains('.') => {
            build(&format!("{}.cmd", command[0])).spawn()
        }
        r => r,
    }
}

impl Conn for Local {
    fn request(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
        cancel: &AtomicBool,
    ) -> Result<Value, String> {
        let id = self.next;
        self.next += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        let deadline = Instant::now() + timeout;
        loop {
            if cancel.load(Ordering::SeqCst) {
                self.notify("notifications/cancelled", cancelled(id, "stopped by the user"));
                return Err("stopped".into());
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                self.notify("notifications/cancelled", cancelled(id, "timed out"));
                return Err(format!("no answer within {} s", timeout.as_secs()));
            }
            match self.lines.recv_timeout(left.min(Duration::from_millis(100))) {
                Ok(line) => {
                    let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
                    match incoming(&msg, id) {
                        Incoming::Answer(r) => return r,
                        Incoming::Reply(reply) => self.send(&reply)?,
                        Incoming::Other => {}
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Err("the server exited".into()),
            }
        }
    }

    fn notify(&mut self, method: &str, params: Value) {
        let _ = self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }
}

impl Drop for Local {
    /// The MCP way: close its input, give it a moment to exit, then kill it.
    fn drop(&mut self) {
        self.stdin = None;
        let deadline = Instant::now() + Duration::from_millis(500);
        while Instant::now() < deadline {
            if !matches!(self.child.try_wait(), Ok(None)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// An HTTP request for a remote server (sent by `HttpFn`, swapped out in tests).
#[derive(Debug, Clone)]
pub struct HttpReq {
    pub method: &'static str,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
    pub timeout: Duration,
}

pub struct HttpResp {
    pub status: u16,
    /// The `Mcp-Session-Id` header.
    pub session: Option<String>,
    pub content_type: String,
    /// On a 401: where and how to sign in (OAuth).
    pub www_authenticate: Option<String>,
    pub body: Box<dyn BufRead + Send>,
}

pub type HttpFn = Arc<dyn Fn(&HttpReq) -> Result<HttpResp, String> + Send + Sync>;

/// A remote server over Streamable HTTP: every message is a POST; the answer comes back as
/// JSON or as a stream of server-sent events.
struct Remote {
    url: String,
    headers: Vec<(String, String)>,
    http: HttpFn,
    session: Option<String>,
    /// OAuth sign-in, for a server without an `Authorization` header in its config.
    auth: Option<Arc<OAuth>>,
    /// `initialize` params, to start a new session when the server forgot ours.
    init: Option<Value>,
    /// The protocol version the server chose, sent with every later request.
    version: String,
    next: u64,
}

enum Failure {
    /// HTTP 404 to a request with a session: the session ended.
    Expired,
    /// HTTP 401/403 from a server we sign in to, with its `WWW-Authenticate`.
    Unauthorized(Option<String>),
    Other(String),
}

/// What a server we must sign in to says when we are not (yet) signed in.
pub const SIGN_IN: &str = "sign in to use its tools";

impl Remote {
    fn req(&self, method: &'static str, body: Option<&Value>, timeout: Duration) -> HttpReq {
        let mut headers = self.headers.clone();
        if let Some(token) = self.auth.as_ref().and_then(|a| a.bearer(&self.url)) {
            headers.push(("Authorization".into(), format!("Bearer {token}")));
        }
        headers.push(("Accept".into(), "application/json, text/event-stream".into()));
        if body.is_some() {
            headers.push(("Content-Type".into(), "application/json".into()));
        }
        if let Some(s) = &self.session {
            headers.push(("Mcp-Session-Id".into(), s.clone()));
            headers.push(("MCP-Protocol-Version".into(), self.version.clone()));
        }
        HttpReq { method, url: self.url.clone(), headers, body: body.map(Value::to_string), timeout }
    }

    fn once(&mut self, method: &str, params: &Value, timeout: Duration, cancel: &AtomicBool) -> Result<Value, Failure> {
        let id = self.next;
        self.next += 1;
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let resp = (self.http)(&self.req("POST", Some(&msg), timeout)).map_err(Failure::Other)?;
        if let Some(s) = resp.session.clone().filter(|_| method == "initialize") {
            self.session = Some(s);
        }
        match resp.status {
            200..=299 => {}
            404 if self.session.is_some() && method != "initialize" => return Err(Failure::Expired),
            401 | 403 if self.auth.is_some() => return Err(Failure::Unauthorized(resp.www_authenticate)),
            401 | 403 => {
                return Err(Failure::Other(format!(
                    "not authorized (HTTP {}): check the server's headers or token",
                    resp.status
                )));
            }
            s => return Err(Failure::Other(format!("HTTP {s}"))),
        }
        if !resp.content_type.starts_with("text/event-stream") {
            let mut text = String::new();
            let _ = std::io::Read::read_to_string(&mut std::io::Read::take(resp.body, 8 << 20), &mut text);
            let v: Value =
                serde_json::from_str(&text).map_err(|_| Failure::Other("the server's answer is not JSON".into()))?;
            return match incoming(&v, id) {
                Incoming::Answer(r) => r.map_err(Failure::Other),
                _ => Err(Failure::Other("the server answered something else".into())),
            };
        }
        let mut data = String::new();
        for line in resp.body.lines() {
            if cancel.load(Ordering::SeqCst) {
                self.notify("notifications/cancelled", cancelled(id, "stopped by the user"));
                return Err(Failure::Other("stopped".into()));
            }
            let line = line.map_err(|e| Failure::Other(format!("the answer stopped: {e}")))?;
            if let Some(d) = line.strip_prefix("data:") {
                data.push_str(d.strip_prefix(' ').unwrap_or(d));
                continue;
            }
            if !line.is_empty() || data.is_empty() {
                continue;
            }
            let event = std::mem::take(&mut data);
            let Ok(v) = serde_json::from_str::<Value>(&event) else { continue };
            match incoming(&v, id) {
                Incoming::Answer(r) => return r.map_err(Failure::Other),
                Incoming::Reply(reply) => {
                    let _ = (self.http)(&self.req("POST", Some(&reply), Duration::from_secs(10)));
                }
                Incoming::Other => {}
            }
        }
        if let Ok(v) = serde_json::from_str::<Value>(&data)
            && let Incoming::Answer(r) = incoming(&v, id)
        {
            return r.map_err(Failure::Other);
        }
        Err(Failure::Other("the server ended the stream without an answer".into()))
    }
}

fn fail(f: Failure) -> String {
    match f {
        Failure::Other(e) => e,
        Failure::Expired => "the server ended the session".into(),
        Failure::Unauthorized(_) => SIGN_IN.into(),
    }
}

impl Remote {
    /// `once`, and on a 401 with a token: refresh it and try again. Still refused (or no
    /// token to refresh): the server is marked as needing a sign-in.
    fn authed(
        &mut self,
        method: &str,
        params: &Value,
        timeout: Duration,
        cancel: &AtomicBool,
    ) -> Result<Value, Failure> {
        let start = Instant::now();
        match self.once(method, params, timeout, cancel) {
            Err(Failure::Unauthorized(www)) => {
                let Some(auth) = self.auth.clone() else { return Err(Failure::Unauthorized(www)) };
                let retried = if auth.signed_in(&self.url) && auth.refresh(&self.url).is_some() {
                    self.once(method, params, timeout.saturating_sub(start.elapsed()), cancel)
                } else {
                    Err(Failure::Unauthorized(www))
                };
                if let Err(Failure::Unauthorized(www)) = &retried {
                    auth.need(&self.url, www.clone());
                }
                retried
            }
            r => r,
        }
    }

    fn initialize(&mut self, params: &Value, timeout: Duration, cancel: &AtomicBool) -> Result<Value, Failure> {
        self.version = PROTOCOL.into();
        let v = self.authed("initialize", params, timeout, cancel)?;
        if let Some(chosen) = v["protocolVersion"].as_str().filter(|s| !s.is_empty() && s.len() < 32) {
            self.version = chosen.into();
        }
        Ok(v)
    }
}

impl Conn for Remote {
    fn request(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
        cancel: &AtomicBool,
    ) -> Result<Value, String> {
        if method == "initialize" {
            self.init = Some(params.clone());
            return self.initialize(&params, timeout, cancel).map_err(fail);
        }
        // One deadline, also when the session has to be started again first.
        let deadline = Instant::now() + timeout;
        let left = || deadline.saturating_duration_since(Instant::now());
        match self.authed(method, &params, left(), cancel) {
            Ok(v) => Ok(v),
            Err(Failure::Expired) => {
                // Start a new session, then try once more.
                self.session = None;
                let init = self.init.clone().unwrap_or_default();
                self.initialize(&init, left(), cancel).map_err(fail)?;
                self.notify("notifications/initialized", json!({}));
                self.authed(method, &params, left(), cancel).map_err(fail)
            }
            Err(f) => Err(fail(f)),
        }
    }

    fn notify(&mut self, method: &str, params: Value) {
        let msg = json!({"jsonrpc": "2.0", "method": method, "params": params});
        let _ = (self.http)(&self.req("POST", Some(&msg), Duration::from_secs(10)));
    }
}

impl Drop for Remote {
    fn drop(&mut self) {
        if self.session.is_some() {
            let _ = (self.http)(&self.req("DELETE", None, Duration::from_secs(2)));
        }
    }
}

// --- Servers and their tools -----------------------------------------------------------------

struct Server {
    conn: Box<dyn Conn>,
}

/// Connect, say hello, and list the server's tools (all pages).
fn connect(conn: Box<dyn Conn>) -> Result<(Server, Vec<Value>), String> {
    let mut conn = conn;
    let never = AtomicBool::new(false);
    let deadline = Instant::now() + START_TIMEOUT;
    let left = || match deadline.saturating_duration_since(Instant::now()) {
        d if d.is_zero() => Err(format!("not ready within {} s", START_TIMEOUT.as_secs())),
        d => Ok(d),
    };
    let hello = json!({
        "protocolVersion": PROTOCOL,
        "capabilities": {},
        "clientInfo": {"name": "kairos-ai", "version": env!("CARGO_PKG_VERSION")},
    });
    conn.request("initialize", hello, left()?, &never)?;
    conn.notify("notifications/initialized", json!({}));
    let mut tools = vec![];
    let mut cursor: Option<String> = None;
    for _ in 0..20 {
        let params = cursor.as_ref().map_or(json!({}), |c| json!({"cursor": c}));
        let page = conn.request("tools/list", params, left()?, &never)?;
        tools.extend(page["tools"].as_array().cloned().unwrap_or_default());
        cursor = page["nextCursor"].as_str().filter(|c| !c.is_empty()).map(str::to_string);
        if cursor.is_none() {
            break;
        }
    }
    Ok((Server { conn }, tools))
}

/// A tool result as text for the model; `Err` when the tool reported an error.
fn result_text(r: &Value) -> Result<String, String> {
    let mut parts: Vec<String> = vec![];
    for c in r["content"].as_array().into_iter().flatten() {
        let s = |k: &str| c[k].as_str().unwrap_or_default().to_string();
        parts.push(match c["type"].as_str().unwrap_or_default() {
            "text" => s("text"),
            "resource" => c["resource"]["text"]
                .as_str()
                .map_or_else(|| format!("[resource {}]", c["resource"]["uri"].as_str().unwrap_or("")), str::to_string),
            "resource_link" => format!("[link {}]", s("uri")),
            other => format!("[{other}]"),
        });
    }
    if parts.is_empty()
        && let Some(sc) = r.get("structuredContent")
    {
        parts.push(sc.to_string());
    }
    let mut text = parts.join("\n");
    if text.len() > MAX_RESULT {
        let total = text.len();
        let mut cut = MAX_RESULT;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
        text += &format!("\n… (cut off: {} KB in all)", total / 1024);
    }
    if r["isError"] == true { Err(text) } else { Ok(text) }
}

/// Where a model-facing tool name leads.
#[derive(Clone)]
struct Route {
    server: usize,
    /// The tool's name on its server.
    tool: String,
    /// Runs without asking (`allow` in the config).
    allowed: bool,
}

struct State {
    servers: Vec<Arc<Mutex<Server>>>,
    tools: Vec<ToolDef>,
    routes: HashMap<String, Route>,
    problems: Vec<String>,
}

/// The configured MCP servers, started on first use.
pub struct Mcp {
    configs: Vec<ServerConfig>,
    http: HttpFn,
    /// Sign-in for remote servers without an `Authorization` header; `None`: no OAuth.
    oauth: Option<Arc<OAuth>>,
    state: Mutex<Option<State>>,
    /// Problems found before starting (bad config entries), shown with the first answer.
    early: Mutex<Vec<String>>,
}

impl Mcp {
    pub fn new(configs: Vec<ServerConfig>, problems: Vec<String>, http: HttpFn) -> Self {
        Self { configs, http, oauth: None, state: Mutex::default(), early: Mutex::new(problems) }
    }

    pub fn with_oauth(mut self, oauth: Arc<OAuth>) -> Self {
        self.oauth = Some(oauth);
        self
    }

    /// The url of a remote server the user signs in to (no `Authorization` header set).
    pub fn sign_in_url(cfg: &ServerConfig) -> Option<&str> {
        match &cfg.kind {
            ServerKind::Http { url, headers }
                if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("authorization")) =>
            {
                Some(url)
            }
            _ => None,
        }
    }

    fn open(&self, cfg: &ServerConfig) -> Result<Box<dyn Conn>, String> {
        Ok(match &cfg.kind {
            ServerKind::Stdio { command, env } => Box::new(Local::spawn(command, env)?),
            ServerKind::Http { url, headers } => Box::new(Remote {
                url: url.clone(),
                headers: headers.clone(),
                http: self.http.clone(),
                session: None,
                auth: Self::sign_in_url(cfg).and(self.oauth.clone()),
                init: None,
                version: PROTOCOL.into(),
                next: 1,
            }),
        })
    }

    fn start(&self) -> State {
        // In parallel: a slow server (npx downloading) does not hold up the others.
        let started: Vec<Result<(Server, Vec<Value>), String>> = std::thread::scope(|s| {
            let handles: Vec<_> =
                self.configs.iter().map(|c| s.spawn(move || self.open(c).and_then(connect))).collect();
            handles.into_iter().map(|h| h.join().unwrap_or_else(|_| Err("crashed while starting".into()))).collect()
        });
        let mut st = State { servers: vec![], tools: vec![], routes: HashMap::new(), problems: vec![] };
        for (cfg, r) in self.configs.iter().zip(started) {
            let (server, tools) = match r {
                Ok(x) => x,
                Err(e) => {
                    st.problems.push(format!("MCP server {}: {e}", cfg.name));
                    continue;
                }
            };
            let index = st.servers.len();
            st.servers.push(Arc::new(Mutex::new(server)));
            for t in tools {
                let Some(tool) = t["name"].as_str() else { continue };
                let name = tool_name(&cfg.name, tool);
                if st.routes.contains_key(&name) {
                    st.problems.push(format!("MCP server {}: tool {tool} skipped (name taken)", cfg.name));
                    continue;
                }
                let mut schema = t.get("inputSchema").cloned().filter(Value::is_object).unwrap_or(json!({}));
                schema["type"] = json!("object");
                let allowed = cfg.allow.iter().any(|a| a == "*" || a == tool);
                st.routes.insert(name.clone(), Route { server: index, tool: tool.to_string(), allowed });
                st.tools.push(ToolDef {
                    name,
                    description: t["description"].as_str().unwrap_or_default().to_string(),
                    schema,
                });
            }
        }
        st
    }

    fn route(&self, name: &str) -> Option<Route> {
        lock(&self.state).as_ref()?.routes.get(name).cloned()
    }
}

impl Tools for Mcp {
    fn stop(&self) {
        *lock(&self.state) = None;
    }

    fn sign_in_needed(&self) -> Vec<String> {
        let Some(oauth) = &self.oauth else { return vec![] };
        self.configs
            .iter()
            .filter(|c| Self::sign_in_url(c).is_some_and(|u| oauth.needs(u)))
            .map(|c| c.name.clone())
            .collect()
    }

    fn list(&self) -> (Vec<ToolDef>, Vec<String>) {
        let mut problems = std::mem::take(&mut *lock(&self.early));
        if self.configs.is_empty() {
            return (vec![], problems);
        }
        let mut state = lock(&self.state);
        let st = state.get_or_insert_with(|| self.start());
        problems.append(&mut st.problems);
        (st.tools.clone(), problems)
    }

    fn allowed(&self, name: &str) -> bool {
        self.route(name).is_some_and(|r| r.allowed)
    }

    fn call(&self, name: &str, input: &Value, cancel: &AtomicBool) -> Result<String, String> {
        let Route { server, tool, .. } = self.route(name).ok_or("no such tool")?;
        let server = lock(&self.state).as_ref().and_then(|st| st.servers.get(server).cloned()).ok_or("no such tool")?;
        let args = if input.is_null() { json!({}) } else { input.clone() };
        let r =
            lock(&server).conn.request("tools/call", json!({"name": tool, "arguments": args}), CALL_TIMEOUT, cancel)?;
        result_text(&r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn names() {
        assert_eq!(tool_name("fs", "read.file"), "fs__read_file");
        assert_eq!(tool_name("s", &"x".repeat(80)).len(), 64);
        assert_eq!(display_name("fs__read__all"), "fs.read__all");
    }

    #[test]
    fn results_become_text() {
        let r = json!({"content": [
            {"type": "text", "text": "a"},
            {"type": "image", "data": "…", "mimeType": "image/png"},
            {"type": "resource", "resource": {"uri": "file:///x", "text": "inside"}},
            {"type": "resource_link", "uri": "file:///y"},
        ]});
        assert_eq!(result_text(&r), Ok("a\n[image]\ninside\n[link file:///y]".into()));
        assert_eq!(
            result_text(&json!({"content": [{"type": "text", "text": "bad"}], "isError": true})),
            Err("bad".into())
        );
        assert_eq!(result_text(&json!({"structuredContent": {"n": 1}})), Ok("{\"n\":1}".into()));
        let long = result_text(&json!({"content": [{"type": "text", "text": "é".repeat(MAX_RESULT)}]})).unwrap();
        assert!(
            long.len() < MAX_RESULT + 100 && long.ends_with("(cut off: 64 KB in all)"),
            "{}",
            &long[long.len() - 40..]
        );
    }

    /// A remote server: sessions, an SSE answer with a ping in it, pagination, and a session
    /// that expires once.
    #[derive(Default)]
    struct Fake {
        log: Mutex<Vec<HttpReq>>,
        sessions: Mutex<u32>,
        expire_once: AtomicBool,
    }

    fn resp(status: u16, session: Option<String>, content_type: &str, body: String) -> Result<HttpResp, String> {
        Ok(HttpResp {
            status,
            session,
            content_type: content_type.into(),
            www_authenticate: None,
            body: Box::new(Cursor::new(body.into_bytes())),
        })
    }

    fn fake_http(f: Arc<Fake>) -> HttpFn {
        Arc::new(move |req: &HttpReq| {
            lock(&f.log).push(req.clone());
            if req.method == "DELETE" {
                return resp(200, None, "", String::new());
            }
            let msg: Value = serde_json::from_str(req.body.as_deref().unwrap_or("")).unwrap();
            if msg.get("result").is_some() || msg["id"].is_null() {
                return resp(202, None, "", String::new()); // a reply or a notification
            }
            let id = &msg["id"];
            let ok = |result: Value| json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string();
            match msg["method"].as_str().unwrap() {
                "initialize" => {
                    let mut n = lock(&f.sessions);
                    *n += 1;
                    resp(200, Some(format!("s{n}")), "application/json", ok(json!({"protocolVersion": "2025-03-26"})))
                }
                "tools/list" if msg["params"]["cursor"].is_null() => {
                    let ping = json!({"jsonrpc": "2.0", "id": "srv1", "method": "ping"});
                    let page = ok(json!({"tools": [{"name": "read", "description": "Read",
                        "inputSchema": {"type": "object", "properties": {}}}], "nextCursor": "p2"}));
                    resp(200, None, "text/event-stream", format!("event: message\ndata: {ping}\n\ndata: {page}\n\n"))
                }
                "tools/list" => resp(200, None, "application/json", ok(json!({"tools": [{"name": "write"}]}))),
                "tools/call" if f.expire_once.swap(false, Ordering::SeqCst) => resp(404, None, "", String::new()),
                "tools/call" => {
                    let text = format!("called {}", msg["params"]["name"].as_str().unwrap());
                    resp(200, None, "application/json", ok(json!({"content": [{"type": "text", "text": text}]})))
                }
                m => panic!("unexpected {m}"),
            }
        })
    }

    fn header<'a>(r: &'a HttpReq, k: &str) -> Option<&'a str> {
        r.headers.iter().find(|(h, _)| h == k).map(|(_, v)| v.as_str())
    }

    #[test]
    fn remote_server() {
        let fake = Arc::new(Fake { expire_once: AtomicBool::new(true), ..Default::default() });
        let cfg = ServerConfig {
            name: "web".into(),
            kind: ServerKind::Http {
                url: "https://h/mcp".into(),
                headers: vec![("Authorization".into(), "Bearer t".into())],
            },
            allow: vec!["read".into()],
        };
        let mcp = Mcp::new(vec![cfg], vec!["MCP server bad: no command".into()], fake_http(fake.clone()));
        let (tools, problems) = mcp.list();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["web__read", "web__write"]);
        assert_eq!(tools[1].schema, json!({"type": "object"}), "a missing schema becomes an object schema");
        assert_eq!(problems, ["MCP server bad: no command"]);
        assert!(mcp.list().1.is_empty(), "problems are shown once");
        assert!(mcp.allowed("web__read") && !mcp.allowed("web__write") && !mcp.allowed("web__nope"));

        let never = AtomicBool::new(false);
        assert_eq!(mcp.call("web__write", &json!({}), &never), Ok("called write".into()), "after a new session");
        assert!(mcp.call("web__nope", &json!({}), &never).is_err());
        mcp.stop();

        let log = lock(&fake.log).clone();
        assert!(log.iter().all(|r| header(r, "Authorization") == Some("Bearer t")));
        assert_eq!(header(&log[0], "Mcp-Session-Id"), None);
        assert_eq!(header(&log[1], "Mcp-Session-Id"), Some("s1"), "initialized");
        assert_eq!(header(&log[1], "MCP-Protocol-Version"), Some("2025-03-26"), "the version the server chose");
        let pong = log.iter().find(|r| r.body.as_deref().is_some_and(|b| b.contains("srv1"))).expect("ping answered");
        assert!(pong.body.as_deref().unwrap().contains("\"result\":{}"));
        let last = log.last().unwrap();
        assert_eq!((last.method, header(last, "Mcp-Session-Id")), ("DELETE", Some("s2")), "the new session ends");
    }

    /// A remote server that wants `Bearer at1` and otherwise answers 401 with where to sign in.
    fn guarded_http() -> HttpFn {
        Arc::new(|req: &HttpReq| {
            if header(req, "Authorization") != Some("Bearer at1") {
                return Ok(HttpResp {
                    status: 401,
                    session: None,
                    content_type: String::new(),
                    www_authenticate: Some(r#"Bearer resource_metadata="https://h/meta""#.into()),
                    body: Box::new(Cursor::new(vec![])),
                });
            }
            let msg: Value = serde_json::from_str(req.body.as_deref().unwrap_or("{}")).unwrap_or_default();
            if msg["id"].is_null() {
                return resp(202, None, "", String::new());
            }
            let result = match msg["method"].as_str() {
                Some("tools/list") => json!({"tools": [{"name": "read"}]}),
                _ => json!({}),
            };
            resp(
                200,
                None,
                "application/json",
                json!({"jsonrpc": "2.0", "id": msg["id"], "result": result}).to_string(),
            )
        })
    }

    #[test]
    fn a_remote_server_without_a_token_asks_to_sign_in() {
        let dir = crate::oauth::tests::TempDir::new("mcp");
        let remote = |headers: Vec<(String, String)>| ServerConfig {
            name: "n".into(),
            kind: ServerKind::Http { url: "https://h/mcp".into(), headers },
            allow: vec![],
        };
        let start = || {
            let oauth = Arc::new(OAuth::new(guarded_http(), Some(&dir.0)));
            Mcp::new(vec![remote(vec![])], vec![], guarded_http()).with_oauth(oauth)
        };
        let mcp = start();
        let (tools, problems) = mcp.list();
        assert!(tools.is_empty());
        assert_eq!(problems, [format!("MCP server n: {SIGN_IN}")]);
        assert_eq!(mcp.sign_in_needed(), ["n"]);

        // Signed in (tokens stored): the token is sent and the tools are there.
        let entry =
            json!({"https://h/mcp": {"client_id": "c", "token_endpoint": "https://h/t", "access_token": "at1"}});
        std::fs::write(dir.0.join("oauth.json"), entry.to_string()).unwrap();
        let mcp = start();
        assert_eq!(mcp.list().0.len(), 1);
        assert!(mcp.sign_in_needed().is_empty());

        // A server with its own Authorization header never signs in.
        let oauth = Arc::new(OAuth::new(guarded_http(), Some(&dir.0)));
        let mcp = Mcp::new(vec![remote(vec![("authorization".into(), "Bearer x".into())])], vec![], guarded_http())
            .with_oauth(oauth);
        assert!(mcp.list().1[0].contains("check the server's headers or token"));
        assert!(mcp.sign_in_needed().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn local_server() {
        let script = r#"
read l; echo '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}}}}'
read l; read l
echo '{"jsonrpc":"2.0","method":"notifications/message","params":{"level":"info","data":"hi"}}'
echo 'not json'
echo '{"jsonrpc":"2.0","id":"p","method":"ping"}'
read l; case "$l" in *'"id":"p"'*'"result"'*) ;; *) exit 1;; esac
echo "{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[{\"name\":\"$TOOL\"}]}}"
read l; echo '{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"pong"}],"isError":true}}'
read l; sleep 10
"#;
        let local = |name: &str, cmd: &str| ServerConfig {
            name: name.into(),
            kind: ServerKind::Stdio {
                command: vec!["sh".into(), "-c".into(), cmd.into()],
                env: vec![("TOOL".into(), "echo".into())],
            },
            allow: vec![],
        };
        let missing = ServerConfig {
            name: "missing".into(),
            kind: ServerKind::Stdio { command: vec!["/nonexistent/server".into()], env: vec![] },
            allow: vec![],
        };
        let mcp = Mcp::new(
            vec![local("t", script), local("gone", "exit 0"), missing],
            vec![],
            Arc::new(|_| Err("no".into())),
        );
        let (tools, problems) = mcp.list();
        assert_eq!(tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["t__echo"]);
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(problems[0].starts_with("MCP server gone: the server exited"), "{problems:?}");
        assert!(problems[1].starts_with("MCP server missing: could not start"), "{problems:?}");

        let never = AtomicBool::new(false);
        assert_eq!(mcp.call("t__echo", &json!({}), &never), Err("pong".into()));
        let cancel = Arc::new(AtomicBool::new(false));
        let c = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            c.store(true, Ordering::SeqCst);
        });
        let started = Instant::now();
        assert_eq!(mcp.call("t__echo", &json!({}), &cancel), Err("stopped".into()));
        assert!(started.elapsed() < Duration::from_secs(2));
        mcp.stop(); // kills the sleeping server
    }
}
