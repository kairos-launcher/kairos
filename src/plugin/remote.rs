//! Remote plugin: the same JSON-RPC 2.0 protocol as `external`, one POST per request to an
//! `https://` URL instead of lines on a child's stdio.
//!
//! The reply is either a single JSON-RPC response (`application/json`) or newline-delimited
//! JSON (`application/x-ndjson`): zero or more `results` notifications, then the response.
//! A superseded query stops reading and drops the connection, which is the cancellation.

use std::io::{BufRead, BufReader, Read};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use ureq::tls::{Certificate, PemItem, RootCerts, TlsConfig, parse_pem};

use crate::core::proxy::Proxy;

use super::error::{Error, Result};
use super::external::{collect_notes, outcome_from_reply, parse_results};
use super::manifest::{Auth, Manifest};
use super::{ExecOutcome, Icon, Provider, Query, ResultItem};
use crate::core::sync::lock;

const API: &str = "0.1";
const INIT_TIMEOUT: Duration = Duration::from_secs(5);
const EXEC_TIMEOUT: Duration = Duration::from_secs(15);
const ICON_TIMEOUT: Duration = Duration::from_secs(2);
/// Wait this long after a keystroke before sending; a newer keystroke cancels the request.
const DEBOUNCE: Duration = Duration::from_millis(120);
/// A reply (or one streamed line) larger than this is an error.
const MAX_BODY: u64 = 1 << 20;
const MAX_BACKOFF: Duration = Duration::from_secs(60);

pub struct HttpReply {
    pub status: u16,
    pub content_type: String,
    /// Value of the `Kairos-Session` response header, if any.
    pub session: Option<String>,
    pub body: Box<dyn Read + Send>,
}

/// One HTTPS POST. A trait so tests can run the protocol without a network.
pub trait Http: Send + Sync {
    fn post(&self, url: &str, headers: &[(String, String)], body: String, timeout: Duration) -> Result<HttpReply>;
    /// Drop pooled connections (the launcher is idle).
    fn close_idle(&self) {}
}

pub struct UreqHttp {
    /// Built on the first request, and again when the proxy changed or the pool was dropped.
    agent: Mutex<Option<(Option<Proxy>, ureq::Agent)>>,
    /// Trust anchors from `ca_file`; replace the OS trust store when set.
    roots: Option<Vec<Certificate<'static>>>,
    /// Why `ca_file` could not be used; every request fails with it instead.
    ca_error: Option<String>,
}

impl UreqHttp {
    pub fn new(ca_file: Option<&str>) -> Self {
        let (roots, ca_error) = match ca_file.map(load_ca) {
            Some(Ok(c)) => (Some(c), None),
            Some(Err(e)) => (None, Some(e)),
            None => (None, None),
        };
        Self { agent: Mutex::new(None), roots, ca_error }
    }

    /// The agent for the current proxy. May block while the proxy is resolved (worker threads).
    fn current_agent(&self) -> ureq::Agent {
        let proxy = crate::core::proxy::current();
        let mut slot = lock(&self.agent);
        match &*slot {
            Some((p, agent)) if *p == proxy => agent.clone(),
            _ => {
                let agent = Self::agent(self.roots.as_deref(), proxy.as_ref());
                *slot = Some((proxy, agent.clone()));
                agent
            }
        }
    }

