//! The Kairos icon: an emerald rounded square with a white lowercase kappa (κ), drawn in code so
//! every size (tray, window, exe, installer) comes from one place.
//! `examples/gen_icons.rs` includes this file to write the PNG/ICO assets.

/// Background of the square (emerald `#0f9d76`) and colour of the κ (white).
pub const BACKGROUND: [u8; 3] = [0x0f, 0x9d, 0x76];
pub const INK: [u8; 3] = [0xff, 0xff, 0xff];

type Point = (f32, f32);

/// The κ strokes as segments in unit coordinates (0..1 across the icon): stem, upper arm, and a
/// lower leg that curls into a flick to the right (what sets κ apart from a small-caps K).
const STROKES: [(Point, Point); 5] = [
    ((0.33, 0.34), (0.33, 0.69)),
    ((0.37, 0.54), (0.60, 0.34)),
    ((0.44, 0.48), (0.57, 0.63)),
    ((0.57, 0.63), (0.63, 0.68)),
    ((0.63, 0.68), (0.69, 0.68)),
];
/// Half the stroke width, as a fraction of the icon size.
const HALF_WIDTH: f32 = 0.06;
/// Strokes never get thinner than this many pixels across, so small sizes stay legible.
const MIN_WIDTH_PX: f32 = 1.6;

/// Draw the icon as RGBA, row-major, `size * size` pixels.
pub fn icon_rgba(size: u32) -> Vec<u8> {
    let s = size as f32;
    let (radius, half) = (s * 0.22, s / 2.0);
    let half_width = (s * HALF_WIDTH).max(MIN_WIDTH_PX / 2.0);
    let strokes = STROKES.map(|((ax, ay), (bx, by))| ((ax * s, ay * s), (bx * s, by * s)));

    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let p = (x as f32 + 0.5, y as f32 + 0.5);
            // Signed distance to a rounded square; coverage fades over one pixel.
            let (dx, dy) = ((p.0 - half).abs() - (half - radius), (p.1 - half).abs() - (half - radius));
            let sd = (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt() + dx.max(dy).min(0.0) - radius;
            let coverage = (0.5 - sd).clamp(0.0, 1.0);

            let d = strokes.iter().map(|&(a, b)| dist_to_segment(p, a, b)).fold(f32::INFINITY, f32::min);
            let ink = (half_width + 0.5 - d).clamp(0.0, 1.0);

            let mix = |i: usize| (f32::from(BACKGROUND[i]) * (1.0 - ink) + f32::from(INK[i]) * ink).round() as u8;
            out.extend_from_slice(&[mix(0), mix(1), mix(2), (coverage * 255.0).round() as u8]);
        }
    }
    out
}

fn dist_to_segment(p: Point, a: Point, b: Point) -> f32 {
    let (abx, aby) = (b.0 - a.0, b.1 - a.1);
    let t = (((p.0 - a.0) * abx + (p.1 - a.1) * aby) / (abx * abx + aby * aby)).clamp(0.0, 1.0);
    ((p.0 - (a.0 + t * abx)).powi(2) + (p.1 - (a.1 + t * aby)).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_is_square_rgba_with_transparent_corners_and_opaque_centre() {
        let px = icon_rgba(32);
        assert_eq!(px.len(), 32 * 32 * 4);
        let alpha = |x: usize, y: usize| px[(y * 32 + x) * 4 + 3];
        assert_eq!(alpha(0, 0), 0, "rounded corner is transparent");
        assert_eq!(alpha(16, 16), 255, "centre is opaque");
        assert!(alpha(2, 16) > 200, "edge midpoint is opaque");
        // The kappa is ink-coloured on the background colour.
        let rgb = |x: usize, y: usize| [px[(y * 32 + x) * 4], px[(y * 32 + x) * 4 + 1], px[(y * 32 + x) * 4 + 2]];
        assert_eq!(rgb(4, 16), BACKGROUND);
        let stem = rgb(10, 16); // x = 0.33 * 32
        assert!(stem.iter().zip(INK).all(|(a, b)| (i32::from(*a) - i32::from(b)).abs() < 60), "{stem:?}");
    }

    #[test]
    fn strokes_stay_visible_at_16px() {
        let px = icon_rgba(16);
        // Red is where background and ink differ most (0x0f vs 0xff).
        let inked = px.chunks(4).map(|p| p[0]).max().unwrap();
        assert!(inked > 0xc0, "some pixel is fully inked at 16px, got {inked}");
    }
}
