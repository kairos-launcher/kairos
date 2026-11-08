//! Signing in to remote MCP servers with OAuth, the way the MCP spec (2025-06-18) describes:
//! find the server's authorization server, register this app with it (dynamic client
//! registration), let the user approve in the browser (authorization code + PKCE, redirected
//! to a one-time listener on 127.0.0.1), then keep and refresh the tokens.
//!
//! Tokens live in `<data_dir>/oauth.json` (the launcher's private folder for this plugin),
//! readable only by the user, keyed by server url.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Map, Value, json};

use crate::config::check_url;
use crate::lock;
use crate::mcp::{HttpFn, HttpReq};

/// How long the browser may take to come back.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(300);
const HTTP_TIMEOUT: Duration = Duration::from_secs(20);
/// Refresh this long before the token runs out.
const EARLY: u64 = 60;

/// Called once a sign-in ends: `Ok` with the tokens stored, or why it failed.
pub type Done = Box<dyn FnOnce(Result<(), String>) + Send>;

pub struct OAuth {
    http: HttpFn,
    /// `oauth.json`; `None` when the launcher gave no data folder (an older version).
    file: Option<PathBuf>,
    /// Server url → its client and tokens, as stored.
    store: Mutex<Map<String, Value>>,
    /// Servers that answered 401 without a usable token, with their `WWW-Authenticate`.
    needed: Mutex<HashMap<String, Option<String>>>,
}

/// Where a server's sign-in happens.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Endpoints {
    authorize: String,
    token: String,
    register: Option<String>,
    scope: Option<String>,
}

impl OAuth {
    pub fn new(http: HttpFn, data_dir: Option<&Path>) -> Self {
        let file = data_dir.map(|d| d.join("oauth.json"));
        let store = file
            .as_ref()
            .and_then(|f| std::fs::read_to_string(f).ok())
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        Self { http, file, store: Mutex::new(store), needed: Mutex::default() }
    }

    pub fn signed_in(&self, url: &str) -> bool {
        lock(&self.store).get(url).is_some_and(|e| e["access_token"].is_string())
    }

    /// The access token for `url`, refreshed first when it is about to run out.
    pub fn bearer(&self, url: &str) -> Option<String> {
        let entry = lock(&self.store).get(url).cloned()?;
        let expires = entry["expires_at"].as_u64();
        if expires.is_some_and(|t| now() + EARLY >= t) {
            return self.refresh(url);
        }
        entry["access_token"].as_str().map(str::to_string)
    }

    /// A new access token from the refresh token. Without one (or when the server refuses it)
    /// the tokens are dropped: the user has to sign in again.
    pub fn refresh(&self, url: &str) -> Option<String> {
        let entry = lock(&self.store).get(url).cloned()?;
        let s = |k: &str| entry[k].as_str().unwrap_or_default().to_string();
        let got = Some(s("refresh_token")).filter(|r| !r.is_empty()).and_then(|refresh| {
            let mut form = vec![
                ("grant_type", "refresh_token".to_string()),
                ("refresh_token", refresh),
                ("client_id", s("client_id")),
                ("resource", url.to_string()),
            ];
            if entry["client_secret"].is_string() {
                form.push(("client_secret", s("client_secret")));
            }
            self.post_form(&s("token_endpoint"), &form).ok()
        });
        let Some(tokens) = got else {
            self.forget_tokens(url);
            return None;
        };
        let mut e = entry;
        keep_tokens(&mut e, &tokens);
        let access = e["access_token"].as_str().map(str::to_string);
        lock(&self.store).insert(url.to_string(), e);
        self.save();
        access
    }

    /// The server answered 401 (or 403) to us: it needs a sign-in.
    pub fn need(&self, url: &str, www_authenticate: Option<String>) {
        lock(&self.needed).insert(url.to_string(), www_authenticate);
    }

    pub fn needs(&self, url: &str) -> bool {
        lock(&self.needed).contains_key(url)
    }

    pub fn sign_out(&self, url: &str) {
        lock(&self.store).remove(url);
        self.save();
    }

    fn forget_tokens(&self, url: &str) {
        if let Some(e) = lock(&self.store).get_mut(url).and_then(Value::as_object_mut) {
            for k in ["access_token", "refresh_token", "expires_at"] {
                e.remove(k);
            }
        }
        self.save();
    }

