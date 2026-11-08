//! Windows: save the icon Explorer shows for a file (e.g. a Start Menu shortcut) or a shell item
//! (a packaged app) as a PNG.
//! Every `unsafe` block is a single FFI call with plain values or pointers to locals; handles
//! are released by small guards on every return path.

use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC, GetDIBits, GetObjectW, HBITMAP,
    ReleaseDC,
};
use windows_sys::Win32::System::Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx};
use windows_sys::Win32::UI::Shell::Common::ITEMIDLIST;
use windows_sys::Win32::UI::Shell::{
    ILFree, SHFILEINFOW, SHGFI_FLAGS, SHGFI_ICON, SHGFI_LARGEICON, SHGFI_PIDL, SHGetFileInfoW, SHParseDisplayName,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO};

/// Larger than any shell icon; guards the buffer size against a bogus bitmap.
const MAX_SIDE: u32 = 256;

/// A cache folder for extracted icons, next to the plugin icon cache (not inside it: its
/// pruning would delete these).
pub fn cache_dir(name: &str) -> PathBuf {
    crate::icon_store::IconStore::default_dir().with_file_name(name)
}

/// The icon of `file` as a PNG in `dir`, extracted once per version of the file: the PNG is
/// named after the file's path and modification time, so later calls only check it exists.
pub fn cached_file_icon(file: &Path, dir: &Path) -> Option<PathBuf> {
    let stamp = crate::config::mtime(file)?.duration_since(UNIX_EPOCH).ok()?;
    let key = format!("{}|{}", file.display(), stamp.as_nanos());
    let out = dir.join(format!("{:016x}.png", crate::icon_store::fnv1a(key.as_bytes())));
    if !out.is_file() {
        std::fs::create_dir_all(dir).ok()?;
        save_file_icon(file, &out)?;
    }
    Some(out)
}

/// The icon of an executable, for the lazy icon request of a row whose icon id is the exe's
/// full path (process killer, window switcher). Extracted once into the `exe-icons` folder.
pub fn exe_icon(path: &str) -> Option<crate::plugin::Icon> {
    let exe = Path::new(path);
    if !exe.is_absolute() || !exe.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe")) {
        return None;
    }
    let png = cached_file_icon(exe, &cache_dir(EXE_ICONS))?;
    Some(crate::plugin::Icon { path: Some(png.to_string_lossy().into_owned()), ..Default::default() })
}

/// Cache folder of `exe_icon`, pruned by the process killer while hidden.
pub const EXE_ICONS: &str = "exe-icons";

/// The icon of a shell item named by its parsing name (`shell:AppsFolder\\<app id>`, a packaged
/// app) as a PNG in `dir`, extracted once: such items have no file, so the PNG is named after
/// the name alone.
pub fn cached_shell_icon(name: &str, dir: &Path) -> Option<PathBuf> {
    let out = dir.join(format!("{:016x}.png", crate::icon_store::fnv1a(name.as_bytes())));
    if !out.is_file() {
        std::fs::create_dir_all(dir).ok()?;
        save_shell_icon(name, &out)?;
    }
    Some(out)
}

/// Write the icon of `file` to `out` as PNG. A shortcut gives its target's icon, without the
/// shortcut arrow. `None` if the file has no usable icon or `out` cannot be written.
pub fn save_file_icon(file: &Path, out: &Path) -> Option<()> {
    init_com();
    let wide: Vec<u16> = file.as_os_str().encode_wide().chain([0]).collect();
    write_png(&shell_icon(wide.as_ptr(), 0)?, out)
}

/// Write the icon of the shell item called `name` (a parsing name) to `out` as PNG.
fn save_shell_icon(name: &str, out: &Path) -> Option<()> {
    init_com();
    let wide: Vec<u16> = name.encode_utf16().chain([0]).collect();
    let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
    // SAFETY: `wide` is NUL-terminated; `pidl` is a live local that receives an id list owned
    // by the guard below. The bind context and attribute output may be null.
    let hr = unsafe { SHParseDisplayName(wide.as_ptr(), std::ptr::null_mut(), &raw mut pidl, 0, std::ptr::null_mut()) };
    if hr < 0 || pidl.is_null() {
        return None;
    }
    let pidl = IdList(pidl);
    // With SHGFI_PIDL the "path" argument is the id list.
    write_png(&shell_icon(pidl.0.cast_const().cast(), SHGFI_PIDL)?, out)
}

/// The large icon Explorer shows for `item`: a NUL-terminated path, or an id list with SHGFI_PIDL.
fn shell_icon(item: *const u16, flags: SHGFI_FLAGS) -> Option<Icon> {
    let mut info = SHFILEINFOW::default();
    let size = std::mem::size_of::<SHFILEINFOW>() as u32;
    // SAFETY: `item` is a live path or id list as `flags` says, and `info` is a live local of
    // `size` bytes.
    let ok = unsafe { SHGetFileInfoW(item, 0, &raw mut info, size, flags | SHGFI_ICON | SHGFI_LARGEICON) };
    (ok != 0 && !info.hIcon.is_null()).then(|| Icon(info.hIcon))
}

