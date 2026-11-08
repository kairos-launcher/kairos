//! Self-update from the GitHub releases. A worker checks once a day; with `auto_install` it
//! also downloads the new release (checked against its `SHA256SUMS`) and installs it the next
//! time the launcher hides, then restarts. `update` shows the state and checks on demand.
//!
//! Only copies built by the release workflow update themselves (it sets
//! `KAIROS_UPDATE_CHANNEL=github`): builds from source and distribution packages are updated
//! the way they were installed.

mod install;
mod release;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Once, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::core::sync::lock;
use crate::core::{fuzzy, icons, process};
use crate::plugin::{self, Error, ExecOutcome, Provider, Query, ResultItem};
use release::{Fetch, Kind, Release};

const ID: &str = "builtin-update";
const TRIGGER: &str = "update ";
const CURRENT: &str = env!("CARGO_PKG_VERSION");
const EVERY: Duration = Duration::from_hours(24);
/// The worker wakes at least this often to notice that its plugin was reloaded.
const NAP: Duration = Duration::from_hours(1);
/// No network right at startup.
const FIRST_CHECK_AFTER: Duration = Duration::from_mins(1);

/// What is ready to install.
#[derive(Clone, Debug)]
enum Staged {
    /// Unpacked release folder whose files replace ours.
    Files(PathBuf),
    /// Windows installer to run.
    Installer(PathBuf),
}

#[derive(Clone, Debug)]
enum Status {
    Idle,
    Busy(String),
    UpToDate,
    Available(Release),
    Ready(Release, Staged),
    Failed(String),
}

struct Inner {
    /// The install folder.
    dir: PathBuf,
    kind: Kind,
    /// We may write to `dir`; otherwise updates are only announced.
    writable: bool,
    auto_install: bool,
    fetch: Box<dyn Fetch>,
    /// When the last successful check found nothing (`update.json`).
    stamp: PathBuf,
    status: Mutex<Status>,
    next_check: Mutex<Instant>,
    /// One check, download or install at a time.
    work: Mutex<()>,
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// When the next check is due, from the time of the last one in `stamp`.
fn next_check(stamp: &Path, now: Instant) -> Instant {
    let earliest = now + FIRST_CHECK_AFTER;
    let checked = std::fs::read(stamp)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v.get("checked").and_then(Value::as_u64));
    let Some(checked) = checked else { return earliest };
    let ago = Duration::from_secs(unix_now().saturating_sub(checked));
    earliest.max(now + EVERY.saturating_sub(ago))
}

