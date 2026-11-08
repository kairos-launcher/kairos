//! "Timer done": a desktop notification and a sound, with whatever the system has. Best effort:
//! nothing here fails loudly. Texts go to the tools as separate arguments (or environment
//! variables), never into a command string, so a label cannot run anything.

use std::process::{Command, Stdio};

pub struct Options {
    pub sound: bool,
    /// Stays on screen until dismissed.
    pub sticky: bool,
}

pub fn show(title: &str, body: &str, o: &Options) {
    #[cfg(windows)]
    windows(title, body, o);
    #[cfg(target_os = "macos")]
    macos(title, body, o);
    #[cfg(all(unix, not(target_os = "macos")))]
    linux(title, body, o);
}

/// Runs `cmd` quietly and waits; `true` if it ran and succeeded.
fn run(cmd: &mut Command) -> bool {
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd.status().is_ok_and(|s| s.success())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn linux(title: &str, body: &str, o: &Options) {
    let urgency = if o.sticky { "critical" } else { "normal" };
    let shown = run(Command::new("notify-send")
        .args(["-a", "Kairos", "-i", "alarm-symbolic", "-u", urgency, "--"])
        .args([title, body]))
        || run(Command::new("gdbus").args([
            "call",
            "--session",
            "--dest=org.freedesktop.Notifications",
            "--object-path=/org/freedesktop/Notifications",
            "--method=org.freedesktop.Notifications.Notify",
            "Kairos",
            "0",
            "alarm-symbolic",
            &gvariant_string(title),
            &gvariant_string(body),
            "[]",
            &format!("{{'urgency': <byte {}>}}", if o.sticky { 2 } else { 1 }),
            "-1",
        ]));
    if !shown {
        eprintln!("timer: no notification tool found (notify-send or gdbus)");
    }
    if o.sound {
        let file = "/usr/share/sounds/freedesktop/stereo/complete.oga";
        let _ = run(Command::new("canberra-gtk-play").args(["-i", "complete", "-d", "Kairos timer"]))
            || std::path::Path::new(file).exists()
                && (run(Command::new("paplay").arg(file)) || run(Command::new("pw-play").arg(file)));
    }
}

/// A `GVariant` string literal for `gdbus call`: `'it\'s'`.
#[cfg(all(unix, not(target_os = "macos")))]
fn gvariant_string(s: &str) -> String {
    let mut out = String::from("'");
    for c in s.chars() {
        match c {
            '\\' | '\'' => {
                out.push('\\');
                out.push(c);
            }
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

#[cfg(target_os = "macos")]
fn macos(title: &str, body: &str, o: &Options) {
    let _ = o.sticky; // macOS decides how long banners stay
    let script = if o.sound {
        "on run argv\ndisplay notification (item 2 of argv) with title (item 1 of argv) sound name \"Glass\"\nend run"
    } else {
        "on run argv\ndisplay notification (item 2 of argv) with title (item 1 of argv)\nend run"
    };
    let _ = run(Command::new("osascript").args(["-e", script, title, body]));
}

#[cfg(windows)]
fn windows(title: &str, body: &str, o: &Options) {
    // A toast from Windows PowerShell's app id (always registered, so no shortcut is needed).
    // The texts come in through the environment and are added as XML text nodes.
    const SCRIPT: &str = r"
$ErrorActionPreference = 'Stop'
[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] | Out-Null
[Windows.Data.Xml.Dom.XmlDocument, Windows.Data.Xml.Dom.XmlDocument, ContentType = WindowsRuntime] | Out-Null
$x = New-Object Windows.Data.Xml.Dom.XmlDocument
$x.LoadXml($env:KAIROS_TIMER_XML)
$t = $x.GetElementsByTagName('text')
$t.Item(0).AppendChild($x.CreateTextNode($env:KAIROS_TIMER_TITLE)) | Out-Null
$t.Item(1).AppendChild($x.CreateTextNode($env:KAIROS_TIMER_BODY)) | Out-Null
$app = '{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\WindowsPowerShell\v1.0\powershell.exe'
[Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier($app).Show([Windows.UI.Notifications.ToastNotification]::new($x))
";
    let duration = if o.sticky { " duration=\"long\"" } else { "" };
    let audio =
        if o.sound { "<audio src=\"ms-winsoundevent:Notification.Reminder\" />" } else { "<audio silent=\"true\" />" };
    let xml = format!(
        "<toast{duration}><visual><binding template=\"ToastGeneric\"><text/><text/></binding></visual>{audio}</toast>"
    );
    let ok = run(Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", SCRIPT])
        .env("KAIROS_TIMER_XML", xml)
        .env("KAIROS_TIMER_TITLE", title)
        .env("KAIROS_TIMER_BODY", body));
    if !ok {
        eprintln!("timer: showing the notification failed");
    }
}

#[cfg(test)]
mod tests {
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn gvariant_strings_are_escaped() {
        assert_eq!(super::gvariant_string(r"it's a \ test"), r"'it\'s a \\ test'");
        assert_eq!(super::gvariant_string("a\nb"), "'a b'");
    }
}
