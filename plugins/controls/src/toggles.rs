//! Wi-Fi, Bluetooth, dark mode and screen brightness.
//!
//! - Linux: NetworkManager and BlueZ over D-Bus (rfkill as the fallback), logind for brightness
//!   (no root needed), GNOME/GTK `gsettings` or KDE Plasma tools for dark mode.
//! - Windows: the radio API for Wi-Fi and Bluetooth, WMI for the built-in screen's brightness,
//!   the per-user theme registry values for dark mode.
//! - macOS: `networksetup`, `blueutil`, `brightness` and AppleScript.
//!
//! `get` is `None` where the control does not exist here (no Wi-Fi card, no backlight), so its
//! row is left out.

use crate::commands::Switch;

pub fn get(s: Switch) -> Option<bool> {
    match s {
        Switch::Wifi => imp::wifi(),
        Switch::Bluetooth => imp::bluetooth(),
        Switch::Dark => imp::dark(),
    }
}

pub fn set(s: Switch, on: bool) -> Result<(), String> {
    match s {
        Switch::Wifi => imp::set_wifi(on),
        Switch::Bluetooth => imp::set_bluetooth(on),
        Switch::Dark => imp::set_dark(on),
    }
}

/// Brightness of the built-in screen, 0–100.
pub fn brightness() -> Option<u8> {
    imp::brightness()
}

pub fn set_brightness(percent: u8) -> Result<(), String> {
    imp::set_brightness(percent.min(100))
}

/// `value` of `max` as 0–100.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn ratio(value: u64, max: u64) -> Option<u8> {
    (max > 0).then(|| u8::try_from((value.min(max) * 100 + max / 2) / max).unwrap_or(100))
}

#[cfg(target_os = "linux")]
mod imp {
    use std::path::{Path, PathBuf};

    use zbus::blocking::{Connection, Proxy, proxy::Builder};
    use zbus::proxy::CacheProperties;

    use super::ratio;
    use crate::run;

