//! Media players: Linux MPRIS over D-Bus, Windows system media sessions (what the volume flyout
//! shows), macOS Music and Spotify through AppleScript.

use crate::commands::MediaOp;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Player {
    /// Backend address: MPRIS bus name, Windows app id, macOS application name.
    pub id: String,
    /// Shown name: `Spotify`, `Firefox`.
    pub name: String,
    pub title: String,
    pub artist: String,
    pub playing: bool,
}

impl Player {
    /// `Song – Artist`, or the player's name when it reports no track.
    pub fn track(&self) -> String {
        match (self.title.is_empty(), self.artist.is_empty()) {
            (true, _) => String::new(),
            (false, true) => self.title.clone(),
            (false, false) => format!("{} – {}", self.title, self.artist),
        }
    }
}

/// The player a plain `pause`/`next` acts on: the preferred one (setting `player`, matched in the
/// name or id) if running, else one that plays, else the first.
pub fn pick<'a>(players: &'a [Player], preferred: &str) -> Option<&'a Player> {
    let pref = preferred.trim().to_lowercase();
    let wanted = |p: &&Player| {
        !pref.is_empty() && (p.name.to_lowercase().contains(&pref) || p.id.to_lowercase().contains(&pref))
    };
    players.iter().find(wanted).or_else(|| players.iter().find(|p| p.playing)).or_else(|| players.first())
}

/// Playing ones first, then by name; `None` when there is no way to ask (no session bus).
pub fn players() -> Option<Vec<Player>> {
    let mut v = imp::players()?;
    v.sort_by(|a, b| b.playing.cmp(&a.playing).then_with(|| a.name.cmp(&b.name)));
    Some(v)
}

pub fn control(id: &str, op: MediaOp) -> Result<(), String> {
    imp::control(id, op)
}

/// `Spotify.exe` → `Spotify`, `Microsoft.ZuneMusic_8wekyb3d8bbwe!Microsoft.ZuneMusic` → `ZuneMusic`.
#[cfg_attr(not(windows), allow(dead_code))]
fn app_name(app_id: &str) -> String {
    let base = app_id.split('!').next_back().unwrap_or(app_id);
    let base = base.rsplit(['\\', '/']).next().unwrap_or(base);
    let base = base.strip_suffix(".exe").or_else(|| base.strip_suffix(".EXE")).unwrap_or(base);
    let base = base.split('_').next().unwrap_or(base);
    let name = base.rsplit('.').next().unwrap_or(base);
    let mut c = name.chars();
    c.next().map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
}

/// `org.mpris.MediaPlayer2.firefox.instance_1_42` → `Firefox` (used when `Identity` is missing).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn bus_name(bus: &str) -> String {
    let short = bus.strip_prefix("org.mpris.MediaPlayer2.").unwrap_or(bus);
    app_name(short.split('.').next().unwrap_or(short))
}

#[cfg(target_os = "linux")]
mod imp {
    use std::collections::HashMap;

    use zbus::blocking::{Connection, Proxy, fdo::DBusProxy, proxy::Builder};
    use zbus::proxy::CacheProperties;
    use zbus::zvariant::{OwnedValue, Value};

    use super::{MediaOp, Player, bus_name};

    const PREFIX: &str = "org.mpris.MediaPlayer2.";
    const PATH: &str = "/org/mpris/MediaPlayer2";
    const PLAYER: &str = "org.mpris.MediaPlayer2.Player";

    fn proxy<'a>(c: &Connection, dest: &'a str, iface: &'a str) -> zbus::Result<Proxy<'a>> {
        Builder::new(c).destination(dest)?.path(PATH)?.interface(iface)?.cache_properties(CacheProperties::No).build()
    }

    fn text(v: &Value) -> String {
        match v {
            Value::Str(s) => s.to_string(),
            Value::Array(a) => a
                .iter()
                .filter_map(|x| if let Value::Str(s) = x { Some(s.to_string()) } else { None })
                .collect::<Vec<_>>()
                .join(", "),
            Value::Value(inner) => text(inner),
            _ => String::new(),
        }
    }

    pub fn players() -> Option<Vec<Player>> {
        let c = Connection::session().ok()?;
        let names = DBusProxy::new(&c).ok()?.list_names().ok()?;
        let mut out = Vec::new();
        for n in &names {
            let bus = n.as_str();
            if !bus.starts_with(PREFIX) {
                continue;
            }
            let Ok(p) = proxy(&c, bus, PLAYER) else { continue };
            let status: String = p.get_property("PlaybackStatus").unwrap_or_default();
            let meta: HashMap<String, OwnedValue> = p.get_property("Metadata").unwrap_or_default();
            let field = |k: &str| meta.get(k).map(|v| text(v)).unwrap_or_default();
            let name = proxy(&c, bus, "org.mpris.MediaPlayer2")
                .and_then(|p| p.get_property::<String>("Identity"))
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| bus_name(bus));
            out.push(Player {
                id: bus.to_string(),
                name,
                title: field("xesam:title"),
                artist: field("xesam:artist"),
                playing: status == "Playing",
            });
        }
        Some(out)
    }

    pub fn control(id: &str, op: MediaOp) -> Result<(), String> {
        let method = match op {
            MediaOp::PlayPause => "PlayPause",
            MediaOp::Next => "Next",
            MediaOp::Previous => "Previous",
            MediaOp::Stop => "Stop",
        };
        let c = Connection::session().map_err(|e| format!("D-Bus: {e}"))?;
        let p = proxy(&c, id, PLAYER).map_err(|e| format!("{id}: {e}"))?;
        p.call_method(method, &()).map(|_| ()).map_err(|e| format!("{method}: {e}"))
    }
}

