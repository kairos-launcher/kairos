//! Private values for plugins (API keys and tokens), kept out of `config.toml`.
//!
//! One file per value under `<config>/secrets/`, readable only by the user:
//! `<id>` holds a remote plugin's API token, `<id>.<key>` a plugin's `secret` setting and
//! `<id>.<key>.<name>` one entry of a `secrets` setting.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The folder holding every secret.
fn dir() -> PathBuf {
    crate::config::config_dir().join("secrets")
}

/// Plugin ids, setting keys and secret names become file names: no separators, no `..`.
pub fn valid_name(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Store `value` privately, replacing any old one; an empty value removes it.
pub fn write(plugin: &str, key: Option<&str>, value: &str) -> Result<PathBuf, String> {
    write_in(&dir(), plugin, key, value)
}

/// `write` into the store in `dir`.
pub fn write_in(dir: &Path, plugin: &str, key: Option<&str>, value: &str) -> Result<PathBuf, String> {
    if dir.as_os_str().is_empty() {
        return Err("no place to store secrets".into());
    }
    if !valid_name(plugin) {
        return Err(format!("{plugin:?} is not a plugin id"));
    }
    if let Some(k) = key
        && !valid_name(k)
    {
        return Err(format!("{k:?} is not a setting key"));
    }
    let path = match key {
        None => dir.join(plugin),
        Some(k) => dir.join(format!("{plugin}.{k}")),
    };
    store(&path, value)
}

/// Write `value` to `path` privately, or remove the file for an empty value.
fn store(path: &Path, value: &str) -> Result<PathBuf, String> {
    let value = value.trim();
    if value.contains(['\r', '\n']) {
        return Err("the value must be one line".into());
    }
    if value.is_empty() {
        return match std::fs::remove_file(path) {
            Ok(()) => Ok(path.to_path_buf()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(path.to_path_buf()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        };
    }
    write_private(path, value)?;
    Ok(path.to_path_buf())
}

fn write_private(path: &Path, value: &str) -> Result<(), String> {
    let dir = path.parent().ok_or("no config folder")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        // Created private from the start (no window where it is world-readable).
        let _ = std::fs::remove_file(path);
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        writeln!(f, "{value}").map_err(|e| e.to_string())?;
    }
    #[cfg(not(unix))]
    std::fs::write(path, format!("{value}\n")).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(())
}

/// The value from the environment (`env_value`, if set and not blank) or else the file at
/// `path`, which must not be readable by others. `None` when neither has one.
pub fn read(env_value: Option<&str>, path: &Path) -> Result<Option<String>, String> {
    if let Some(t) = env_value.map(str::trim).filter(|t| !t.is_empty()) {
        return Ok(Some(t.to_string()));
    }
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path).map_err(|e| e.to_string())?.permissions().mode();
        if mode & 0o077 != 0 {
            return Err(format!("{} is accessible by others; run chmod 600", path.display()));
        }
    }
    let t = text.trim();
    Ok((!t.is_empty()).then(|| t.to_string()))
}

/// Store one entry of a `secrets` setting (an empty value removes it).
pub fn write_entry_in(dir: &Path, plugin: &str, key: &str, name: &str, value: &str) -> Result<PathBuf, String> {
    if dir.as_os_str().is_empty() {
        return Err("no place to store secrets".into());
    }
    if !valid_name(plugin) {
        return Err(format!("{plugin:?} is not a plugin id"));
    }
    if !valid_name(key) {
        return Err(format!("{key:?} is not a setting key"));
    }
    if !valid_name(name) || name.len() > 64 {
        return Err("a name may only use letters, digits, - and _".into());
    }
    store(&dir.join(format!("{plugin}.{key}.{name}")), value)
}

