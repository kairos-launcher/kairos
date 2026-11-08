//! Start the launcher (hidden) when the user signs in.

const NAME: &str = "kairos";

fn exe() -> Result<std::path::PathBuf, String> {
    std::env::current_exe().map_err(|e| e.to_string())
}

#[cfg(not(windows))]
fn desktop_file() -> Result<std::path::PathBuf, String> {
    let dir = dirs::config_dir().ok_or("no config directory")?.join("autostart");
    Ok(dir.join(format!("{NAME}.desktop")))
}

/// Quote for the `Exec=` key of a .desktop file.
#[cfg(not(windows))]
fn exec_quote(p: &std::path::Path) -> String {
    let s = p.display().to_string().replace('\\', "\\\\").replace('"', "\\\"").replace('`', "\\`").replace('$', "\\$");
    format!("\"{s}\"")
}

#[cfg(not(windows))]
fn desktop_contents(exe: &std::path::Path, icon: Option<&std::path::Path>) -> String {
    let icon = icon.map(|p| format!("Icon={}\n", p.display())).unwrap_or_default();
    format!(
        "[Desktop Entry]\nType=Application\nName=Kairos\nComment=Keyboard launcher\nExec={} --background\n{icon}Terminal=false\nX-GNOME-Autostart-enabled=true\n",
        exec_quote(exe)
    )
}

#[cfg(not(windows))]
pub fn install() -> Result<(), String> {
    let path = desktop_file()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let exe = exe()?;
    // `scripts/dist.sh` ships the icon next to the binary.
    let icon = exe.with_file_name("kairos.png");
    let contents = desktop_contents(&exe, icon.is_file().then_some(icon.as_path()));
    std::fs::write(&path, contents).map_err(|e| e.to_string())?;
    println!("Kairos will start at sign-in ({}).", path.display());
    Ok(())
}

#[cfg(not(windows))]
pub fn uninstall() -> Result<(), String> {
    match std::fs::remove_file(desktop_file()?) {
        Ok(()) => println!("Autostart removed."),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => println!("Autostart was not enabled."),
        Err(e) => return Err(e.to_string()),
    }
    Ok(())
}

#[cfg(windows)]
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

#[cfg(windows)]
fn reg(args: &[&str]) -> Result<std::process::Output, String> {
    std::process::Command::new("reg").args(args).output().map_err(|e| e.to_string())
}

#[cfg(windows)]
pub fn install() -> Result<(), String> {
    let value = format!("\"{}\" --background", exe()?.display());
    let out = reg(&["add", RUN_KEY, "/v", NAME, "/t", "REG_SZ", "/d", &value, "/f"])?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    println!("Kairos will start at sign-in.");
    Ok(())
}

#[cfg(windows)]
pub fn uninstall() -> Result<(), String> {
    let out = reg(&["delete", RUN_KEY, "/v", NAME, "/f"])?;
    println!("{}", if out.status.success() { "Autostart removed." } else { "Autostart was not enabled." });
    Ok(())
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;

    #[test]
    fn desktop_entry_quotes_the_path_and_starts_hidden() {
        let s = desktop_contents(std::path::Path::new("/opt/my apps/kairos"), None);
        assert!(s.contains("Exec=\"/opt/my apps/kairos\" --background\n"));
        assert!(!s.contains("Icon="));
    }

    #[test]
    fn desktop_entry_names_the_icon_when_shipped() {
        let s = desktop_contents(
            std::path::Path::new("/opt/kairos/kairos"),
            Some(std::path::Path::new("/opt/kairos/kairos.png")),
        );
        assert!(s.contains("\nIcon=/opt/kairos/kairos.png\n"));
    }
}
