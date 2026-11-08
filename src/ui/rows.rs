//! Turning engine results into list rows, with decoded icons cached by path.

use std::collections::{HashMap, VecDeque};
use std::path::Path;

use super::Item;
use crate::plugin::ResultItem;

/// Decoded icons by file path, so scrolling and re-ranking never decode an image twice.
/// Evicts the least recently inserted entry once full.
#[derive(Default)]
pub(super) struct IconCache {
    images: HashMap<String, slint::Image>,
    order: VecDeque<String>,
}

impl IconCache {
    const CAPACITY: usize = 128;

    fn get_or_load(&mut self, path: &str) -> Option<slint::Image> {
        if let Some(img) = self.images.get(path) {
            return Some(img.clone());
        }
        let img = if is_svg(path) {
            rasterize_svg(Path::new(path))?
        } else {
            slint::Image::load_from_path(Path::new(path)).ok()?
        };
        if self.order.len() >= Self::CAPACITY
            && let Some(oldest) = self.order.pop_front()
        {
            self.images.remove(&oldest);
        }
        self.order.push_back(path.to_string());
        self.images.insert(path.to_string(), img.clone());
        Some(img)
    }
}

/// Pixel size SVG icons are drawn at: rows show 32 logical pixels, so this is sharp up to 2x.
const SVG_PX: u32 = 64;
/// Larger SVG files are not drawn (icons are a few KB; this is not an icon).
const SVG_MAX_BYTES: u64 = 1024 * 1024;

fn is_svg(path: &str) -> bool {
    Path::new(path).extension().is_some_and(|e| e.eq_ignore_ascii_case("svg") || e.eq_ignore_ascii_case("svgz"))
}

/// Draw an SVG file once into a `SVG_PX` square bitmap, keeping its aspect ratio. Only the
/// bitmap is kept; the parsed drawing is dropped right away.
fn rasterize_svg(path: &Path) -> Option<slint::Image> {
    use resvg::{tiny_skia, usvg};
    if std::fs::metadata(path).ok()?.len() > SVG_MAX_BYTES {
        return None;
    }
    let data = std::fs::read(path).ok()?;
    let tree = usvg::Tree::from_data(&data, &usvg::Options::default()).ok()?;
    let size = tree.size();
    let scale = SVG_PX as f32 / size.width().max(size.height());
    let (dx, dy) = ((SVG_PX as f32 - size.width() * scale) / 2.0, (SVG_PX as f32 - size.height() * scale) / 2.0);
    let mut pixmap = tiny_skia::Pixmap::new(SVG_PX, SVG_PX)?;
    resvg::render(&tree, tiny_skia::Transform::from_row(scale, 0.0, 0.0, scale, dx, dy), &mut pixmap.as_mut());
    let mut buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(SVG_PX, SVG_PX);
    for (dst, src) in buf.make_mut_slice().iter_mut().zip(pixmap.pixels()) {
        let c = src.demultiply();
        *dst = slint::Rgba8Pixel { r: c.red(), g: c.green(), b: c.blue(), a: c.alpha() };
    }
    Some(slint::Image::from_rgba8(buf))
}

/// The row for a result: its image if one loads, else a glyph (the plugin's, or the title's
/// first letter).
pub(super) fn to_item(r: &ResultItem, icons: &mut IconCache) -> Item {
    let icon = r.icon.as_ref().and_then(|i| i.path.as_deref()).and_then(|p| icons.get_or_load(p));
    let glyph = r
        .icon
        .as_ref()
        .and_then(|i| i.glyph.clone())
        .unwrap_or_else(|| r.title.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default());
    Item {
        title: r.title.clone().into(),
        subtitle: r.subtitle.clone().into(),
        glyph: glyph.into(),
        has_icon: icon.is_some(),
        icon: icon.unwrap_or_default(),
        fresh: false,
    }
}

#[cfg(test)]
mod svg_tests {
    use super::*;

    #[test]
    fn svg_icons_are_drawn_into_a_square_bitmap() {
        let tmp = crate::test_support::tmp();
        let path = tmp.path().join("wide.svg");
        // A red bar twice as wide as tall: centred, transparent above and below.
        std::fs::write(&path, r#"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10"><rect width="20" height="10" fill="red"/></svg>"#).unwrap();
        let img = rasterize_svg(&path).unwrap();
        assert_eq!((img.size().width, img.size().height), (SVG_PX, SVG_PX));
        let px = img.to_rgba8().unwrap();
        let at = |x: u32, y: u32| px.as_slice()[(y * SVG_PX + x) as usize];
        assert_eq!(at(32, 32), slint::Rgba8Pixel { r: 255, g: 0, b: 0, a: 255 });
        assert_eq!(at(32, 2).a, 0, "letterboxed");
        assert!(is_svg("/x/Icon.SVG") && is_svg("a.svgz") && !is_svg("a.png"));
        std::fs::write(&path, "not svg").unwrap();
        assert!(rasterize_svg(&path).is_none());
        let mut cache = IconCache::default();
        assert!(cache.get_or_load(path.to_str().unwrap()).is_none());
    }
}
