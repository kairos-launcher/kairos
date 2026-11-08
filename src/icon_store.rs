//! Turns plugin-supplied icon data into image files the UI can load, and remembers lazily
//! fetched icons. Inline `base64` icons are decoded once and cached on disk by content, so a
//! plugin that repeats the same icon on every keystroke costs one hash lookup, not a decode.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::core::sync::lock;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

/// Largest decoded icon accepted. Icons are drawn at about 32 px; anything bigger is a mistake.
const MAX_BYTES: usize = 512 * 1024;
/// Files kept in the on-disk cache after pruning.
const KEEP_FILES: usize = 300;
/// In-memory `content hash -> file` entries before the map is reset.
const MAX_MEMO: usize = 512;

/// The answer to a lazy icon request, cached so each icon is asked for once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LazyIcon {
    /// The plugin has no image for this id.
    Missing,
    File(PathBuf),
}

pub struct IconStore {
    dir: PathBuf,
    /// `(plugin id, content hash)` -> stored file, `None` if the data was not a usable image.
    memo: Mutex<HashMap<(String, u64), Option<PathBuf>>>,
    /// `(plugin id, icon id)` -> what the plugin answered.
    lazy: Mutex<HashMap<(String, String), LazyIcon>>,
}

impl IconStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir, memo: Mutex::default(), lazy: Mutex::default() }
    }

    pub fn default_dir() -> PathBuf {
        if let Some(p) = crate::config::portable_dir() {
            return p.join("cache").join("icons");
        }
        dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("kairos").join("icons")
    }

    /// Decode `data` (base64 PNG or JPEG), store it content-addressed, return the file.
    /// Files are kept per plugin: FNV-1a is fast but easy to collide on purpose, and a
    /// collision must not let one plugin (say, a remote server) replace another plugin's icon.
    pub fn store_base64(&self, plugin: &str, data: &str) -> Option<PathBuf> {
        let key = (plugin.to_string(), fnv1a(data.as_bytes()));
        if let Some(hit) = lock(&self.memo).get(&key) {
            return hit.clone();
        }
        let path = self.decode_and_write(data, &key.0, key.1);
        let mut memo = lock(&self.memo);
        if memo.len() >= MAX_MEMO {
            memo.clear();
        }
        memo.insert(key, path.clone());
        path
    }

    fn decode_and_write(&self, data: &str, plugin: &str, hash: u64) -> Option<PathBuf> {
        let compact: String = data.split_whitespace().collect();
        if compact.len() > MAX_BYTES * 4 / 3 + 8 {
            return None;
        }
        let bytes = STANDARD.decode(compact).ok()?;
        if bytes.len() > MAX_BYTES {
            return None;
        }
        // Trust the content, not the declared mime type.
        let ext = if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
            "png"
        } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
            "jpg"
        } else {
            return None;
        };
        let owner: String =
            plugin.chars().map(|c| if c.is_ascii_alphanumeric() || "-_.".contains(c) { c } else { '_' }).collect();
        let path = self.dir.join(format!("{owner}-{hash:016x}.{ext}"));
        if !path.is_file() {
            std::fs::create_dir_all(&self.dir).ok()?;
            std::fs::write(&path, bytes).ok()?;
        }
        Some(path)
    }

    /// What is known about a lazy icon; `None` if the plugin was never asked.
    pub fn lazy_get(&self, plugin: &str, icon_id: &str) -> Option<LazyIcon> {
        lock(&self.lazy).get(&(plugin.to_string(), icon_id.to_string())).cloned()
    }

    pub fn lazy_put(&self, plugin: &str, icon_id: &str, icon: LazyIcon) {
        lock(&self.lazy).insert((plugin.to_string(), icon_id.to_string()), icon);
    }

    /// Delete the oldest cached files in `dir` beyond `KEEP_FILES`. Run once at startup, off
    /// the UI thread.
    pub fn prune(dir: &Path) {
        prune_dir(dir, KEEP_FILES);
    }
}