    fn system_proxy<'a>(c: &Connection, dest: &'a str, path: &'a str, iface: &'a str) -> zbus::Result<Proxy<'a>> {
        Builder::new(c).destination(dest)?.path(path)?.interface(iface)?.cache_properties(CacheProperties::No).build()
    }

    fn entries(dir: &str) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> =
            std::fs::read_dir(dir).map(|rd| rd.flatten().map(|e| e.path()).collect()).unwrap_or_default();
        v.sort();
        v
    }

    fn read(p: &Path) -> Option<String> {
        std::fs::read_to_string(p).ok().map(|s| s.trim().to_string())
    }

    /// rfkill switches of `kind` (`wlan`, `bluetooth`): `Some(true)` when none is soft-blocked.
    fn rfkill(kind: &str) -> Option<bool> {
        let devs: Vec<PathBuf> = entries("/sys/class/rfkill")
            .into_iter()
            .filter(|d| read(&d.join("type")).as_deref() == Some(kind))
            .collect();
        if devs.is_empty() {
            return None;
        }
        Some(devs.iter().all(|d| read(&d.join("soft")).as_deref() == Some("0")))
    }

    fn set_rfkill(kind: &str, on: bool) -> Result<(), String> {
        run::status(&["rfkill", if on { "unblock" } else { "block" }, kind])
    }

    // --- Wi-Fi -----------------------------------------------------------------------------

    const NM: &str = "org.freedesktop.NetworkManager";
    const NM_PATH: &str = "/org/freedesktop/NetworkManager";

    fn has_wifi() -> bool {
        !entries("/sys/class/ieee80211").is_empty() || rfkill("wlan").is_some()
    }

    pub fn wifi() -> Option<bool> {
        if !has_wifi() {
            return None;
        }
        let from_nm = Connection::system()
            .ok()
            .and_then(|c| system_proxy(&c, NM, NM_PATH, NM).ok()?.get_property::<bool>("WirelessEnabled").ok());
        from_nm.or_else(|| rfkill("wlan"))
    }

    pub fn set_wifi(on: bool) -> Result<(), String> {
        // NetworkManager first, so the desktop's own switch agrees; rfkill without it (iwd, …).
        let via_nm = Connection::system().map_err(|e| e.to_string()).and_then(|c| {
            let p = system_proxy(&c, NM, NM_PATH, NM).map_err(|e| e.to_string())?;
            p.set_property("WirelessEnabled", on).map_err(|e| e.to_string())
        });
        via_nm.or_else(|nm_err| set_rfkill("wlan", on).map_err(|e| format!("NetworkManager: {nm_err}; {e}")))
    }

    // --- Bluetooth -------------------------------------------------------------------------

    const BLUEZ: &str = "org.bluez";
    const ADAPTER: &str = "org.bluez.Adapter1";

    /// BlueZ object paths of the adapters (`/org/bluez/hci0`).
    fn adapters() -> Vec<String> {
        entries("/sys/class/bluetooth")
            .iter()
            .filter_map(|p| p.file_name()?.to_str().map(str::to_string))
            .filter(|n| n.starts_with("hci") && !n.contains(':'))
            .map(|n| format!("/org/bluez/{n}"))
            .collect()
    }

    pub fn bluetooth() -> Option<bool> {
        let paths = adapters();
        if paths.is_empty() {
            // Blocked adapters can vanish from sysfs; rfkill still knows them.
            return rfkill("bluetooth");
        }
        let c = Connection::system().ok();
        let powered = c
            .as_ref()
            .and_then(|c| system_proxy(c, BLUEZ, &paths[0], ADAPTER).ok()?.get_property::<bool>("Powered").ok());
        powered.or_else(|| rfkill("bluetooth"))
    }

    fn set_powered(on: bool) -> Result<(), String> {
        let c = Connection::system().map_err(|e| format!("D-Bus: {e}"))?;
        let paths = adapters();
        let path = paths.first().ok_or("no Bluetooth adapter")?;
        let p = system_proxy(&c, BLUEZ, path, ADAPTER).map_err(|e| e.to_string())?;
        p.set_property("Powered", on).map_err(|e| e.to_string())
    }

    pub fn set_bluetooth(on: bool) -> Result<(), String> {
        if !on {
            // Like the desktop's own switch: block the radio (it stays off after a reboot too).
            return set_rfkill("bluetooth", false).or_else(|_| set_powered(false));
        }
        if rfkill("bluetooth") == Some(false) {
            set_rfkill("bluetooth", true)?;
        }
        // An adapter that was just unblocked takes a moment to come back.
        let mut last = String::new();
        for _ in 0..10 {
            match set_powered(true) {
                Ok(()) => return Ok(()),
                Err(e) => last = e,
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        Err(last)
    }

    // --- Brightness ------------------------------------------------------------------------

    /// The backlight to use: firmware before platform before raw, as systemd orders them.
    fn backlight() -> Option<PathBuf> {
        let rank = |d: &PathBuf| match read(&d.join("type")).as_deref() {
            Some("firmware") => 0,
            Some("platform") => 1,
            _ => 2,
        };
        entries("/sys/class/backlight").into_iter().min_by_key(rank)
    }

    fn level(dev: &Path, file: &str) -> Option<u64> {
        read(&dev.join(file))?.parse().ok()
    }

    pub fn brightness() -> Option<u8> {
        let d = backlight()?;
        ratio(level(&d, "brightness")?, level(&d, "max_brightness")?)
    }

    pub fn set_brightness(percent: u8) -> Result<(), String> {
        let d = backlight().ok_or("no screen backlight")?;
        let name = d.file_name().and_then(|n| n.to_str()).ok_or("bad backlight name")?.to_string();
        let max = level(&d, "max_brightness").ok_or("unreadable max_brightness")?;
        // Never fully dark: 1 % at least, so `bright 0` cannot blank the screen.
        let value = u32::try_from((max * u64::from(percent.max(1)) / 100).max(1)).unwrap_or(u32::MAX);
        let c = Connection::system().map_err(|e| format!("D-Bus: {e}"))?;
        let p = system_proxy(
            &c,
            "org.freedesktop.login1",
            "/org/freedesktop/login1/session/auto",
            "org.freedesktop.login1.Session",
        )
        .map_err(|e| e.to_string())?;
        p.call_method("SetBrightness", &("backlight", name.as_str(), value))
            .map(|_| ())
            .map_err(|e| format!("logind: {e}"))
    }

    // --- Dark mode -------------------------------------------------------------------------

    fn kde() -> bool {
        run::desktop_is("KDE") && run::has("plasma-apply-colorscheme")
    }

    fn kreadconfig() -> &'static str {
        if run::has("kreadconfig6") { "kreadconfig6" } else { "kreadconfig5" }
    }

    pub fn dark() -> Option<bool> {
        if kde() {
            let scheme =
                run::output(&[kreadconfig(), "--file", "kdeglobals", "--group", "General", "--key", "ColorScheme"])?;
            return Some(scheme.to_lowercase().contains("dark"));
        }
        let v = run::output(&["gsettings", "get", "org.gnome.desktop.interface", "color-scheme"])?;
        Some(v.contains("prefer-dark"))
    }

    pub fn set_dark(on: bool) -> Result<(), String> {
        if kde() {
            return run::status(&["plasma-apply-colorscheme", if on { "BreezeDark" } else { "BreezeLight" }]);
        }
        let v = if on { "prefer-dark" } else { "default" };
        run::status(&["gsettings", "set", "org.gnome.desktop.interface", "color-scheme", v])
    }
}

