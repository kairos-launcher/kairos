//! The HTTP proxy used by the launcher's own requests (remote plugins) and handed to plugin
//! processes.
//!
//! `proxy` in `config.toml` is `"auto"` (default), `"none"`, or a proxy URL. Auto takes the
//! standard environment variables, else the system settings: on Windows the Internet Options
//! (a fixed proxy, or a setup script / automatic detection that WinHTTP evaluates), on Linux
//! GNOME or KDE. Plugins get the result as `HTTP_PROXY`/`HTTPS_PROXY`/`ALL_PROXY`/`NO_PROXY`
//! (and the lowercase forms), which most HTTP clients honour.
//!
//! Resolving can take a moment (a setup script is downloaded and run), so it happens on worker
//! threads only and is cached for `REFRESH`.

use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::core::sync::lock;

/// How long a resolved proxy is used before the system settings are read again.
const REFRESH: Duration = Duration::from_mins(30);
/// A setup script decides per address; this one stands for "the internet".
#[cfg(windows)]
const PROBE_URL: &str = "https://www.ecb.europa.eu/";

/// The `proxy` setting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Auto,
    Off,
    Url(String),
}

impl Mode {
    pub fn parse(s: &str) -> Self {
        match s.trim() {
            "" | "auto" => Self::Auto,
            "none" | "off" | "direct" => Self::Off,
            url => Self::Url(with_scheme(url)),
        }
    }
}

/// A proxy URL (`http://host:port`, `socks5://…`) and the hosts that bypass it
/// (comma-separated, `NO_PROXY` syntax).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proxy {
    pub url: String,
    pub no_proxy: String,
}

struct State {
    mode: Mode,
    extra_no_proxy: String,
    cached: Option<(Option<Proxy>, Instant)>,
}

static STATE: Mutex<State> = Mutex::new(State { mode: Mode::Auto, extra_no_proxy: String::new(), cached: None });
/// Held while resolving, so concurrent callers wait for one answer instead of all asking.
static RESOLVING: Mutex<()> = Mutex::new(());

/// Apply the `proxy` and `no_proxy` settings. Returns whether they changed.
pub fn configure(proxy: &str, no_proxy: &str) -> bool {
    let mode = Mode::parse(proxy);
    let mut s = lock(&STATE);
    if s.mode == mode && s.extra_no_proxy == no_proxy.trim() {
        return false;
    }
    s.mode = mode;
    s.extra_no_proxy = no_proxy.trim().to_string();
    s.cached = None;
    true
}

/// The proxy to use now, if any. May block while the system settings are read (the first
/// time, and every `REFRESH`); call it off the UI thread.
pub fn current() -> Option<Proxy> {
    let fresh = |s: &State| s.cached.as_ref().filter(|(_, at)| at.elapsed() < REFRESH).map(|(p, _)| p.clone());
    if let Some(p) = fresh(&lock(&STATE)) {
        return p;
    }
    let _one = lock(&RESOLVING);
    let (mode, extra) = {
        let s = lock(&STATE);
        if let Some(p) = fresh(&s) {
            return p;
        }
        (s.mode.clone(), s.extra_no_proxy.clone())
    };
    let p = resolve(&mode, &extra);
    match &p {
        Some(p) => log::info!("proxy: {} (bypass: {})", redact(&p.url), p.no_proxy),
        None => log::debug!("proxy: none"),
    }
    let mut s = lock(&STATE);
    // A `configure` while we resolved wins; its next `current` resolves again.
    if s.mode == mode && s.extra_no_proxy == extra {
        s.cached = Some((p.clone(), Instant::now()));
    }
    p
}

/// Whether the `proxy` setting turns proxies off altogether.
fn is_off() -> bool {
    lock(&STATE).mode == Mode::Off
}

const ENV_URL: [&str; 6] = ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"];
const ENV_BYPASS: [&str; 2] = ["NO_PROXY", "no_proxy"];

