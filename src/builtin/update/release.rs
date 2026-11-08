//! GitHub releases: which one is newest, which of its files fits this install, and fetching
//! that file with its SHA-256 checked against the release's `SHA256SUMS`.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

pub const LATEST_URL: &str = "https://api.github.com/repos/kairos-launcher/kairos/releases/latest";
pub const RELEASES_PAGE: &str = "https://github.com/kairos-launcher/kairos/releases";
const MAX_JSON: u64 = 1 << 20;
const MAX_SUMS: u64 = 64 << 10;
const MAX_ASSET: u64 = 512 << 20;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    /// Without the leading `v`.
    pub version: String,
    /// The release's web page (notes).
    pub page: String,
    /// The file this install updates from.
    pub asset: Asset,
    pub sums: Asset,
}

/// How this copy was installed, which decides the file it updates from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Linux tarball or Windows portable zip: unpack and replace the files.
    Archive,
    /// Windows installer: run the new one.
    Installer,
}

/// Whether `name` is the release file for `kind` on this system.
fn wanted(kind: Kind, name: &str) -> bool {
    match kind {
        Kind::Installer => name.starts_with("kairos-") && name.ends_with("-setup.exe"),
        Kind::Archive if cfg!(windows) => name.ends_with("-windows-x86_64-portable.zip"),
        Kind::Archive => name.ends_with("-linux-x86_64.tar.gz"),
    }
}

/// `1.2.3`, `v1.2.3` or `1.2.3-rc1` as comparable numbers (pre-release suffixes are ignored:
/// GitHub's "latest" never is one).
fn version(s: &str) -> Option<[u64; 3]> {
    let s = s.trim().trim_start_matches('v');
    let core = s.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    let v = [parts.next()??, parts.next().flatten().unwrap_or(0), parts.next().flatten().unwrap_or(0)];
    parts.next().is_none().then_some(v)
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    matches!((version(candidate), version(current)), (Some(c), Some(cur)) if c > cur)
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
}

/// The release in GitHub's `releases/latest` reply if it is newer than `current`. An error if
/// the reply is not a release, or it lacks the file for `kind` or the checksums.
pub fn parse_latest(json: &str, current: &str, kind: Kind) -> Result<Option<Release>, String> {
    let r: GhRelease = serde_json::from_str(json).map_err(|e| format!("unexpected reply from GitHub: {e}"))?;
    if r.draft || r.prerelease || !is_newer(&r.tag_name, current) {
        return Ok(None);
    }
    let find = |pick: &dyn Fn(&str) -> bool| {
        r.assets
            .iter()
            .find(|a| pick(&a.name))
            .map(|a| Asset { name: a.name.clone(), url: a.browser_download_url.clone() })
    };
    let tag = &r.tag_name;
    let asset = find(&|n| wanted(kind, n)).ok_or_else(|| format!("release {tag} has no download for this system"))?;
    let sums = find(&|n| n == "SHA256SUMS").ok_or_else(|| format!("release {tag} has no SHA256SUMS"))?;
    let page = if r.html_url.starts_with("https://") { r.html_url } else { RELEASES_PAGE.into() };
    Ok(Some(Release { version: tag.trim_start_matches('v').into(), page, asset, sums }))
}

/// The hex SHA-256 of `name` in a `sha256sum` listing.
fn expected_sum(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let mut it = line.split_whitespace();
        let (hash, file) = (it.next()?, it.next()?);
        let ok =
            file.trim_start_matches('*') == name && hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit());
        ok.then(|| hash.to_ascii_lowercase())
    })
}

#[derive(Debug, PartialEq, Eq)]
pub enum FetchError {
    /// HTTP 404: for `releases/latest`, nothing is published (or the repository is private).
    NotFound,
    Failed(String),
}

impl From<FetchError> for String {
    fn from(e: FetchError) -> Self {
        match e {
            FetchError::NotFound => "not found (HTTP 404)".into(),
            FetchError::Failed(m) => m,
        }
    }
}

/// One HTTPS GET into `out`, at most `max` bytes. A trait so tests run without a network.
pub trait Fetch: Send + Sync {
    fn get(&self, url: &str, accept: &str, max: u64, out: &mut dyn Write) -> Result<(), FetchError>;
}

