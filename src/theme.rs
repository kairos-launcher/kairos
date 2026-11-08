//! Does the desktop prefer a dark theme? Used when `theme = "auto"`.

/// GNOME's `color-scheme` is `'prefer-dark'`, `'prefer-light'` or `'default'`. `default` means
/// "no preference", so fall back to the GTK theme name (`Yaru-dark`, `Adwaita-dark`, ...).
#[cfg_attr(windows, allow(dead_code))]
pub fn parse_color_scheme(scheme: &str, gtk_theme: &str) -> bool {
    match scheme.trim().trim_matches('\'') {
        "prefer-dark" => true,
        "prefer-light" => false,
        _ => gtk_theme.to_lowercase().contains("dark"),
    }
}

/// Windows stores `AppsUseLightTheme` as `0x0` (dark) or `0x1` (light).
#[cfg_attr(not(windows), allow(dead_code))]
pub fn parse_reg_light_theme(reg_output: &str) -> Option<bool> {
    let value = reg_output.lines().find(|l| l.contains("AppsUseLightTheme"))?.split_whitespace().last()?;
    let light = u32::from_str_radix(value.trim_start_matches("0x"), 16).ok()?;
    Some(light == 0)
}

/// KDE: `ColorScheme=BreezeDark` in `kdeglobals`.
#[cfg_attr(windows, allow(dead_code))]
pub fn parse_kde_scheme(kdeglobals: &str) -> Option<bool> {
    let line = kdeglobals.lines().find_map(|l| l.trim().strip_prefix("ColorScheme="))?;
    Some(line.to_lowercase().contains("dark"))
}

/// `None` if the desktop does not say.
pub fn system_prefers_dark() -> Option<bool> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let out = std::process::Command::new("reg")
            .args([
                "query",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
                "/v",
                "AppsUseLightTheme",
            ])
            .creation_flags(0x0800_0000)
            .output()
            .ok()?;
        parse_reg_light_theme(&String::from_utf8_lossy(&out.stdout))
    }
    #[cfg(not(windows))]
    {
        let gsettings = |key: &str| {
            std::process::Command::new("gsettings")
                .args(["get", "org.gnome.desktop.interface", key])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        };
        if let Some(scheme) = gsettings("color-scheme") {
            return Some(parse_color_scheme(&scheme, &gsettings("gtk-theme").unwrap_or_default()));
        }
        let kde = std::fs::read_to_string(dirs::config_dir()?.join("kdeglobals")).ok()?;
        parse_kde_scheme(&kde)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gnome_color_scheme() {
        assert!(parse_color_scheme("'prefer-dark'", ""));
        assert!(!parse_color_scheme("'prefer-light'", "Yaru-dark"), "an explicit preference wins");
        assert!(parse_color_scheme("'default'", "'Yaru-olive-dark'"), "no preference: use the theme name");
        assert!(!parse_color_scheme("'default'", "'Yaru'"));
        assert!(!parse_color_scheme("", ""));
    }

    #[test]
    fn windows_registry() {
        let dark = "HKEY_CURRENT_USER\\...\\Personalize\n    AppsUseLightTheme    REG_DWORD    0x0\n";
        let light = "    AppsUseLightTheme    REG_DWORD    0x1\n";
        assert_eq!(parse_reg_light_theme(dark), Some(true));
        assert_eq!(parse_reg_light_theme(light), Some(false));
        assert_eq!(parse_reg_light_theme("garbage"), None);
    }

    #[test]
    fn kde() {
        assert_eq!(parse_kde_scheme("[General]\nColorScheme=BreezeDark\n"), Some(true));
        assert_eq!(parse_kde_scheme("[General]\nColorScheme=BreezeLight\n"), Some(false));
        assert_eq!(parse_kde_scheme("[General]\n"), None);
    }
}