    /// Prepare a sign-in to `url` and return the page to open in the browser. A thread waits
    /// for the browser to come back, stores the tokens and calls `done`.
    pub fn begin(self: &Arc<Self>, url: &str, done: Done) -> Result<String, String> {
        if self.file.is_none() {
            return Err("update Kairos to sign in (this version gives plugins no data folder)".into());
        }
        let www = lock(&self.needed).get(url).cloned().flatten();
        let ep = self.discover(url, www.as_deref())?;
        let register = ep.register.as_deref().ok_or(
            "this server needs a pre-registered OAuth client, which is not supported yet; \
             set an Authorization header instead",
        )?;
        let listener =
            TcpListener::bind("127.0.0.1:0").map_err(|e| format!("could not listen for the sign-in: {e}"))?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let redirect = format!("http://127.0.0.1:{port}/callback");
        let client = self.register(register, &redirect)?;
        let verifier = random(32)?;
        let state = random(16)?;
        let challenge = URL_SAFE_NO_PAD.encode(ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes()));
        let mut query = vec![
            ("response_type", "code".to_string()),
            ("client_id", client.id.clone()),
            ("redirect_uri", redirect.clone()),
            ("code_challenge", challenge),
            ("code_challenge_method", "S256".into()),
            ("state", state.clone()),
            ("resource", url.to_string()),
        ];
        if let Some(scope) = &ep.scope {
            query.push(("scope", scope.clone()));
        }
        let sep = if ep.authorize.contains('?') { '&' } else { '?' };
        let auth_url = format!("{}{sep}{}", ep.authorize, form(&query));
        let me = self.clone();
        let flow = Flow { url: url.to_string(), token: ep.token, client, redirect, verifier, state };
        std::thread::spawn(move || {
            let r = me.wait(&listener, &flow);
            if r.is_ok() {
                lock(&me.needed).remove(&flow.url);
            }
            done(r);
        });
        Ok(auth_url)
    }

    /// Answer the browser until it brings a code for this sign-in, then exchange it.
    fn wait(&self, listener: &TcpListener, flow: &Flow) -> Result<(), String> {
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let deadline = Instant::now() + SIGN_IN_TIMEOUT;
        while Instant::now() < deadline {
            let mut stream = match listener.accept() {
                Ok((s, _)) => s,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
                Err(e) => return Err(format!("the sign-in listener failed: {e}")),
            };
            let Some(params) = read_callback(&mut stream) else {
                page(&mut stream, 404, "Not found", "");
                continue;
            };
            let get = |k: &str| params.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
            // Anything without our state is not the browser coming back from this sign-in.
            if get("state").as_deref() != Some(flow.state.as_str()) {
                page(&mut stream, 400, "Wrong sign-in", "This link is not from the sign-in Kairos started.");
                continue;
            }
            if let Some(err) = get("error") {
                let why = get("error_description").unwrap_or(err);
                page(&mut stream, 400, "Sign-in failed", &why);
                return Err(format!("the sign-in failed: {why}"));
            }
            let Some(code) = get("code") else {
                page(&mut stream, 400, "Sign-in failed", "No code came back.");
                return Err("the server sent no code".into());
            };
            return match self.exchange(flow, &code) {
                Ok(()) => {
                    page(&mut stream, 200, "Signed in", "You can close this tab and go back to Kairos.");
                    Ok(())
                }
                Err(e) => {
                    page(&mut stream, 400, "Sign-in failed", &e);
                    Err(e)
                }
            };
        }
        Err("the sign-in timed out".into())
    }

    fn exchange(&self, flow: &Flow, code: &str) -> Result<(), String> {
        let mut f = vec![
            ("grant_type", "authorization_code".to_string()),
            ("code", code.to_string()),
            ("redirect_uri", flow.redirect.clone()),
            ("client_id", flow.client.id.clone()),
            ("code_verifier", flow.verifier.clone()),
            ("resource", flow.url.clone()),
        ];
        if let Some(secret) = &flow.client.secret {
            f.push(("client_secret", secret.clone()));
        }
        let tokens = self.post_form(&flow.token, &f)?;
        let mut e = json!({ "client_id": flow.client.id, "token_endpoint": flow.token });
        if let Some(secret) = &flow.client.secret {
            e["client_secret"] = json!(secret);
        }
        keep_tokens(&mut e, &tokens);
        if !e["access_token"].is_string() {
            return Err("the server sent no access token".into());
        }
        lock(&self.store).insert(flow.url.clone(), e);
        self.save();
        Ok(())
    }

    /// Find the authorization server of the MCP server at `url`.
    fn discover(&self, url: &str, www: Option<&str>) -> Result<Endpoints, String> {
        let (origin, path) = split_url(url);
        let mut candidates = vec![];
        if let Some(m) = www.and_then(|w| auth_param(w, "resource_metadata")) {
            candidates.push(m);
        }
        candidates.extend(well_known(&origin, &path, &["oauth-protected-resource"]));
        let resource = candidates.iter().find_map(|c| self.get_json(c));
        let issuer = resource
            .as_ref()
            .and_then(|r| r["authorization_servers"][0].as_str().map(str::to_string))
            .unwrap_or_else(|| origin.clone());
        let (i_origin, i_path) = split_url(&issuer);
        let mut meta_urls = well_known(&i_origin, &i_path, &["oauth-authorization-server", "openid-configuration"]);
        if !i_path.is_empty() {
            meta_urls.push(format!("{i_origin}{i_path}/.well-known/openid-configuration"));
        }
        let meta = meta_urls.iter().find_map(|u| self.get_json(u));
        let scope = www.and_then(|w| auth_param(w, "scope")).or_else(|| {
            let list = resource.as_ref()?["scopes_supported"].as_array()?;
            let s: Vec<&str> = list.iter().filter_map(Value::as_str).collect();
            (!s.is_empty()).then(|| s.join(" "))
        });
        let ep = match meta {
            Some(m) => {
                let methods = m["code_challenge_methods_supported"].as_array();
                if methods.is_some_and(|l| !l.iter().any(|x| x == "S256")) {
                    return Err("the server's sign-in does not support PKCE (S256)".into());
                }
                let s = |k: &str| m[k].as_str().map(str::to_string);
                Endpoints {
                    authorize: s("authorization_endpoint").ok_or("the server's sign-in has no authorize page")?,
                    token: s("token_endpoint").ok_or("the server's sign-in has no token endpoint")?,
                    register: s("registration_endpoint"),
                    scope,
                }
            }
            // The 2025-03-26 spec: no metadata means default paths on the server itself.
            None if resource.is_none() => Endpoints {
                authorize: format!("{origin}/authorize"),
                token: format!("{origin}/token"),
                register: Some(format!("{origin}/register")),
                scope,
            },
            None => return Err(format!("could not read the sign-in details of {issuer}")),
        };
        for u in [Some(&ep.authorize), Some(&ep.token), ep.register.as_ref()].into_iter().flatten() {
            check_url(u).map_err(|e| format!("the sign-in url {u} {e}"))?;
        }
        Ok(ep)
    }

    fn register(&self, endpoint: &str, redirect: &str) -> Result<Client, String> {
        let body = json!({
            "client_name": "Kairos",
            "redirect_uris": [redirect],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none",
        });
        let v = self.send("POST", endpoint, "application/json", body.to_string())?;
        let id = v["client_id"].as_str().ok_or("the server did not register Kairos (no client_id)")?;
        Ok(Client { id: id.into(), secret: v["client_secret"].as_str().map(str::to_string) })
    }

    fn get_json(&self, url: &str) -> Option<Value> {
        let req = HttpReq {
            method: "GET",
            url: url.into(),
            headers: vec![("Accept".into(), "application/json".into())],
            body: None,
            timeout: HTTP_TIMEOUT,
        };
        let resp = (self.http)(&req).ok().filter(|r| r.status == 200)?;
        serde_json::from_str::<Value>(&read_body(resp.body)).ok().filter(Value::is_object)
    }

    fn post_form(&self, url: &str, fields: &[(&str, String)]) -> Result<Value, String> {
        self.send("POST", url, "application/x-www-form-urlencoded", form(fields))
    }

    fn send(&self, method: &'static str, url: &str, content_type: &str, body: String) -> Result<Value, String> {
        let req = HttpReq {
            method,
            url: url.into(),
            headers: vec![("Content-Type".into(), content_type.into()), ("Accept".into(), "application/json".into())],
            body: Some(body),
            timeout: HTTP_TIMEOUT,
        };
        let resp = (self.http)(&req)?;
        let status = resp.status;
        let v: Value = serde_json::from_str(&read_body(resp.body)).unwrap_or(Value::Null);
        if !(200..300).contains(&status) {
            let why = v["error_description"].as_str().or(v["error"].as_str()).unwrap_or("");
            return Err(format!("the sign-in server answered HTTP {status} {why}").trim_end().to_string());
        }
        if !v.is_object() {
            return Err("the sign-in server's answer is not JSON".into());
        }
        Ok(v)
    }

    fn save(&self) {
        let Some(file) = &self.file else { return };
        let text = Value::Object(lock(&self.store).clone()).to_string();
        if let Err(e) = write_private(file, text.as_bytes()) {
            eprintln!("ai: could not save {}: {e}", file.display());
        }
    }
}

