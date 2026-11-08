//! `kairos --install-shortcut`: bind the configured hotkey to `kairos --toggle`
//! in the desktop environment. Wayland apps cannot grab global keys themselves.

use std::process::Command;

const MEDIA_KEYS: &str = "org.gnome.settings-daemon.plugins.media-keys";
const BINDING_PATH: &str = "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/kairos/";

/// `"alt+space"` -> `"<Alt>space"` (GNOME accelerator syntax).
pub fn gnome_accelerator(hotkey: &str) -> Result<String, String> {
    let parts: Vec<String> = hotkey.split('+').map(|p| p.trim().to_lowercase()).collect();
    let (key, mods) = parts.split_last().ok_or("empty hotkey")?;
    if key.is_empty() {
        return Err(format!("no key in {hotkey:?}"));
    }
    let mut out = String::new();
    for m in mods {
        out.push_str(match m.as_str() {
            "alt" | "option" => "<Alt>",
            "ctrl" | "control" => "<Control>",
            "shift" => "<Shift>",
            "super" | "win" | "meta" | "cmd" => "<Super>",
            other => return Err(format!("unknown modifier {other:?}")),
        });
    }
    out.push_str(key);
    Ok(out)
}

/// Extract the quoted strings from a gsettings array such as `['a', 'b']` or `@as []`.
fn parse_str_array(s: &str) -> Vec<String> {
    s.split('\'').skip(1).step_by(2).map(str::to_string).collect()
}

fn format_str_array(items: &[String]) -> String {
    let inner: Vec<String> = items.iter().map(|i| format!("'{i}'")).collect();
    format!("[{}]", inner.join(", "))
}

fn gsettings(args: &[&str]) -> Result<String, String> {
    let out = Command::new("gsettings").args(args).output().map_err(|e| format!("gsettings: {e}"))?;
    if !out.status.success() {
        return Err(format!("gsettings {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn is_gnome() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|d| d.to_uppercase().contains("GNOME"))
}

pub fn install(hotkey: &str) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let command = format!("\"{}\" --toggle", exe.display());

    if !is_gnome() {
        print_snippets(hotkey, &command);
        return Ok(());
    }

    let accel = gnome_accelerator(hotkey)?;
    // Alt+Space is GNOME's window menu and Super+Space its input-source switch by default.
    for key in ["activate-window-menu", "switch-input-source", "switch-input-source-backward"] {
        if let Ok(v) = gsettings(&["get", "org.gnome.desktop.wm.keybindings", key])
            && v.to_lowercase().contains(&accel.to_lowercase())
        {
            eprintln!("warning: {hotkey} is already bound to GNOME's {key}; free it with:");
            eprintln!("  gsettings set org.gnome.desktop.wm.keybindings {key} \"[]\"");
        }
    }

    let mut list = parse_str_array(&gsettings(&["get", MEDIA_KEYS, "custom-keybindings"])?);
    if !list.iter().any(|p| p == BINDING_PATH) {
        list.push(BINDING_PATH.to_string());
        gsettings(&["set", MEDIA_KEYS, "custom-keybindings", &format_str_array(&list)])?;
    }
    let schema = format!("{MEDIA_KEYS}.custom-keybinding:{BINDING_PATH}");
    gsettings(&["set", &schema, "name", "'Kairos'"])?;
    gsettings(&["set", &schema, "command", &format!("'{command}'")])?;
    gsettings(&["set", &schema, "binding", &format!("'{accel}'")])?;
    println!("Installed GNOME shortcut {hotkey} -> {command}");
    println!("Make sure `kairos` is running (`kairos --install-autostart` starts it at sign-in).");
    Ok(())
}

pub fn uninstall() -> Result<(), String> {
    if !is_gnome() {
        println!("Nothing to undo: remove the launcher binding from your desktop's shortcut settings.");
        return Ok(());
    }
    let mut list = parse_str_array(&gsettings(&["get", MEDIA_KEYS, "custom-keybindings"])?);
    list.retain(|p| p != BINDING_PATH);
    gsettings(&["set", MEDIA_KEYS, "custom-keybindings", &format_str_array(&list)])?;
    let schema = format!("{MEDIA_KEYS}.custom-keybinding:{BINDING_PATH}");
    for key in ["name", "command", "binding"] {
        let _ = gsettings(&["reset", &schema, key]);
    }
    println!("Removed GNOME shortcut.");
    Ok(())
}

fn print_snippets(hotkey: &str, command: &str) {
    println!("Bind {hotkey} to `{command}` in your desktop environment:\n");
    println!("KDE Plasma : System Settings > Keyboard > Shortcuts > Add Command, command above");
    println!("sway/i3    : bindsym Mod1+space exec {command}");
    println!("Hyprland   : bind = ALT, space, exec, {command}");
    println!("\nAdapt the key to your hotkey. Start `kairos` at login so it is already running.");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accelerators() {
        assert_eq!(gnome_accelerator("alt+space").unwrap(), "<Alt>space");
        assert_eq!(gnome_accelerator("Ctrl+Shift+P").unwrap(), "<Control><Shift>p");
        assert_eq!(gnome_accelerator("super+space").unwrap(), "<Super>space");
        assert!(gnome_accelerator("hyper+x").is_err());
        assert!(gnome_accelerator("alt+").is_err());
    }

    #[test]
    fn gsettings_arrays() {
        assert!(parse_str_array("@as []").is_empty());
        let l = parse_str_array("['/a/', '/b/']");
        assert_eq!(l, ["/a/", "/b/"]);
        assert_eq!(format_str_array(&l), "['/a/', '/b/']");
        assert_eq!(format_str_array(&[]), "[]");
    }
}