/// Give a plugin process the current proxy (or, with `proxy = "none"`, none at all).
pub fn apply_env(cmd: &mut Command) {
    if let Some(p) = current() {
        for k in ENV_URL {
            cmd.env(k, &p.url);
        }
        for k in ENV_BYPASS {
            cmd.env(k, &p.no_proxy);
        }
    } else if is_off() {
        for k in ENV_URL.iter().chain(&ENV_BYPASS) {
            cmd.env_remove(k);
        }
    }
}

/// The current proxy for ureq; `None` connects directly.
#[cfg(feature = "remote")]
pub fn ureq_proxy(p: Option<&Proxy>) -> Option<ureq::Proxy> {
    let p = p?;
    let parsed = ureq::Proxy::new(&p.url).map_err(|e| log::warn!("proxy {}: {e}", redact(&p.url))).ok()?;
    let mut b = ureq::Proxy::builder(parsed.protocol()).host(parsed.host()).port(parsed.port());
    if let Some(u) = parsed.username() {
        b = b.username(u);
    }
    if let Some(pw) = parsed.password() {
        b = b.password(pw);
    }
    for host in p.no_proxy.split(',').map(str::trim).filter(|h| !h.is_empty()) {
        b = b.no_proxy(host);
    }
    b.build().map_err(|e| log::warn!("proxy {}: {e}", redact(&p.url))).ok()
}

fn resolve(mode: &Mode, extra: &str) -> Option<Proxy> {
    let mut p = match mode {
        Mode::Off => return None,
        Mode::Url(url) => Proxy { url: url.clone(), no_proxy: String::new() },
        Mode::Auto => from_env(|k| std::env::var(k).ok()).or_else(system)?,
    };
    p.no_proxy = join_hosts(&[&p.no_proxy, extra]);
    Some(p)
}

/// The standard variables, as the launcher itself was started with them.
fn from_env(get: impl Fn(&str) -> Option<String>) -> Option<Proxy> {
    let get = |k: &str| get(k).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    // HTTPS first: nearly every plugin request is HTTPS.
    let url = ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy", "HTTP_PROXY", "http_proxy"]
        .into_iter()
        .find_map(get)?;
    let no_proxy = ENV_BYPASS.into_iter().find_map(get).unwrap_or_default();
    Some(Proxy { url: with_scheme(&url), no_proxy })
}

#[cfg(windows)]
fn system() -> Option<Proxy> {
    win::system()
}

#[cfg(not(windows))]
fn system() -> Option<Proxy> {
    gnome().or_else(kde)
}

/// `host:port` -> `http://host:port`; URLs with a scheme are kept.
fn with_scheme(s: &str) -> String {
    let s = s.trim();
    if s.contains("://") { s.to_string() } else { format!("http://{s}") }
}

/// Merge host lists (separated by commas, semicolons or spaces) into one `NO_PROXY` value,
/// without duplicates.
fn join_hosts(lists: &[&str]) -> String {
    let mut out: Vec<&str> = vec![];
    for h in lists.iter().flat_map(|l| l.split([',', ';', ' '])).map(str::trim).filter(|h| !h.is_empty()) {
        if !out.contains(&h) {
            out.push(h);
        }
    }
    out.join(",")
}

/// A proxy URL for the log, without its password.
fn redact(url: &str) -> String {
    match (url.find("://"), url.rfind('@')) {
        (Some(s), Some(at)) if at > s => format!("{}***{}", &url[..s + 3], &url[at..]),
        _ => url.to_string(),
    }
}

/// A WinHTTP / Internet Options proxy list: `host:port`, or per scheme
/// `http=host:port;https=host:port;socks=host:port`, possibly several separated by `;` or spaces.
/// Picks the one for HTTPS.
#[cfg_attr(not(windows), allow(dead_code))]
fn parse_windows_proxy(s: &str) -> Option<String> {
    let entries: Vec<&str> = s.split([';', ' ']).map(str::trim).filter(|e| !e.is_empty()).collect();
    let scheme = |name: &str| entries.iter().find_map(|e| e.strip_prefix(name)?.strip_prefix('='));
    if let Some(e) = scheme("https").or_else(|| scheme("http")) {
        return Some(with_scheme(e));
    }
    if let Some(plain) = entries.iter().find(|e| !e.contains('=')) {
        return Some(with_scheme(plain));
    }
    // Internet Options' "socks" is SOCKS4.
    scheme("socks").map(|e| if e.contains("://") { e.to_string() } else { format!("socks4://{e}") })
}

