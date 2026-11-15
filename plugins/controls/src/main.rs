//! Controls plugin: media playback, volume, Wi-Fi, Bluetooth, dark mode, brightness and Do Not
//! Disturb (`pause`, `vol 40`, `wifi off`, `dnd 30m`). JSON-RPC 2.0 over stdio, one message per
//! line (see docs/develop/protocol.md).
//!
//! Plain searches match a fixed table of names first ([`commands`]); the system is only asked
//! about what matched, in the background, and the answers are kept for a moment so typing stays
//! fast (see [`Cache`]).

mod commands;
mod dnd;
mod media;
mod run;
mod toggles;
mod volume;
#[cfg(windows)]
mod win;

/// The timer plugin's parser, so `dnd 30m`, `dnd 1h30` and `dnd until 14:30` read like `timer`.
#[path = "../../timer/src/parse.rs"]
#[allow(dead_code)]
mod parse;

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use commands::{Cmd, Dnd, Group, Level, Match, MediaOp, Scope, Switch, Want};
use jiff::Zoned;
use media::Player;
use serde_json::{Value, json};
use volume::Volume;

/// How long an answer from the system is reused.
const FRESH: Duration = Duration::from_secs(2);
/// How long a query waits for the system before answering with what it has.
const WAIT: Duration = Duration::from_millis(100);
const DND_HINT: &str = "e.g. dnd 30m, dnd 1h30, dnd until 14:30";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Area {
    Media,
    Volume,
    Switch(Switch),
    Brightness,
    Dnd,
}

#[derive(Debug, Clone)]
enum State {
    Players(Vec<Player>),
    Volume(Volume),
    On(bool),
    Percent(u8),
    Dnd(Result<(dnd::Backend, dnd::Status), String>),
}

fn area(cmd: &Cmd) -> Area {
    match cmd {
        Cmd::Media(_) | Cmd::Players => Area::Media,
        Cmd::Volume(_) | Cmd::Mute => Area::Volume,
        Cmd::Brightness(_) => Area::Brightness,
        Cmd::Switch(s, _) => Area::Switch(*s),
        Cmd::Dnd(_) => Area::Dnd,
    }
}

/// Ask the system. `None`: this control does not exist here.
fn probe(a: Area, dir: &Path) -> Option<State> {
    match a {
        Area::Media => media::players().map(State::Players),
        Area::Volume => volume::get().map(State::Volume),
        Area::Switch(s) => toggles::get(s).map(State::On),
        Area::Brightness => toggles::brightness().map(State::Percent),
        Area::Dnd => Some(State::Dnd(dnd::detect().map(|b| {
            let saved = dnd::load(dir);
            (b, dnd::status(dnd::is_on(b), &saved, dnd::now()))
        }))),
    }
}

/// What the system said per area. Missing from a snapshot: not known yet (still asking).
type Snapshot = HashMap<Area, Option<State>>;

#[derive(Default)]
struct Known {
    states: HashMap<Area, (Instant, Option<State>)>,
    asking: HashSet<Area>,
}

/// Answers from the system, refreshed in background threads: a slow backend (PowerShell on
/// Windows, a D-Bus service starting) never holds up a keystroke for longer than [`WAIT`].
struct Cache {
    dir: PathBuf,
    known: Mutex<Known>,
    changed: Condvar,
}

