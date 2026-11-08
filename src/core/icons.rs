//! Resolve a `.desktop` `Icon=` value to an image file the UI can load (PNG, JPEG or SVG).
//! Follows the freedesktop icon-theme lookup in a simplified form: the user's theme, then the
//! themes it inherits from, then `hicolor`, then `pixmaps`. Within a theme a PNG of any size wins
//! over an SVG (which the UI has to rasterize), but the theme order comes first, so an SVG-only
//! theme such as Papirus is used as chosen.

#[cfg(windows)]
#[allow(unsafe_code)]
pub mod win;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Preferred pixel sizes, best first: rows draw icons at 32 logical pixels.
const SIZES: [u32; 9] = [48, 64, 32, 96, 128, 256, 24, 22, 16];
const EXTENSIONS: [&str; 3] = ["png", "jpg", "jpeg"];
/// Vector formats, tried after the raster ones.
const VECTOR: [&str; 2] = ["svg", "svgz"];

pub struct IconResolver {
    /// `.../icons` directories, highest priority first.
    roots: Vec<PathBuf>,
    pixmaps: Vec<PathBuf>,
    /// Theme names to search, most specific first, always ending with `hicolor`.
    themes: Vec<String>,
    cache: HashMap<String, Option<PathBuf>>,
}

impl IconResolver {
    /// Detect the current icon theme and the system's icon directories.
    pub fn new() -> Self {
        let (mut roots, mut pixmaps) = (vec![], vec![]);
        if let Some(h) = dirs::home_dir() {
            roots.push(h.join(".icons"));
            roots.push(h.join(".local/share/flatpak/exports/share/icons"));
        }
        let data_dirs = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
        let mut data: Vec<PathBuf> = dirs::data_dir().into_iter().collect();
        data.extend(data_dirs.split(':').filter(|s| !s.is_empty()).map(PathBuf::from));
        data.push("/var/lib/flatpak/exports/share".into());
        for d in &data {
            roots.push(d.join("icons"));
            pixmaps.push(d.join("pixmaps"));
        }
        // The user's own directory wins over the flatpak/system ones added above.
        roots.sort_by_key(|r| !r.starts_with(dirs::home_dir().unwrap_or_default()));
        Self::with(roots, pixmaps, &current_theme())
    }

    pub fn with(roots: Vec<PathBuf>, pixmaps: Vec<PathBuf>, theme: &str) -> Self {
        let roots: Vec<PathBuf> = roots.into_iter().filter(|r| r.is_dir()).collect();
        let themes = theme_chain(&roots, theme);
        Self { roots, pixmaps, themes, cache: HashMap::new() }
    }

    /// Look up an application icon (`apps` context).
    #[cfg_attr(windows, allow(dead_code))]
    pub fn resolve(&mut self, name: &str) -> Option<PathBuf> {
        self.resolve_in(name, &["apps"])
    }

    /// Look up `name` in the given theme contexts (`apps`, `actions`, `places`, `mimetypes`, ...).
    pub fn resolve_in(&mut self, name: &str, contexts: &[&str]) -> Option<PathBuf> {
        let key = format!("{}|{name}", contexts.join(","));
        if let Some(hit) = self.cache.get(&key) {
            return hit.clone();
        }
        let found = self.lookup(name, contexts);
        self.cache.insert(key, found.clone());
        found
    }

    fn lookup(&self, name: &str, contexts: &[&str]) -> Option<PathBuf> {
        let path = Path::new(name);
        if path.is_absolute() {
            return (supported(path) && path.is_file()).then(|| path.to_path_buf());
        }
        // Some files name the icon with its extension: `foo.png`.
        let stem = path.file_stem().and_then(|s| s.to_str()).filter(|_| supported(path)).unwrap_or(name);
        for theme in &self.themes {
            for exts in [&EXTENSIONS[..], &VECTOR[..]] {
                for root in &self.roots {
                    let base = root.join(theme);
                    if !base.is_dir() {
                        continue;
                    }
                    for size in SIZES {
                        for ctx in contexts {
                            for dir in
                                [format!("{size}x{size}/{ctx}"), format!("{ctx}/{size}"), format!("{size}/{ctx}")]
                            {
                                if let Some(p) = find(&base.join(&dir), stem, exts) {
                                    return Some(p);
                                }
                            }
                        }
                    }
                    if exts == VECTOR {
                        for ctx in contexts {
                            for dir in [format!("scalable/{ctx}"), format!("{ctx}/scalable")] {
                                if let Some(p) = find(&base.join(&dir), stem, exts) {
                                    return Some(p);
                                }
                            }
                        }
                    }
                }
            }
        }
        for exts in [&EXTENSIONS[..], &VECTOR[..]] {
            for dir in &self.pixmaps {
                if let Some(p) = find(dir, stem, exts) {
                    return Some(p);
                }
            }
        }
        None
    }
}