/// Every entry of a plugin's `secrets` setting, by name. Unusable files are reported and
/// left out, as in `setting_in`.
pub fn table_in(dir: &Path, plugin: &str, key: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if dir.as_os_str().is_empty() || !valid_name(plugin) || !valid_name(key) {
        return out;
    }
    let prefix = format!("{plugin}.{key}.");
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    for e in entries.flatten() {
        let file = e.file_name();
        let Some(name) = file.to_str().and_then(|f| f.strip_prefix(&prefix)).filter(|n| valid_name(n)) else {
            continue;
        };
        match read(None, &e.path()) {
            Ok(Some(v)) => {
                out.insert(name.to_string(), v);
            }
            Ok(None) => {}
            Err(err) => log::warn!("plugins.{plugin}.{key}.{name}: {err}; treating it as unset"),
        }
    }
    out
}

/// The folder of the user's real secrets.
pub fn user_dir() -> PathBuf {
    dir()
}

/// A plugin's `secret` setting from the store in `dir`, or `None` if unset (or `dir` is empty,
/// as in tests). A file that cannot be used (e.g. readable by others) is reported and counts
/// as unset.
pub fn setting_in(dir: &Path, plugin: &str, key: &str) -> Option<String> {
    if dir.as_os_str().is_empty() || !valid_name(plugin) || !valid_name(key) {
        return None;
    }
    read(None, &dir.join(format!("{plugin}.{key}"))).unwrap_or_else(|e| {
        log::warn!("plugins.{plugin}.{key}: {e}; treating it as unset");
        None
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_wins_over_the_file() {
        let missing = Path::new("/nonexistent/kairos-secret");
        assert_eq!(read(Some(" from-env \n"), missing).unwrap().as_deref(), Some("from-env"));
        assert_eq!(read(Some("  "), missing).unwrap(), None, "blank env falls through to the file");
        assert_eq!(read(None, missing).unwrap(), None);
    }

    #[test]
    fn names_cannot_escape_the_folder_and_values_are_one_line() {
        assert!(write("../evil", None, "t").is_err());
        assert!(write("ok", Some("../k"), "t").is_err());
        assert!(write("ok", None, "two\nlines").is_err());
        assert_eq!(setting_in(Path::new("/x"), "../evil", "k"), None);
        assert_eq!(setting_in(Path::new(""), "ai", "k"), None, "no store in tests");
    }

    #[cfg(unix)]
    #[test]
    fn files_are_private_and_shared_ones_are_refused() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = crate::test_support::tmp();
        let p = tmp.path().join("s").join("x.key");
        write_private(&p, "sk-123").unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(read(None, &p).unwrap().as_deref(), Some("sk-123"));
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read(None, &p).unwrap_err().contains("chmod 600"));
        assert_eq!(setting_in(&tmp.path().join("s"), "x", "key"), None, "a shared file counts as unset");

        let store = tmp.path().join("store");
        let path = write_in(&store, "ai", Some("api_key"), " sk-2 \n").unwrap();
        assert_eq!(path, store.join("ai.api_key"));
        assert_eq!(write_in(&store, "jira", None, "t").unwrap(), store.join("jira"), "a remote token");
        assert_eq!(setting_in(&store, "ai", "api_key").as_deref(), Some("sk-2"));
        write_in(&store, "ai", Some("api_key"), "").unwrap();
        assert_eq!(setting_in(&store, "ai", "api_key"), None, "an empty value removes it");
        write_in(&store, "ai", Some("api_key"), "").unwrap(); // removing twice is fine

        write_entry_in(&store, "ai", "secrets", "gh", "t1").unwrap();
        write_entry_in(&store, "ai", "secrets", "jira", "t2").unwrap();
        write_entry_in(&store, "other", "secrets", "x", "t3").unwrap();
        assert!(write_entry_in(&store, "ai", "secrets", "a.b", "t").is_err(), "no dots in names");
        assert!(write_entry_in(&store, "ai", "secrets", "../x", "t").is_err());
        let all = table_in(&store, "ai", "secrets");
        assert_eq!(all.into_iter().collect::<Vec<_>>(), [("gh".into(), "t1".into()), ("jira".into(), "t2".into())]);
        write_entry_in(&store, "ai", "secrets", "gh", "").unwrap();
        assert_eq!(table_in(&store, "ai", "secrets").len(), 1, "an empty value removes it");
        assert!(table_in(Path::new(""), "ai", "secrets").is_empty(), "no store in tests");
    }
}
