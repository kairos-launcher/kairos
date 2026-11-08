//! Which monitor to open on: a point (physical desktop pixels) on the wanted monitor,
//! taken from the mouse cursor or the focused window. X11 and Windows only; on Wayland
//! the compositor places the window.

use crate::config::Placement;

/// A point on the monitor to open on, or `None` for the primary monitor.
/// Called before our window is shown, so "active" is still the window the user was in.
pub fn anchor_point(mode: Placement) -> Option<(i32, i32)> {
    match mode {
        Placement::Primary => None,
        Placement::Active => sys::active_window_center().or_else(sys::cursor),
        Placement::Mouse => sys::cursor(),
    }
}

/// Index of the monitor (`(x, y, width, height)`) that contains `point`.
pub fn monitor_containing(point: (i32, i32), monitors: &[(i32, i32, u32, u32)]) -> Option<usize> {
    let (px, py) = point;
    monitors.iter().position(|&(x, y, w, h)| px >= x && py >= y && px < x + w as i32 && py < y + h as i32)
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod sys {
    use windows_sys::Win32::Foundation::{POINT, RECT};
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetCursorPos, GetForegroundWindow, GetWindowRect};

    pub fn cursor() -> Option<(i32, i32)> {
        let mut p = POINT { x: 0, y: 0 };
        // SAFETY: the out pointer is a live local.
        (unsafe { GetCursorPos(&raw mut p) } != 0).then_some((p.x, p.y))
    }

    pub fn active_window_center() -> Option<(i32, i32)> {
        // SAFETY: no arguments; a null result is checked below.
        let hwnd = unsafe { GetForegroundWindow() };
        if hwnd.is_null() {
            return None;
        }
        let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        // SAFETY: the out pointer is a live local; a stale handle makes the call fail.
        (unsafe { GetWindowRect(hwnd, &raw mut r) } != 0)
            .then_some((i32::midpoint(r.left, r.right), i32::midpoint(r.top, r.bottom)))
    }
}

#[cfg(target_os = "linux")]
mod sys {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt};

    fn connect() -> Option<(x11rb::rust_connection::RustConnection, u32)> {
        let (conn, screen) = x11rb::connect(None).ok()?;
        let root = conn.setup().roots.get(screen)?.root;
        Some((conn, root))
    }

    pub fn cursor() -> Option<(i32, i32)> {
        let (conn, root) = connect()?;
        let r = conn.query_pointer(root).ok()?.reply().ok()?;
        Some((r.root_x.into(), r.root_y.into()))
    }

    pub fn active_window_center() -> Option<(i32, i32)> {
        let (conn, root) = connect()?;
        let atom = conn.intern_atom(true, b"_NET_ACTIVE_WINDOW").ok()?.reply().ok()?.atom;
        let prop = conn.get_property(false, root, atom, AtomEnum::WINDOW, 0, 1).ok()?.reply().ok()?;
        let win = prop.value32()?.next().filter(|&w| w != 0)?;
        let geo = conn.get_geometry(win).ok()?.reply().ok()?;
        let pos = conn.translate_coordinates(win, root, 0, 0).ok()?.reply().ok()?;
        Some((i32::from(pos.dst_x) + i32::from(geo.width) / 2, i32::from(pos.dst_y) + i32::from(geo.height) / 2))
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod sys {
    pub fn cursor() -> Option<(i32, i32)> {
        None
    }
    pub fn active_window_center() -> Option<(i32, i32)> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_monitor_under_a_point() {
        // 1920x1080 on the left, 4K to the right, top-aligned
        let mons = [(0, 0, 1920, 1080), (1920, 0, 3840, 2160)];
        assert_eq!(monitor_containing((10, 10), &mons), Some(0));
        assert_eq!(monitor_containing((1919, 1079), &mons), Some(0));
        assert_eq!(monitor_containing((1920, 0), &mons), Some(1));
        assert_eq!(monitor_containing((100, 1500), &mons), None, "below the small monitor: nothing");
        assert_eq!(monitor_containing((-5, 0), &mons), None);
    }
}
