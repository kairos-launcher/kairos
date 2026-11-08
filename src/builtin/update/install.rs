//! Putting a downloaded release in place of the running one.
//!
//! Files are replaced by renaming: the old file becomes `<name>.old` and the new one moves in.
//! That works while the program runs, on Windows too (where a running file cannot be
//! overwritten or deleted, only renamed). The next start deletes the `.old` files.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Folder in the install folder where updates are downloaded and unpacked.
pub const STAGING: &str = ".update";
/// The user's data in a portable install, and the file that turns portable mode on.
const KEEP: [&str; 2] = ["data", "portable"];

pub fn exe_name() -> String {
    format!("kairos{}", std::env::consts::EXE_SUFFIX)
}

fn with_old(p: &Path) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(".old");
    s.into()
}

/// Remove what an earlier update left behind: the staging folder and the replaced files.
pub fn clean_up(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir.join(STAGING));
    remove_old(dir, true);
}

fn remove_old(dir: &Path, top: bool) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let (p, name) = (e.path(), e.file_name());
        if top && KEEP.iter().any(|k| name == *k) {
            continue;
        }
        match e.file_type() {
            Ok(t) if t.is_dir() => remove_old(&p, false),
            Ok(t) if t.is_file() && name.to_string_lossy().ends_with(".old") => {
                let _ = std::fs::remove_file(&p);
            }
            _ => {}
        }
    }
}

