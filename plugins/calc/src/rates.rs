//! Exchange rates for fend: the ECB daily reference rates (EUR base, no API key).
//!
//! Queries never wait for the network. The first currency lookup in a process reads the
//! cached file; if it is missing or older than `MAX_AGE`, one background thread fetches a
//! fresh copy. Until the very first download lands, lookups fail with `LOADING`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use fend_core::{ExchangeRateFnV2, ExchangeRateFnV2Options};

pub const ECB_URL: &str = "https://www.ecb.europa.eu/stats/eurofxref/eurofxref-daily.xml";
/// The ECB publishes once per working day; refreshing twice a day is plenty.
const MAX_AGE: Duration = Duration::from_secs(12 * 3600);
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
/// Shown to the user (via fend's error) while the first download is running.
pub const LOADING: &str = "exchange rates are loading, type again in a moment";
/// Starts fend's error when there are no rates because the download failed; the reason follows.
pub const UNAVAILABLE: &str = "exchange rates unavailable: ";
/// After a failed download, the next currency lookup this long after it tries again.
const RETRY_AFTER: Duration = Duration::from_secs(60);

pub type Fetch = Arc<dyn Fn() -> Result<String, String> + Send + Sync>;

#[derive(Debug)]
pub struct Rates {
    /// Publication date from the file, e.g. `2026-10-20`.
    pub date: String,
    /// Units of each currency that one euro buys (EUR itself is 1).
    per_eur: HashMap<String, f64>,
}

/// Parse the ECB XML by scanning for its attributes; `None` if it holds no usable rate.
pub fn parse(xml: &str) -> Option<Rates> {
    let attr = |tag: &str, name: &str| -> Option<String> {
        let start = tag.find(&format!("{name}='"))? + name.len() + 2;
        let len = tag[start..].find('\'')?;
        Some(tag[start..start + len].to_string())
    };
    let mut date = None;
    let mut per_eur = HashMap::from([("EUR".to_string(), 1.0)]);
    for tag in xml.split('<') {
        if date.is_none() {
            date = attr(tag, "time");
        }
        let (Some(code), Some(rate)) = (attr(tag, "currency"), attr(tag, "rate")) else { continue };
        if let Ok(r) = rate.parse::<f64>()
            && r.is_finite()
            && r > 0.0
            && code.len() == 3
            && code.bytes().all(|b| b.is_ascii_uppercase())
        {
            per_eur.insert(code, r);
        }
    }
    (per_eur.len() > 1).then(|| Rates { date: date.unwrap_or_default(), per_eur })
}

/// `<cache>/kairos/calc/eurofxref.xml`, next to the launcher's own icon cache.
pub fn default_cache_file() -> Option<PathBuf> {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let root = if cfg!(windows) {
        env("LOCALAPPDATA")
    } else if cfg!(target_os = "macos") {
        env("HOME").map(|h| h.join("Library/Caches"))
    } else {
        env("XDG_CACHE_HOME").or_else(|| env("HOME").map(|h| h.join(".cache")))
    }?;
    Some(root.join("kairos").join("calc").join("eurofxref.xml"))
}

pub fn fetch_ecb() -> Result<String, String> {
    // Trust the OS store, so TLS-inspecting company proxies with an installed CA work.
    let tls = ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::PlatformVerifier).build();
    let agent: ureq::Agent =
        ureq::Agent::config_builder().tls_config(tls).timeout_global(Some(FETCH_TIMEOUT)).build().into();
    let mut resp = agent.get(ECB_URL).call().map_err(|e| e.to_string())?;
    resp.body_mut().with_config().limit(1 << 20).read_to_string().map_err(|e| e.to_string())
}

struct Shared {
    rates: RwLock<Option<Arc<Rates>>>,
    started: AtomicBool,
    /// Date of the rates used by the latest evaluation, taken by `take_used`.
    used: Mutex<Option<String>>,
    /// Why the latest download failed, and when.
    failed: Mutex<Option<(String, Instant)>>,
    cache: Option<PathBuf>,
    fetch: Fetch,
}

/// Handed to fend (cloned into each `Context`); all clones share one state.
#[derive(Clone)]
pub struct Source(Arc<Shared>);

impl Source {
    pub fn new(cache: Option<PathBuf>, fetch: Fetch) -> Self {
        Self(Arc::new(Shared {
            rates: RwLock::new(None),
            started: AtomicBool::new(false),
            used: Mutex::new(None),
            failed: Mutex::new(None),
            cache,
            fetch,
        }))
    }

