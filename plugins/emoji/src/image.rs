//! Colour emoji images from the system's emoji font.
//!
//! Noto Color Emoji (Linux) and Apple Color Emoji (macOS) store each emoji as a PNG bitmap. The
//! sequence is shaped to find its glyph (flags, keycaps, skin tones and ZWJ sequences are
//! ligatures), the bitmap is read and scaled down to icon size. Windows' Segoe UI Emoji has no
//! bitmaps; there the rows show the emoji as text instead.

use std::io::Cursor;
use std::path::{Path, PathBuf};

use harfrust::font::glyph::bitmap::Pixels;
use harfrust::{Buffer, Font, GlyphId, ShapeOptions, ShaperFont};

/// Rows draw icons at 32 logical pixels; 64 stays sharp at 2x.
pub const ICON_PX: u32 = 64;

pub struct EmojiFont {
    font: Font,
}

impl EmojiFont {
    /// The first emoji font with PNG bitmaps found on this system.
    pub fn find() -> Option<Self> {
        candidates().into_iter().find_map(|(path, index)| Self::open(&path, index))
    }

    pub fn open(path: &Path, index: u32) -> Option<Self> {
        let font = Font::new(std::fs::read(path).ok()?, index)?;
        if font.bitmap_strikes().is_empty() {
            return None;
        }
        Some(Self { font })
    }

    /// The emoji as a square PNG of `px` pixels, if the font has it.
    pub fn png(&self, emoji: &str, px: u32) -> Option<Vec<u8>> {
        let shaper = ShaperFont::new(&self.font);
        let mut buf = Buffer::new();
        buf.push_str(emoji);
        buf.guess_segment_properties();
        harfrust::shape(&shaper, &mut buf, ShapeOptions::new()).ok()?;
        let strikes = self.font.bitmap_strikes();
        // A sequence the font knows is one glyph; otherwise the first part that has an image.
        #[allow(clippy::cast_precision_loss, reason = "icon sizes are small")]
        let bitmap = buf.glyph_infos().iter().filter(|g| g.glyph_id != 0).find_map(|g| {
            match strikes.select_bitmap(GlyphId::new(g.glyph_id), Some(px as f32))?.pixels() {
                Pixels::Png(bytes) => Some(bytes),
                _ => None,
            }
        })?;
        scale_png(bitmap, px)
    }
}

/// Where emoji fonts usually are (path, index in a collection).
fn candidates() -> Vec<(PathBuf, u32)> {
    let mut v: Vec<(PathBuf, u32)> = vec![];
    if cfg!(target_os = "macos") {
        v.push(("/System/Library/Fonts/Apple Color Emoji.ttc".into(), 0));
        return v;
    }
    if cfg!(windows) {
        return v;
    }
    // fontconfig knows best where the colour emoji font is.
    if let Ok(out) = std::process::Command::new("fc-match").args(["-f", "%{file}", "emoji:color=true"]).output()
        && out.status.success()
    {
        let p = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
        if !p.as_os_str().is_empty() {
            v.push((p, 0));
        }
    }
    for p in [
        "/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf",
        "/usr/share/fonts/noto/NotoColorEmoji.ttf",
        "/usr/share/fonts/google-noto-emoji/NotoColorEmoji.ttf",
        "/usr/share/fonts/noto-color-emoji/NotoColorEmoji.ttf",
        "/usr/share/fonts/TTF/NotoColorEmoji.ttf",
    ] {
        v.push((p.into(), 0));
    }
    v
}