fn write_png(icon: &Icon, out: &Path) -> Option<()> {
    let (w, h, rgba) = icon_pixels(icon)?;
    let mut png_bytes = vec![];
    let mut enc = png::Encoder::new(&mut png_bytes, w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().ok()?;
    writer.write_image_data(&rgba).ok()?;
    writer.finish().ok()?;
    crate::core::fs::write_atomic(out, &png_bytes).ok()
}

struct IdList(*mut ITEMIDLIST);

impl Drop for IdList {
    fn drop(&mut self) {
        // SAFETY: we own this id list (from SHParseDisplayName) and free it once.
        unsafe { ILFree(self.0) };
    }
}

/// The shell needs COM on the calling thread; set it up once per thread.
pub(crate) fn init_com() {
    thread_local!(static DONE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) });
    if !DONE.replace(true) {
        // Already initialised in another mode is fine: the shell call works either way.
        // SAFETY: plain call; the reserved pointer must be null.
        unsafe { CoInitializeEx(std::ptr::null(), (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32) };
    }
}

struct Icon(HICON);

impl Drop for Icon {
    fn drop(&mut self) {
        // SAFETY: we own this icon handle (from SHGetFileInfoW) and drop it once.
        unsafe { DestroyIcon(self.0) };
    }
}

struct Bitmap(HBITMAP);

impl Drop for Bitmap {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: we own this bitmap (from GetIconInfo) and drop it once.
            unsafe { DeleteObject(self.0) };
        }
    }
}

/// The icon as `(width, height, RGBA pixels)`.
fn icon_pixels(icon: &Icon) -> Option<(u32, u32, Vec<u8>)> {
    let mut ii = ICONINFO::default();
    // SAFETY: `ii` is a live local; the bitmaps it receives are owned by the guards below.
    if unsafe { GetIconInfo(icon.0, &raw mut ii) } == 0 {
        return None;
    }
    let mask = Bitmap(ii.hbmMask);
    let color = Bitmap(ii.hbmColor);
    if color.0.is_null() {
        return None; // monochrome icon: rare, the letter tile is fine
    }
    let mut bm = BITMAP::default();
    let size = std::mem::size_of::<BITMAP>() as i32;
    // SAFETY: `bm` is a live local of `size` bytes.
    if unsafe { GetObjectW(color.0, size, (&raw mut bm).cast()) } == 0 {
        return None;
    }
    let w = u32::try_from(bm.bmWidth).ok().filter(|&w| (1..=MAX_SIDE).contains(&w))?;
    let h = u32::try_from(bm.bmHeight).ok().filter(|&h| (1..=MAX_SIDE).contains(&h))?;
    let mut px = dib(&color, w, h)?;
    // Old icons carry no alpha: take transparency from the mask (set bits are transparent).
    if px.as_chunks::<4>().0.iter().all(|p| p[3] == 0) {
        let m = dib(&mask, w, h)?;
        for (p, m) in px.as_chunks_mut::<4>().0.iter_mut().zip(m.as_chunks::<4>().0) {
            p[3] = if m[0] == 0 { 255 } else { 0 };
        }
    }
    for p in px.as_chunks_mut::<4>().0 {
        p.swap(0, 2); // BGRA -> RGBA
    }
    Some((w, h, px))
}

/// The bitmap's pixels as top-down 32-bit BGRA.
fn dib(bmp: &Bitmap, w: u32, h: u32) -> Option<Vec<u8>> {
    let header = BITMAPINFOHEADER {
        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: i32::try_from(w).ok()?,
        biHeight: -i32::try_from(h).ok()?, // negative: top-down rows
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB,
        ..Default::default()
    };
    let mut bi = BITMAPINFO { bmiHeader: header, ..Default::default() };
    let mut buf = vec![0u8; (w * h * 4) as usize];
    // SAFETY: plain call; the screen DC is released below.
    let dc = unsafe { GetDC(std::ptr::null_mut()) };
    if dc.is_null() {
        return None;
    }
    // SAFETY: `buf` holds `h` rows of `w` 32-bit pixels as `bi` describes; `bi` is a live local.
    let lines = unsafe { GetDIBits(dc, bmp.0, 0, h, buf.as_mut_ptr().cast(), &raw mut bi, DIB_RGB_COLORS) };
    // SAFETY: `dc` came from GetDC above.
    unsafe { ReleaseDC(std::ptr::null_mut(), dc) };
    (lines == i32::try_from(h).ok()?).then_some(buf)
}
