//! Where the window opens: on the monitor the user is working on, at their dragged position or
//! centred near the top. X11 and Windows only; Wayland compositors place windows themselves.

use slint::ComponentHandle;
use slint::winit_030::{WinitWindowAccessor, winit};

use super::MainWindow;
use crate::screen;

/// Top edge of the window as a fraction of the monitor height.
const TOP_FRACTION: f64 = 0.22;
/// Logical width of the window, and the height that must stay on screen.
const WIDTH: f64 = 640.0;
const MIN_VISIBLE_HEIGHT: f64 = 100.0;

/// A monitor in physical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Monitor {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale: f64,
}

/// Where to put the window on `monitor`: at the user's dragged offset (kept on the monitor),
/// else centred near the top.
pub(super) fn placement(user_offset: Option<(i32, i32)>, m: Monitor) -> (i32, i32) {
    let width = (WIDTH * m.scale) as i32;
    let (mw, mh) = (m.width as i32, m.height as i32);
    match user_offset {
        Some((dx, dy)) => (
            m.x + dx.clamp(0, (mw - width).max(0)),
            m.y + dy.clamp(0, (mh - (MIN_VISIBLE_HEIGHT * m.scale) as i32).max(0)),
        ),
        None => (m.x + (mw - width) / 2, m.y + (f64::from(m.height) * TOP_FRACTION) as i32),
    }
}

/// Move the window onto the monitor containing `anchor` (else the primary one).
pub(super) fn move_to_monitor(ui: &MainWindow, anchor: Option<(i32, i32)>, user_offset: Option<(i32, i32)>) {
    ui.window().with_winit_window(|w| {
        let monitors: Vec<_> = w.available_monitors().collect();
        let rects: Vec<_> =
            monitors.iter().map(|m| (m.position().x, m.position().y, m.size().width, m.size().height)).collect();
        let found = anchor.and_then(|p| screen::monitor_containing(p, &rects)).and_then(|i| monitors.get(i).cloned());
        let Some(m) = found.or_else(|| w.primary_monitor()).or_else(|| w.current_monitor()) else { return };
        let (size, pos) = (m.size(), m.position());
        let monitor = Monitor { x: pos.x, y: pos.y, width: size.width, height: size.height, scale: m.scale_factor() };
        let (x, y) = placement(user_offset, monitor);
        w.set_outer_position(winit::dpi::PhysicalPosition::new(x, y));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(x: i32, width: u32, height: u32, scale: f64) -> Monitor {
        Monitor { x, y: 0, width, height, scale }
    }

    #[test]
    fn placement_prefers_the_users_position_else_centres() {
        let first = monitor(0, 1920, 1080, 1.0);
        assert_eq!(placement(None, first), (640, (1080.0 * TOP_FRACTION) as i32));
        assert_eq!(placement(Some((12, 34)), first), (12, 34));
        // second monitor to the right, 2x scale: 640 logical = 1280 physical
        let second = monitor(1920, 3840, 2160, 2.0);
        assert_eq!(placement(None, second).0, 1920 + (3840 - 1280) / 2);
        // the dragged offset applies relative to whichever monitor it opens on
        assert_eq!(placement(Some((12, 34)), second), (1932, 34));
        // and is pulled back onto a smaller monitor
        assert_eq!(placement(Some((3000, 2000)), first), (1920 - 640, 1080 - 100));
        assert_eq!(placement(Some((-50, -50)), first), (0, 0));
    }
}
