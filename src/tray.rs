//! System tray icon. Double-click shows the launcher; right-click offers Show, Settings, Quit.
//!
//! Linux (X11 and Wayland alike) speaks the StatusNotifierItem D-Bus protocol through `ksni`:
//! the desktop draws the icon and menu, we only answer messages. Windows uses `tray-icon`.
//! Stock GNOME hides tray icons unless the AppIndicator extension is enabled (Ubuntu ships it).

use std::time::{Duration, Instant};

/// What the user asked for through the tray.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    Show,
    Settings,
    Quit,
}

const ICON_SIZE: u32 = 32;

/// Turns separate clicks into double-clicks for backends that only report single ones
/// (the StatusNotifierItem protocol has no double-click event).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub struct DoubleClick {
    window: Duration,
    last: Option<Instant>,
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
impl DoubleClick {
    pub fn new(window: Duration) -> Self {
        Self { window, last: None }
    }

    /// Record a click; true if it completes a double-click.
    pub fn click_at(&mut self, now: Instant) -> bool {
        match self.last.take() {
            Some(prev) if now.saturating_duration_since(prev) <= self.window => true,
            _ => {
                self.last = Some(now);
                false
            }
        }
    }
}

/// RGBA -> ARGB32 byte order, as the StatusNotifierItem `IconPixmap` property wants.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn rgba_to_argb(rgba: &[u8]) -> Vec<u8> {
    rgba.as_chunks::<4>().0.iter().flat_map(|p| [p[3], p[0], p[1], p[2]]).collect()
}

#[cfg(target_os = "linux")]
pub use linux::spawn;
#[cfg(not(any(target_os = "linux", windows)))]
pub use other::spawn;
#[cfg(windows)]
pub use windows::spawn;

/// Keeps the tray alive; dropping it removes the icon. Not `Send` on Windows, so hold it on
/// the thread that created it.
pub struct TrayHandle {
    _inner: Box<dyn std::any::Any>,
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{
        DoubleClick, Duration, ICON_SIZE, Instant, TrayCommand, TrayHandle, host_detects_double_click, rgba_to_argb,
    };
    use crate::app_icon::icon_rgba;
    use ksni::blocking::TrayMethods;
    use std::sync::Mutex;

    struct KairosTray {
        on_command: Mutex<Box<dyn Fn(TrayCommand) + Send>>,
        clicks: DoubleClick,
        /// The host detects double-clicks itself and sends one `Activate` per double-click.
        host_double_click: bool,
    }

    impl KairosTray {
        fn send(&self, cmd: TrayCommand) {
            (crate::core::sync::lock(&self.on_command))(cmd);
        }
    }

    impl ksni::Tray for KairosTray {
        fn id(&self) -> String {
            "kairos".into()
        }

        fn title(&self) -> String {
            "Kairos".into()
        }

        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            vec![ksni::Icon {
                width: ICON_SIZE as i32,
                height: ICON_SIZE as i32,
                data: rgba_to_argb(&icon_rgba(ICON_SIZE)),
            }]
        }

        /// Most hosts report every primary click as `Activate`; two within 500 ms show the
        /// launcher. GNOME's AppIndicator extension reports only the double-click itself (a
        /// single click opens the menu), so there one `Activate` is enough.
        fn activate(&mut self, _x: i32, _y: i32) {
            if self.host_double_click || self.clicks.click_at(Instant::now()) {
                self.send(TrayCommand::Show);
            }
        }

        fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
            use ksni::menu::{MenuItem, StandardItem};
            let item = |label: &str, cmd: TrayCommand| {
                StandardItem {
                    label: label.into(),
                    activate: Box::new(move |t: &mut Self| t.send(cmd)),
                    ..Default::default()
                }
                .into()
            };
            vec![
                item("Show", TrayCommand::Show),
                item("Settings", TrayCommand::Settings),
                MenuItem::Separator,
                item("Quit", TrayCommand::Quit),
            ]
        }
    }

    /// Start the tray. `None` (with a message on stderr) when there is no tray host.
    pub fn spawn(on_command: impl Fn(TrayCommand) + Send + 'static) -> Option<TrayHandle> {
        let tray = KairosTray {
            on_command: Mutex::new(Box::new(on_command)),
            clicks: DoubleClick::new(Duration::from_millis(500)),
            host_double_click: host_detects_double_click(std::env::var("XDG_CURRENT_DESKTOP").as_deref().unwrap_or("")),
        };
        match tray.spawn() {
            Ok(handle) => Some(TrayHandle { _inner: Box::new(handle) }),
            Err(e) => {
                log::warn!("tray: not available ({e}); on GNOME enable the AppIndicator extension");
                None
            }
        }
    }
}