fn can_write(dir: &Path) -> bool {
    let probe = dir.join(".kairos-write-test");
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

impl Inner {
    fn set(&self, s: Status) {
        *lock(&self.status) = s;
    }

    fn status(&self) -> Status {
        lock(&self.status).clone()
    }

    /// Ask GitHub for the newest release; with `download`, also fetch it when it is newer.
    fn check(&self, download: bool) -> Result<(), String> {
        let _work = lock(&self.work);
        *lock(&self.next_check) = Instant::now() + EVERY;
        if let Status::Ready(..) = self.status() {
            return Ok(()); // already downloaded; nothing newer is worth a second download today
        }
        self.set(Status::Busy("Checking for updates…".into()));
        match release::latest(self.fetch.as_ref(), CURRENT, self.kind) {
            Err(e) => {
                self.set(Status::Failed(e.clone()));
                Err(e)
            }
            Ok(None) => {
                let _ =
                    crate::core::fs::write_atomic(&self.stamp, format!("{{\"checked\":{}}}", unix_now()).as_bytes());
                self.set(Status::UpToDate);
                Ok(())
            }
            Ok(Some(r)) => {
                log::info!("update: Kairos {} is available", r.version);
                self.set(Status::Available(r.clone()));
                if download && self.writable { self.download(&r) } else { Ok(()) }
            }
        }
    }

    fn download(&self, r: &Release) -> Result<(), String> {
        self.set(Status::Busy(format!("Downloading Kairos {}…", r.version)));
        let staged = self.stage(r);
        match staged {
            Ok(s) => {
                self.set(Status::Ready(r.clone(), s));
                Ok(())
            }
            Err(e) => {
                log::warn!("update: {e}");
                self.set(Status::Failed(e.clone()));
                Err(e)
            }
        }
    }

    fn stage(&self, r: &Release) -> Result<Staged, String> {
        let staging = self.dir.join(install::STAGING);
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(&staging).map_err(|e| format!("{}: {e}", staging.display()))?;
        let file = release::download(self.fetch.as_ref(), r, &staging)?;
        Ok(match self.kind {
            Kind::Installer => Staged::Installer(file),
            Kind::Archive => {
                let out = staging.join("unpacked");
                std::fs::create_dir(&out).map_err(|e| e.to_string())?;
                install::extract(&file, &out)?;
                Staged::Files(install::release_root(&out)?)
            }
        })
    }

    /// Install what is staged and restart. `shown`: the new instance opens its window.
    fn install(&self, shown: bool) -> Result<(), String> {
        let _work = lock(&self.work);
        let Status::Ready(r, staged) = self.status() else { return Err("no update is downloaded".into()) };
        let argv = match staged {
            Staged::Installer(setup) => install::installer_argv(&setup),
            Staged::Files(root) => {
                install::swap(&root, &self.dir).inspect_err(|e| self.set(Status::Failed(e.clone())))?;
                let mut argv = vec![self.dir.join(install::exe_name()).to_string_lossy().into_owned()];
                if !shown {
                    argv.push("--background".into());
                }
                argv
            }
        };
        log::info!("update: installing Kairos {}", r.version);
        self.set(Status::Busy(format!("Restarting into Kairos {}…", r.version)));
        crate::ui::quit_then(argv);
        Ok(())
    }
}

/// Checks daily until its plugin is dropped (disabled or reloaded).
fn spawn_worker(me: Weak<Inner>) {
    let spawned = std::thread::Builder::new().name("update".into()).spawn(move || {
        loop {
            let wait = {
                let Some(inner) = me.upgrade() else { return };
                lock(&inner.next_check).saturating_duration_since(Instant::now())
            };
            if !wait.is_zero() {
                std::thread::sleep(wait.min(NAP));
                continue;
            }
            let Some(inner) = me.upgrade() else { return };
            if let Err(e) = inner.check(inner.auto_install) {
                log::warn!("update check: {e}");
            }
        }
    });
    if let Err(e) = spawned {
        log::warn!("update: cannot start the worker: {e}");
    }
}

/// Where this copy is installed and how, or why it does not update itself.
fn target() -> Result<(PathBuf, Kind), &'static str> {
    if option_env!("KAIROS_UPDATE_CHANNEL") != Some("github") {
        return Err("Built from source: update it the way you installed it");
    }
    if !(cfg!(windows) || cfg!(all(target_os = "linux", target_arch = "x86_64"))) {
        return Err("There are no release builds for this system");
    }
    let dir = crate::config::exe_dir().ok_or("Cannot find the launcher's folder")?;
    let kind = if cfg!(windows) && !dir.join("portable").exists() { Kind::Installer } else { Kind::Archive };
    Ok((dir, kind))
}

pub struct UpdateProvider {
    inner: Result<Arc<Inner>, &'static str>,
}

impl UpdateProvider {
    pub fn new(settings: &Value) -> Self {
        let auto_install = settings.get("auto_install").and_then(Value::as_bool).unwrap_or(true);
        let inner = target().map(|(dir, kind)| {
            // Leftovers of the update that started this process.
            static CLEAN: Once = Once::new();
            CLEAN.call_once(|| install::clean_up(&dir));
            let stamp = crate::config::state_file("update.json");
            let inner = Arc::new(Inner {
                writable: can_write(&dir),
                next_check: Mutex::new(next_check(&stamp, Instant::now())),
                dir,
                kind,
                auto_install,
                fetch: Box::new(release::Ureq),
                stamp,
                status: Mutex::new(Status::Idle),
                work: Mutex::new(()),
            });
            spawn_worker(Arc::downgrade(&inner));
            inner
        });
        Self { inner }
    }

