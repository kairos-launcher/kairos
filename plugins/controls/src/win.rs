//! Windows helpers: per-user registry values and telling running apps that a setting changed.

use windows::Win32::Foundation::{ERROR_SUCCESS, LPARAM, WPARAM};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, REG_DWORD, RRF_RT_REG_DWORD, RegGetValueW, RegSetKeyValueW};
use windows::Win32::UI::WindowsAndMessaging::{
    HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
};
use windows::core::HSTRING;

/// A `DWORD` under `HKEY_CURRENT_USER\<key>`; `None` when it is not set.
pub fn get_dword(key: &str, name: &str) -> Option<u32> {
    let (key, name) = (HSTRING::from(key), HSTRING::from(name));
    let mut value = 0u32;
    let mut size = 4u32;
    // SAFETY: `value` is a 4-byte buffer and `size` says so.
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &key,
            &name,
            RRF_RT_REG_DWORD,
            None,
            Some((&raw mut value).cast()),
            Some(&raw mut size),
        )
    };
    (r == ERROR_SUCCESS).then_some(value)
}

/// Set a `DWORD` under `HKEY_CURRENT_USER\<key>`, creating the key when needed.
pub fn set_dword(key: &str, name: &str, value: u32) -> Result<(), String> {
    let (k, n) = (HSTRING::from(key), HSTRING::from(name));
    // SAFETY: reads 4 bytes from `value`.
    let r = unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, &k, &n, REG_DWORD.0, Some((&raw const value).cast()), 4) };
    if r == ERROR_SUCCESS { Ok(()) } else { Err(format!("registry {key}\\{name}: error {}", r.0)) }
}

/// Broadcast `WM_SETTINGCHANGE` (with `area`, e.g. `ImmersiveColorSet`), so open windows and the
/// taskbar pick up the new value. Windows that hang are skipped after a second.
pub fn setting_changed(area: Option<&str>) {
    let area = area.map(HSTRING::from);
    let lparam = area.as_ref().map_or(0, |a| a.as_ptr() as isize);
    // SAFETY: `area` outlives the call; the result is not needed.
    unsafe {
        SendMessageTimeoutW(HWND_BROADCAST, WM_SETTINGCHANGE, WPARAM(0), LPARAM(lparam), SMTO_ABORTIFHUNG, 1000, None);
    }
}