    fn agent(roots: Option<&[Certificate<'static>]>, proxy: Option<&Proxy>) -> ureq::Agent {
        agent_builder(roots, proxy)
            // Never follow redirects: a redirect could move the request (and the token
            // header) to another host.
            .max_redirects(0)
            .build()
            .into()
    }
}

/// Settings every request of the launcher itself shares: https only, the OS trust store
/// (or `roots`), our proxy, a small idle pool. Callers add their redirect policy.
pub fn agent_builder(
    roots: Option<&[Certificate<'static>]>,
    proxy: Option<&Proxy>,
) -> ureq::config::ConfigBuilder<ureq::typestate::AgentScope> {
    // Without `ca_file`, trust what the OS trusts (so CAs installed by IT work too).
    let roots = roots.map_or(RootCerts::PlatformVerifier, RootCerts::new_with_certs);
    ureq::Agent::config_builder()
        .tls_config(TlsConfig::builder().root_certs(roots).build())
        .https_only(true)
        .http_status_as_error(false)
        .max_idle_connections(2)
        .max_idle_connections_per_host(2)
        .user_agent(concat!("kairos/", env!("CARGO_PKG_VERSION")))
        .max_idle_age(Duration::from_secs(30))
        // Ours, not ureq's own reading of the environment (`crate::core::proxy`).
        .proxy(crate::core::proxy::ureq_proxy(proxy))
}

impl Http for UreqHttp {
    fn post(&self, url: &str, headers: &[(String, String)], body: String, timeout: Duration) -> Result<HttpReply> {
        if let Some(e) = &self.ca_error {
            return Err(e.as_str().into());
        }
        let agent = self.current_agent();
        let mut req = agent
            .post(url)
            .config()
            .timeout_global(Some(timeout))
            .build()
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, application/x-ndjson");
        for (k, v) in headers {
            req = req.header(k.as_str(), v.as_str());
        }
        let resp = req.send(body).map_err(|e| match e {
            ureq::Error::Timeout(_) => Error::Timeout,
            e => Error::Message(e.to_string()),
        })?;
        let (parts, body) = resp.into_parts();
        let text = |name: &str| parts.headers.get(name).and_then(|v| v.to_str().ok()).map(str::to_string);
        Ok(HttpReply {
            status: parts.status.as_u16(),
            content_type: text("content-type").unwrap_or_default(),
            session: text("kairos-session"),
            body: Box::new(body.into_reader()),
        })
    }

    fn close_idle(&self) {
        // Dropping the agent drops its connection pool (clones in flight keep theirs).
        *lock(&self.agent) = None;
    }
}

#[derive(Default)]
struct State {
    initialized: bool,
    session: Option<String>,
    failures: u32,
    retry_at: Option<Instant>,
}

/// Finds the API token for a plugin id.
type TokenLookup = dyn Fn(&str) -> Result<Option<String>, String> + Send + Sync;

pub struct RemotePlugin {
    manifest: Manifest,
    settings: Value,
    http: Box<dyn Http>,
    state: Mutex<State>,
    /// Id of the newest query; an older one gives up.
    qid: AtomicU64,
    next_id: AtomicU64,
    /// Looks up the API token (env / token file); replaced in tests.
    token: Box<TokenLookup>,
    debounce: Duration,
}

impl RemotePlugin {
    pub fn new(manifest: Manifest, settings: Value) -> Self {
        let ca_file = settings.get("ca_file").and_then(Value::as_str).map(str::to_string);
        Self::with_http(manifest, settings, Box::new(UreqHttp::new(ca_file.as_deref())))
    }

    pub fn with_http(manifest: Manifest, settings: Value, http: Box<dyn Http>) -> Self {
        Self {
            manifest,
            settings,
            http,
            state: Mutex::new(State::default()),
            qid: AtomicU64::new(0),
            next_id: AtomicU64::new(1),
            token: Box::new(token),
            debounce: DEBOUNCE,
        }
    }

    fn url(&self) -> &str {
        self.manifest.run.url.as_deref().unwrap_or_default()
    }

    fn headers(&self) -> Result<Vec<(String, String)>> {
        let mut h = vec![];
        let r = &self.manifest.remote;
        if r.auth == Auth::None {
            return Ok(h);
        }
        let Some(tok) = (self.token)(&self.manifest.id)? else {
            return Err(Error::Message(format!(
                "no API token (set {} or write {})",
                token_env(&self.manifest.id),
                token_file(&self.manifest.id).display()
            )));
        };
        if tok.contains(['\r', '\n']) {
            return Err("API token contains a newline".into());
        }
        match r.auth {
            Auth::Bearer => h.push(("Authorization".into(), format!("Bearer {tok}"))),
            Auth::Header => h.push((r.header.clone().unwrap_or_default(), tok)),
            Auth::None => {}
        }
        if let Some(s) = lock(&self.state).session.clone() {
            h.push(("Kairos-Session".into(), s));
        }
        Ok(h)
    }

    /// POST one request and read its reply. `on_note` gets each notification; `cancelled`
    /// is polled between streamed lines.
    fn call(
        &self,
        method: &'static str,
        params: &Value,
        timeout: Duration,
        cancelled: &dyn Fn() -> bool,
        on_note: &mut dyn FnMut(Value),
    ) -> Result<Value> {
        self.call_inner(params, method, timeout, cancelled, on_note).map_err(|e| e.during(method))
    }

    fn call_inner(
        &self,
        params: &Value,
        method: &str,
        timeout: Duration,
        cancelled: &dyn Fn() -> bool,
        on_note: &mut dyn FnMut(Value),
    ) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let body = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string();
        let reply = self.http.post(self.url(), &self.headers()?, body, timeout)?;
        match reply.status {
            200..=299 => {}
            s @ (401 | 403) => return Err(Error::Unauthorized(s)),
            s => return Err(Error::Http(s)),
        }
        if let Some(s) = reply.session {
            lock(&self.state).session = Some(s);
        }

        let mut lines = BufReader::new(reply.body);
        let streaming = reply.content_type.contains("ndjson");
        let mut answer = None;
        let mut buf = String::new();
        loop {
            if cancelled() {
                return Err(Error::Cancelled);
            }
            buf.clear();
            // A JSON body is one value (possibly pretty-printed); only NDJSON is line-wise.
            // The cap applies to the whole JSON body, or to each NDJSON line.
            let mut capped = (&mut lines).take(MAX_BODY);
            let n =
                if streaming { capped.read_line(&mut buf) } else { capped.read_to_string(&mut buf) }.map_err(|e| {
                    if e.kind() == std::io::ErrorKind::TimedOut {
                        Error::Timeout
                    } else {
                        Error::Message(format!("read: {e}"))
                    }
                })?;
            if n == 0 {
                break;
            }
            let Ok(v) = serde_json::from_str::<Value>(buf.trim()) else {
                if streaming {
                    continue;
                }
                return Err("reply is not JSON".into());
            };
            if v.get("id").and_then(Value::as_u64) == Some(id) {
                answer = Some(v);
                if streaming {
                    break;
                }
            } else if v.get("method").is_some() {
                on_note(v);
            }
            if !streaming {
                break;
            }
        }
        let v = answer.ok_or("no reply")?;
        match v.get("error") {
            Some(err) => Err(Error::Rpc(err.clone())),
            None => Ok(v.get("result").cloned().unwrap_or(Value::Null)),
        }
    }

    /// Send `initialize` once per plugin lifetime (the server may hand back a session id).
    fn ensure_initialized(&self, cancelled: &dyn Fn() -> bool) -> Result<()> {
        if lock(&self.state).initialized {
            return Ok(());
        }
        let params = json!({ "api": API, "host": "kairos", "settings": self.settings, "locale": "en" });
        // A server that rejects `initialize` is unusable, so it counts as a failure.
        self.call("initialize", &params, INIT_TIMEOUT, cancelled, &mut |_| {}).map_err(|e| match e.cause() {
            Error::Rpc(err) => Error::Rejected(err.clone()).during("initialize"),
            _ => e,
        })?;
        lock(&self.state).initialized = true;
        Ok(())
    }

    /// Run `f` unless the server is in back-off; track failures for the back-off.
    fn guarded<T>(&self, cancelled: &dyn Fn() -> bool, f: impl FnOnce() -> Result<T>) -> Result<T> {
        if lock(&self.state).retry_at.is_some_and(|t| Instant::now() < t) {
            return Err(Error::BackingOff);
        }
        let out = self.ensure_initialized(cancelled).and_then(|()| f());
        let mut st = lock(&self.state);
        match &out {
            Ok(_) => (st.failures, st.retry_at) = (0, None),
            // The server answered, it just said no: not an outage.
            Err(e) if e.is_answer() => {}
            Err(_) => {
                st.failures += 1;
                st.initialized = false;
                let wait = Duration::from_secs(1u64 << st.failures.min(6)).min(MAX_BACKOFF);
                st.retry_at = Some(Instant::now() + wait);
            }
        }
        out
    }
}

impl Provider for RemotePlugin {
    fn id(&self) -> &str {
        &self.manifest.id
    }