/// Internet Options' bypass list (`<local>;*.corp.example;10.*`) in `NO_PROXY` form.
#[cfg_attr(not(windows), allow(dead_code))]
fn parse_windows_bypass(s: &str) -> String {
    let hosts: Vec<String> = s
        .split([';', ' ', ','])
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .map(|h| if h.eq_ignore_ascii_case("<local>") { "localhost,127.0.0.1,::1".to_string() } else { h.to_string() })
        .collect();
    join_hosts(&hosts.iter().map(String::as_str).collect::<Vec<_>>())
}

/// GNOME's proxy settings, read in one `gsettings` call.
#[cfg(not(windows))]
fn gnome() -> Option<Proxy> {
    let out = Command::new("gsettings").args(["list-recursively", "org.gnome.system.proxy"]).output().ok()?;
    out.status.success().then_some(())?;
    parse_gnome(&String::from_utf8_lossy(&out.stdout))
}

/// `gsettings list-recursively org.gnome.system.proxy` output -> the manual proxy, if set.
#[cfg_attr(windows, allow(dead_code))]
fn parse_gnome(text: &str) -> Option<Proxy> {
    let get = |schema: &str, key: &str| {
        text.lines().find_map(|l| {
            let mut it = l.splitn(3, ' ');
            (it.next()? == schema && it.next()? == key).then(|| it.next().unwrap_or("").trim().to_string())
        })
    };
    let unquote = |v: String| v.trim_matches('\'').to_string();
    let mode = unquote(get("org.gnome.system.proxy", "mode")?);
    if mode == "auto" {
        log::info!("proxy: GNOME uses a setup script, which the launcher cannot run; set `proxy` in config.toml");
        return None;
    }
    if mode != "manual" {
        return None;
    }
    let server = |kind: &str| {
        let schema = format!("org.gnome.system.proxy.{kind}");
        let host = unquote(get(&schema, "host")?);
        let port: u16 = get(&schema, "port")?.parse().ok()?;
        (!host.is_empty() && port != 0).then(|| format!("{host}:{port}"))
    };
    let url = server("https")
        .or_else(|| server("http"))
        .map(|h| with_scheme(&h))
        .or_else(|| server("socks").map(|h| format!("socks5://{h}")))?;
    // ['localhost', '127.0.0.0/8', '::1']
    let ignore = get("org.gnome.system.proxy", "ignore-hosts").unwrap_or_default();
    let hosts: Vec<&str> = ignore
        .trim_matches(['[', ']'])
        .split(',')
        .map(|h| h.trim().trim_matches('\''))
        .filter(|h| !h.is_empty())
        .collect();
    Some(Proxy { url, no_proxy: join_hosts(&hosts) })
}

/// KDE's proxy settings (`~/.config/kioslaverc`), manual proxies only.
#[cfg(not(windows))]
fn kde() -> Option<Proxy> {
    parse_kioslaverc(&std::fs::read_to_string(dirs::config_dir()?.join("kioslaverc")).ok()?)
}