#[cfg(windows)]
mod imp {
    use windows::Media::Control::{
        GlobalSystemMediaTransportControlsSession as Session,
        GlobalSystemMediaTransportControlsSessionManager as Manager,
        GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
    };

    use super::{MediaOp, Player, app_name};

    fn sessions() -> windows::core::Result<Vec<Session>> {
        let m = Manager::RequestAsync()?.join()?;
        Ok(m.GetSessions()?.into_iter().collect())
    }

    pub fn players() -> Option<Vec<Player>> {
        let mut out = Vec::new();
        for s in sessions().ok()? {
            let id = s.SourceAppUserModelId().map(|h| h.to_string()).unwrap_or_default();
            let props = s.TryGetMediaPropertiesAsync().and_then(|a| a.join()).ok();
            let field = |f: fn(&_) -> windows::core::Result<windows::core::HSTRING>| {
                props.as_ref().and_then(|p| f(p).ok()).map(|h| h.to_string()).unwrap_or_default()
            };
            let playing = s.GetPlaybackInfo().and_then(|i| i.PlaybackStatus()).is_ok_and(|st| st == Status::Playing);
            out.push(Player {
                name: app_name(&id),
                title: field(|p| p.Title()),
                artist: field(|p| p.Artist()),
                playing,
                id,
            });
        }
        Some(out)
    }

    pub fn control(id: &str, op: MediaOp) -> Result<(), String> {
        let all = sessions().map_err(|e| format!("media sessions: {e}"))?;
        let s = all
            .into_iter()
            .find(|s| s.SourceAppUserModelId().is_ok_and(|h| h == id))
            .ok_or("that player has closed")?;
        let done = match op {
            MediaOp::PlayPause => s.TryTogglePlayPauseAsync(),
            MediaOp::Next => s.TrySkipNextAsync(),
            MediaOp::Previous => s.TrySkipPreviousAsync(),
            MediaOp::Stop => s.TryStopAsync(),
        }
        .and_then(|a| a.join())
        .map_err(|e| e.to_string())?;
        if done { Ok(()) } else { Err("the player did not accept it".into()) }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{MediaOp, Player};
    use crate::run;

    const APPS: [&str; 2] = ["Spotify", "Music"];

    fn tell(app: &str, script: &str) -> Option<String> {
        run::output(&["osascript", "-e", &format!("tell application \"{app}\" to {script}")])
    }

    pub fn players() -> Option<Vec<Player>> {
        let mut out = Vec::new();
        for app in APPS {
            let running = run::output(&["osascript", "-e", &format!("application \"{app}\" is running")]);
            if running.as_deref() != Some("true") {
                continue;
            }
            out.push(Player {
                id: app.to_string(),
                name: app.to_string(),
                title: tell(app, "name of current track").unwrap_or_default(),
                artist: tell(app, "artist of current track").unwrap_or_default(),
                playing: tell(app, "player state as string").as_deref() == Some("playing"),
            });
        }
        Some(out)
    }

    pub fn control(id: &str, op: MediaOp) -> Result<(), String> {
        let verb = match op {
            MediaOp::PlayPause => "playpause",
            MediaOp::Next => "next track",
            MediaOp::Previous => "previous track",
            MediaOp::Stop => "pause",
        };
        run::status(&["osascript", "-e", &format!("tell application \"{id}\" to {verb}")])
    }
}

#[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
mod imp {
    use super::{MediaOp, Player};

    pub fn players() -> Option<Vec<Player>> {
        None
    }

    pub fn control(_: &str, _: MediaOp) -> Result<(), String> {
        Err("not supported on this system".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(name: &str, playing: bool) -> Player {
        Player {
            id: format!("org.mpris.MediaPlayer2.{name}"),
            name: name.into(),
            title: String::new(),
            artist: String::new(),
            playing,
        }
    }

    #[test]
    fn picks_preferred_then_playing_then_first() {
        let v = [p("Firefox", false), p("Spotify", true), p("mpv", false)];
        assert_eq!(pick(&v, "").unwrap().name, "Spotify");
        assert_eq!(pick(&v, "mpv").unwrap().name, "mpv");
        assert_eq!(pick(&v, "vlc").unwrap().name, "Spotify", "preferred one not running");
        assert_eq!(pick(&[p("a", false), p("b", false)], "").unwrap().name, "a");
        assert!(pick(&[], "").is_none());
    }

    #[test]
    fn names() {
        assert_eq!(app_name("Spotify.exe"), "Spotify");
        assert_eq!(app_name("Microsoft.ZuneMusic_8wekyb3d8bbwe!Microsoft.ZuneMusic"), "ZuneMusic");
        assert_eq!(app_name("C:\\Program Files\\VLC\\vlc.exe"), "Vlc");
        assert_eq!(app_name("Chrome"), "Chrome");
        assert_eq!(bus_name("org.mpris.MediaPlayer2.firefox.instance_1_42"), "Firefox");
    }

    #[test]
    fn track_line() {
        let mut x = p("Spotify", true);
        assert_eq!(x.track(), "");
        x.title = "Song".into();
        assert_eq!(x.track(), "Song");
        x.artist = "Band".into();
        assert_eq!(x.track(), "Song – Band");
    }
}