fn prune_dir(dir: &Path, keep: usize) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<_> =
        rd.flatten().filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path()))).collect();
    if files.len() <= keep {
        return;
    }
    files.sort(); // oldest first
    for (_, p) in files.iter().take(files.len() - keep) {
        let _ = std::fs::remove_file(p);
    }
}

/// Stable across runs and Rust versions (unlike `DefaultHasher`), which the file names rely on.
pub(crate) fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A valid 1x1 PNG.
    const PNG_B64: &str =
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

    /// A store in a folder that does not exist yet, inside a temporary folder.
    fn store() -> (tempfile::TempDir, IconStore) {
        let tmp = crate::test_support::tmp();
        let s = IconStore::new(tmp.path().join("icons"));
        (tmp, s)
    }

    #[test]
    fn stores_png_once_by_content_and_ignores_whitespace() {
        let (_tmp, s) = store();
        let a = s.store_base64("p", PNG_B64).expect("valid png");
        assert!(a.is_file() && a.extension().unwrap() == "png");
        assert_eq!(std::fs::read(&a).unwrap()[..4], [0x89, b'P', b'N', b'G']);
        let spaced = format!("{}\n{}", &PNG_B64[..20], &PNG_B64[20..]);
        let b = s.store_base64("p", &spaced).unwrap();
        assert_eq!(std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap(), "same bytes either way");
        assert_eq!(s.store_base64("p", PNG_B64), Some(a), "second call is a cache hit");
    }

    #[test]
    fn rejects_garbage_non_images_and_oversize() {
        let (_tmp, s) = store();
        assert_eq!(s.store_base64("p", "not base64 !!!"), None);
        assert_eq!(s.store_base64("p", &STANDARD.encode(b"<svg></svg>")), None, "only PNG/JPEG");
        assert_eq!(s.store_base64("p", &STANDARD.encode(vec![0x89u8; MAX_BYTES + 1])), None);
        assert!(!s.dir.exists() || std::fs::read_dir(&s.dir).unwrap().next().is_none(), "nothing written");
    }

    #[test]
    fn plugins_never_share_icon_files() {
        let (_tmp, s) = store();
        let a = s.store_base64("a", PNG_B64).unwrap();
        let b = s.store_base64("b", PNG_B64).unwrap();
        assert_ne!(a, b, "the same bytes from another plugin get their own file");
        assert!(a.file_name().unwrap().to_string_lossy().starts_with("a-"));
        let odd = s.store_base64("../x", PNG_B64).unwrap();
        assert_eq!(odd.parent(), a.parent(), "an odd id cannot leave the cache folder");
    }

    #[test]
    fn jpeg_is_detected_by_magic_bytes() {
        let (_tmp, s) = store();
        let p = s.store_base64("p", &STANDARD.encode([0xFF, 0xD8, 0xFF, 0xE0, 0, 0])).unwrap();
        assert_eq!(p.extension().unwrap(), "jpg");
    }

    #[test]
    fn prune_keeps_the_newest() {
        let (_tmp, s) = store();
        std::fs::create_dir_all(&s.dir).unwrap();
        for i in 0..5 {
            std::fs::write(s.dir.join(format!("{i}.png")), b"x").unwrap();
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        prune_dir(&s.dir, 2);
        let mut left: Vec<_> =
            std::fs::read_dir(&s.dir).unwrap().flatten().map(|e| e.file_name().into_string().unwrap()).collect();
        left.sort();
        assert_eq!(left, ["3.png", "4.png"]);
    }

    #[test]
    fn lazy_cache_distinguishes_missing_from_unknown() {
        let (_tmp, s) = store();
        assert_eq!(s.lazy_get("p", "a"), None, "never asked");
        s.lazy_put("p", "a", LazyIcon::Missing);
        assert_eq!(s.lazy_get("p", "a"), Some(LazyIcon::Missing), "asked, plugin had none");
        s.lazy_put("p", "b", LazyIcon::File(PathBuf::from("/x.png")));
        assert_eq!(s.lazy_get("p", "b"), Some(LazyIcon::File(PathBuf::from("/x.png"))));
    }
}