struct Client {
    id: String,
    secret: Option<String>,
}

/// One sign-in waiting for the browser.
struct Flow {
    url: String,
    token: String,
    client: Client,
    redirect: String,
    verifier: String,
    state: String,
}

/// Copy a token response into a stored entry; a refresh may leave out the refresh token.
fn keep_tokens(entry: &mut Value, tokens: &Value) {
    if let Some(a) = tokens["access_token"].as_str() {
        entry["access_token"] = json!(a);
    }
    if let Some(r) = tokens["refresh_token"].as_str() {
        entry["refresh_token"] = json!(r);
    }
    match tokens["expires_in"].as_u64() {
        Some(s) => entry["expires_at"] = json!(now() + s),
        None => {
            if let Some(e) = entry.as_object_mut() {
                e.remove("expires_at");
            }
        }
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn random(bytes: usize) -> Result<String, String> {
    let mut b = vec![0u8; bytes];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut b)
        .map_err(|_| "no random numbers on this system".to_string())?;
    Ok(URL_SAFE_NO_PAD.encode(b))
}

/// `https://host:port/a/b?q` → (`https://host:port`, `/a/b`), without a trailing `/`.
fn split_url(url: &str) -> (String, String) {
    let url = url.split(['?', '#']).next().unwrap_or(url);
    let start = url.find("://").map_or(0, |i| i + 3);
    match url[start..].find('/') {
        Some(i) => (url[..start + i].to_string(), url[start + i..].trim_end_matches('/').to_string()),
        None => (url.to_string(), String::new()),
    }
}

/// RFC 8414 / 9728 well-known urls: the path after the well-known part, then the root.
fn well_known(origin: &str, path: &str, names: &[&str]) -> Vec<String> {
    let mut out = vec![];
    for n in names {
        if !path.is_empty() {
            out.push(format!("{origin}/.well-known/{n}{path}"));
        }
        out.push(format!("{origin}/.well-known/{n}"));
    }
    out
}

/// A parameter of a `WWW-Authenticate: Bearer a="x", b=y` header.
fn auth_param(header: &str, name: &str) -> Option<String> {
    let mut rest = header;
    while let Some(i) = rest.find(name) {
        let before = rest[..i].chars().next_back();
        let after = rest[i + name.len()..].trim_start();
        rest = &rest[i + name.len()..];
        if before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_') {
            continue;
        }
        let Some(v) = after.strip_prefix('=') else { continue };
        let v = v.trim_start();
        return Some(match v.strip_prefix('"') {
            Some(q) => q.split('"').next().unwrap_or("").to_string(),
            None => v.split([',', ' ']).next().unwrap_or("").to_string(),
        });
    }
    None
}