#[cfg(windows)]
mod imp {
    use windows::Devices::Radios::{Radio, RadioAccessStatus, RadioKind, RadioState};

    use crate::{run, win};

    fn radio(kind: RadioKind) -> Option<Radio> {
        // Asks once; desktop apps are normally allowed without a prompt.
        let _ = Radio::RequestAccessAsync().and_then(|a| a.join());
        let radios = Radio::GetRadiosAsync().ok()?.join().ok()?;
        radios.into_iter().find(|r| r.Kind().is_ok_and(|k| k == kind))
    }

    fn get_radio(kind: RadioKind) -> Option<bool> {
        Some(radio(kind)?.State().ok()? == RadioState::On)
    }

    fn set_radio(kind: RadioKind, on: bool) -> Result<(), String> {
        let r = radio(kind).ok_or("no such radio")?;
        let state = if on { RadioState::On } else { RadioState::Off };
        match r.SetStateAsync(state).and_then(|a| a.join()).map_err(|e| e.to_string())? {
            RadioAccessStatus::Allowed => Ok(()),
            RadioAccessStatus::DeniedByUser => Err("denied in Settings › Privacy › Radios".into()),
            _ => Err("Windows did not allow changing the radio".into()),
        }
    }

    pub fn wifi() -> Option<bool> {
        get_radio(RadioKind::WiFi)
    }

    pub fn set_wifi(on: bool) -> Result<(), String> {
        set_radio(RadioKind::WiFi, on)
    }

    pub fn bluetooth() -> Option<bool> {
        get_radio(RadioKind::Bluetooth)
    }

    pub fn set_bluetooth(on: bool) -> Result<(), String> {
        set_radio(RadioKind::Bluetooth, on)
    }

    const BRIGHTNESS: &str = "Get-CimInstance -Namespace root/WMI -ClassName WmiMonitorBrightness";

    pub fn brightness() -> Option<u8> {
        run::powershell(&format!("({BRIGHTNESS} | Select-Object -First 1).CurrentBrightness"))?.parse().ok()
    }

    pub fn set_brightness(percent: u8) -> Result<(), String> {
        let script = format!(
            "Get-CimInstance -Namespace root/WMI -ClassName WmiMonitorBrightnessMethods | \
             Invoke-CimMethod -MethodName WmiSetBrightness -Arguments @{{Timeout=0; Brightness={percent}}}"
        );
        run::status(&["powershell", "-NoProfile", "-NonInteractive", "-Command", &script])
    }

    const THEME: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";

