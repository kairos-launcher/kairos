//! Turning engine results into list rows, with decoded icons cached by path.

use std::collections::{HashMap, VecDeque};
use std::path::Path;

use super::Item;
use crate::plugin::ResultItem;

/// Decoded icons by file path, so scrolling and re-ranking never decode an image twice.
/// Evicts the least recently inserted entry once full. Every icon is stored at `px`, the physical
/// size rows draw it at.
pub(super) struct IconCache {
    images: HashMap<String, slint::Image>,
    order: VecDeque<String>,
    px: u32,
}

impl Default for IconCache {
    fn default() -> Self {
        Self::new(ICON_LOGICAL_PX as u32)
    }
}

impl IconCache {
    const CAPACITY: usize = 128;

    fn new(px: u32) -> Self {
        Self { images: HashMap::new(), order: VecDeque::new(), px }
    }

    /// Size icons for a window at `scale`; cached icons of another size are dropped.
    pub(super) fn for_scale(&mut self, scale: f32) {
        let px = (ICON_LOGICAL_PX * scale).round().max(1.0) as u32;
        if px != self.px {
            *self = Self::new(px);
        }
    }

    fn get_or_load(&mut self, path: &str) -> Option<slint::Image> {
        if let Some(img) = self.images.get(path) {
            return Some(img.clone());
        }
        let (file, px) = (Path::new(path), self.px);
        let img = if is_svg(path) { rasterize_svg(file, px)? } else { load_bitmap(file, px)? };
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

/// Logical size of a row icon; must match `width: 32px` of the row image in `ui/main.slint`.
/// Icons are stored at exactly this many physical pixels (times the scale factor): the software
/// renderer scales images by nearest pixel, so anything else would come out jagged.
const ICON_LOGICAL_PX: f32 = 32.0;
/// Larger SVG files are not drawn (icons are a few KB; this is not an icon).
const SVG_MAX_BYTES: u64 = 1024 * 1024;
/// Larger PNG/JPEG files are not loaded (a 1024 px app icon is well under 1 MB).
const BITMAP_MAX_BYTES: u64 = 4 * 1024 * 1024;

/// Load a PNG or JPEG icon, shrunk to fit `px` (a smaller one is kept as is). App icons come as large as 1024 px (4 MB of
/// pixels each); only the small copy is kept. Decoded from memory, not by path: Slint caches
/// what it loads by path (up to 5 MB) and would keep the pixels after `IconCache` drops them.
fn load_bitmap(path: &Path, px: u32) -> Option<slint::Image> {
    if std::fs::metadata(path).ok()?.len() > BITMAP_MAX_BYTES {
        return None;
    }
    let img = slint::Image::load_from_data(&std::fs::read(path).ok()?, None).ok()?;
    let size = img.size();
    if size.width.max(size.height) <= px {
        return Some(img);
    }
    Some(slint::Image::from_rgba8(shrink(&img.to_rgba8()?, px)))
}

/// `src` scaled down so its longer side is `max` pixels: every target pixel is the average of
/// the source pixels it covers, weighted by alpha so transparent pixels do not darken edges.
fn shrink(
    src: &slint::SharedPixelBuffer<slint::Rgba8Pixel>,
    max: u32,
) -> slint::SharedPixelBuffer<slint::Rgba8Pixel> {
    let (sw, sh) = (src.width() as usize, src.height() as usize);
    let longer = sw.max(sh);
    let (w, h) = ((sw * max as usize / longer).max(1), (sh * max as usize / longer).max(1));
    let pixels = src.as_slice();
    let mut out = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(w as u32, h as u32);
    for (i, dst) in out.make_mut_slice().iter_mut().enumerate() {
        let (x, y) = (i % w, i / w);
        let (x0, x1) = (x * sw / w, ((x + 1) * sw / w).max(x * sw / w + 1));
        let (y0, y1) = (y * sh / h, ((y + 1) * sh / h).max(y * sh / h + 1));
        let (mut r, mut g, mut b, mut a) = (0u64, 0u64, 0u64, 0u64);
        for row in y0..y1 {
            for p in &pixels[row * sw + x0..row * sw + x1] {
                let pa = u64::from(p.a);
                r += u64::from(p.r) * pa;
                g += u64::from(p.g) * pa;
                b += u64::from(p.b) * pa;
                a += pa;
            }
        }
        let n = ((x1 - x0) * (y1 - y0)) as u64;
        let c = |sum: u64| sum.checked_div(a).unwrap_or(0) as u8;
        *dst = slint::Rgba8Pixel { r: c(r), g: c(g), b: c(b), a: (a / n) as u8 };
    }
    out
}

fn is_svg(path: &str) -> bool {
    Path::new(path).extension().is_some_and(|e| e.eq_ignore_ascii_case("svg") || e.eq_ignore_ascii_case("svgz"))
}

/// Draw an SVG file once into a `px` square bitmap, keeping its aspect ratio. Only the
/// bitmap is kept; the parsed drawing is dropped right away.
fn rasterize_svg(path: &Path, px: u32) -> Option<slint::Image> {
    use resvg::{tiny_skia, usvg};
    if std::fs::metadata(path).ok()?.len() > SVG_MAX_BYTES {
        return None;
    }
    let data = std::fs::read(path).ok()?;
    let tree = usvg::Tree::from_data(&data, &usvg::Options::default()).ok()?;
    let size = tree.size();
    let scale = px as f32 / size.width().max(size.height());
    let (dx, dy) = ((px as f32 - size.width() * scale) / 2.0, (px as f32 - size.height() * scale) / 2.0);
    let mut pixmap = tiny_skia::Pixmap::new(px, px)?;
    resvg::render(&tree, tiny_skia::Transform::from_row(scale, 0.0, 0.0, scale, dx, dy), &mut pixmap.as_mut());
    let mut buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(px, px);
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

    /// Write a `w`x`h` PNG: opaque red on the left half, transparent on the right.
    fn write_png(path: &Path, w: u32, h: u32) {
        let mut data = Vec::with_capacity((w * h * 4) as usize);
        for i in 0..w * h {
            data.extend(if i % w < w / 2 { [255, 0, 0, 255] } else { [0, 0, 0, 0] });
        }
        let mut enc = png::Encoder::new(std::fs::File::create(path).unwrap(), w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&data).unwrap();
    }

    #[test]
    fn large_bitmap_icons_are_shrunk_and_small_ones_kept() {
        let tmp = crate::test_support::tmp();
        let big = tmp.path().join("big.png");
        write_png(&big, 512, 256);
        let img = load_bitmap(&big, 32).unwrap();
        assert_eq!((img.size().width, img.size().height), (32, 16), "aspect ratio kept");
        let px = img.to_rgba8().unwrap();
        let at = |x: u32, y: u32| px.as_slice()[(y * 32 + x) as usize];
        assert_eq!(at(2, 8), slint::Rgba8Pixel { r: 255, g: 0, b: 0, a: 255 });
        assert_eq!(at(30, 8).a, 0);

        let mid = tmp.path().join("mid.png");
        write_png(&mid, 48, 48);
        let img = load_bitmap(&mid, 32).unwrap();
        assert_eq!((img.size().width, img.size().height), (32, 32), "drawn 1:1, never scaled by Slint");

        let small = tmp.path().join("small.png");
        write_png(&small, 16, 16);
        let img = load_bitmap(&small, 32).unwrap();
        assert_eq!((img.size().width, img.size().height), (16, 16));

        let junk = tmp.path().join("junk.png");
        std::fs::write(&junk, "not an image").unwrap();
        assert!(load_bitmap(&junk, 32).is_none());
    }

    #[test]
    fn svg_icons_are_drawn_into_a_square_bitmap() {
        let tmp = crate::test_support::tmp();
        let path = tmp.path().join("wide.svg");
        // A red bar twice as wide as tall: centred, transparent above and below.
        std::fs::write(&path, r#"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10"><rect width="20" height="10" fill="red"/></svg>"#).unwrap();
        let img = rasterize_svg(&path, 32).unwrap();
        assert_eq!((img.size().width, img.size().height), (32, 32));
        let px = img.to_rgba8().unwrap();
        let at = |x: u32, y: u32| px.as_slice()[(y * 32 + x) as usize];
        assert_eq!(at(16, 16), slint::Rgba8Pixel { r: 255, g: 0, b: 0, a: 255 });
        assert_eq!(at(16, 1).a, 0, "letterboxed");
        assert!(is_svg("/x/Icon.SVG") && is_svg("a.svgz") && !is_svg("a.png"));
        std::fs::write(&path, "not svg").unwrap();
        assert!(rasterize_svg(&path, 32).is_none());
        let mut cache = IconCache::default();
        assert!(cache.get_or_load(path.to_str().unwrap()).is_none());
    }

    #[test]
    fn icons_follow_the_window_scale() {
        let tmp = crate::test_support::tmp();
        let path = tmp.path().join("big.png");
        write_png(&path, 256, 256);
        let path = path.to_str().unwrap();
        let mut cache = IconCache::default();
        assert_eq!(cache.get_or_load(path).unwrap().size().width, 32);
        cache.for_scale(1.0);
        assert_eq!(cache.images.len(), 1, "same size keeps the cache");
        cache.for_scale(2.0);
        assert!(cache.images.is_empty(), "icons of the old size are dropped");
        assert_eq!(cache.get_or_load(path).unwrap().size().width, 64);
    }
}