/// `<dir>/<stem>.<ext>` for the first extension that exists.
fn find(dir: &Path, stem: &str, exts: &[&str]) -> Option<PathBuf> {
    exts.iter().map(|ext| dir.join(format!("{stem}.{ext}"))).find(|p| p.is_file())
}

fn supported(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase)
        .is_some_and(|e| EXTENSIONS.contains(&e.as_str()) || VECTOR.contains(&e.as_str()))
}

/// Theme contexts searched for the non-application icons bundled plugins use.
const GENERIC_CONTEXTS: [&str; 7] = ["actions", "places", "mimetypes", "apps", "devices", "status", "categories"];

/// Shared resolver for bundled plugins, built on first use.
static SHARED: std::sync::Mutex<Option<IconResolver>> = std::sync::Mutex::new(None);

/// A row icon: the first of `names` found in the desktop's icon theme, else `glyph`.
pub fn themed(names: &[&str], glyph: &str) -> crate::plugin::Icon {
    theme_icon(names).unwrap_or_else(|| crate::plugin::Icon { glyph: Some(glyph.into()), ..Default::default() })
}

/// The first of `names` found in the desktop's icon theme (none on Windows: it has no themes).
pub fn theme_icon(names: &[&str]) -> Option<crate::plugin::Icon> {
    if !cfg!(unix) {
        return None;
    }
    let mut shared = crate::core::sync::lock(&SHARED);
    let r = shared.get_or_insert_with(IconResolver::new);
    let path = names.iter().find_map(|n| r.resolve_in(n, &GENERIC_CONTEXTS))?;
    Some(crate::plugin::Icon { path: Some(path.to_string_lossy().into_owned()), ..Default::default() })
}

/// An application icon (`Icon=` of a `.desktop` file: a theme name or an absolute path).
#[cfg(unix)]
pub fn app_icon(name: &str) -> Option<crate::plugin::Icon> {
    let mut shared = crate::core::sync::lock(&SHARED);
    let path = shared.get_or_insert_with(IconResolver::new).resolve(name)?;
    Some(crate::plugin::Icon { path: Some(path.to_string_lossy().into_owned()), ..Default::default() })
}