    #[cfg(test)]
    fn with(inner: Inner) -> Self {
        Self { inner: Ok(Arc::new(inner)) }
    }

    fn row(&self, score: f64) -> ResultItem {
        let icon = icons::themed(&["software-update-available", "system-software-update"], "U");
        let mk = |title: String, sub: String| ResultItem::new("update", title, sub, score).with_icon(icon.clone());
        let inner = match &self.inner {
            Ok(i) => i,
            Err(why) => {
                return mk(format!("Kairos {CURRENT}"), (*why).into()).with_action("page", "Open releases page");
            }
        };
        match inner.status() {
            Status::Idle => mk("Check for updates".into(), format!("Kairos {CURRENT}")).with_action("check", "Check"),
            Status::UpToDate => mk("Check for updates".into(), format!("Kairos {CURRENT} is the newest version"))
                .with_action("check", "Check"),
            Status::Busy(what) => mk(what, format!("Kairos {CURRENT}")).with_action("refresh", "Refresh"),
            Status::Failed(e) => mk("Update failed".into(), e).with_action("check", "Try again"),
            Status::Available(r) if !inner.writable => mk(
                format!("Kairos {} is available", r.version),
                "This folder is read-only for you: download it from the release page".into(),
            )
            .with_action("page", "Open release page"),
            Status::Available(r) => mk(
                format!("Update Kairos to {}", r.version),
                format!("You have {CURRENT} · downloads, installs and restarts"),
            )
            .with_action("install", "Update now")
            .with_action("page", "Release notes"),
            Status::Ready(r, _) => mk(format!("Restart into Kairos {}", r.version), "Downloaded and verified".into())
                .with_action("install", "Restart now")
                .with_action("page", "Release notes"),
        }
    }
}

impl Provider for UpdateProvider {
    fn id(&self) -> &str {
        ID
    }

    fn query(&self, q: &Query) -> Vec<ResultItem> {
        let text = q.text.trim();
        if q.trigger.is_some() {
            return vec![self.row(1.0)];
        }
        // Plain searches: only for "update", "kairos update" and the like.
        let score = ["update", "kairos update", "check for updates"]
            .iter()
            .filter_map(|t| fuzzy::score(text, t))
            .reduce(f64::max);
        match score {
            Some(s) if text.chars().count() >= 3 && s >= 0.7 => vec![self.row(s * 0.85)],
            _ => vec![],
        }
    }

    fn execute(&self, _result_id: &str, action_id: &str, _query: &str) -> plugin::Result<ExecOutcome> {
        let page = || match &self.inner {
            Ok(i) => match i.status() {
                Status::Available(r) | Status::Ready(r, _) => r.page,
                _ => release::RELEASES_PAGE.into(),
            },
            Err(_) => release::RELEASES_PAGE.into(),
        };
        if action_id == "page" {
            process::open_url(&page())?;
            return Ok(ExecOutcome::Close);
        }
        let inner = self.inner.as_ref().map_err(|why| Error::Message((*why).into()))?;
        match action_id {
            "check" => {
                inner.check(false)?;
                Ok(ExecOutcome::complete(TRIGGER))
            }
            "refresh" => Ok(ExecOutcome::complete(TRIGGER)),
            "install" => {
                if let Status::Available(r) = inner.status() {
                    let _work = lock(&inner.work);
                    inner.download(&r)?;
                }
                inner.install(true)?;
                Ok(ExecOutcome::Close)
            }
            _ => Err(Error::UnknownAction(action_id.into())),
        }
    }