/// Decode a PNG, fit it into a transparent `px` square (keeping its shape) and encode it again.
fn scale_png(bytes: &[u8], px: u32) -> Option<Vec<u8>> {
    let mut dec = png::Decoder::new(Cursor::new(bytes));
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = dec.read_info().ok()?;
    let mut raw = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut raw).ok()?;
    let (w, h) = (info.width as usize, info.height as usize);
    let channels = match info.color_type {
        png::ColorType::Rgba => 4,
        png::ColorType::Rgb => 3,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Grayscale => 1,
        png::ColorType::Indexed => return None,
    };
    let rgba = |x: usize, y: usize| -> [f32; 4] {
        let i = y * info.line_size + x * channels;
        let p = &raw[i..i + channels];
        let [r, g, b, a] = match channels {
            4 => [p[0], p[1], p[2], p[3]],
            3 => [p[0], p[1], p[2], 255],
            2 => [p[0], p[0], p[0], p[1]],
            _ => [p[0], p[0], p[0], 255],
        };
        [f32::from(r), f32::from(g), f32::from(b), f32::from(a)]
    };
    // Box filter over the source pixels each target pixel covers, weighted by alpha.
    let px_us = px as usize;
    let scale = (w.max(h) as f32) / px as f32;
    let (tw, th) = (((w as f32 / scale).round() as usize).max(1), ((h as f32 / scale).round() as usize).max(1));
    let (ox, oy) = ((px_us - tw.min(px_us)) / 2, (px_us - th.min(px_us)) / 2);
    let mut out = vec![0u8; px_us * px_us * 4];
    for ty in 0..th.min(px_us) {
        for tx in 0..tw.min(px_us) {
            let (x0, x1) = ((tx as f32 * scale) as usize, (((tx + 1) as f32 * scale) as usize).clamp(1, w));
            let (y0, y1) = ((ty as f32 * scale) as usize, (((ty + 1) as f32 * scale) as usize).clamp(1, h));
            let mut acc = [0f32; 4];
            let mut n = 0f32;
            for y in y0..y1.max(y0 + 1).min(h) {
                for x in x0..x1.max(x0 + 1).min(w) {
                    let [r, g, b, a] = rgba(x, y);
                    acc[0] += r * a;
                    acc[1] += g * a;
                    acc[2] += b * a;
                    acc[3] += a;
                    n += 1.0;
                }
            }
            let o = ((oy + ty) * px_us + ox + tx) * 4;
            if acc[3] > 0.0 {
                for c in 0..3 {
                    out[o + c] = (acc[c] / acc[3]).round().clamp(0.0, 255.0) as u8;
                }
                out[o + 3] = (acc[3] / n).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    let mut png_out = vec![];
    {
        let mut enc = png::Encoder::new(&mut png_out, px, px);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().ok()?;
        w.write_image_data(&out).ok()?;
    }
    Some(png_out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2x2 RGBA PNG: red, green, blue, transparent.
    fn tiny_png() -> Vec<u8> {
        let mut v = vec![];
        let mut enc = png::Encoder::new(&mut v, 2, 2);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().unwrap();
        w.write_image_data(&[255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 0, 0, 0, 0]).unwrap();
        drop(w);
        v
    }

    #[test]
    fn scaling_keeps_size_and_transparency() {
        let out = scale_png(&tiny_png(), 1).unwrap();
        let mut r = png::Decoder::new(Cursor::new(out)).read_info().unwrap();
        let mut buf = vec![0; r.output_buffer_size().unwrap()];
        let info = r.next_frame(&mut buf).unwrap();
        assert_eq!((info.width, info.height), (1, 1));
        // Three opaque colours and one transparent pixel average to 3/4 alpha, colour from the opaque ones.
        assert_eq!(buf[3], 191);
        assert_eq!(&buf[..3], &[85, 85, 85]);
        assert!(scale_png(b"not a png", 64).is_none());
    }

    /// Against this machine's emoji font, when there is one.
    #[test]
    fn real_font_gives_images_for_sequences() {
        let Some(font) = EmojiFont::find() else { return };
        for e in ["😀", "👍🏾", "🇨🇭", "👩‍💻", "❤️", "1️⃣"] {
            let png = font.png(e, ICON_PX).unwrap_or_else(|| panic!("no image for {e}"));
            assert!(png.starts_with(b"\x89PNG"), "{e}");
            assert!(png.len() < 32 * 1024, "{e}: {} bytes", png.len());
        }
        assert!(font.png("a", ICON_PX).is_none(), "not an emoji");
    }
}