/// `Inherits=A,B` from an `index.theme`.
pub fn parse_inherits(index_theme: &str) -> Vec<String> {
    index_theme
        .lines()
        .find_map(|l| l.trim().strip_prefix("Inherits="))
        .map(|v| v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
        .unwrap_or_default()
}

/// The theme, everything it inherits (depth first, once each), then `hicolor`.
fn theme_chain(roots: &[PathBuf], start: &str) -> Vec<String> {
    let mut chain: Vec<String> = vec![];
    let mut seen = HashSet::new();
    let mut stack = vec![start.to_string()];
    while let Some(theme) = stack.pop() {
        if !seen.insert(theme.clone()) {
            continue;
        }
        let parents = roots
            .iter()
            .find_map(|r| std::fs::read_to_string(r.join(&theme).join("index.theme")).ok())
            .map(|t| parse_inherits(&t))
            .unwrap_or_default();
        chain.push(theme);
        stack.extend(parents.into_iter().rev());
    }
    if !chain.iter().any(|t| t == "hicolor") {
        chain.push("hicolor".into());
    }
    chain
}

/// `'Yaru-olive'` (gsettings output) -> `Yaru-olive`.
pub fn parse_gsettings_string(s: &str) -> Option<String> {
    let v = s.trim().trim_matches('\'').trim_matches('"').trim();
    (!v.is_empty()).then(|| v.to_string())
}

fn current_theme() -> String {
    let from_gsettings = std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "icon-theme"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| parse_gsettings_string(&String::from_utf8_lossy(&o.stdout)));
    let from_gtk = || {
        let ini = std::fs::read_to_string(dirs::config_dir()?.join("gtk-3.0/settings.ini")).ok()?;
        ini.lines().find_map(|l| l.strip_prefix("gtk-icon-theme-name=")).and_then(parse_gsettings_string)
    };
    from_gsettings.or_else(from_gtk).unwrap_or_else(|| "hicolor".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn touch(root: &Path, rel: &str) -> PathBuf {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, b"png").unwrap();
        p
    }

    fn tmp() -> (tempfile::TempDir, PathBuf) {
        let t = crate::test_support::tmp();
        let d = t.path().to_path_buf();
        (t, d)
    }

    #[test]
    fn inherits_and_gsettings_parsing() {
        assert_eq!(
            parse_inherits("[Icon Theme]\nName=X\nInherits=Yaru, Adwaita ,hicolor\n"),
            ["Yaru", "Adwaita", "hicolor"]
        );
        assert!(parse_inherits("[Icon Theme]\nName=X\n").is_empty());
        assert_eq!(parse_gsettings_string("'Yaru-olive'\n").as_deref(), Some("Yaru-olive"));
        assert_eq!(parse_gsettings_string("''"), None);
    }

    #[test]
    fn follows_inheritance_prefers_size_then_falls_back() {
        let (_tmp, root) = tmp();
        fs::create_dir_all(root.join("Child")).unwrap();
        fs::write(root.join("Child/index.theme"), "[Icon Theme]\nInherits=Parent\n").unwrap();
        touch(&root, "Parent/16x16/apps/small.png");
        let big = touch(&root, "Parent/48x48/apps/small.png");
        let own = touch(&root, "Child/64x64/apps/mine.png");
        let hi = touch(&root, "hicolor/32x32/apps/last.png");
        let layout2 = touch(&root, "Parent/apps/48/other.png");

        let mut r = IconResolver::with(vec![root.clone()], vec![], "Child");
        assert_eq!(r.themes, ["Child", "Parent", "hicolor"]);
        assert_eq!(r.resolve("small"), Some(big), "48px beats 16px");
        assert_eq!(r.resolve("mine"), Some(own));
        assert_eq!(r.resolve("last"), Some(hi), "hicolor is the last resort");
        assert_eq!(r.resolve("other"), Some(layout2), "apps/<size> layout");
        assert_eq!(r.resolve("nope"), None);
        let action = touch(&root, "Parent/24x24/actions/system-shutdown.png");
        assert_eq!(r.resolve("system-shutdown"), None, "apps context only");
        assert_eq!(r.resolve_in("system-shutdown", &["places", "actions"]), Some(action));
    }

    #[test]
    fn child_theme_wins_over_parent_and_pixmaps_are_last() {
        let (_tmp, root) = tmp();
        fs::create_dir_all(root.join("Child")).unwrap();
        fs::write(root.join("Child/index.theme"), "Inherits=Parent\n").unwrap();
        touch(&root, "Parent/48x48/apps/app.png");
        let child = touch(&root, "Child/32x32/apps/app.png");
        let pix_dir = root.join("pixmaps");
        let pix = touch(&root, "pixmaps/legacy.png");
        let mut r = IconResolver::with(vec![root.clone()], vec![pix_dir], "Child");
        assert_eq!(r.resolve("app"), Some(child), "theme order beats size across themes");
        assert_eq!(r.resolve("legacy"), Some(pix));
    }

    #[test]
    fn png_beats_svg_within_a_theme_but_theme_order_comes_first() {
        let (_tmp, root) = tmp();
        fs::create_dir_all(root.join("Papirus")).unwrap();
        fs::write(root.join("Papirus/index.theme"), "Inherits=hicolor\n").unwrap();
        let papirus = touch(&root, "Papirus/48x48/apps/firefox.svg");
        touch(&root, "hicolor/48x48/apps/firefox.png");
        let png = touch(&root, "hicolor/32x32/apps/both.png");
        touch(&root, "hicolor/scalable/apps/both.svg");
        let scalable = touch(&root, "hicolor/scalable/apps/only.svg");
        let mut r = IconResolver::with(vec![root.clone()], vec![], "Papirus");
        assert_eq!(r.resolve("firefox"), Some(papirus), "the chosen theme, even as SVG");
        assert_eq!(r.resolve("both"), Some(png), "a PNG of any size before an SVG");
        assert_eq!(r.resolve("only"), Some(scalable));
    }

    #[test]
    fn absolute_paths_and_extensions() {
        let (_tmp, root) = tmp();
        let p = touch(&root, "x/icon.png");
        let svg = touch(&root, "x/vector.svg");
        let mut r = IconResolver::with(vec![], vec![], "hicolor");
        assert_eq!(r.resolve(p.to_str().unwrap()), Some(p.clone()));
        assert_eq!(r.resolve(svg.to_str().unwrap()), Some(svg.clone()));
        let txt = touch(&root, "x/notes.txt");
        assert_eq!(r.resolve(txt.to_str().unwrap()), None, "not an image");
        assert_eq!(r.resolve("/does/not/exist.png"), None);
    }
}

#[cfg(test)]
mod dev {
    /// Dev check of the icons bundled plugins ask for: `cargo test real_themed -- --ignored --nocapture`.
    #[test]
    #[ignore = "dev check against the icon theme installed on this machine"]
    fn real_themed_icons() {
        for n in [
            "system-lock-screen",
            "system-shutdown",
            "system-reboot",
            "system-log-out",
            "user-trash",
            "web-browser",
            "folder",
            "text-x-generic",
            "utilities-terminal",
            "network-server",
            "system-software-install",
        ] {
            let i = super::themed(&[n], "?");
            eprintln!("{n:28} {}", i.path.unwrap_or_else(|| format!("glyph {}", i.glyph.unwrap_or_default())));
        }
    }
}