/// The real thing. Builds an agent per request: checks are a day apart, so nothing is pooled.
pub struct Ureq;

impl Fetch for Ureq {
    fn get(&self, url: &str, accept: &str, max: u64, out: &mut dyn Write) -> Result<(), FetchError> {
        let failed = |e: &dyn std::fmt::Display| FetchError::Failed(format!("{url}: {e}"));
        let proxy = crate::core::proxy::current();
        let agent: ureq::Agent = crate::plugin::remote::agent_builder(None, proxy.as_ref())
            // Release downloads redirect to GitHub's file host; no credentials are sent.
            .max_redirects(5)
            .timeout_connect(Some(Duration::from_secs(15)))
            .timeout_global(Some(Duration::from_mins(10)))
            .build()
            .into();
        let resp = agent.get(url).header("Accept", accept).call().map_err(|e| failed(&e))?;
        let status = resp.status().as_u16();
        match status {
            200 => {}
            404 => return Err(FetchError::NotFound),
            _ => return Err(failed(&format!("HTTP {status}"))),
        }
        let mut body = resp.into_body().into_with_config().limit(max).reader();
        io::copy(&mut body, out).map_err(|e| failed(&e))?;
        Ok(())
    }
}

/// The newest release, if it is newer than `current`. None published yet: no update.
pub fn latest(fetch: &dyn Fetch, current: &str, kind: Kind) -> Result<Option<Release>, String> {
    let mut json = Vec::new();
    match fetch.get(LATEST_URL, "application/vnd.github+json", MAX_JSON, &mut json) {
        Ok(()) => {}
        Err(FetchError::NotFound) => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    parse_latest(&String::from_utf8_lossy(&json), current, kind)
}

/// Writes through to `inner` and hashes what passes.
struct Hashing<W> {
    inner: W,
    hash: ring::digest::Context,
}

impl<W: Write> Write for Hashing<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hash.update(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    bytes.iter().flat_map(|b| [DIGITS[usize::from(b >> 4)], DIGITS[usize::from(b & 15)]]).map(char::from).collect()
}

/// Download the release's file into `dir` and check it against `SHA256SUMS`. A file that does
/// not match is deleted.
pub fn download(fetch: &dyn Fetch, release: &Release, dir: &Path) -> Result<PathBuf, String> {
    let name = &release.asset.name;
    // It becomes a file name: no path parts.
    if name.is_empty() || name.contains(['/', '\\']) || name.starts_with('.') {
        return Err(format!("odd file name {name:?} in the release"));
    }
    let mut sums = Vec::new();
    fetch.get(&release.sums.url, "application/octet-stream", MAX_SUMS, &mut sums)?;
    let want = expected_sum(&String::from_utf8_lossy(&sums), name)
        .ok_or_else(|| format!("SHA256SUMS of {} does not list {name}", release.version))?;

    let path = dir.join(name);
    let file = std::fs::File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut w = Hashing { inner: io::BufWriter::new(file), hash: ring::digest::Context::new(&ring::digest::SHA256) };
    let got = fetch
        .get(&release.asset.url, "application/octet-stream", MAX_ASSET, &mut w)
        .map_err(String::from)
        .and_then(|()| w.flush().map_err(|e| format!("{}: {e}", path.display())))
        .map(|()| hex(w.hash.finish().as_ref()));
    match got {
        Ok(h) if h == want => Ok(path),
        other => {
            let _ = std::fs::remove_file(&path);
            Err(other.err().unwrap_or_else(|| format!("{name} does not match its checksum; not installed")))
        }
    }
}

#[cfg(test)]
pub mod tests {
    use std::collections::HashMap;

    use super::*;

    /// Serves canned bodies by URL.
    pub struct Fake(pub HashMap<String, Vec<u8>>);

    impl Fetch for Fake {
        fn get(&self, url: &str, _: &str, max: u64, out: &mut dyn Write) -> Result<(), FetchError> {
            let body = self.0.get(url).ok_or(FetchError::NotFound)?;
            if body.len() as u64 > max {
                return Err(FetchError::Failed("too big".into()));
            }
            out.write_all(body).map_err(|e| FetchError::Failed(e.to_string()))
        }
    }

    pub fn sha256_hex(data: &[u8]) -> String {
        hex(ring::digest::digest(&ring::digest::SHA256, data).as_ref())
    }

    pub fn release_json(tag: &str) -> String {
        serde_json::json!({
            "tag_name": tag,
            "html_url": format!("https://github.com/kairos-launcher/kairos/releases/tag/{tag}"),
            "assets": [
                { "name": "kairos-linux-x86_64.tar.gz", "browser_download_url": "https://x/linux.tgz" },
                { "name": "kairos-windows-x86_64-portable.zip", "browser_download_url": "https://x/win.zip" },
                { "name": "kairos-setup.exe", "browser_download_url": "https://x/setup.exe" },
                { "name": "SHA256SUMS", "browser_download_url": "https://x/SHA256SUMS" },
            ],
        })
        .to_string()
    }

    #[test]
    fn versions_compare_numerically() {
        assert!(is_newer("v0.2.0", "0.1.0"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("v1", "0.9.0"));
        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(!is_newer("v0.1.0-rc1", "0.1.0"));
        assert!(!is_newer("0.0.9", "0.1.0"));
        assert!(!is_newer("nightly", "0.1.0"));
        assert!(!is_newer("1.2.3.4", "0.1.0"));
    }

    #[test]
    fn picks_the_file_for_the_install() {
        let r = parse_latest(&release_json("v0.2.0"), "0.1.0", Kind::Installer).unwrap().unwrap();
        assert_eq!(r.version, "0.2.0");
        assert_eq!(r.asset.name, "kairos-setup.exe");
        assert_eq!(r.sums.url, "https://x/SHA256SUMS");
        assert!(r.page.ends_with("/tag/v0.2.0"));
        let r = parse_latest(&release_json("v0.2.0"), "0.1.0", Kind::Archive).unwrap().unwrap();
        let want = if cfg!(windows) { "https://x/win.zip" } else { "https://x/linux.tgz" };
        assert_eq!(r.asset.url, want);
    }

    #[test]
    fn same_older_or_prerelease_is_no_update() {
        assert_eq!(parse_latest(&release_json("v0.1.0"), "0.1.0", Kind::Archive), Ok(None));
        let pre = release_json("v9.0.0").replace("\"tag_name\"", "\"prerelease\":true,\"tag_name\"");
        assert_eq!(parse_latest(&pre, "0.1.0", Kind::Archive), Ok(None));
        assert!(parse_latest("{\"message\":\"rate limited\"}", "0.1.0", Kind::Archive).is_err());
        let no_sums = release_json("v0.2.0").replace("SHA256SUMS", "README");
        assert!(parse_latest(&no_sums, "0.1.0", Kind::Archive).unwrap_err().contains("SHA256SUMS"));
    }

    #[test]
    fn checksum_lines_are_matched_by_exact_name() {
        let h = "a".repeat(64);
        let sums = format!("{h}  kairos.zip\n{}  *other.exe\n", "B".repeat(64));
        assert_eq!(expected_sum(&sums, "kairos.zip"), Some(h));
        assert_eq!(expected_sum(&sums, "other.exe"), Some("b".repeat(64)));
        assert_eq!(expected_sum(&sums, "kairos"), None);
        assert_eq!(expected_sum("xyz  kairos.zip", "kairos.zip"), None);
    }

    fn fake_release(body: &[u8], listed: &str) -> (Fake, Release) {
        let r = parse_latest(&release_json("v0.2.0"), "0.1.0", Kind::Installer).unwrap().unwrap();
        let sums = format!("{listed}  {}\n", r.asset.name);
        let fake = Fake(HashMap::from([(r.asset.url.clone(), body.to_vec()), (r.sums.url.clone(), sums.into_bytes())]));
        (fake, r)
    }

    #[test]
    fn download_keeps_only_a_matching_file() {
        let tmp = crate::test_support::tmp();
        let (fake, r) = fake_release(b"new build", &sha256_hex(b"new build"));
        let path = download(&fake, &r, tmp.path()).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new build");

        let (fake, r) = fake_release(b"tampered", &sha256_hex(b"new build"));
        let err = download(&fake, &r, tmp.path()).unwrap_err();
        assert!(err.contains("checksum"), "{err}");
        assert!(!tmp.path().join(&r.asset.name).exists(), "a bad download is deleted");
    }
}