    /// The rates date if the latest evaluation converted a currency; resets it.
    pub fn take_used(&self) -> Option<String> {
        self.0.used.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take()
    }

    /// Load the cache once, and refresh it in the background when missing or stale.
    fn start(&self) {
        if self.0.started.swap(true, Ordering::SeqCst) {
            return;
        }
        let cached = self.0.cache.as_deref().and_then(read_cache);
        let fresh = cached.as_ref().is_some_and(|(_, age)| *age < MAX_AGE);
        if let Some((rates, _)) = cached {
            self.set(rates);
        }
        if !fresh {
            let me = self.clone();
            std::thread::spawn(move || me.refresh());
        }
    }

    fn refresh(&self) {
        let fail = |e: String| {
            eprintln!("calc: fetching exchange rates failed: {e}");
            *self.0.failed.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some((e, Instant::now()));
        };
        let xml = match (self.0.fetch)() {
            Ok(xml) => xml,
            Err(e) => return fail(e),
        };
        let Some(rates) = parse(&xml) else {
            return fail("the download had no rates".into());
        };
        *self.0.failed.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        if let Some(path) = &self.0.cache
            && let Err(e) = write_atomic(path, &xml)
        {
            eprintln!("calc: caching exchange rates in {} failed: {e}", path.display());
        }
        self.set(rates);
    }

    fn set(&self, rates: Rates) {
        *self.0.rates.write().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::new(rates));
    }
}

impl ExchangeRateFnV2 for Source {
    /// fend's contract: how many units of `currency` one base unit (here: EUR) buys.
    fn relative_to_base_currency(
        &self,
        currency: &str,
        _options: &ExchangeRateFnV2Options,
    ) -> Result<f64, Box<dyn std::error::Error + Send + Sync + 'static>> {
        self.start();
        let rates = self.0.rates.read().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        let Some(rates) = rates else {
            let failed = self.0.failed.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
            let Some((why, at)) = failed else { return Err(LOADING.into()) };
            // Try again on a later lookup (say, after the proxy setting was fixed).
            if at.elapsed() >= RETRY_AFTER {
                *self.0.failed.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                self.0.started.store(false, Ordering::SeqCst);
            }
            return Err(format!("{UNAVAILABLE}{why}").into());
        };
        let rate = *rates.per_eur.get(currency).ok_or_else(|| format!("no ECB rate for {currency}"))?;
        *self.0.used.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(rates.date.clone());
        Ok(rate)
    }
}

fn read_cache(path: &Path) -> Option<(Rates, Duration)> {
    let age = fs::metadata(path).ok()?.modified().ok()?.elapsed().unwrap_or_default();
    Some((parse(&fs::read_to_string(path).ok()?)?, age))
}

fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path)
}

/// Test helper: a source whose rates are already loaded, with no file and no network.
#[cfg(test)]
pub fn loaded(xml: &str) -> Source {
    let s = Source::new(None, Arc::new(|| Err("no network in tests".into())));
    s.0.started.store(true, Ordering::SeqCst);
    s.set(parse(xml).expect("fixture parses"));
    s
}