    /// The launcher was hidden a while ago: a good moment to restart into a downloaded update.
    fn reap_idle(&self) {
        let Ok(inner) = &self.inner else { return };
        if inner.auto_install
            && matches!(inner.status(), Status::Ready(..))
            && let Err(e) = inner.install(false)
        {
            log::warn!("update: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use release::tests::{Fake, release_json, sha256_hex};

    fn q<'a>(text: &'a str, trigger: Option<&'a str>) -> Query<'a> {
        Query { text, trigger, limit: 8 }
    }

    fn inner(dir: &Path, fake: Fake) -> Inner {
        Inner {
            dir: dir.to_path_buf(),
            kind: Kind::Installer,
            writable: true,
            auto_install: true,
            fetch: Box::new(fake),
            stamp: dir.join("update.json"),
            status: Mutex::new(Status::Idle),
            next_check: Mutex::new(Instant::now()),
            work: Mutex::new(()),
        }
    }

    #[test]
    fn next_check_follows_the_stamp() {
        let t = crate::test_support::tmp();
        let stamp = t.path().join("update.json");
        let now = Instant::now();
        assert_eq!(next_check(&stamp, now), now + FIRST_CHECK_AFTER, "never checked");
        std::fs::write(&stamp, format!("{{\"checked\":{}}}", unix_now() - 3600)).unwrap();
        let due = next_check(&stamp, now);
        assert!(due > now + Duration::from_hours(22) && due <= now + Duration::from_hours(23));
        std::fs::write(&stamp, "{\"checked\":0}").unwrap();
        assert_eq!(next_check(&stamp, now), now + FIRST_CHECK_AFTER, "long ago");
    }

    #[test]
    fn check_downloads_a_newer_release_and_offers_it() {
        let t = crate::test_support::tmp();
        let json = release_json("v99.0.0");
        let fake = Fake(HashMap::from([
            (release::LATEST_URL.to_string(), json.into_bytes()),
            ("https://x/setup.exe".into(), b"setup".to_vec()),
            ("https://x/SHA256SUMS".into(), format!("{}  kairos-setup.exe\n", sha256_hex(b"setup")).into_bytes()),
        ]));
        let p = UpdateProvider::with(inner(t.path(), fake));
        let i = p.inner.as_ref().unwrap();

        i.check(false).unwrap();
        let row = &p.query(&q("", Some(TRIGGER)))[0];
        assert_eq!(row.title, "Update Kairos to 99.0.0");
        assert_eq!(row.default_action().unwrap().id, "install");
        assert!(!t.path().join("update.json").exists(), "only a check that found nothing is remembered");

        i.check(true).unwrap();
        let Status::Ready(_, Staged::Installer(setup)) = i.status() else { panic!("{:?}", i.status()) };
        assert_eq!(std::fs::read(setup).unwrap(), b"setup");
        assert!(p.query(&q("", Some(TRIGGER)))[0].title.starts_with("Restart into"));
    }

    #[test]
    fn up_to_date_and_failures_show_in_the_row() {
        let t = crate::test_support::tmp();
        let fake = Fake(HashMap::from([(release::LATEST_URL.to_string(), release_json("v0.0.1").into_bytes())]));
        let p = UpdateProvider::with(inner(t.path(), fake));
        p.inner.as_ref().unwrap().check(true).unwrap();
        assert!(p.query(&q("", Some(TRIGGER)))[0].subtitle.contains("newest"));
        assert!(t.path().join("update.json").exists());

        let p = UpdateProvider::with(inner(t.path(), Fake(HashMap::new())));
        p.execute("update", "check", "").unwrap();
        assert!(p.query(&q("", Some(TRIGGER)))[0].subtitle.contains("newest"), "no release published yet");

        let fake = Fake(HashMap::from([(release::LATEST_URL.to_string(), b"<html>".to_vec())]));
        let p = UpdateProvider::with(inner(t.path(), fake));
        assert!(p.execute("update", "check", "").is_err());
        let row = &p.query(&q("", Some(TRIGGER)))[0];
        assert_eq!(row.title, "Update failed");
        assert!(row.subtitle.contains("GitHub"));
    }

    #[test]
    fn plain_searches_need_the_word() {
        let p = UpdateProvider { inner: Err("source build") };
        assert!(p.query(&q("up", None)).is_empty());
        assert!(p.query(&q("firefox", None)).is_empty());
        assert_eq!(p.query(&q("update", None)).len(), 1);
        assert_eq!(p.query(&q("", Some(TRIGGER)))[0].subtitle, "source build");
    }
}
