//! Timer plugin: `timer 10m tea`, `timer 14:30 standup`; `timer ` lists the running ones.
//! JSON-RPC 2.0 over stdio, one message per line (see docs/develop/protocol.md).
//!
//! The launcher stops this process when it has been idle for a while, so a timer does not live
//! here: starting one runs this binary again as a detached sleeper (`--fire <id>`) that waits
//! until the timer is due, then notifies. `timers.json` (see `store`) is how both sides agree:
//! cancelling only removes the entry, and the sleeper finds it gone and exits quietly.

mod notify;
mod parse;
mod store;

use std::io::{BufRead, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

use jiff::Zoned;
use serde_json::{Value, json};
use store::{State, Store, Timer};

/// A timer this long past due whose sleeper never removed it is reported as missed (the sleeper
/// was gone: logout, reboot). Sleepers look at the clock at least every `CHECK_S`.
const GRACE_S: i64 = 90;
const CHECK_S: i64 = 30;
/// Missed timers are forgotten after a day.
const FORGET_S: i64 = 24 * 3600;
const ICON: &str = "◔";
const HINT: &str = "e.g. 10m tea, 1h30, 25, 14:30 standup, at 7pm";

struct Plugin {
    store: Store,
    /// Seconds a bare number means.
    unit: u64,
    notify: notify::Options,
}

impl Plugin {
    fn new(store: Store) -> Self {
        Self { store, unit: 60, notify: notify::Options { sound: true, sticky: true } }
    }

    fn configure(&mut self, settings: &Value) {
        self.unit = if settings["default_unit"].as_str() == Some("s") { 1 } else { 60 };
        self.notify.sound = settings["sound"].as_bool().unwrap_or(true);
        self.notify.sticky = settings["sticky"].as_bool().unwrap_or(true);
    }

    fn query(&self, text: &str, limit: usize, now: &Zoned) -> Vec<Value> {
        let text = text.trim();
        let state = self.store.load();
        let mut timers: Vec<&Timer> = state.timers.iter().collect();
        timers.sort_by_key(|t| t.due);
        let mut rows = Vec::new();
        if text.is_empty() {
            rows.extend(timers.iter().map(|t| timer_row(t, now, 0.9)));
            for r in &state.recent {
                if let Some(spec) = parse::parse(r, self.unit, now) {
                    rows.push(start_row(r, &spec, now, 0.4));
                }
            }
        } else {
            if let Some(spec) = parse::parse(text, self.unit, now) {
                rows.push(start_row(text, &spec, now, 1.0));
            }
            let needle = text.to_lowercase();
            rows.extend(
                timers.iter().filter(|t| t.label.to_lowercase().contains(&needle)).map(|t| timer_row(t, now, 0.8)),
            );
        }
        if rows.is_empty() {
            let title = if text.is_empty() { "No timers running" } else { "Not a time" };
            rows.push(
                json!({ "id": "hint", "title": title, "subtitle": HINT, "icon": { "glyph": ICON }, "score": 0.1 }),
            );
        }
        rows.truncate(limit.max(1));
        rows
    }

    fn execute(&self, result_id: &str, action: &str) -> Value {
        let now = Zoned::now();
        let done = if let Some(text) = result_id.strip_prefix("start:") {
            match parse::parse(text, self.unit, &now) {
                Some(spec) => self.start(&spec.label, spec.due, spec.seconds, spec.alarm, Some(text)),
                None => Err(format!("not a time: {text}")),
            }
        } else if let Some(id) = result_id.strip_prefix("t:") {
            self.change(id, action, now.timestamp().as_second())
        } else {
            return json!({ "ok": true, "hide": false }); // the hint row
        };
        match done {
            Ok(()) => json!({ "ok": true }),
            Err(message) => json!({ "ok": false, "message": message }),
        }
    }

    /// Cancel, dismiss, restart or extend timer `id`.
    fn change(&self, id: &str, action: &str, now: i64) -> Result<(), String> {
        let old = self
            .store
            .update(|s| s.timers.iter().position(|t| t.id == id).map(|i| s.timers.remove(i)))
            .map_err(|e| e.to_string())?
            .ok_or("this timer has already ended")?;
        match action {
            "cancel" | "dismiss" => Ok(()),
            "restart" => self.start(&old.label, now + secs(old.seconds), old.seconds, false, None),
            "add5" => {
                let due = old.due.max(now) + 300;
                self.start(&old.label, due, u64::try_from(due - now).unwrap_or(300), old.alarm, None)
            }
            _ => Err(format!("unknown action {action}")),
        }
    }

    /// Record a timer and start its sleeper; `typed` is remembered for the recent list.
    fn start(&self, label: &str, due: i64, seconds: u64, alarm: bool, typed: Option<&str>) -> Result<(), String> {
        let timer = Timer { id: store::new_id(), label: label.to_string(), due, seconds, alarm };
        let id = timer.id.clone();
        let now = store::now();
        self.store
            .update(|s| {
                s.timers.retain(|t| t.due > now - FORGET_S);
                s.timers.push(timer);
                if let Some(t) = typed {
                    s.remember(t);
                }
            })
            .map_err(|e| e.to_string())?;
        if let Err(e) = spawn_sleeper(&id, &self.notify) {
            let _ = self.store.update(|s| s.timers.retain(|t| t.id != id));
            return Err(format!("starting the timer: {e}"));
        }
        Ok(())
    }

    fn handle(&mut self, method: &str, params: &Value) -> Result<Value, String> {
        Ok(match method {
            "initialize" => {
                self.configure(&params["settings"]);
                json!({ "api": "0.1", "capabilities": { "streaming": false, "actions": true } })
            }
            "query" => {
                let limit = usize::try_from(params["limit"].as_u64().unwrap_or(8)).unwrap_or(8);
                json!({ "results": self.query(params["text"].as_str().unwrap_or(""), limit, &Zoned::now()) })
            }
            "execute" => {
                self.execute(params["result_id"].as_str().unwrap_or(""), params["action_id"].as_str().unwrap_or(""))
            }
            "shutdown" => json!({}),
            _ => return Err("unknown method".into()),
        })
    }
}

fn secs(s: u64) -> i64 {
    i64::try_from(s).unwrap_or(i64::MAX)
}

/// What a timer is called: its label, or `10 min timer` / `Alarm 14:30`.
fn name(t: &Timer, now: &Zoned) -> String {
    match (t.label.is_empty(), t.alarm) {
        (false, _) => t.label.clone(),
        (true, false) => format!("{} timer", parse::length(t.seconds)),
        (true, true) => format!("Alarm {}", parse::clock_time(t.due, now)),
    }
}

fn start_row(text: &str, spec: &parse::Spec, now: &Zoned, score: f64) -> Value {
    let at = parse::clock_time(spec.due, now);
    let (mut title, subtitle) = if spec.alarm {
        (format!("Set alarm for {at}"), format!("in {}", parse::left(spec.seconds)))
    } else {
        (format!("Start {} timer", parse::length(spec.seconds)), format!("ends at {at}"))
    };
    if !spec.label.is_empty() {
        title = format!("{title}: {}", spec.label);
    }
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    json!({
        // The typed text, lower-cased: the same row for frecency whenever it is typed again.
        "id": format!("start:{}", text.to_lowercase()),
        "title": title,
        "subtitle": subtitle,
        "icon": { "glyph": ICON },
        "score": score,
        "actions": [{ "id": "start", "title": if spec.alarm { "Set alarm" } else { "Start timer" }, "default": true }],
    })
}

fn timer_row(t: &Timer, now: &Zoned, score: f64) -> Value {
    let n = now.timestamp().as_second();
    let at = parse::clock_time(t.due, now);
    let restart = (!t.alarm).then(|| json!({ "id": "restart", "title": "Restart" }));
    let (title, subtitle, actions) = if t.due + GRACE_S < n {
        let mut a = vec![json!({ "id": "dismiss", "title": "Dismiss", "default": true })];
        a.extend(restart);
        (format!("Missed: {}", name(t, now)), format!("ended {at} while the timer was not running"), a)
    } else {
        let left = u64::try_from(t.due - n).unwrap_or(0);
        let subtitle = match (left, t.alarm) {
            (0, _) => "ending now".to_string(),
            (_, true) => format!("alarm {at} · in {}", parse::left(left)),
            (_, false) => format!("{} left · ends {at}", parse::left(left)),
        };
        let mut a = vec![json!({ "id": "cancel", "title": "Cancel", "default": true })];
        a.extend(restart);
        a.push(json!({ "id": "add5", "title": "+5 min" }));
        (name(t, now), subtitle, a)
    };
    json!({ "id": format!("t:{}", t.id), "title": title, "subtitle": subtitle, "icon": { "glyph": ICON }, "score": score, "actions": actions })
}

/// Run this binary again, detached, as the sleeper for timer `id`. It must outlive this process
/// (the launcher kills idle plugins), so it gets its own process group and no inherited pipes.
fn spawn_sleeper(id: &str, o: &notify::Options) -> std::io::Result<()> {
    let mut cmd = Command::new(std::env::current_exe()?);
    cmd.args(["--fire", id]);
    if !o.sound {
        cmd.arg("--no-sound");
    }
    if !o.sticky {
        cmd.arg("--no-sticky");
    }
    // Not the plugin folder: on Windows that would keep it from being removed.
    cmd.current_dir(std::env::temp_dir()).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0000_0008 | 0x0000_0200); // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP
    }
    let mut child = cmd.spawn()?;
    // Reaped here while this process lives; afterwards by whoever inherits it.
    std::thread::spawn(move || child.wait());
    Ok(())
}