/// GNOME Shell's AppIndicator extension handles single vs. double clicks itself.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn host_detects_double_click(xdg_current_desktop: &str) -> bool {
    xdg_current_desktop.split(':').any(|d| d.eq_ignore_ascii_case("gnome"))
}

#[cfg(windows)]
mod windows {
    use super::{ICON_SIZE, TrayCommand, TrayHandle};
    use crate::app_icon::icon_rgba;
    use std::sync::{Arc, Mutex};
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, MouseButton, TrayIconBuilder, TrayIconEvent};

    /// Must be called on the thread that runs the event loop (it owns the tray's message window).
    pub fn spawn(on_command: impl Fn(TrayCommand) + Send + 'static) -> Option<TrayHandle> {
        let on_command = Arc::new(Mutex::new(on_command));
        let send = move |cmd| {
            (crate::core::sync::lock(&on_command))(cmd);
        };

        let (show, settings, quit) = (
            MenuItem::new("Show", true, None),
            MenuItem::new("Settings", true, None),
            MenuItem::new("Quit", true, None),
        );
        let menu = Menu::new();
        menu.append_items(&[&show, &settings, &PredefinedMenuItem::separator(), &quit]).ok()?;
        let (show_id, settings_id, quit_id) = (show.id().clone(), settings.id().clone(), quit.id().clone());

        let on_menu = send.clone();
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            let cmd = if e.id == show_id {
                TrayCommand::Show
            } else if e.id == settings_id {
                TrayCommand::Settings
            } else if e.id == quit_id {
                TrayCommand::Quit
            } else {
                return;
            };
            on_menu(cmd);
        }));
        TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
            if let TrayIconEvent::DoubleClick { button: MouseButton::Left, .. } = e {
                send(TrayCommand::Show);
            }
        }));

        let icon = Icon::from_rgba(icon_rgba(ICON_SIZE), ICON_SIZE, ICON_SIZE).ok()?;
        match TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .with_tooltip("Kairos")
            .with_icon(icon)
            .build()
        {
            Ok(tray) => Some(TrayHandle { _inner: Box::new(tray) }),
            Err(e) => {
                log::warn!("tray: {e}");
                None
            }
        }
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
mod other {
    use super::*;

    pub fn spawn(_on_command: impl Fn(TrayCommand) + Send + 'static) -> Option<TrayHandle> {
        None
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn gnome_hosts_send_one_activate_per_double_click() {
        use super::host_detects_double_click as gnome;
        assert!(gnome("ubuntu:GNOME") && gnome("GNOME") && gnome("GNOME-Classic:GNOME"));
        assert!(!gnome("KDE") && !gnome("XFCE") && !gnome("") && !gnome("X-Cinnamon"));
    }

    use super::*;

    #[test]
    fn two_clicks_within_the_window_are_a_double_click() {
        let mut d = DoubleClick::new(Duration::from_millis(500));
        let t0 = Instant::now();
        assert!(!d.click_at(t0), "first click alone is not a double-click");
        assert!(d.click_at(t0 + Duration::from_millis(300)));
        assert!(!d.click_at(t0 + Duration::from_millis(400)), "a triple click must not fire twice");
    }

    #[test]
    fn slow_clicks_never_double() {
        let mut d = DoubleClick::new(Duration::from_millis(500));
        let t0 = Instant::now();
        assert!(!d.click_at(t0));
        assert!(!d.click_at(t0 + Duration::from_millis(800)), "too slow, restarts the pair");
        assert!(d.click_at(t0 + Duration::from_millis(1100)), "...and that restarted pair can complete");
    }

    #[test]
    fn argb_byte_order() {
        assert_eq!(rgba_to_argb(&[1, 2, 3, 4, 5, 6, 7, 8]), [4, 1, 2, 3, 8, 5, 6, 7]);
    }
}