    fn query(&self, q: &Query) -> Vec<ResultItem> {
        let mut all = vec![];
        self.query_stream(q, &mut |items| all.extend(items));
        all
    }

    fn query_stream(&self, q: &Query, emit: &mut dyn FnMut(Vec<ResultItem>)) {
        if q.text.chars().count() < self.manifest.query.min_chars {
            return;
        }
        let qid = self.qid.fetch_add(1, Ordering::SeqCst) + 1;
        let cancelled = || self.qid.load(Ordering::SeqCst) != qid;
        std::thread::sleep(self.debounce);
        if cancelled() {
            return;
        }
        let budget = Duration::from_millis(self.manifest.query.timeout_ms);
        let params = json!({ "qid": qid, "text": q.text, "trigger": q.trigger, "limit": q.limit });
        let limit = q.limit;
        let mut sent = 0;
        let mut emit_batch = |mut items: Vec<ResultItem>| {
            sanitize(&mut items, limit.saturating_sub(sent));
            sent += items.len();
            if !items.is_empty() {
                emit(items);
            }
        };
        let res = self.guarded(&cancelled, || {
            // Each streamed note is shown as soon as its line arrives.
            let reply = self.call("query", &params, budget, &cancelled, &mut |n| {
                let mut items = vec![];
                collect_notes(std::slice::from_ref(&n), qid, &mut items);
                emit_batch(items);
            })?;
            emit_batch(parse_results(&reply));
            Ok(())
        });
        if let Err(e) = res
            && !e.is_cancelled()
        {
            log::warn!("plugin {}: {e}", self.manifest.id);
        }
    }

