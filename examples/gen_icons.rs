//! Writes the Kairos icon files from the same code that draws the tray icon:
//!   assets/icons/kairos-<size>.png  and  packaging/kairos.ico
//! Run after changing `src/app_icon.rs`: `cargo run --example gen_icons`, then commit the files.

#[path = "../src/app_icon.rs"]
#[allow(dead_code)]
mod app_icon;

use std::path::Path;

const PNG_SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];
const ICO_SIZES: [u32; 6] = [16, 24, 32, 48, 64, 256];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    std::fs::create_dir_all(root.join("assets/icons"))?;

    for size in PNG_SIZES {
        let path = root.join(format!("assets/icons/kairos-{size}.png"));
        std::fs::write(&path, png(size)?)?;
        println!("wrote {}", path.display());
    }

    let path = root.join("packaging/kairos.ico");
    std::fs::write(&path, ico(&ICO_SIZES)?)?;
    println!("wrote {}", path.display());
    Ok(())
}

fn png(size: u32) -> Result<Vec<u8>, png::EncodingError> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, size, size);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::High);
    enc.write_header()?.write_image_data(&app_icon::icon_rgba(size))?;
    Ok(out)
}

/// An ICO whose entries are PNGs (supported since Windows Vista).
fn ico(sizes: &[u32]) -> Result<Vec<u8>, png::EncodingError> {
    let images = sizes.iter().map(|&s| png(s)).collect::<Result<Vec<_>, _>>()?;
    let count = sizes.len() as u16;
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes()); // reserved
    out.extend_from_slice(&1u16.to_le_bytes()); // type: icon
    out.extend_from_slice(&count.to_le_bytes());

    let mut offset = 6 + 16 * u32::from(count);
    for (&size, data) in sizes.iter().zip(&images) {
        let dim = if size >= 256 { 0 } else { size as u8 }; // 0 means 256
        out.extend_from_slice(&[dim, dim, 0, 0]); // width, height, palette, reserved
        out.extend_from_slice(&1u16.to_le_bytes()); // colour planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += data.len() as u32;
    }
    for data in images {
        out.extend_from_slice(&data);
    }
    Ok(out)
}
