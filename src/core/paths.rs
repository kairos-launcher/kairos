//! Path helpers shared by plugins and config.

use std::path::{Path, PathBuf};

/// `~` and `~/x` become the home folder, as in a shell; anything else is returned unchanged.
/// (`~user` is not expanded.)
pub fn expand_home(p: impl AsRef<Path>) -> PathBuf {
    let p = p.as_ref();
    match (p.strip_prefix("~"), dirs::home_dir()) {
        (Ok(rest), Some(home)) if rest.as_os_str().is_empty() => home,
        (Ok(rest), Some(home)) => home.join(rest),
        _ => p.to_path_buf(),
    }
}

/// [`expand_home`], then anchor a relative path at `base`.
pub fn resolve(p: impl AsRef<Path>, base: &Path) -> PathBuf {
    base.join(expand_home(p)) // `join` keeps absolute paths as they are
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_only_a_leading_tilde_component() {
        let Some(home) = dirs::home_dir() else { return };
        assert_eq!(expand_home("~"), home);
        assert_eq!(expand_home("~/a/b"), home.join("a/b"));
        assert_eq!(expand_home("~user/x"), PathBuf::from("~user/x"));
        assert_eq!(expand_home("a/~/b"), PathBuf::from("a/~/b"));
        assert_eq!(resolve("x", Path::new("/base")), PathBuf::from("/base/x"));
        assert_eq!(resolve("/abs", Path::new("/base")), PathBuf::from("/abs"));
        assert_eq!(resolve("~/x", Path::new("/base")), home.join("x"));
    }
}