    fn execute(&self, result_id: &str, action_id: &str, query: &str) -> Result<ExecOutcome> {
        let params = json!({ "result_id": result_id, "action_id": action_id, "query": query });
        let never = || false;
        let v = self.guarded(&never, || self.call("execute", &params, EXEC_TIMEOUT, &never, &mut |_| {}))?;
        // Output in the reply is shown, but remote plugins cannot stream more of it yet.
        let mut out = outcome_from_reply(&v)?;
        if let ExecOutcome::Output(o) = &mut out {
            o.streaming = false;
        }
        Ok(out)
    }

    fn icon(&self, icon_id: &str) -> Option<Icon> {
        let params = json!({ "icon_id": icon_id, "size": 64 });
        let never = || false;
        let v = self.guarded(&never, || self.call("icon", &params, ICON_TIMEOUT, &never, &mut |_| {})).ok()?;
        let mut icon: Icon = serde_json::from_value(v).ok()?;
        icon.path = None;
        Some(icon)
    }

    fn reap_idle(&self) {
        self.http.close_idle();
    }
}

/// Read the certificates of a PEM file. They replace the OS trust store, so a server with a
/// private CA (company, self-hosted) can be reached, and nothing else is trusted for it.
fn load_ca(path: &str) -> Result<Vec<Certificate<'static>>, String> {
    let pem = std::fs::read(path).map_err(|e| format!("ca_file {path}: {e}"))?;
    let certs: Vec<_> = parse_pem(&pem)
        .filter_map(|i| match i {
            Ok(PemItem::Certificate(c)) => Some(Ok(c)),
            Ok(_) => None,
            Err(e) => Some(Err(format!("ca_file {path}: {e}"))),
        })
        .collect::<Result<_, _>>()?;
    if certs.is_empty() {
        return Err(format!("ca_file {path}: no certificate found (expected PEM)"));
    }
    Ok(certs)
}

/// Untrusted input: cap the count and drop anything that points at the local machine.
fn sanitize(items: &mut Vec<ResultItem>, room: usize) {
    items.truncate(room);
    for i in items {
        if let Some(icon) = &mut i.icon {
            icon.path = None; // a server must not make the launcher read local files
        }
    }
}

fn token_env(id: &str) -> String {
    format!("KAIROS_TOKEN_{}", id.to_ascii_uppercase().replace('-', "_"))
}

pub fn token_file(id: &str) -> std::path::PathBuf {
    super::secrets::user_dir().join(id)
}

