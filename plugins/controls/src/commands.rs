//! What was typed: which control, and its argument (`vol 40`, `wifi off`, `dnd 30m`). Pure, so
//! it is tested without a desktop.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    /// `media `: players, playback, volume.
    Media,
    /// `toggle `: Wi-Fi, Bluetooth, dark mode, Do Not Disturb, brightness.
    System,
}

/// Where the text came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// A plain search: only clear matches, so other plugins' results are not crowded out.
    Global,
    /// After a trigger: every prefix counts, and an empty text lists the group.
    Group(Group),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaOp {
    PlayPause,
    Next,
    Previous,
    Stop,
}

impl MediaOp {
    pub const ALL: [Self; 4] = [Self::PlayPause, Self::Next, Self::Previous, Self::Stop];

    pub fn id(self) -> &'static str {
        match self {
            Self::PlayPause => "playpause",
            Self::Next => "next",
            Self::Previous => "previous",
            Self::Stop => "stop",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|o| o.id() == id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Switch {
    Wifi,
    Bluetooth,
    Dark,
}

impl Switch {
    pub fn id(self) -> &'static str {
        match self {
            Self::Wifi => "wifi",
            Self::Bluetooth => "bluetooth",
            Self::Dark => "dark",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        [Self::Wifi, Self::Bluetooth, Self::Dark].into_iter().find(|s| s.id() == id)
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Wifi => "Wi-Fi",
            Self::Bluetooth => "Bluetooth",
            Self::Dark => "Dark mode",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Want {
    Toggle,
    On,
    Off,
}

/// A volume or brightness argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Nothing typed: offer up, down (and mute).
    Show,
    Set(u8),
    /// One step (setting `volume_step`).
    Up,
    Down,
    /// `+10`, `-10`.
    By(i16),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dnd {
    Toggle,
    On,
    Off,
    /// A time limit as typed (`30m`, `until 14:30`), checked against the clock later.
    For(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cmd {
    Media(MediaOp),
    /// List the players with what they play.
    Players,
    Volume(Level),
    Mute,
    Brightness(Level),
    Switch(Switch, Want),
    Dnd(Dnd),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub cmd: Cmd,
    /// The whole name was typed (`wifi`, `vol 40`), not just its start (`wif`).
    pub exact: bool,
}

struct Alias {
    name: &'static str,
    group: Group,
    /// The command for what follows the name; `None` when that is not a valid argument.
    make: fn(&str) -> Option<Cmd>,
    /// Listed when the group's trigger is typed alone (one alias per command).
    listed: bool,
}

const fn alias(name: &'static str, group: Group, make: fn(&str) -> Option<Cmd>, listed: bool) -> Alias {
    Alias { name, group, make, listed }
}

fn bare(rest: &str, cmd: Cmd) -> Option<Cmd> {
    rest.is_empty().then_some(cmd)
}

fn want(rest: &str) -> Option<Want> {
    match rest {
        "" | "toggle" => Some(Want::Toggle),
        "on" | "enable" => Some(Want::On),
        "of" | "off" | "disable" => Some(Want::Off),
        _ => None,
    }
}

/// `40`, `40%`, `up`, `down`, `+10`, `-10`.
fn level(rest: &str) -> Option<Level> {
    match rest {
        "" => return Some(Level::Show),
        "up" | "+" | "louder" | "brighter" => return Some(Level::Up),
        "down" | "-" | "quieter" | "dimmer" => return Some(Level::Down),
        _ => {}
    }
    let num = rest.strip_suffix('%').unwrap_or(rest).trim();
    if let Some(n) = num.strip_prefix('+') {
        return n.parse::<i16>().ok().filter(|n| (1..=100).contains(n)).map(Level::By);
    }
    if let Some(n) = num.strip_prefix('-') {
        return n.parse::<i16>().ok().filter(|n| (1..=100).contains(n)).map(|n| Level::By(-n));
    }
    num.parse::<u8>().ok().filter(|n| *n <= 100).map(Level::Set)
}

fn dnd(rest: &str) -> Option<Cmd> {
    Some(Cmd::Dnd(match rest {
        "" | "toggle" => Dnd::Toggle,
        "on" => Dnd::On,
        "of" | "off" => Dnd::Off,
        t => Dnd::For(t.to_string()),
    }))
}

use Group::{Media, System};

const ALIASES: &[Alias] = &[
    alias("play", Media, |r| bare(r, Cmd::Media(MediaOp::PlayPause)), true),
    alias("pause", Media, |r| bare(r, Cmd::Media(MediaOp::PlayPause)), false),
    alias("resume", Media, |r| bare(r, Cmd::Media(MediaOp::PlayPause)), false),
    alias("next track", Media, |r| bare(r, Cmd::Media(MediaOp::Next)), true),
    alias("next", Media, |r| bare(r, Cmd::Media(MediaOp::Next)), false),
    alias("skip", Media, |r| bare(r, Cmd::Media(MediaOp::Next)), false),
    alias("previous track", Media, |r| bare(r, Cmd::Media(MediaOp::Previous)), true),
    alias("previous", Media, |r| bare(r, Cmd::Media(MediaOp::Previous)), false),
    alias("prev", Media, |r| bare(r, Cmd::Media(MediaOp::Previous)), false),
    alias("stop playback", Media, |r| bare(r, Cmd::Media(MediaOp::Stop)), true),
    alias("stop", Media, |r| bare(r, Cmd::Media(MediaOp::Stop)), false),
    alias("media", Media, |r| bare(r, Cmd::Players), false),
    alias("players", Media, |r| bare(r, Cmd::Players), true),
    alias("now playing", Media, |r| bare(r, Cmd::Players), false),
    alias("volume", Media, |r| level(r).map(Cmd::Volume), true),
    alias("vol", Media, |r| level(r).map(Cmd::Volume), false),
    alias("sound", Media, |r| level(r).map(Cmd::Volume), false),
    alias("louder", Media, |r| bare(r, Cmd::Volume(Level::Up)), false),
    alias("quieter", Media, |r| bare(r, Cmd::Volume(Level::Down)), false),
    alias("mute", Media, |r| bare(r, Cmd::Mute), true),
    alias("unmute", Media, |r| bare(r, Cmd::Mute), false),
    alias("wifi", System, |r| want(r).map(|w| Cmd::Switch(Switch::Wifi, w)), true),
    alias("wi-fi", System, |r| want(r).map(|w| Cmd::Switch(Switch::Wifi, w)), false),
    alias("wlan", System, |r| want(r).map(|w| Cmd::Switch(Switch::Wifi, w)), false),
    alias("wireless", System, |r| want(r).map(|w| Cmd::Switch(Switch::Wifi, w)), false),
    alias("bluetooth", System, |r| want(r).map(|w| Cmd::Switch(Switch::Bluetooth, w)), true),
    alias("bt", System, |r| want(r).map(|w| Cmd::Switch(Switch::Bluetooth, w)), false),
    alias("dark mode", System, |r| want(r).map(|w| Cmd::Switch(Switch::Dark, w)), true),
    alias("dark", System, |r| want(r).map(|w| Cmd::Switch(Switch::Dark, w)), false),
    alias("dark theme", System, |r| want(r).map(|w| Cmd::Switch(Switch::Dark, w)), false),
    alias("night mode", System, |r| want(r).map(|w| Cmd::Switch(Switch::Dark, w)), false),
    alias("light mode", System, |r| bare(r, Cmd::Switch(Switch::Dark, Want::Off)), false),
    alias("light theme", System, |r| bare(r, Cmd::Switch(Switch::Dark, Want::Off)), false),
    alias("do not disturb", System, dnd, true),
    alias("dnd", System, dnd, false),
    alias("focus mode", System, dnd, false),
    alias("brightness", System, |r| level(r).map(Cmd::Brightness), true),
    alias("bright", System, |r| level(r).map(Cmd::Brightness), false),
];

/// Shortest start of a name that a plain search matches (`blu` → Bluetooth); short names such as
/// `bt` and `dnd` match when typed in full.
const GLOBAL_MIN: usize = 3;

/// The commands `text` asks for, best first: exact names before mere starts of names.
pub fn parse(text: &str, scope: Scope) -> Vec<Match> {
    let t = text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    let group = match scope {
        Scope::Global => None,
        Scope::Group(g) => Some(g),
    };
    let aliases = || ALIASES.iter().filter(move |a| group.is_none_or(|g| a.group == g));
    let mut out: Vec<Match> = Vec::new();
    let mut push = |cmd: Cmd, exact: bool| match out.iter_mut().find(|m| m.cmd == cmd) {
        Some(m) => m.exact |= exact,
        None => out.push(Match { cmd, exact }),
    };
    if t.is_empty() {
        if group.is_some() {
            aliases().filter(|a| a.listed).filter_map(|a| (a.make)("")).for_each(|c| push(c, false));
        }
        return out;
    }
    let min = if group.is_some() { 1 } else { GLOBAL_MIN };
    for a in aliases() {
        let cmd = if t == a.name {
            (a.make)("").map(|c| (c, true))
        } else if let Some(rest) = t.strip_prefix(a.name).and_then(|r| r.strip_prefix(' ')) {
            (a.make)(rest).map(|c| (c, true))
        } else if t.len() >= min && a.name.starts_with(&t) {
            (a.make)("").map(|c| (c, false))
        } else {
            None
        };
        if let Some((c, exact)) = cmd {
            push(c, exact);
        }
    }
    // Exact matches first; otherwise table order (the stable sort keeps it).
    out.sort_by_key(|m| !m.exact);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmds(text: &str, scope: Scope) -> Vec<Cmd> {
        parse(text, scope).into_iter().map(|m| m.cmd).collect()
    }

    #[test]
    fn levels() {
        assert_eq!(cmds("vol 40", Scope::Global), [Cmd::Volume(Level::Set(40))]);
        assert_eq!(cmds("Volume  40%", Scope::Global), [Cmd::Volume(Level::Set(40))]);
        assert_eq!(cmds("vol up", Scope::Global), [Cmd::Volume(Level::Up)]);
        assert_eq!(cmds("vol -10", Scope::Global), [Cmd::Volume(Level::By(-10))]);
        assert_eq!(cmds("bright +5", Scope::Global), [Cmd::Brightness(Level::By(5))]);
        assert_eq!(cmds("vol 140", Scope::Global), [], "out of range");
        assert_eq!(cmds("vol loud", Scope::Global), []);
    }

    #[test]
    fn switches() {
        assert_eq!(cmds("wifi off", Scope::Global), [Cmd::Switch(Switch::Wifi, Want::Off)]);
        assert_eq!(cmds("wifi", Scope::Global), [Cmd::Switch(Switch::Wifi, Want::Toggle)]);
        assert_eq!(cmds("bt on", Scope::Global), [Cmd::Switch(Switch::Bluetooth, Want::On)]);
        assert_eq!(cmds("light mode", Scope::Global), [Cmd::Switch(Switch::Dark, Want::Off)]);
        assert_eq!(cmds("wifi maybe", Scope::Global), []);
    }

    #[test]
    fn media() {
        assert_eq!(cmds("next", Scope::Global), [Cmd::Media(MediaOp::Next)]);
        assert_eq!(cmds("pause", Scope::Global), [Cmd::Media(MediaOp::PlayPause)]);
        let m = parse("pau", Scope::Global);
        assert_eq!(m, [Match { cmd: Cmd::Media(MediaOp::PlayPause), exact: false }]);
        assert_eq!(cmds("media", Scope::Global), [Cmd::Players]);
    }

    #[test]
    fn dnd_takes_a_time() {
        assert_eq!(cmds("dnd", Scope::Global), [Cmd::Dnd(Dnd::Toggle)]);
        assert_eq!(cmds("dnd 30m", Scope::Global), [Cmd::Dnd(Dnd::For("30m".into()))]);
        assert_eq!(cmds("do not disturb until 14:30", Scope::Global), [Cmd::Dnd(Dnd::For("until 14:30".into()))]);
        assert_eq!(cmds("dnd off", Scope::Global), [Cmd::Dnd(Dnd::Off)]);
    }

    #[test]
    fn plain_searches_need_three_letters_or_a_whole_name() {
        assert_eq!(cmds("pa", Scope::Global), []);
        assert_eq!(cmds("bt", Scope::Global), [Cmd::Switch(Switch::Bluetooth, Want::Toggle)]);
        assert_eq!(cmds("blu", Scope::Global), [Cmd::Switch(Switch::Bluetooth, Want::Toggle)]);
        assert_eq!(cmds("firefox", Scope::Global), []);
    }

    #[test]
    fn exact_names_rank_first() {
        // `stop` is a whole name; `stop playback` would only be its start.
        let m = parse("stop", Scope::Global);
        assert_eq!(m, [Match { cmd: Cmd::Media(MediaOp::Stop), exact: true }]);
        let m = parse("vol", Scope::Global);
        assert_eq!(m[0], Match { cmd: Cmd::Volume(Level::Show), exact: true });
    }

    #[test]
    fn triggers_list_their_group() {
        let media = cmds("", Scope::Group(Group::Media));
        assert_eq!(
            media,
            [
                Cmd::Media(MediaOp::PlayPause),
                Cmd::Media(MediaOp::Next),
                Cmd::Media(MediaOp::Previous),
                Cmd::Media(MediaOp::Stop),
                Cmd::Players,
                Cmd::Volume(Level::Show),
                Cmd::Mute,
            ]
        );
        let sys = cmds("", Scope::Group(Group::System));
        assert_eq!(sys.len(), 5);
        assert_eq!(cmds("w", Scope::Group(Group::System)), [Cmd::Switch(Switch::Wifi, Want::Toggle)]);
        assert_eq!(cmds("wifi", Scope::Group(Group::Media)), [], "other group");
    }
}