/// The sleeper: wait for timer `id`, then notify unless it was cancelled meanwhile. Wakes every
/// `CHECK_S` to look at the wall clock, as a monotonic sleep does not count time suspended, and
/// to exit soon after a cancel.
fn fire(store: &Store, id: &str, o: &notify::Options) {
    loop {
        let Some(t) = store.load().timers.into_iter().find(|t| t.id == id) else { return };
        let left = t.due - store::now();
        if left <= 0 {
            break;
        }
        std::thread::sleep(Duration::from_secs(u64::try_from(left.min(CHECK_S)).unwrap_or(1)));
    }
    // Take it out under the lock: if it is gone, it was cancelled just now.
    let Ok(Some(t)) =
        store.update(|s: &mut State| s.timers.iter().position(|t| t.id == id).map(|i| s.timers.remove(i)))
    else {
        return;
    };
    let now = Zoned::now();
    let what = if t.alarm { format!("Alarm · {}", parse::clock_time(t.due, &now)) } else { parse::length(t.seconds) };
    let late = store::now() - t.due > GRACE_S;
    let late = if late { format!(" (ended {})", parse::clock_time(t.due, &now)) } else { String::new() };
    let (title, body) = if t.label.is_empty() {
        (if t.alarm { "Alarm".to_string() } else { "Timer done".to_string() }, format!("{what}{late}"))
    } else {
        (t.label.clone(), format!("{}{what}{late}", if t.alarm { "" } else { "Timer done · " }))
    };
    notify::show(&title, &body, o);
}