/// `a=1&b=x%20y`
fn form(fields: &[(&str, String)]) -> String {
    fields.iter().map(|(k, v)| format!("{}={}", encode(k), encode(v))).collect::<Vec<_>>().join("&")
}

fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let hex = |i: usize| b.get(i).and_then(|c| (*c as char).to_digit(16));
    let mut out = vec![];
    let mut i = 0;
    while i < b.len() {
        match (b[i], hex(i + 1), hex(i + 2)) {
            (b'+', ..) => out.push(b' '),
            (b'%', Some(h), Some(l)) => {
                out.push(u8::try_from(h * 16 + l).unwrap_or(b'?'));
                i += 2;
            }
            (c, ..) => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The query of a `GET /callback?…` request, or `None` for anything else.
fn read_callback(stream: &mut TcpStream) -> Option<Vec<(String, String)>> {
    stream.set_nonblocking(false).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let mut line = String::new();
    BufReader::new(Read::take(&mut *stream, 8192)).read_line(&mut line).ok()?;
    let target = line.strip_prefix("GET ")?.split(' ').next()?;
    let query = target.strip_prefix("/callback?").or_else(|| (target == "/callback").then_some(""))?;
    Some(
        query
            .split('&')
            .filter(|p| !p.is_empty())
            .map(|p| {
                let (k, v) = p.split_once('=').unwrap_or((p, ""));
                (decode(k), decode(v))
            })
            .collect(),
    )
}

/// A small page for the browser tab.
fn page(stream: &mut TcpStream, status: u16, title: &str, text: &str) {
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let body = format!(
        "<!doctype html><meta charset=utf-8><title>{t}</title>\
         <body style=\"font:16px system-ui;margin:4em auto;max-width:30em\"><h1>{t}</h1><p>{x}</p>",
        t = esc(title),
        x = esc(text)
    );
    let _ = write!(
        stream,
        "HTTP/1.1 {status} {title}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
         Connection: close\r\nCache-Control: no-store\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.flush();
}

fn read_body(body: Box<dyn BufRead + Send>) -> String {
    let mut text = String::new();
    let _ = Read::take(body, 1 << 20).read_to_string(&mut text);
    text
}

/// Write `contents` readable only by the user (Unix), via a temporary file and a rename.
fn write_private(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o.open(&tmp)?;
    f.write_all(contents)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::mcp::HttpResp;
    use std::io::Cursor;
    use std::sync::mpsc;

    /// A folder under the system temp dir, removed when dropped.
    pub(crate) struct TempDir(pub(crate) PathBuf);
    impl TempDir {
        pub(crate) fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "kairos-ai-{tag}-{}-{}",
                std::process::id(),
                SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
            ));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    type Routes = Arc<dyn Fn(&HttpReq) -> Option<(u16, Value)> + Send + Sync>;

    /// HTTP answered by `routes` (404 for anything else); every request is logged.
    fn fake(routes: Routes) -> (HttpFn, Arc<Mutex<Vec<HttpReq>>>) {
        let log: Arc<Mutex<Vec<HttpReq>>> = Arc::default();
        let l = log.clone();
        let http: HttpFn = Arc::new(move |req: &HttpReq| {
            lock(&l).push(req.clone());
            let (status, body) = routes(req).unwrap_or((404, Value::Null));
            Ok(HttpResp {
                status,
                session: None,
                content_type: "application/json".into(),
                www_authenticate: None,
                body: Box::new(Cursor::new(body.to_string().into_bytes())),
            })
        });
        (http, log)
    }

    fn form_value(body: &str, key: &str) -> Option<String> {
        body.split('&').find_map(|p| p.split_once('=').filter(|(k, _)| *k == key).map(|(_, v)| decode(v)))
    }

    /// A typical server: resource metadata at the path form, an issuer with metadata, DCR.
    fn server() -> Routes {
        Arc::new(|req: &HttpReq| match (req.method, req.url.as_str()) {
            ("GET", "https://h/.well-known/oauth-protected-resource/mcp") => Some((
                200,
                json!({"resource": "https://h/mcp", "authorization_servers": ["https://auth.h"], "scopes_supported": ["a", "b"]}),
            )),
            ("GET", "https://auth.h/.well-known/oauth-authorization-server") => Some((
                200,
                json!({"authorization_endpoint": "https://auth.h/authorize?x=1", "token_endpoint": "https://auth.h/token",
                       "registration_endpoint": "https://auth.h/register", "code_challenge_methods_supported": ["S256"]}),
            )),
            ("POST", "https://auth.h/register") => Some((201, json!({"client_id": "c1"}))),
            ("POST", "https://auth.h/token") => {
                let body = req.body.clone().unwrap_or_default();
                match form_value(&body, "grant_type").as_deref() {
                    Some("authorization_code") if form_value(&body, "code").as_deref() == Some("the-code") => {
                        Some((200, json!({"access_token": "at1", "refresh_token": "rt1", "expires_in": 3600})))
                    }
                    Some("refresh_token") if form_value(&body, "refresh_token").as_deref() == Some("rt1") => {
                        Some((200, json!({"access_token": "at2", "expires_in": 3600})))
                    }
                    _ => Some((400, json!({"error": "invalid_grant"}))),
                }
            }
            _ => None,
        })
    }

    fn query_value(url: &str, key: &str) -> String {
        form_value(url.split_once('?').unwrap().1, key).unwrap_or_default()
    }

    fn get(port_url: &str, path_and_query: &str) -> String {
        let addr = port_url.trim_start_matches("http://").split('/').next().unwrap();
        let mut s = TcpStream::connect(addr).unwrap();
        write!(s, "GET {path_and_query} HTTP/1.1\r\nHost: {addr}\r\n\r\n").unwrap();
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        out
    }

    #[test]
    fn signs_in_with_registration_and_pkce() {
        let dir = TempDir::new("signin");
        let (http, log) = fake(server());
        let oauth = Arc::new(OAuth::new(http, Some(&dir.0)));
        let (tx, rx) = mpsc::channel();
        let page = oauth.begin("https://h/mcp", Box::new(move |r| tx.send(r).unwrap())).unwrap();

        assert!(page.starts_with("https://auth.h/authorize?x=1&response_type=code&"), "{page}");
        assert_eq!(query_value(&page, "client_id"), "c1");
        assert_eq!(query_value(&page, "code_challenge_method"), "S256");
        assert_eq!(query_value(&page, "scope"), "a b");
        assert_eq!(query_value(&page, "resource"), "https://h/mcp");
        let register: Value = serde_json::from_str(lock(&log)[2].body.as_deref().unwrap()).unwrap();
        let redirect = query_value(&page, "redirect_uri");
        assert_eq!(register["redirect_uris"], json!([redirect]));
        assert_eq!(register["token_endpoint_auth_method"], "none");

        let state = query_value(&page, "state");
        assert!(get(&redirect, "/favicon.ico").starts_with("HTTP/1.1 404"));
        assert!(get(&redirect, "/callback?code=x&state=wrong").starts_with("HTTP/1.1 400"), "another state is ignored");
        assert!(oauth.bearer("https://h/mcp").is_none());
        let ok = get(&redirect, &format!("/callback?code=the-code&state={state}"));
        assert!(ok.starts_with("HTTP/1.1 200") && ok.contains("Signed in"), "{ok}");
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), Ok(()));

        // The verifier sent with the code is the one the challenge was made from.
        let token_req = lock(&log).iter().rev().find(|r| r.url == "https://auth.h/token").cloned().unwrap();
        let verifier = form_value(token_req.body.as_deref().unwrap(), "code_verifier").unwrap();
        let challenge = URL_SAFE_NO_PAD.encode(ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes()));
        assert_eq!(query_value(&page, "code_challenge"), challenge);

        assert_eq!(oauth.bearer("https://h/mcp").as_deref(), Some("at1"));
        let file = dir.0.join("oauth.json");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let again = OAuth::new(fake(server()).0, Some(&dir.0));
        assert!(again.signed_in("https://h/mcp"), "kept across restarts");
        again.sign_out("https://h/mcp");
        assert!(!OAuth::new(fake(server()).0, Some(&dir.0)).signed_in("https://h/mcp"));
    }

    #[test]
    fn a_denied_sign_in_reports_why() {
        let dir = TempDir::new("denied");
        let oauth = Arc::new(OAuth::new(fake(server()).0, Some(&dir.0)));
        let (tx, rx) = mpsc::channel();
        let page = oauth.begin("https://h/mcp", Box::new(move |r| tx.send(r).unwrap())).unwrap();
        let (redirect, state) = (query_value(&page, "redirect_uri"), query_value(&page, "state"));
        let r = get(&redirect, &format!("/callback?error=access_denied&error_description=No+thanks&state={state}"));
        assert!(r.contains("No thanks"), "{r}");
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), Err("the sign-in failed: No thanks".into()));
    }

    fn stored(dir: &Path, entry: Value) -> OAuth {
        std::fs::write(dir.join("oauth.json"), json!({ "https://h/mcp": entry }).to_string()).unwrap();
        OAuth::new(fake(server()).0, Some(dir))
    }

    #[test]
    fn tokens_are_refreshed_before_they_run_out() {
        let dir = TempDir::new("refresh");
        let entry = json!({"client_id": "c1", "token_endpoint": "https://auth.h/token",
                           "access_token": "old", "refresh_token": "rt1", "expires_at": now() + 10});
        let oauth = stored(&dir.0, entry.clone());
        assert_eq!(oauth.bearer("https://h/mcp").as_deref(), Some("at2"));
        assert_eq!(oauth.bearer("https://h/mcp").as_deref(), Some("at2"), "the new one is kept");
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(dir.0.join("oauth.json")).unwrap()).unwrap();
        assert_eq!(
            saved["https://h/mcp"]["refresh_token"], "rt1",
            "a refresh without a new refresh token keeps the old"
        );

        let mut refused = entry;
        refused["refresh_token"] = json!("revoked");
        let oauth = stored(&dir.0, refused);
        assert_eq!(oauth.bearer("https://h/mcp"), None);
        assert!(!oauth.signed_in("https://h/mcp"), "a refused refresh means signing in again");
        let fresh = json!({"client_id": "c1", "token_endpoint": "https://auth.h/token", "access_token": "a"});
        assert_eq!(stored(&dir.0, fresh).bearer("https://h/mcp").as_deref(), Some("a"), "no expiry: used as is");
    }

    #[test]
    fn discovery() {
        let (http, _) = fake(server());
        let o = OAuth::new(http, None);
        let ep = o.discover("https://h/mcp", None).unwrap();
        assert_eq!(
            (ep.token.as_str(), ep.register.as_deref()),
            ("https://auth.h/token", Some("https://auth.h/register"))
        );
        // The header names the metadata and the scope.
        let routes: Routes = Arc::new(|req: &HttpReq| match req.url.as_str() {
            "https://h/meta" => Some((200, json!({"authorization_servers": ["https://h/tenant"]}))),
            "https://h/.well-known/oauth-authorization-server/tenant" => {
                Some((200, json!({"authorization_endpoint": "https://h/a", "token_endpoint": "https://h/t"})))
            }
            _ => None,
        });
        let o = OAuth::new(fake(routes).0, None);
        let www = r#"Bearer error="invalid_token", resource_metadata="https://h/meta", scope="read write""#;
        let ep = o.discover("https://h/mcp", Some(www)).unwrap();
        assert_eq!(
            (ep.authorize.as_str(), ep.scope.as_deref(), ep.register),
            ("https://h/a", Some("read write"), None)
        );
        // No metadata at all: the 2025-03-26 defaults on the server itself.
        let o = OAuth::new(fake(Arc::new(|_: &HttpReq| None)).0, None);
        assert_eq!(o.discover("https://h/mcp", None).unwrap().authorize, "https://h/authorize");
        // No PKCE, plain http elsewhere, no data folder: refused.
        let routes: Routes = Arc::new(|req: &HttpReq| match req.url.as_str() {
            "https://h/.well-known/oauth-authorization-server" => Some((
                200,
                json!({"authorization_endpoint": "http://evil/a", "token_endpoint": "https://h/t",
                       "code_challenge_methods_supported": ["plain"]}),
            )),
            _ => None,
        });
        let o = Arc::new(OAuth::new(fake(routes.clone()).0, None));
        assert!(o.discover("https://h/mcp", None).unwrap_err().contains("PKCE"));
        assert!(o.begin("https://h/mcp", Box::new(|_| {})).unwrap_err().contains("update Kairos"));
    }

    #[test]
    fn helpers() {
        assert_eq!(split_url("https://h:8/a/b/?q=1"), ("https://h:8".into(), "/a/b".into()));
        assert_eq!(split_url("https://h"), ("https://h".into(), String::new()));
        assert_eq!(auth_param(r#"Bearer realm="x", scope=files"#, "scope").as_deref(), Some("files"));
        assert_eq!(auth_param(r#"Bearer xscope="no""#, "scope"), None);
        assert_eq!(encode("a b/é"), "a%20b%2F%C3%A9");
        assert_eq!(decode("a+b%2F%C3%A9%zz"), "a b/é%zz");
    }
}