#[cfg(test)]
pub const FIXTURE: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<gesmes:Envelope xmlns:gesmes=\"http://www.gesmes.org/xml/2002-08-01\">
\t<Cube>
\t\t<Cube time='2026-10-20'>
\t\t\t<Cube currency='USD' rate='1.1000'/>
\t\t\t<Cube currency='JPY' rate='160.00'/>
\t\t\t<Cube currency='CHF' rate='0.9350'/>
\t\t\t<Cube currency='GBP' rate='0.8500'/>
\t\t\t<Cube currency='bad' rate='1.0'/>
\t\t\t<Cube currency='XYZ' rate='nope'/>
\t\t</Cube>
\t</Cube>
</gesmes:Envelope>";

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn parses_date_and_rates_skipping_junk() {
        let r = parse(FIXTURE).unwrap();
        assert_eq!(r.date, "2026-10-20");
        assert_eq!(r.per_eur.get("USD"), Some(&1.1));
        assert_eq!(r.per_eur.get("EUR"), Some(&1.0));
        assert!(!r.per_eur.contains_key("bad") && !r.per_eur.contains_key("XYZ"));
        assert!(parse("<html>maintenance</html>").is_none());
    }

    fn temp_file(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kairos-calc-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir.join("sub").join("eurofxref.xml")
    }

    fn counting_fetch(n: &Arc<AtomicUsize>) -> Fetch {
        let n = n.clone();
        Arc::new(move || {
            n.fetch_add(1, Ordering::SeqCst);
            Ok(FIXTURE.to_string())
        })
    }

    fn lookup(s: &Source, c: &str) -> Result<f64, String> {
        // ExchangeRateFnV2Options has no public constructor; go through fend.
        let mut ctx = fend_core::Context::new();
        ctx.set_exchange_rate_handler_v2(s.clone());
        fend_core::evaluate(&format!("1 EUR to {c}"), &mut ctx).map(|r| {
            r.get_main_result().split(' ').next().unwrap().trim_start_matches("approx.").parse().unwrap_or(0.0)
        })
    }

    #[test]
    fn failed_download_says_why_and_retries_later() {
        let s = Source::new(None, Arc::new(|| Err("proxy refused".into())));
        assert!(lookup(&s, "USD").unwrap_err().contains(LOADING));
        for _ in 0..200 {
            if s.0.failed.lock().unwrap().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let e = lookup(&s, "USD").unwrap_err();
        assert!(e.contains(UNAVAILABLE) && e.contains("proxy refused"), "{e}");
        assert!(s.0.started.load(Ordering::SeqCst), "no retry right away");
        s.0.failed.lock().unwrap().as_mut().unwrap().1 = Instant::now().checked_sub(RETRY_AFTER).unwrap();
        let _ = lookup(&s, "USD");
        assert!(!s.0.started.load(Ordering::SeqCst), "the next lookup downloads again");
    }

    fn wait_for_rates(s: &Source) {
        for _ in 0..200 {
            if s.0.rates.read().unwrap().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("rates never arrived");
    }

    #[test]
    fn missing_cache_fetches_once_in_background_and_writes_the_file() {
        let path = temp_file("missing");
        let n = Arc::new(AtomicUsize::new(0));
        let s = Source::new(Some(path.clone()), counting_fetch(&n));
        let e = lookup(&s, "USD").unwrap_err();
        assert!(e.contains(LOADING), "{e}");
        wait_for_rates(&s);
        assert_eq!(lookup(&s, "USD"), Ok(1.1));
        assert_eq!(n.load(Ordering::SeqCst), 1, "one download per process");
        assert!(parse(&fs::read_to_string(&path).unwrap()).is_some(), "cache written");
        let _ = fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn fresh_cache_answers_at_once_without_fetching() {
        let path = temp_file("fresh");
        write_atomic(&path, FIXTURE).unwrap();
        let n = Arc::new(AtomicUsize::new(0));
        let s = Source::new(Some(path.clone()), counting_fetch(&n));
        assert_eq!(lookup(&s, "JPY"), Ok(160.0));
        assert_eq!(s.take_used().as_deref(), Some("2026-10-20"));
        assert_eq!(s.take_used(), None, "taking resets");
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(n.load(Ordering::SeqCst), 0);
        let _ = fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn stale_cache_answers_at_once_and_refreshes_once() {
        let path = temp_file("stale");
        write_atomic(&path, &FIXTURE.replace("1.1000", "1.0500")).unwrap();
        let old = std::time::SystemTime::now() - MAX_AGE - Duration::from_secs(60);
        fs::File::options().write(true).open(&path).unwrap().set_modified(old).unwrap();
        let n = Arc::new(AtomicUsize::new(0));
        let s = Source::new(Some(path.clone()), counting_fetch(&n));
        assert_eq!(lookup(&s, "USD"), Ok(1.05), "old rates right away");
        for _ in 0..200 {
            if lookup(&s, "USD") == Ok(1.1) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(lookup(&s, "USD"), Ok(1.1), "refreshed in the background");
        assert_eq!(n.load(Ordering::SeqCst), 1);
        let _ = fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn failed_download_does_not_retry_right_away() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let s = Source::new(
            None,
            Arc::new(move || {
                c.fetch_add(1, Ordering::SeqCst);
                Err("offline".into())
            }),
        );
        for _ in 0..3 {
            let e = lookup(&s, "USD").unwrap_err();
            assert!(e.contains(LOADING) || e.contains(UNAVAILABLE), "{e}");
        }
        std::thread::sleep(Duration::from_millis(50));
        assert!(lookup(&s, "USD").unwrap_err().contains(UNAVAILABLE));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