fn main() {
    let store = Store::new(&Store::default_dir());
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--fire") {
        let id = args.get(1).map_or("", String::as_str);
        let has = |f: &str| args.iter().any(|a| a == f);
        fire(&store, id, &notify::Options { sound: !has("--no-sound"), sticky: !has("--no-sticky") });
        return;
    }
    let mut plugin = Plugin::new(store);
    let mut out = std::io::stdout().lock();
    // stdin closing means: exit
    for line in std::io::stdin().lock().lines().map_while(Result::ok) {
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        let (method, id) = (msg["method"].as_str().unwrap_or(""), &msg["id"]);
        if id.is_null() {
            continue; // notifications ($/cancel): queries take microseconds
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

    fn plugin() -> (Plugin, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("kairos-timer-main-{}", store::new_id()));
        (Plugin::new(Store::new(&dir)), dir)
    }

    fn now() -> Zoned {
        "2026-10-25T10:00:00+01:00[Europe/Zurich]".parse().unwrap()
    }

    fn add(p: &Plugin, id: &str, label: &str, due_in: i64) {
        let due = now().timestamp().as_second() + due_in;
        p.store
            .update(|s| s.timers.push(Timer { id: id.into(), label: label.into(), due, seconds: 600, alarm: false }))
            .unwrap();
    }

    #[test]
    fn typed_time_offers_to_start() {
        let (p, _) = plugin();
        let rows = p.query("10m  Tea", 8, &now());
        assert_eq!(rows[0]["title"], "Start 10 min timer: Tea");
        assert_eq!(rows[0]["subtitle"], "ends at 10:10");
        assert_eq!(rows[0]["id"], "start:10m tea");
        let rows = p.query("14:30 standup", 8, &now());
        assert_eq!(rows[0]["title"], "Set alarm for 14:30: standup");
        assert_eq!(rows[0]["subtitle"], "in 4 h 30 min");
        assert_eq!(p.query("pizza", 8, &now())[0]["id"], "hint");
    }

    #[test]
    fn lists_running_missed_and_recent() {
        let (p, dir) = plugin();
        assert_eq!(p.query("", 8, &now())[0]["title"], "No timers running");
        add(&p, "b", "pasta", 3600);
        add(&p, "a", "tea", 462);
        add(&p, "c", "", -600);
        p.store.update(|s| s.remember("25 focus")).unwrap();
        let rows = p.query("", 8, &now());
        let titles: Vec<&str> = rows.iter().map(|r| r["title"].as_str().unwrap()).collect();
        assert_eq!(titles, ["Missed: 10 min timer", "tea", "pasta", "Start 25 min timer: focus"]);
        assert_eq!(rows[1]["subtitle"], "8 min left · ends 10:07");
        assert_eq!(rows[0]["actions"][0]["id"], "dismiss");
        assert_eq!(rows[1]["actions"][0]["id"], "cancel");
        assert_eq!(p.query("past", 8, &now())[0]["title"], "pasta", "found by label");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn cancel_removes_and_unknown_is_reported() {
        let (p, dir) = plugin();
        add(&p, "a", "tea", 600);
        assert_eq!(p.execute("t:a", "cancel")["ok"], true);
        assert_eq!(p.store.load().timers, vec![]);
        assert_eq!(p.execute("t:a", "cancel")["ok"], false, "already gone");
        assert_eq!(p.execute("hint", "")["hide"], false);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sleeper_skips_cancelled_timers() {
        let (p, dir) = plugin();
        // Not in the store: returns at once instead of sleeping or notifying.
        fire(&p.store, "nope", &p.notify);
        std::fs::remove_dir_all(dir).ok();
    }
}