/// The API token for plugin `id`: `KAIROS_TOKEN_<ID>` or `<config>/secrets/<id>`.
/// The file must not be readable by others.
fn token(id: &str) -> Result<Option<String>, String> {
    super::secrets::read(std::env::var(token_env(id)).ok().as_deref(), &token_file(id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::Arc;

    type Headers = Vec<(String, String)>;
    type Log = Arc<Mutex<Vec<(Value, Headers)>>>;
    type Responder = dyn Fn(&Value, &[(String, String)]) -> Result<(u16, &'static str, String), String> + Send + Sync;

    struct Fake {
        respond: Box<Responder>,
        log: Log,
    }

    impl Http for Fake {
        fn post(&self, _url: &str, headers: &[(String, String)], body: String, _t: Duration) -> Result<HttpReply> {
            let req: Value = serde_json::from_str(&body).unwrap();
            self.log.lock().unwrap().push((req.clone(), headers.to_vec()));
            let (status, ct, text) = (self.respond)(&req, headers)?;
            Ok(HttpReply {
                status,
                content_type: ct.into(),
                session: Some("sess1".into()),
                body: Box::new(Cursor::new(text.into_bytes())),
            })
        }
    }

    fn manifest(extra: &str) -> Manifest {
        Manifest::parse(
            &format!(
                "id = \"rem\"\nname = \"Rem\"\nversion = \"1\"\n[run]\nurl = \"https://example.com/rpc\"\n{extra}"
            ),
            "test",
        )
        .unwrap()
    }

    fn plugin(
        m: Manifest,
        respond: impl Fn(&Value) -> (u16, &'static str, String) + Send + Sync + 'static,
    ) -> (RemotePlugin, Log) {
        let log = Arc::new(Mutex::new(vec![]));
        let http = Fake { respond: Box::new(move |req, _| Ok(respond(req))), log: log.clone() };
        let mut p = RemotePlugin::with_http(m, json!({"k": 1}), Box::new(http));
        p.token = Box::new(|_| Ok(Some("sekret".into())));
        p.debounce = Duration::ZERO;
        (p, log)
    }

    fn reply(req: &Value, result: &Value) -> String {
        json!({"jsonrpc": "2.0", "id": req["id"], "result": result}).to_string()
    }

    fn q(text: &str) -> Query<'_> {
        Query { text, trigger: None, limit: 5 }
    }

    #[test]
    fn json_reply_sends_token_in_header_only_and_initializes_once() {
        let (p, log) = plugin(manifest(""), |req| {
            let result = match req["method"].as_str().unwrap() {
                "query" => json!({"results": [{"id": "1", "title": "hit"}]}),
                _ => json!({}),
            };
            (200, "application/json", reply(req, &result))
        });
        assert_eq!(p.query(&q("a"))[0].title, "hit");
        p.query(&q("b"));
        let log = log.lock().unwrap();
        let methods: Vec<_> = log.iter().map(|(r, _)| r["method"].as_str().unwrap()).collect();
        assert_eq!(methods, ["initialize", "query", "query"]);
        assert_eq!(log[0].0["params"]["settings"]["k"], 1);
        for (req, headers) in log.iter() {
            assert!(headers.contains(&("Authorization".into(), "Bearer sekret".into())));
            assert!(!req.to_string().contains("sekret"), "token leaked into the body");
        }
    }

    #[test]
    fn ndjson_streams_notes_and_drops_stale_qids() {
        let (p, _) = plugin(manifest(""), |req| {
            if req["method"] != "query" {
                return (200, "application/json", reply(req, &json!({})));
            }
            let qid = req["params"]["qid"].as_u64().unwrap();
            let note = |q: u64, id: &str| json!({"jsonrpc": "2.0", "method": "results", "params": {"qid": q, "results": [{"id": id, "title": id}], "done": false}});
            let body = [
                note(qid, "a"),
                note(qid - 1, "stale"),
                json!({"jsonrpc": "2.0", "id": req["id"], "result": {"results": [{"id": "z", "title": "z"}]}}),
            ]
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
            (200, "application/x-ndjson", body + "\n")
        });
        let mut ids: Vec<_> = p.query(&q("x")).into_iter().map(|r| r.id).collect();
        ids.sort();
        assert_eq!(ids, ["a", "z"]);
    }

    #[test]
    fn server_input_is_capped_and_local_icon_paths_removed() {
        let (p, _) = plugin(manifest(""), |req| {
            let items: Vec<_> =
                (0..20).map(|i| json!({"id": i.to_string(), "title": "t", "icon": {"path": "/etc/passwd"}})).collect();
            (200, "application/json", reply(req, &json!({"results": items})))
        });
        let r = p.query(&q("x"));
        assert_eq!(r.len(), 5);
        assert!(r.iter().all(|i| i.icon.as_ref().unwrap().path.is_none()));
    }

    #[test]
    fn auth_errors_back_off_and_skip_the_network() {
        let (p, log) = plugin(manifest(""), |_| (401, "application/json", "{}".into()));
        assert!(p.query(&q("x")).is_empty());
        let calls = log.lock().unwrap().len();
        assert!(p.query(&q("y")).is_empty());
        assert_eq!(log.lock().unwrap().len(), calls, "second query must not hit the network while backing off");
        assert!(p.execute("1", "open", "x").unwrap_err().to_string().contains("backing off"));
    }

    #[test]
    fn oversized_reply_is_an_error_not_a_hang() {
        let (p, _) = plugin(manifest(""), |req| {
            if req["method"] == "initialize" {
                return (200, "application/json", reply(req, &json!({})));
            }
            (200, "application/json", "x".repeat(2 << 20))
        });
        assert!(p.query(&q("x")).is_empty());
    }

    #[test]
    fn min_chars_skips_short_queries_and_a_newer_query_cancels_older() {
        let (p, log) =
            plugin(manifest("[query]\nmin_chars = 3\n"), |req| (200, "application/json", reply(req, &json!({}))));
        assert!(p.query(&q("ab")).is_empty());
        assert!(log.lock().unwrap().is_empty());

        let (mut p, log) = plugin(manifest(""), |req| (200, "application/json", reply(req, &json!({}))));
        p.debounce = Duration::from_millis(150);
        let p = Arc::new(p);
        let old = {
            let p = p.clone();
            std::thread::spawn(move || p.query(&q("a")))
        };
        std::thread::sleep(Duration::from_millis(30));
        p.query(&q("ab"));
        old.join().unwrap();
        let queries = log.lock().unwrap().iter().filter(|(r, _)| r["method"] == "query").count();
        assert_eq!(queries, 1, "the superseded query must not be sent");
    }

    #[test]
    fn header_auth_and_missing_token() {
        let m = manifest("[remote]\nauth = \"header\"\nheader = \"X-Api-Key\"\n");
        let (p, log) = plugin(m, |req| (200, "application/json", reply(req, &json!({}))));
        p.query(&q("x"));
        assert!(log.lock().unwrap()[0].1.contains(&("X-Api-Key".into(), "sekret".into())));

        let (mut p, _) = plugin(manifest(""), |req| (200, "application/json", reply(req, &json!({}))));
        p.token = Box::new(|_| Ok(None));
        assert!(p.execute("1", "a", "").unwrap_err().to_string().contains("no API token"));
    }

    #[test]
    fn notes_are_emitted_as_they_arrive_and_server_errors_do_not_back_off() {
        let (p, log) = plugin(manifest(""), |req| match req["method"].as_str().unwrap() {
            "query" => {
                let qid = req["params"]["qid"].as_u64().unwrap();
                let note = |id: &str| {
                    json!({"jsonrpc": "2.0", "method": "results", "params": {"qid": qid, "results": [{"id": id, "title": id}]}}).to_string()
                };
                (
                    200,
                    "application/x-ndjson",
                    format!(
                        "{}\n{}\n{}\n",
                        note("a"),
                        note("b"),
                        json!({"jsonrpc": "2.0", "id": req["id"], "result": {}})
                    ),
                )
            }
            "execute" => (
                200,
                "application/json",
                json!({"jsonrpc": "2.0", "id": req["id"], "error": {"code": 1, "message": "expired"}}).to_string(),
            ),
            _ => (200, "application/json", reply(req, &json!({}))),
        });
        let mut batches = vec![];
        p.query_stream(&q("x"), &mut |b| batches.push(b.len()));
        assert_eq!(batches, [1, 1], "one emit per streamed note");

        assert!(p.execute("1", "a", "").unwrap_err().to_string().contains("expired"));
        let before = log.lock().unwrap().len();
        assert_eq!(p.query(&q("y")).len(), 2, "an error reply to execute must not block queries");
        assert!(log.lock().unwrap().len() > before);
    }

    #[test]
    fn session_header_from_the_server_is_sent_back() {
        let (p, log) = plugin(manifest(""), |req| (200, "application/json", reply(req, &json!({}))));
        p.query(&q("a"));
        let log = log.lock().unwrap();
        let has = |i: usize| log[i].1.contains(&("Kairos-Session".into(), "sess1".into()));
        assert!(!has(0), "nothing to send before the first reply");
        assert!(has(1), "the query after initialize carries the session");
    }

    #[test]
    fn ca_file_must_hold_a_pem_certificate() {
        assert!(load_ca("/nonexistent/ca.pem").unwrap_err().contains("ca_file"));
        let tmp_f = crate::test_support::tmp();
        let f = tmp_f.path().join("f.pem");
        std::fs::write(&f, "not a certificate\n").unwrap();
        assert!(load_ca(f.to_str().unwrap()).unwrap_err().contains("no certificate"));
        // A bad ca_file fails requests instead of silently trusting the default roots.
        let http = UreqHttp::new(Some("/nonexistent/ca.pem"));
        assert!(http.post("https://example.com/", &[], String::new(), Duration::from_secs(1)).is_err());
    }

    const TEST_CA: &str = "-----BEGIN CERTIFICATE-----\nMIIBfDCCASGgAwIBAgIUJaP5wXemDZoIIRFV+8OUkyfYIA0wCgYIKoZIzj0EAwIw\nEjEQMA4GA1UEAwwHdGVzdC1jYTAgFw0yNjEwMTcyMDIyMDhaGA8yMTI2MDkyMzIw\nMjIwOFowEjEQMA4GA1UEAwwHdGVzdC1jYTBZMBMGByqGSM49AgEGCCqGSM49AwEH\nA0IABGazBHZTBAauDdBvNSwVL+dlSfAV/n/AVhiQ98YhdUTH0chxisEAwCeUNyzq\ndwTvdxIpAVLx0WhQSbrmEGeC2oijUzBRMB0GA1UdDgQWBBSXJm3e7pfCcbA+HR1T\nh+S8pDvv6DAfBgNVHSMEGDAWgBSXJm3e7pfCcbA+HR1Th+S8pDvv6DAPBgNVHRMB\nAf8EBTADAQH/MAoGCCqGSM49BAMCA0kAMEYCIQDmUyjWWxlf2atf4dRJfx+XJCvk\ni/gpjUI54pBdGu6YnQIhAIijxyzRdRTPeVU6jKOPoKspudCVpy7/P4ByEsta80kM\n-----END CERTIFICATE-----\n";

    #[test]
    fn a_valid_pem_ca_loads_and_survives_close_idle() {
        let tmp_f = crate::test_support::tmp();
        let f = tmp_f.path().join("f.pem");
        std::fs::write(&f, TEST_CA).unwrap();
        let certs = load_ca(f.to_str().unwrap()).unwrap();
        assert_eq!(certs.len(), 1);
        let http = UreqHttp::new(Some(f.to_str().unwrap()));
        assert!(http.ca_error.is_none() && http.roots.is_some());
        http.close_idle();
        assert_eq!(http.roots.as_ref().map(Vec::len), Some(1), "roots are kept when the pool is dropped");
        let plain = UreqHttp::new(None);
        assert!(plain.roots.is_none() && plain.ca_error.is_none());
    }

    #[test]
    fn plugin_reads_ca_file_from_its_settings() {
        let p = RemotePlugin::new(manifest(""), json!({"ca_file": "/nonexistent/ca.pem"}));
        assert!(p.execute("1", "a", "").is_err());
    }

    #[test]
    fn ndjson_cap_is_per_line_not_per_reply() {
        let big = "y".repeat(400 << 10);
        let (p, _) = plugin(manifest(""), |req| {
            if req["method"] != "query" {
                return (200, "application/json", reply(req, &json!({})));
            }
            let qid = req["params"]["qid"].as_u64().unwrap();
            let note = |id: &str| {
                json!({"jsonrpc": "2.0", "method": "results", "params": {"qid": qid, "results": [{"id": id, "title": "t", "subtitle": "%BIG%"}]}}).to_string()
            };
            let body = ["a", "b", "c"].map(note).join("\n").replace("%BIG%", &"y".repeat(400 << 10));
            (
                200,
                "application/x-ndjson",
                format!("{body}\n{}\n", json!({"jsonrpc": "2.0", "id": req["id"], "result": {}})),
            )
        });
        assert!(big.len() * 3 > MAX_BODY as usize);
        assert_eq!(p.query(&q("x")).len(), 3, "1.2 MiB over three lines is fine");
    }

    #[test]
    fn an_ndjson_line_over_the_cap_is_not_a_result() {
        let (p, _) = plugin(manifest(""), |req| {
            if req["method"] != "query" {
                return (200, "application/json", reply(req, &json!({})));
            }
            (200, "application/x-ndjson", format!("{}\n", "z".repeat(2 << 20)))
        });
        assert!(p.query(&q("x")).is_empty());
    }

    #[test]
    fn a_rejected_initialize_counts_as_a_failure() {
        let (p, log) = plugin(manifest(""), |req| {
            (
                200,
                "application/json",
                json!({"jsonrpc": "2.0", "id": req["id"], "error": {"message": "no"}}).to_string(),
            )
        });
        assert!(p.execute("1", "a", "").unwrap_err().to_string().contains("rejected:"));
        let calls = log.lock().unwrap().len();
        assert!(p.execute("1", "a", "").unwrap_err().to_string().contains("backing off"));
        assert_eq!(log.lock().unwrap().len(), calls);
    }

    #[test]
    fn http_errors_bad_json_and_bad_tokens() {
        let (p, _) = plugin(manifest(""), |_| (500, "application/json", "{}".into()));
        assert!(p.execute("1", "a", "").unwrap_err().to_string().contains("HTTP 500"));
        let (p, _) = plugin(manifest(""), |req| {
            if req["method"] == "initialize" {
                (200, "application/json", reply(req, &json!({})))
            } else {
                (200, "application/json", "nope".into())
            }
        });
        assert!(p.execute("1", "a", "").unwrap_err().to_string().contains("not JSON"));
        let (mut p, _) = plugin(manifest(""), |req| (200, "application/json", reply(req, &json!({}))));
        p.token = Box::new(|_| Ok(Some("a\nb".into())));
        assert!(p.execute("1", "a", "").unwrap_err().to_string().contains("newline"));
    }

    #[test]
    fn icon_and_close_idle_go_through_the_server() {
        let (p, _) = plugin(manifest(""), |req| match req["method"].as_str().unwrap() {
            "icon" => (200, "application/json", reply(req, &json!({"path": "/etc/passwd", "text": "x"}))),
            _ => (200, "application/json", reply(req, &json!({}))),
        });
        let icon = p.icon("i").expect("icon");
        assert!(icon.path.is_none(), "a server must not name local files");
        p.reap_idle();
        let (p, _) = plugin(manifest(""), |_| (500, "application/json", "{}".into()));
        assert!(p.icon("i").is_none());
    }

    #[test]
    fn token_comes_from_the_environment_or_a_private_file() {
        let id = format!("tt-{}", std::process::id());
        let env = token_env(&id);
        assert_eq!(env, format!("KAIROS_TOKEN_{}", id.to_ascii_uppercase().replace('-', "_")));
        assert!(token(&id).unwrap().is_none());
    }

    #[test]
    fn manifest_rules_for_remote_plugins() {
        let base = "id = \"r\"\nname = \"R\"\nversion = \"1\"\n";
        let parse = |run: &str| Manifest::parse(&format!("{base}{run}"), "t");
        assert!(parse("[run]\nurl = \"http://x.com/\"\n").unwrap_err().contains("https://"));
        assert!(parse("[run]\nurl = \"https://u:p@x.com/\"\n").unwrap_err().contains("credentials"));
        assert!(parse("[run]\nurl = \"https://\"\n").unwrap_err().contains("host"));
        assert!(parse("[run]\nurl = \"https://x.com/\"\nexec = [\"a\"]\n").unwrap_err().contains("exclusive"));
        assert!(parse("[run]\nurl = \"https://x.com/\"\n[remote]\nauth = \"header\"\n").is_err());
        let m = parse("[run]\nurl = \"https://x.com/\"\n").unwrap();
        assert!(!m.query.global && m.query.timeout_ms == 800);
        let m = parse("[run]\nurl = \"https://x.com/\"\n[query]\nglobal = true\ntimeout_ms = 300\n").unwrap();
        assert!(m.query.global && m.query.timeout_ms == 300);
    }
}