    pub fn dark() -> Option<bool> {
        // Missing means light (a fresh profile).
        Some(win::get_dword(THEME, "AppsUseLightTheme").unwrap_or(1) == 0)
    }

    pub fn set_dark(on: bool) -> Result<(), String> {
        let light = u32::from(!on);
        win::set_dword(THEME, "AppsUseLightTheme", light)?;
        win::set_dword(THEME, "SystemUsesLightTheme", light)?;
        win::setting_changed(Some("ImmersiveColorSet"));
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use crate::run;

    /// The Wi-Fi interface (`en0`) from `networksetup -listallhardwareports`.
    fn wifi_device() -> Option<String> {
        let ports = run::output(&["networksetup", "-listallhardwareports"])?;
        let mut lines = ports.lines();
        while let Some(l) = lines.next() {
            if l.contains("Wi-Fi") || l.contains("AirPort") {
                return lines.next()?.strip_prefix("Device:").map(|d| d.trim().to_string());
            }
        }
        None
    }

    pub fn wifi() -> Option<bool> {
        let dev = wifi_device()?;
        Some(run::output(&["networksetup", "-getairportpower", &dev])?.ends_with("On"))
    }

    pub fn set_wifi(on: bool) -> Result<(), String> {
        let dev = wifi_device().ok_or("no Wi-Fi interface")?;
        run::status(&["networksetup", "-setairportpower", &dev, if on { "on" } else { "off" }])
    }

    pub fn bluetooth() -> Option<bool> {
        run::has("blueutil").then(|| run::output(&["blueutil", "-p"]).is_some_and(|s| s == "1"))
    }

    pub fn set_bluetooth(on: bool) -> Result<(), String> {
        if !run::has("blueutil") {
            return Err("needs blueutil (brew install blueutil)".into());
        }
        run::status(&["blueutil", "-p", if on { "1" } else { "0" }])
    }

    pub fn brightness() -> Option<u8> {
        // `brightness -l`: "display 0: brightness 0.750000"
        let out = run::output(&["brightness", "-l"])?;
        let level: f64 = out.lines().find_map(|l| l.split("brightness ").nth(1)?.trim().parse().ok())?;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        Some((level.clamp(0.0, 1.0) * 100.0).round() as u8)
    }

    pub fn set_brightness(percent: u8) -> Result<(), String> {
        if !run::has("brightness") {
            return Err("needs the brightness tool (brew install brightness)".into());
        }
        run::status(&["brightness", &format!("{:.2}", f64::from(percent) / 100.0)])
    }

    const APPEARANCE: &str = "tell application \"System Events\" to tell appearance preferences to";

    pub fn dark() -> Option<bool> {
        Some(run::output(&["osascript", "-e", &format!("{APPEARANCE} get dark mode")])? == "true")
    }

    pub fn set_dark(on: bool) -> Result<(), String> {
        run::status(&["osascript", "-e", &format!("{APPEARANCE} set dark mode to {on}")])
    }
}

#[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
mod imp {
    const NO: &str = "not supported on this system";

    pub fn wifi() -> Option<bool> {
        None
    }
    pub fn set_wifi(_: bool) -> Result<(), String> {
        Err(NO.into())
    }
    pub fn bluetooth() -> Option<bool> {
        None
    }
    pub fn set_bluetooth(_: bool) -> Result<(), String> {
        Err(NO.into())
    }
    pub fn dark() -> Option<bool> {
        None
    }
    pub fn set_dark(_: bool) -> Result<(), String> {
        Err(NO.into())
    }
    pub fn brightness() -> Option<u8> {
        None
    }
    pub fn set_brightness(_: u8) -> Result<(), String> {
        Err(NO.into())
    }
}

#[cfg(test)]
mod tests {
    use super::ratio;

    #[test]
    fn ratios() {
        assert_eq!(ratio(0, 255), Some(0));
        assert_eq!(ratio(128, 255), Some(50));
        assert_eq!(ratio(255, 255), Some(100));
        assert_eq!(ratio(300, 255), Some(100));
        assert_eq!(ratio(1, 0), None);
    }
}