#[cfg_attr(windows, allow(dead_code))]
fn parse_kioslaverc(text: &str) -> Option<Proxy> {
    let mut in_section = false;
    let mut values = std::collections::HashMap::new();
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_section = line == "[Proxy Settings]";
        } else if in_section && let Some((k, v)) = line.split_once('=') {
            values.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    match values.get("ProxyType").map(String::as_str) {
        Some("1") => {}
        Some("2" | "3") => {
            log::info!("proxy: KDE uses a setup script, which the launcher cannot run; set `proxy` in config.toml");
            return None;
        }
        _ => return None,
    }
    // KDE writes `http://proxy.example 8080` (a space before the port).
    let server = |key: &str| {
        let v = values.get(key).filter(|v| !v.is_empty())?;
        Some(match v.rsplit_once(' ') {
            Some((host, port)) if port.parse::<u16>().is_ok() => format!("{host}:{port}"),
            _ => v.clone(),
        })
    };
    let url = server("httpsProxy").or_else(|| server("httpProxy")).or_else(|| {
        server("socksProxy")
            .map(|s| if s.contains("://") { s.replacen("socks://", "socks5://", 1) } else { format!("socks5://{s}") })
    })?;
    Some(Proxy { url: with_scheme(&url), no_proxy: join_hosts(&[values.get("NoProxyFor").map_or("", String::as_str)]) })
}

/// Internet Options through WinHTTP: a fixed proxy, or what the setup script / automatic
/// detection answers for `PROBE_URL`.
#[cfg(windows)]
#[allow(unsafe_code)]
mod win {
    use super::{PROBE_URL, Proxy, parse_windows_bypass, parse_windows_proxy};
    use windows_sys::Win32::Foundation::GlobalFree;
    use windows_sys::Win32::Networking::WinHttp::{
        WINHTTP_ACCESS_TYPE_NAMED_PROXY, WINHTTP_ACCESS_TYPE_NO_PROXY, WINHTTP_AUTO_DETECT_TYPE_DHCP,
        WINHTTP_AUTO_DETECT_TYPE_DNS_A, WINHTTP_AUTOPROXY_AUTO_DETECT, WINHTTP_AUTOPROXY_CONFIG_URL,
        WINHTTP_AUTOPROXY_OPTIONS, WINHTTP_CURRENT_USER_IE_PROXY_CONFIG, WINHTTP_PROXY_INFO, WinHttpCloseHandle,
        WinHttpGetIEProxyConfigForCurrentUser, WinHttpGetProxyForUrl, WinHttpOpen, WinHttpSetTimeouts,
    };
    use windows_sys::core::PWSTR;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Copy a WinHTTP-allocated string and free it.
    fn take(p: PWSTR) -> Option<String> {
        if p.is_null() {
            return None;
        }
        // SAFETY: WinHTTP returns NUL-terminated UTF-16 strings; we read up to the NUL.
        let s = unsafe {
            let mut len = 0;
            while *p.add(len) != 0 {
                len += 1;
            }
            String::from_utf16_lossy(std::slice::from_raw_parts(p, len))
        };
        // SAFETY: WinHTTP documents these strings as GlobalAlloc'ed, to be freed by the caller.
        unsafe { GlobalFree(p.cast()) };
        Some(s).filter(|s| !s.trim().is_empty())
    }

    pub fn system() -> Option<Proxy> {
        let mut ie = WINHTTP_CURRENT_USER_IE_PROXY_CONFIG::default();
        // SAFETY: `ie` is a valid out struct; its strings are taken (and freed) below.
        if unsafe { WinHttpGetIEProxyConfigForCurrentUser(&raw mut ie) } == 0 {
            return None;
        }
        let auto_detect = ie.fAutoDetect != 0;
        let (script, fixed, bypass) = (take(ie.lpszAutoConfigUrl), take(ie.lpszProxy), take(ie.lpszProxyBypass));
        if auto_detect || script.is_some() {
            match from_script(script.as_deref(), auto_detect) {
                Script::Proxy(p) => return Some(p),
                Script::Direct => return None,
                Script::Unavailable => {}
            }
        }
        let url = parse_windows_proxy(&fixed?)?;
        Some(Proxy { url, no_proxy: parse_windows_bypass(bypass.as_deref().unwrap_or("")) })
    }

    /// What a setup script says for the probe URL.
    enum Script {
        Proxy(Proxy),
        Direct,
        /// It could not be found or run: the fixed proxy (if any) applies.
        Unavailable,
    }