impl Cache {
    fn new(dir: PathBuf) -> Arc<Self> {
        Arc::new(Self { dir, known: Mutex::new(Known::default()), changed: Condvar::new() })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Known> {
        self.known.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Refresh what is stale among `areas`, wait up to `wait` for it, and return what is known.
    fn get(self: &Arc<Self>, areas: &[Area], wait: Duration) -> Snapshot {
        let mut k = self.lock();
        for &a in areas {
            let stale = k.states.get(&a).is_none_or(|(at, _)| at.elapsed() > FRESH);
            if stale && k.asking.insert(a) {
                let me = Arc::clone(self);
                std::thread::spawn(move || {
                    let state = probe(a, &me.dir);
                    let mut k = me.lock();
                    k.states.insert(a, (Instant::now(), state));
                    k.asking.remove(&a);
                    me.changed.notify_all();
                });
            }
        }
        let deadline = Instant::now() + wait;
        while areas.iter().any(|a| k.asking.contains(a)) {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else { break };
            k = self.changed.wait_timeout(k, left).unwrap_or_else(PoisonError::into_inner).0;
        }
        areas.iter().filter_map(|a| Some((*a, k.states.get(a)?.1.clone()))).collect()
    }

    /// Ask again now, waiting for the answer (before acting on it).
    fn fresh(self: &Arc<Self>, a: Area) -> Option<State> {
        self.forget(a);
        self.get(&[a], Duration::from_secs(5)).remove(&a).flatten()
    }

    fn forget(&self, a: Area) {
        self.lock().states.remove(&a);
    }
}

struct Settings {
    /// Preferred player name (`spotify`); empty: the one playing.
    player: String,
    step: u8,
}

struct Plugin {
    settings: Settings,
    cache: Arc<Cache>,
}

fn icon(name: &str) -> Value {
    json!({ "path": format!("icons/{name}.svg") })
}

fn row(id: &str, title: &str, subtitle: &str, icon_name: &str, score: f64) -> Value {
    json!({ "id": id, "title": title, "subtitle": subtitle, "icon": icon(icon_name), "score": score })
}

/// A row that only explains something; Enter keeps the launcher open.
fn hint(id: &str, title: &str, subtitle: &str, icon_name: &str, score: f64) -> Value {
    row(&format!("hint:{id}"), title, subtitle, icon_name, score)
}

fn now_line(percent: u8, muted: bool) -> String {
    if muted { format!("Now {percent} % · muted") } else { format!("Now {percent} %") }
}

/// The level after `l`, from `cur`, in whole percent.
fn target(l: Level, cur: u8, step: u8) -> u8 {
    let cur = i16::from(cur);
    let next = match l {
        Level::Show => cur,
        Level::Set(n) => i16::from(n),
        Level::Up => cur + i16::from(step),
        Level::Down => cur - i16::from(step),
        Level::By(d) => cur + d,
    };
    u8::try_from(next.clamp(0, 100)).unwrap_or(0)
}

fn level_id(prefix: &str, l: Level) -> String {
    match l {
        Level::Show => format!("{prefix}:show"),
        Level::Set(n) => format!("{prefix}:set:{n}"),
        Level::Up => format!("{prefix}:up"),
        Level::Down => format!("{prefix}:down"),
        Level::By(d) => format!("{prefix}:by:{d}"),
    }
}

fn parse_level(s: &str) -> Option<Level> {
    Some(match s.split_once(':') {
        Some(("set", n)) => Level::Set(n.parse().ok()?),
        Some(("by", n)) => Level::By(n.parse().ok()?),
        None if s == "up" => Level::Up,
        None if s == "down" => Level::Down,
        _ => return None,
    })
}

/// `30m`, `1h30`, `until 14:30`, `for 2h`: the end of a Do Not Disturb limit.
fn dnd_until(text: &str, now: &Zoned) -> Option<parse::Spec> {
    let t = text.trim();
    let t = if let Some(rest) = t.strip_prefix("until ") {
        format!("at {rest}")
    } else {
        t.strip_prefix("for ").unwrap_or(t).to_string()
    };
    parse::parse(&t, 60, now).filter(|s| s.label.is_empty())
}

impl Plugin {
    fn new(dir: PathBuf) -> Self {
        Self { settings: Settings { player: String::new(), step: 5 }, cache: Cache::new(dir) }
    }

    fn configure(&mut self, params: &Value) {
        let s = &params["settings"];
        self.settings.player = s["player"].as_str().unwrap_or("").to_string();
        self.settings.step = s["volume_step"].as_u64().and_then(|n| u8::try_from(n.clamp(1, 50)).ok()).unwrap_or(5);
        if let Some(d) = params["data_dir"].as_str().filter(|d| !d.is_empty()) {
            self.cache = Cache::new(PathBuf::from(d));
        }
    }

    fn query(&self, text: &str, trigger: Option<&str>, limit: usize) -> Vec<Value> {
        let scope = match trigger.map(str::trim) {
            Some("media") => Scope::Group(Group::Media),
            Some("toggle") => Scope::Group(Group::System),
            _ => Scope::Global,
        };
        let matches = commands::parse(text, scope);
        if matches.is_empty() {
            if let Scope::Group(_) = scope {
                return vec![hint(
                    "none",
                    "No such control",
                    "media: play, next, vol 40, mute · toggle: wifi, bt, dark, dnd 30m, bright 60",
                    "sliders",
                    0.1,
                )];
            }
            return Vec::new();
        }
        let mut areas: Vec<Area> = matches.iter().map(|m| area(&m.cmd)).collect();
        areas.dedup();
        let snap = self.cache.get(&areas, WAIT);
        let now = Zoned::now();
        let mut rows = Vec::new();
        for (i, m) in matches.iter().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let score = match (scope, m.exact) {
                (Scope::Group(_), _) => 0.9 - i as f64 * 0.01,
                (Scope::Global, true) => 0.9,
                (Scope::Global, false) => 0.5,
            };
            self.rows(m, &snap, scope, score, &now, &mut rows);
        }
        // Several commands can explain the same thing ("No media player is running"): once.
        let mut seen = HashSet::new();
        rows.retain(|r| seen.insert(r["id"].as_str().unwrap_or_default().to_string()));
        rows.truncate(limit.max(1));
        rows
    }

    /// The rows for one match; none when its control does not exist here.
    #[allow(clippy::too_many_lines)]
    fn rows(&self, m: &Match, snap: &Snapshot, scope: Scope, score: f64, now: &Zoned, out: &mut Vec<Value>) {
        // `Some(None)`: asked, and this system has no such control.
        let state = snap.get(&area(&m.cmd));
        if matches!(state, Some(None)) {
            return;
        }
        let state = state.and_then(Option::as_ref);
        match &m.cmd {
            Cmd::Media(op) => {
                let players = match state {
                    Some(State::Players(v)) => Some(v.as_slice()),
                    _ => None,
                };
                let target = players.and_then(|v| media::pick(v, &self.settings.player));
                if players.is_some_and(<[Player]>::is_empty) {
                    if m.exact || matches!(scope, Scope::Group(_)) {
                        out.push(hint("media", "Nothing to control", "No media player is running", "play", score));
                    }
                    return;
                }
                let (title, icon_name) = match op {
                    MediaOp::PlayPause => match target {
                        Some(p) if p.playing => ("Pause", "pause"),
                        Some(_) => ("Play", "play"),
                        None => ("Play / pause", "play"),
                    },
                    MediaOp::Next => ("Next track", "next"),
                    MediaOp::Previous => ("Previous track", "previous"),
                    MediaOp::Stop => ("Stop playback", "stop"),
                };
                let subtitle = target.map_or_else(
                    || "Media".to_string(),
                    |p| match p.track() {
                        t if t.is_empty() => p.name.clone(),
                        t => format!("{} · {t}", p.name),
                    },
                );
                out.push(row(&format!("media:{}", op.id()), title, &subtitle, icon_name, score));
            }
            Cmd::Players => {
                let Some(State::Players(v)) = state else { return };
                if v.is_empty() {
                    out.push(hint("media", "No media player is running", "Start one and try again", "play", score));
                }
                for (i, p) in v.iter().enumerate() {
                    let track = p.track();
                    let title = if track.is_empty() { p.name.clone() } else { track };
                    let what = if p.playing { "Playing" } else { "Paused" };
                    let toggle = if p.playing { "Pause" } else { "Play" };
                    #[allow(clippy::cast_precision_loss)]
                    let mut r = row(
                        &format!("player:{}", p.id),
                        &title,
                        &format!("{} · {what}", p.name),
                        if p.playing { "pause" } else { "play" },
                        score - i as f64 * 0.001,
                    );
                    r["actions"] = json!([
                        { "id": "playpause", "title": toggle, "default": true },
                        { "id": "next", "title": "Next track" },
                        { "id": "previous", "title": "Previous track" },
                        { "id": "stop", "title": "Stop" },
                    ]);
                    out.push(r);
                }
            }
            Cmd::Volume(l) => {
                let v = match state {
                    Some(State::Volume(v)) => Some(*v),
                    _ => None,
                };
                let sub = v.map_or_else(|| "Volume".to_string(), |v| now_line(v.percent, v.muted));
                let step = self.settings.step;
                let mut level_row = |l: Level, score: f64| {
                    let (title, icon_name) = match l {
                        Level::Set(n) => (format!("Set volume to {n} %"), "volume"),
                        Level::Up | Level::Show => (format!("Volume up ({step} %)"), "volume"),
                        Level::Down => (format!("Volume down ({step} %)"), "volume-low"),
                        Level::By(d) if d > 0 => (format!("Volume up {d} %"), "volume"),
                        Level::By(d) => (format!("Volume down {} %", -d), "volume-low"),
                    };
                    out.push(row(&level_id("vol", l), &title, &sub, icon_name, score));
                };
                if *l == Level::Show {
                    level_row(Level::Up, score);
                    level_row(Level::Down, score - 0.001);
                    out.push(Self::mute_row(v, score - 0.002));
                } else {
                    level_row(*l, score);
                }
            }
            Cmd::Mute => {
                let v = match state {
                    Some(State::Volume(v)) => Some(*v),
                    _ => None,
                };
                out.push(Self::mute_row(v, score));
            }
            Cmd::Brightness(l) => {
                let cur = match state {
                    Some(State::Percent(p)) => Some(*p),
                    _ => None,
                };
                let sub = cur.map_or_else(|| "Screen brightness".to_string(), |p| format!("Now {p} %"));
                let step = self.settings.step;
                let mut level_row = |l: Level, score: f64| {
                    let title = match l {
                        Level::Set(n) => format!("Set brightness to {n} %"),
                        Level::Up | Level::Show => format!("Brighter ({step} %)"),
                        Level::Down => format!("Dimmer ({step} %)"),
                        Level::By(d) if d > 0 => format!("Brighter by {d} %"),
                        Level::By(d) => format!("Dimmer by {} %", -d),
                    };
                    out.push(row(&level_id("bright", l), &title, &sub, "brightness", score));
                };
                if *l == Level::Show {
                    level_row(Level::Up, score);
                    level_row(Level::Down, score - 0.001);
                } else {
                    level_row(*l, score);
                }
            }
            Cmd::Switch(s, want) => {
                let on = match state {
                    Some(State::On(b)) => Some(*b),
                    _ => None,
                };
                let thing = if *s == Switch::Dark { "dark mode" } else { s.name() };
                let turn_on = match want {
                    Want::On => Some(true),
                    Want::Off => Some(false),
                    Want::Toggle => on.map(|b| !b),
                };
                let title = match turn_on {
                    Some(true) => format!("Turn {thing} on"),
                    Some(false) => format!("Turn {thing} off"),
                    None => format!("Turn {thing} on or off"),
                };
                let sub = on.map_or_else(
                    || s.name().to_string(),
                    |b| format!("{} is {}", s.name(), if b { "on" } else { "off" }),
                );
                let want_id = match want {
                    Want::Toggle => "toggle",
                    Want::On => "on",
                    Want::Off => "off",
                };
                let icon_name = match s {
                    Switch::Wifi => "wifi",
                    Switch::Bluetooth => "bluetooth",
                    Switch::Dark => "dark",
                };
                out.push(row(&format!("sw:{}:{want_id}", s.id()), &title, &sub, icon_name, score));
            }
            Cmd::Dnd(d) => Self::dnd_rows(d, state, m.exact, scope, score, now, out),
        }
    }

    fn mute_row(v: Option<Volume>, score: f64) -> Value {
        let (title, icon_name) = match v {
            Some(v) if v.muted => ("Unmute", "volume"),
            Some(_) => ("Mute", "mute"),
            None => ("Mute / unmute", "mute"),
        };
        let sub = v.map_or_else(|| "Volume".to_string(), |v| now_line(v.percent, v.muted));
        row("mute", title, &sub, icon_name, score)
    }

    #[allow(clippy::too_many_arguments)]
    fn dnd_rows(
        d: &Dnd,
        state: Option<&State>,
        exact: bool,
        scope: Scope,
        score: f64,
        now: &Zoned,
        out: &mut Vec<Value>,
    ) {
        let st = match state {
            Some(State::Dnd(Err(why))) => {
                out.push(hint("dnd", "Do Not Disturb", why, "dnd", score));
                return;
            }
            Some(State::Dnd(Ok((b, st)))) => Some((*b, *st)),
            _ => None,
        };
        let status_line = |st: Option<(dnd::Backend, dnd::Status)>| match st {
            None => "Do Not Disturb".to_string(),
            Some((b, s)) => match (s.on, s.until) {
                (true, Some(u)) => format!("On until {} · {}", parse::clock_time(u, now), b.name()),
                (true, None) => format!("On · {}", b.name()),
                (false, _) => format!("Off · {}", b.name()),
            },
        };
        let sub = status_line(st);
        match d {
            Dnd::Toggle => {
                let on = st.map(|(_, s)| s.on);
                let title = match on {
                    Some(true) => "Turn Do Not Disturb off",
                    Some(false) => "Turn Do Not Disturb on",
                    None => "Do Not Disturb on or off",
                };
                out.push(row("dnd:toggle", title, &sub, "dnd", score));
                if on == Some(false) && (exact || matches!(scope, Scope::Group(_))) {
                    out.push(row("dnd:for:1h", "Do Not Disturb for 1 hour", &sub, "dnd", score - 0.01));
                }
            }
            Dnd::On => out.push(row("dnd:on", "Turn Do Not Disturb on", &sub, "dnd", score)),
            Dnd::Off => out.push(row("dnd:off", "Turn Do Not Disturb off", &sub, "dnd", score)),
            Dnd::For(text) => match dnd_until(text, now) {
                Some(spec) => {
                    let at = parse::clock_time(spec.due, now);
                    let (title, detail) = if spec.alarm {
                        (format!("Do Not Disturb until {at}"), format!("for {}", parse::left(spec.seconds)))
                    } else {
                        (format!("Do Not Disturb for {}", parse::length(spec.seconds)), format!("until {at}"))
                    };
                    let id = format!("dnd:for:{}", text.split_whitespace().collect::<Vec<_>>().join(" "));
                    out.push(row(&id, &title, &format!("{detail} · {sub}"), "dnd", score));
                }
                None if matches!(scope, Scope::Group(_)) => out.push(hint("dnd", "Not a time", DND_HINT, "dnd", score)),
                None => {}
            },
        }
    }

    fn execute(&self, result_id: &str, action: &str) -> Value {
        if result_id.starts_with("hint:") {
            return json!({ "ok": true, "hide": false });
        }
        match self.run(result_id, action) {
            Ok(()) => json!({ "ok": true }),
            Err(message) => json!({ "ok": false, "message": message }),
        }
    }

    fn run(&self, id: &str, action: &str) -> Result<(), String> {
        let (kind, rest) = id.split_once(':').unwrap_or((id, ""));
        let result = match kind {
            "media" => {
                let op = MediaOp::from_id(rest).ok_or("unknown media action")?;
                let Some(State::Players(v)) = self.cache.fresh(Area::Media) else {
                    return Err("media players cannot be reached".into());
                };
                let p = media::pick(&v, &self.settings.player).ok_or("no media player is running")?;
                media::control(&p.id, op)
            }
            "player" => media::control(rest, MediaOp::from_id(action).unwrap_or(MediaOp::PlayPause)),
            "vol" => {
                let Some(State::Volume(v)) = self.cache.fresh(Area::Volume) else {
                    return Err("the volume cannot be read".into());
                };
                let l = parse_level(rest).ok_or("unknown volume action")?;
                let to = target(l, v.percent, self.settings.step);
                // Turning it up (or to a level) unmutes, as volume keys do.
                if v.muted && to > 0 && l != Level::Down {
                    volume::set_muted(false)?;
                }
                volume::set(to)
            }
            "mute" => {
                let Some(State::Volume(v)) = self.cache.fresh(Area::Volume) else {
                    return Err("the volume cannot be read".into());
                };
                volume::set_muted(!v.muted)
            }
            "bright" => {
                let l = parse_level(rest).ok_or("unknown brightness action")?;
                let cur = match self.cache.fresh(Area::Brightness) {
                    Some(State::Percent(p)) => p,
                    _ if matches!(l, Level::Set(_)) => 0,
                    _ => return Err("the brightness cannot be read".into()),
                };
                toggles::set_brightness(target(l, cur, self.settings.step))
            }
            "sw" => {
                let (s, want) = rest.split_once(':').ok_or("bad switch")?;
                let s = Switch::from_id(s).ok_or("unknown switch")?;
                let on = match want {
                    "on" => true,
                    "off" => false,
                    _ => match self.cache.fresh(Area::Switch(s)) {
                        Some(State::On(b)) => !b,
                        _ => return Err(format!("{} cannot be read", s.name())),
                    },
                };
                toggles::set(s, on)
            }
            "dnd" => self.run_dnd(rest),
            _ => Err(format!("unknown result {id}")),
        };
        self.cache.forget(match kind {
            "media" | "player" => Area::Media,
            "vol" | "mute" => Area::Volume,
            "bright" => Area::Brightness,
            "sw" => rest.split(':').next().and_then(Switch::from_id).map_or(Area::Dnd, Area::Switch),
            _ => Area::Dnd,
        });
        result
    }

    fn run_dnd(&self, what: &str) -> Result<(), String> {
        let dir = &self.cache.dir;
        let (b, st) = match self.cache.fresh(Area::Dnd) {
            Some(State::Dnd(r)) => r?,
            _ => return Err("Do Not Disturb cannot be reached".into()),
        };
        match what {
            "toggle" => dnd::set(b, dir, !st.on, None),
            "on" => dnd::set(b, dir, true, None),
            "off" => dnd::set(b, dir, false, None),
            other => {
                let text = other.strip_prefix("for:").ok_or("unknown Do Not Disturb action")?;
                let spec = dnd_until(text, &Zoned::now()).ok_or_else(|| format!("not a time: {text}"))?;
                dnd::set(b, dir, true, Some(spec.due))
            }
        }
    }

    fn handle(&mut self, method: &str, params: &Value) -> Result<Value, String> {
        Ok(match method {
            "initialize" => {
                self.configure(params);
                json!({ "api": "0.1", "capabilities": { "streaming": false, "actions": true } })
            }
            "query" => {
                let limit = usize::try_from(params["limit"].as_u64().unwrap_or(8)).unwrap_or(8);
                let text = params["text"].as_str().unwrap_or("");
                json!({ "results": self.query(text, params["trigger"].as_str(), limit) })
            }
            "execute" => {
                self.execute(params["result_id"].as_str().unwrap_or(""), params["action_id"].as_str().unwrap_or(""))
            }
            "shutdown" => json!({}),
            _ => return Err("unknown method".into()),
        })
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--dnd-off") {
        if let (Some(dir), Some(until)) = (args.get(1), args.get(2).and_then(|u| u.parse().ok())) {
            dnd::sleeper(Path::new(dir), until);
        }
        return;
    }
    let mut plugin = Plugin::new(std::env::temp_dir().join("kairos-controls"));
    let mut out = std::io::stdout().lock();
    // stdin closing means: exit
    for line in std::io::stdin().lock().lines().map_while(Result::ok) {
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        let (method, id) = (msg["method"].as_str().unwrap_or(""), &msg["id"]);
        if id.is_null() {
            continue; // notifications ($/cancel): a query waits at most `WAIT`
        }
        let reply = match plugin.handle(method, &msg["params"]) {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(message) => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": message } }),
        };
        let _ = writeln!(out, "{reply}");
        let _ = out.flush();
        if method == "shutdown" {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plugin whose cache already holds `states`, so no system is asked.
    fn plugin(states: Vec<(Area, Option<State>)>) -> Plugin {
        let p = Plugin::new(std::env::temp_dir().join("kairos-controls-test"));
        {
            let mut k = p.cache.lock();
            for (a, s) in states {
                k.states.insert(a, (Instant::now(), s));
            }
        }
        p
    }

    fn titles(rows: &[Value]) -> Vec<&str> {
        rows.iter().map(|r| r["title"].as_str().unwrap()).collect()
    }

    fn player(name: &str, playing: bool) -> Player {
        Player {
            id: format!("org.mpris.MediaPlayer2.{name}"),
            name: name.into(),
            title: "Song".into(),
            artist: "Band".into(),
            playing,
        }
    }

    #[test]
    fn media_rows_show_the_track() {
        let p = plugin(vec![(Area::Media, Some(State::Players(vec![player("Spotify", true)])))]);
        let rows = p.query("pause", None, 8);
        assert_eq!(titles(&rows), ["Pause"]);
        assert_eq!(rows[0]["subtitle"], "Spotify · Song – Band");
        assert_eq!(rows[0]["id"], "media:playpause");
        let rows = p.query("", Some("media "), 8);
        assert!(titles(&rows).contains(&"Song – Band"), "players listed: {:?}", titles(&rows));
    }

    #[test]
    fn missing_controls_are_left_out() {
        let p = plugin(vec![(Area::Switch(Switch::Wifi), None), (Area::Brightness, None)]);
        assert!(p.query("wifi", None, 8).is_empty());
        assert!(p.query("bright 50", None, 8).is_empty());
        let p = plugin(vec![(Area::Media, Some(State::Players(vec![])))]);
        assert_eq!(titles(&p.query("next", None, 8)), ["Nothing to control"]);
        assert!(p.query("nex", None, 8).is_empty(), "only a whole name explains");
    }

    #[test]
    fn switch_and_volume_rows() {
        let p = plugin(vec![
            (Area::Switch(Switch::Wifi), Some(State::On(true))),
            (Area::Volume, Some(State::Volume(Volume { percent: 40, muted: false }))),
        ]);
        let rows = p.query("wifi", None, 8);
        assert_eq!(titles(&rows), ["Turn Wi-Fi off"]);
        assert_eq!(rows[0]["subtitle"], "Wi-Fi is on");
        assert_eq!(rows[0]["id"], "sw:wifi:toggle");
        assert_eq!(titles(&p.query("wifi on", None, 8)), ["Turn Wi-Fi on"]);
        let rows = p.query("vol 30", None, 8);
        assert_eq!(titles(&rows), ["Set volume to 30 %"]);
        assert_eq!(rows[0]["subtitle"], "Now 40 %");
        assert_eq!(titles(&p.query("vol", None, 8)), ["Volume up (5 %)", "Volume down (5 %)", "Mute"]);
    }

    #[test]
    fn dnd_rows_with_and_without_a_limit() {
        let st = dnd::Status { on: false, until: None };
        let p = plugin(vec![(Area::Dnd, Some(State::Dnd(Ok((dnd::Backend::Gnome, st)))))]);
        assert_eq!(titles(&p.query("dnd", None, 8)), ["Turn Do Not Disturb on", "Do Not Disturb for 1 hour"]);
        let rows = p.query("dnd 30m", None, 8);
        assert_eq!(titles(&rows), ["Do Not Disturb for 30 min"]);
        assert_eq!(rows[0]["id"], "dnd:for:30m");
        assert!(rows[0]["subtitle"].as_str().unwrap().starts_with("until "));
        assert_eq!(titles(&p.query("dnd until 23:59", None, 8)), ["Do Not Disturb until 23:59"]);
        assert!(p.query("dnd pizza", None, 8).is_empty());
        assert_eq!(titles(&p.query("dnd pizza", Some("toggle "), 8)), ["Not a time"]);
        let p = plugin(vec![(Area::Dnd, Some(State::Dnd(Err("no daemon".into()))))]);
        assert_eq!(p.query("dnd", None, 8)[0]["subtitle"], "no daemon");
    }

    #[test]
    fn levels_are_clamped() {
        assert_eq!(target(Level::Up, 98, 5), 100);
        assert_eq!(target(Level::Down, 3, 5), 0);
        assert_eq!(target(Level::By(-10), 40, 5), 30);
        assert_eq!(target(Level::Set(70), 40, 5), 70);
        for l in [Level::Up, Level::Down, Level::Set(7), Level::By(-3)] {
            assert_eq!(parse_level(level_id("vol", l).strip_prefix("vol:").unwrap()), Some(l));
        }
    }

    #[test]
    fn dnd_times() {
        let now: Zoned = "2026-11-15T10:00:00+01:00[Europe/Zurich]".parse().unwrap();
        assert_eq!(dnd_until("30m", &now).map(|s| s.seconds), Some(1800));
        assert_eq!(dnd_until("for 1h30", &now).map(|s| s.seconds), Some(5400));
        assert_eq!(dnd_until("until 14:30", &now).map(|s| (s.seconds, s.alarm)), Some((16200, true)));
        assert_eq!(dnd_until("30m tea", &now), None, "no label");
        assert_eq!(dnd_until("soon", &now), None);
    }

    #[test]
    fn hints_keep_the_launcher_open() {
        let p = plugin(vec![]);
        assert_eq!(p.execute("hint:media", "")["hide"], false);
        assert_eq!(p.execute("nope", "")["ok"], false);
    }
}
