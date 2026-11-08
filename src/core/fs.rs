//! File writes that never leave a half-written file behind, and file safety checks.

use std::io::{self, Write};
use std::path::Path;

/// Whether every user may write to `path` (Unix). Code from such a place could have been put
/// there by anyone, and plugins and scripts run as the user.
pub fn world_writable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o002 != 0)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        false
    }
}

/// Create `dir` (and its parents) and make it readable only by the user (mode 0700 on Unix).
pub fn create_private_dir(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Write `contents` to a temporary file next to `path`, then rename it over `path`. A crash
/// leaves either the old file or the new one, never a truncated mix. Creates parent folders.
pub fn write_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    write(path, contents, false)
}

/// Like [`write_atomic`], but the file is readable only by the user (Unix). For files that may
/// hold secrets typed into the launcher, such as command history.
pub fn write_private(path: &Path, contents: &[u8]) -> io::Result<()> {
    write(path, contents, true)
}

fn write(path: &Path, contents: &[u8], private: bool) -> io::Result<()> {
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let name = path.file_name().ok_or_else(|| io::Error::other(format!("{} is not a file path", path.display())))?;
    let tmp = dir.join(format!(".{}.tmp", name.to_string_lossy()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    #[cfg(not(unix))]
    let _ = private;
    let mut file = options.open(&tmp)?;
    file.write_all(contents)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_the_file_and_leaves_no_temp_behind() {
        let tmp_dir = crate::test_support::tmp();
        let dir = tmp_dir.path().to_path_buf();
        let path = dir.join("nested").join("f.json");
        write_atomic(&path, b"one").unwrap();
        write_atomic(&path, b"two").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        let names: Vec<_> =
            std::fs::read_dir(path.parent().unwrap()).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(names, ["f.json"]);
    }

    #[cfg(unix)]
    #[test]
    fn private_files_are_user_only() {
        use std::os::unix::fs::PermissionsExt;
        let tmp_dir = crate::test_support::tmp();
        let dir = tmp_dir.path().to_path_buf();
        let path = dir.join("h.json");
        write_private(&path, b"secret").unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }
}