/// Unpack `archive` into `into` with the system tar. On Windows that is its own tar.exe,
/// which also reads zip (a tar from Git for Windows on PATH would not).
pub fn extract(archive: &Path, into: &Path) -> Result<(), String> {
    let tar = if cfg!(windows) {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        PathBuf::from(root).join("System32").join("tar.exe")
    } else {
        PathBuf::from("tar")
    };
    let out = Command::new(&tar)
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(into)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{}: {e}", tar.display()))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("unpacking the update failed: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// The folder in `extracted` that holds the release (the Linux archive wraps it in one folder).
pub fn release_root(extracted: &Path) -> Result<PathBuf, String> {
    let exe = exe_name();
    if extracted.join(&exe).is_file() {
        return Ok(extracted.to_path_buf());
    }
    let entries: Vec<_> = std::fs::read_dir(extracted).map_err(|e| e.to_string())?.flatten().collect();
    match entries.as_slice() {
        [one] if one.path().join(&exe).is_file() => Ok(one.path()),
        _ => Err(format!("the download does not contain {exe}")),
    }
}

/// Regular files under `dir`, as paths relative to it. Links are skipped.
fn files(dir: &Path, rel: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for e in std::fs::read_dir(dir.join(rel))? {
        let e = e?;
        let r = rel.join(e.file_name());
        let t = e.file_type()?;
        if t.is_dir() {
            files(dir, &r, out)?;
        } else if t.is_file() {
            out.push(r);
        }
    }
    Ok(())
}

/// Move every file of `from` to the same place in `to`, renaming a file already there to
/// `<name>.old` first. If a move fails, the ones before it are undone. `data/` and `portable`
/// in `to` are never touched.
pub fn swap(from: &Path, to: &Path) -> Result<(), String> {
    let mut rels = vec![];
    files(from, Path::new(""), &mut rels).map_err(|e| format!("{}: {e}", from.display()))?;
    rels.retain(|r| !r.components().next().is_some_and(|c| KEEP.iter().any(|k| c.as_os_str() == *k)));

    // (new file in place, old one kept as `.old`)
    let mut done: Vec<(PathBuf, bool)> = vec![];
    let mut step = |rel: &Path| -> std::io::Result<()> {
        let dst = to.join(rel);
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let had = dst.exists();
        if had {
            let old = with_old(&dst);
            let _ = std::fs::remove_file(&old);
            std::fs::rename(&dst, &old)?;
        }
        if let Err(e) = std::fs::rename(from.join(rel), &dst) {
            if had {
                let _ = std::fs::rename(with_old(&dst), &dst);
            }
            return Err(e);
        }
        done.push((dst, had));
        Ok(())
    };
    for rel in &rels {
        if let Err(e) = step(rel) {
            for (dst, had) in done.iter().rev() {
                let _ = std::fs::remove_file(dst);
                if *had {
                    let _ = std::fs::rename(with_old(dst), dst);
                }
            }
            return Err(format!("replacing {}: {e}", rel.display()));
        }
    }
    Ok(())
}

/// The command that installs the downloaded Windows installer without asking. It closes the
/// launcher if still running and starts it again afterwards (`packaging/kairos.iss`).
pub fn installer_argv(setup: &Path) -> Vec<String> {
    let mut argv = vec![setup.to_string_lossy().into_owned()];
    argv.extend(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/CLOSEAPPLICATIONS"].map(String::from));
    argv
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(p: &Path, s: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, s).unwrap();
    }

    fn read(p: &Path) -> String {
        std::fs::read_to_string(p).unwrap()
    }

    #[test]
    fn swap_replaces_files_and_keeps_user_data() {
        let (new_t, app_t) = (crate::test_support::tmp(), crate::test_support::tmp());
        let (new, app) = (new_t.path(), app_t.path());
        let exe = exe_name();
        write(&new.join(&exe), "v2");
        write(&new.join("plugins/calc/plugin.toml"), "calc v2");
        write(&new.join("plugins/new/plugin.toml"), "new plugin");
        write(&new.join("portable"), "");
        write(&new.join("data/config.toml"), "from the archive");
        write(&app.join(&exe), "v1");
        write(&app.join("plugins/calc/plugin.toml"), "calc v1");
        write(&app.join("data/config.toml"), "mine");

        swap(new, app).unwrap();
        assert_eq!(read(&app.join(&exe)), "v2");
        assert_eq!(read(&app.join(format!("{exe}.old"))), "v1");
        assert_eq!(read(&app.join("plugins/calc/plugin.toml")), "calc v2");
        assert_eq!(read(&app.join("plugins/new/plugin.toml")), "new plugin");
        assert_eq!(read(&app.join("data/config.toml")), "mine");
        assert!(!app.join("portable").exists(), "portable mode is not switched on by an update");

        clean_up(app);
        assert!(!app.join(format!("{exe}.old")).exists());
        assert!(!app.join("plugins/calc/plugin.toml.old").exists());
        assert_eq!(read(&app.join(&exe)), "v2");
    }

    #[test]
    fn clean_up_leaves_old_files_in_data_alone() {
        let t = crate::test_support::tmp();
        write(&t.path().join("data/notes.old"), "mine");
        write(&t.path().join(STAGING).join("x"), "");
        clean_up(t.path());
        assert!(t.path().join("data/notes.old").exists());
        assert!(!t.path().join(STAGING).exists());
    }

    #[test]
    fn release_root_finds_the_wrapped_folder() {
        let t = crate::test_support::tmp();
        let inner = t.path().join("kairos-v2-linux-x86_64");
        write(&inner.join(exe_name()), "");
        assert_eq!(release_root(t.path()).unwrap(), inner);
        write(&t.path().join("stray"), "");
        assert!(release_root(t.path()).is_err());
        write(&t.path().join(exe_name()), "");
        assert_eq!(release_root(t.path()).unwrap(), t.path());
    }

    #[cfg(unix)]
    #[test]
    fn extract_unpacks_a_tarball() {
        let t = crate::test_support::tmp();
        let src = t.path().join("src");
        write(&src.join("kairos-v2/kairos"), "v2");
        let tgz = t.path().join("a.tar.gz");
        let ok = Command::new("tar").arg("-czf").arg(&tgz).arg("-C").arg(&src).arg("kairos-v2").status().unwrap();
        assert!(ok.success());
        let out = t.path().join("out");
        std::fs::create_dir(&out).unwrap();
        extract(&tgz, &out).unwrap();
        assert_eq!(read(&release_root(&out).unwrap().join("kairos")), "v2");
        assert!(extract(&t.path().join("missing.tgz"), &out).is_err());
    }
}