    /// Evaluate the setup script (or detect one) for the probe URL.
    fn from_script(script: Option<&str>, auto_detect: bool) -> Script {
        let agent = wide("kairos");
        // SAFETY: valid NUL-terminated agent string; null proxy strings with NO_PROXY access.
        let session =
            unsafe { WinHttpOpen(agent.as_ptr(), WINHTTP_ACCESS_TYPE_NO_PROXY, std::ptr::null(), std::ptr::null(), 0) };
        if session.is_null() {
            return Script::Unavailable;
        }
        // SAFETY: `session` is a valid handle; downloading the script must not take forever.
        unsafe { WinHttpSetTimeouts(session, 5000, 5000, 5000, 5000) };
        let script_w = script.map(wide);
        let mut opts = WINHTTP_AUTOPROXY_OPTIONS {
            dwFlags: if script_w.is_some() { WINHTTP_AUTOPROXY_CONFIG_URL } else { 0 }
                | if auto_detect { WINHTTP_AUTOPROXY_AUTO_DETECT } else { 0 },
            dwAutoDetectFlags: if auto_detect {
                WINHTTP_AUTO_DETECT_TYPE_DHCP | WINHTTP_AUTO_DETECT_TYPE_DNS_A
            } else {
                0
            },
            lpszAutoConfigUrl: script_w.as_ref().map_or(std::ptr::null(), Vec::as_ptr),
            lpvReserved: std::ptr::null_mut(),
            dwReserved: 0,
            // Company proxies serving the script often want the Windows sign-in.
            fAutoLogonIfChallenged: 1,
        };
        let url = wide(PROBE_URL);
        let mut info = WINHTTP_PROXY_INFO::default();
        // SAFETY: valid session, NUL-terminated URL, options and out struct that outlive the call.
        let ok = unsafe { WinHttpGetProxyForUrl(session, url.as_ptr(), &raw mut opts, &raw mut info) } != 0;
        let err = (!ok).then(std::io::Error::last_os_error);
        // SAFETY: `session` came from WinHttpOpen and is not used afterwards.
        unsafe { WinHttpCloseHandle(session) };
        let (proxy, bypass) = (take(info.lpszProxy), take(info.lpszProxyBypass));
        if let Some(e) = err {
            log::info!("proxy: the setup script could not be used ({e})");
            return Script::Unavailable;
        }
        if info.dwAccessType != WINHTTP_ACCESS_TYPE_NAMED_PROXY {
            return Script::Direct;
        }
        match proxy.as_deref().and_then(parse_windows_proxy) {
            Some(url) => {
                Script::Proxy(Proxy { url, no_proxy: parse_windows_bypass(bypass.as_deref().unwrap_or("<local>")) })
            }
            None => Script::Unavailable,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_parse() {
        assert_eq!(Mode::parse(""), Mode::Auto);
        assert_eq!(Mode::parse(" auto "), Mode::Auto);
        assert_eq!(Mode::parse("none"), Mode::Off);
        assert_eq!(Mode::parse("proxy.corp:8080"), Mode::Url("http://proxy.corp:8080".into()));
        assert_eq!(Mode::parse("socks5://127.0.0.1:1080"), Mode::Url("socks5://127.0.0.1:1080".into()));
    }

    #[test]
    fn environment_prefers_https_and_reads_no_proxy() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| (*v).to_string())
        };
        let p = from_env(env(&[("http_proxy", "http://a:1"), ("HTTPS_PROXY", "b:2"), ("no_proxy", "x,.y")])).unwrap();
        assert_eq!(p, Proxy { url: "http://b:2".into(), no_proxy: "x,.y".into() });
        assert_eq!(from_env(env(&[("HTTPS_PROXY", " ")])), None);
        assert_eq!(from_env(env(&[])), None);
    }

    #[test]
    fn settings_override_and_extend() {
        let p = resolve(&Mode::Url("http://p:3128".into()), "intra.example, localhost").unwrap();
        assert_eq!(p, Proxy { url: "http://p:3128".into(), no_proxy: "intra.example,localhost".into() });
        assert_eq!(resolve(&Mode::Off, "x"), None);
    }

    #[test]
    fn windows_proxy_lists() {
        assert_eq!(parse_windows_proxy("proxy.corp:8080").as_deref(), Some("http://proxy.corp:8080"));
        assert_eq!(parse_windows_proxy("http=h:80;https=s:443;ftp=f:21").as_deref(), Some("http://s:443"));
        assert_eq!(parse_windows_proxy("http=h:80").as_deref(), Some("http://h:80"));
        assert_eq!(parse_windows_proxy("p1:80; p2:80").as_deref(), Some("http://p1:80"));
        assert_eq!(parse_windows_proxy("socks=s:1080").as_deref(), Some("socks4://s:1080"));
        assert_eq!(parse_windows_proxy(" "), None);
        assert_eq!(parse_windows_bypass("<local>;*.corp.example;10.*"), "localhost,127.0.0.1,::1,*.corp.example,10.*");
    }

    #[test]
    fn gnome_manual_proxy() {
        let text = "org.gnome.system.proxy autoconfig-url ''
org.gnome.system.proxy ignore-hosts ['localhost', '127.0.0.0/8', '::1']
org.gnome.system.proxy mode 'manual'
org.gnome.system.proxy.http host 'web.corp'
org.gnome.system.proxy.http port 8080
org.gnome.system.proxy.https host ''
org.gnome.system.proxy.https port 0
org.gnome.system.proxy.socks host ''
org.gnome.system.proxy.socks port 0
";
        let p = parse_gnome(text).unwrap();
        assert_eq!(p, Proxy { url: "http://web.corp:8080".into(), no_proxy: "localhost,127.0.0.0/8,::1".into() });
        assert_eq!(parse_gnome(&text.replace("'manual'", "'none'")), None);
        assert_eq!(parse_gnome(&text.replace("'manual'", "'auto'")), None);
    }

    #[test]
    fn kde_manual_proxy() {
        let text = "[Proxy Settings]\nProxyType=1\nhttpProxy=http://web.corp 8080\nhttpsProxy=\nNoProxyFor=localhost,.corp\n[Other]\nProxyType=2\n";
        let p = parse_kioslaverc(text).unwrap();
        assert_eq!(p, Proxy { url: "http://web.corp:8080".into(), no_proxy: "localhost,.corp".into() });
        assert_eq!(parse_kioslaverc(&text.replace("ProxyType=1", "ProxyType=0")), None);
        let socks = "[Proxy Settings]\nProxyType=1\nsocksProxy=socks://127.0.0.1 1080\n";
        assert_eq!(parse_kioslaverc(socks).unwrap().url, "socks5://127.0.0.1:1080");
    }

    /// Runs the real WinHTTP calls (and frees what they return) without crashing.
    #[cfg(windows)]
    #[test]
    fn windows_system_settings_can_be_read() {
        let _ = system();
        let _ = win::system();
    }

    #[test]
    fn passwords_stay_out_of_the_log() {
        assert_eq!(redact("http://me:secret@p:80"), "http://***@p:80");
        assert_eq!(redact("http://p:80"), "http://p:80");
    }

    #[cfg(feature = "remote")]
    #[test]
    fn ureq_proxy_keeps_credentials_and_bypass() {
        let p = Proxy { url: "http://me:pw@p.corp:3128".into(), no_proxy: "localhost,.corp".into() };
        let u = ureq_proxy(Some(&p)).unwrap();
        assert_eq!((u.host(), u.port(), u.username(), u.password()), ("p.corp", 3128, Some("me"), Some("pw")));
        assert!(u.is_no_proxy(&"https://x.corp/".parse().unwrap()));
        assert!(!u.is_no_proxy(&"https://example.com/".parse().unwrap()));
        assert!(ureq_proxy(None).is_none());
    }
}
