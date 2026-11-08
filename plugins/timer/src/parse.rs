//! What the user typed after `timer `: a duration (`10m`, `1h30`, `90 s`, `25`) or a clock time
//! (`14:30`, `7pm`, `at 7:15am`), then an optional label.

// Durations are small floats and whole seconds; the casts cannot lose anything that matters.
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

use jiff::{ToSpan, Zoned};

/// Longest timer accepted: a week.
const MAX_SECONDS: u64 = 7 * 24 * 3600;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    /// When it ends, Unix seconds.
    pub due: i64,
    /// How long it runs from `now`.
    pub seconds: u64,
    /// Set for a clock time (`14:30`), not a duration.
    pub alarm: bool,
    pub label: String,
}

/// `unit` is what a bare number means, in seconds (60 for minutes).
pub fn parse(text: &str, unit: u64, now: &Zoned) -> Option<Spec> {
    let words: Vec<&str> = text.split_whitespace().collect();
    let (first, rest) = words.split_first()?;
    let (alarm, seconds, used) = if first.eq_ignore_ascii_case("at") {
        let (secs, n) = clock(rest, true, now)?;
        (true, secs, n + 1)
    } else if let Some((secs, n)) = clock(&words, false, now) {
        (true, secs, n)
    } else {
        let skip = usize::from(first.eq_ignore_ascii_case("in"));
        let (secs, n) = duration(&words[skip..], unit)?;
        (false, secs, n + skip)
    };
    if seconds == 0 || seconds > MAX_SECONDS {
        return None;
    }
    let due = now.timestamp().as_second().checked_add(i64::try_from(seconds).ok()?)?;
    Some(Spec { due, seconds, alarm, label: words[used..].join(" ") })
}

/// A clock time at the start of `words` and the seconds until it next comes round, plus how many
/// words it took. Only words that are clearly a time count: `14:30`, `7pm`, `7 pm`, `7:15am`;
/// after `at` (`forced`) a bare hour does too.
fn clock(words: &[&str], forced: bool, now: &Zoned) -> Option<(u64, usize)> {
    let (first, rest) = words.split_first()?;
    let lower = first.to_ascii_lowercase();
    let (body, mut half, mut used) = match lower.strip_suffix("am").or_else(|| lower.strip_suffix("pm")) {
        Some(b) => (b.to_string(), Some(lower.ends_with("pm")), 1),
        None => (lower.clone(), None, 1),
    };
    if half.is_none()
        && let Some(next) = rest.first().map(|w| w.to_ascii_lowercase())
        && (next == "am" || next == "pm")
    {
        half = Some(next == "pm");
        used = 2;
    }
    let (h, m) = match body.split_once(':') {
        Some((h, m)) if m.len() == 2 => (h.parse::<i8>().ok()?, m.parse::<i8>().ok()?),
        None if half.is_some() || forced => (body.parse().ok()?, 0),
        _ => return None,
    };
    let hour = match half {
        Some(pm) if (1..=12).contains(&h) => h % 12 + if pm { 12 } else { 0 },
        Some(_) => return None,
        None => h,
    };
    if !(0..24).contains(&hour) || !(0..60).contains(&m) {
        return None;
    }
    let today = now.with().hour(hour).minute(m).second(0).subsec_nanosecond(0).build().ok()?;
    let at = if today > *now { today } else { today.checked_add(1.day()).ok()? };
    let secs = at.timestamp().as_second() - now.timestamp().as_second();
    Some((u64::try_from(secs).ok()?, used))
}

/// A duration at the start of `words`: `10m`, `1h30`, `1.5h`, `1h 30m`, `10 min`, or a bare
/// number in `unit` seconds. Returns the seconds and how many words it took.
fn duration(words: &[&str], unit: u64) -> Option<(u64, usize)> {
    let (total, explicit) = token(words.first()?)?;
    let mut total = total.unwrap_or(0.0);
    let mut used = 1;
    if !explicit {
        // A bare number: `10 min` names the unit in the next word.
        let n = token_number(words[0])?;
        match words.get(1).and_then(|w| unit_seconds(&w.to_ascii_lowercase())) {
            Some(u) => {
                total = n * u as f64;
                used = 2;
            }
            None => total = n * unit as f64,
        }
    }
    // `1h 30m`: more words with units add up.
    while let Some(w) = words.get(used) {
        match token(w) {
            Some((Some(s), true)) => {
                total += s;
                used += 1;
            }
            _ => break,
        }
    }
    let secs = total.round();
    (secs >= 1.0 && secs <= MAX_SECONDS as f64).then_some((secs as u64, used))
}

fn token_number(w: &str) -> Option<f64> {
    w.parse::<f64>().ok().filter(|n| n.is_finite() && *n > 0.0)
}

/// One word of a duration. `(Some(seconds), true)` when it carries units (`1h30`, `90s`);
/// `(None, false)` for a bare number; `None` when it is not a duration.
fn token(w: &str) -> Option<(Option<f64>, bool)> {
    let w = w.to_ascii_lowercase();
    if token_number(&w).is_some() {
        return Some((None, false));
    }
    let mut total = 0.0;
    let mut last_unit = None;
    let mut rest = w.as_str();
    while !rest.is_empty() {
        let n_end = rest.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(rest.len());
        if n_end == 0 {
            return None;
        }
        let n: f64 = rest[..n_end].parse().ok()?;
        rest = &rest[n_end..];
        let u_end = rest.find(|c: char| c.is_ascii_digit() || c == '.').unwrap_or(rest.len());
        let u = if u_end == 0 {
            // A trailing number: `1h30` is minutes, `2m30` seconds.
            match last_unit {
                Some(3600) => 60,
                Some(60) => 1,
                _ => return None,
            }
        } else {
            unit_seconds(&rest[..u_end])?
        };
        rest = &rest[u_end..];
        total += n * u as f64;
        last_unit = Some(u);
    }
    Some((Some(total), true))
}

fn unit_seconds(u: &str) -> Option<u64> {
    Some(match u {
        "h" | "hr" | "hrs" | "hour" | "hours" => 3600,
        "m" | "min" | "mins" | "minute" | "minutes" => 60,
        "s" | "sec" | "secs" | "second" | "seconds" => 1,
        _ => return None,
    })
}

/// A duration exactly: `1 h 30 min`, `10 min`, `1 min 30 s`, `45 s`.
pub fn length(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, secs % 3600 / 60, secs % 60);
    let parts: Vec<String> =
        [(h, "h"), (m, "min"), (s, "s")].iter().filter(|(n, _)| *n > 0).map(|(n, u)| format!("{n} {u}")).collect();
    if parts.is_empty() { "0 s".into() } else { parts.join(" ") }
}

/// Time left, roughly: `1 h 12 min`, `8 min` (rounded up), `42 s`.
pub fn left(secs: u64) -> String {
    if secs < 60 {
        return format!("{secs} s");
    }
    let mins = secs.div_ceil(60);
    if mins < 60 { format!("{mins} min") } else { length(mins * 60) }
}

/// When `due` is, in local time: `14:32` today, `Tue 14:32` within a week, `Oct 30 14:32` later.
pub fn clock_time(due: i64, now: &Zoned) -> String {
    let Ok(ts) = jiff::Timestamp::from_second(due) else { return String::new() };
    let at = ts.to_zoned(now.time_zone().clone());
    let days = (at.date() - now.date()).get_days();
    if days == 0 {
        at.strftime("%H:%M").to_string()
    } else if (0..7).contains(&days) {
        at.strftime("%a %H:%M").to_string()
    } else {
        at.strftime("%b %-d %H:%M").to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> Zoned {
        "2026-10-25T10:00:00+01:00[Europe/Zurich]".parse().unwrap()
    }

    fn p(text: &str) -> Option<(u64, bool, String)> {
        parse(text, 60, &now()).map(|s| (s.seconds, s.alarm, s.label))
    }

    #[test]
    fn durations() {
        assert_eq!(p("10m tea"), Some((600, false, "tea".into())));
        assert_eq!(p("1h30m"), Some((5400, false, String::new())));
        assert_eq!(p("1h30"), Some((5400, false, String::new())));
        assert_eq!(p("2m30 eggs"), Some((150, false, "eggs".into())));
        assert_eq!(p("1.5h"), Some((5400, false, String::new())));
        assert_eq!(p("1h 30m call mum"), Some((5400, false, "call mum".into())));
        assert_eq!(p("90s"), Some((90, false, String::new())));
        assert_eq!(p("10 min pasta"), Some((600, false, "pasta".into())));
        assert_eq!(p("in 5 minutes"), Some((300, false, String::new())));
        assert_eq!(p("25"), Some((1500, false, String::new())), "a bare number is minutes");
        assert_eq!(parse("25", 1, &now()).unwrap().seconds, 25, "or seconds, as set");
        assert_eq!(p("5 10"), Some((300, false, "10".into())), "only the first bare number");
    }

    #[test]
    fn clock_times() {
        assert_eq!(p("14:30 standup"), Some((4 * 3600 + 1800, true, "standup".into())));
        assert_eq!(p("9:00"), Some((23 * 3600, true, String::new())), "passed today: tomorrow");
        assert_eq!(p("7pm"), Some((9 * 3600, true, String::new())));
        assert_eq!(p("7 pm dinner"), Some((9 * 3600, true, "dinner".into())));
        assert_eq!(p("12am"), Some((14 * 3600, true, String::new())));
        assert_eq!(p("at 10:15am"), Some((900, true, String::new())));
        assert_eq!(p("at 11 review"), Some((3600, true, "review".into())), "a bare hour after `at`");
        assert_eq!(p("10:00"), Some((24 * 3600, true, String::new())), "now: tomorrow");
    }

    #[test]
    fn clock_across_dst_change() {
        // Clocks go back from 03:00 CEST to 02:00 CET that night: 23:00 to 04:00 is 6 real hours.
        let night: Zoned = "2026-10-24T23:00:00+02:00[Europe/Zurich]".parse().unwrap();
        let s = parse("4:00", 60, &night).unwrap();
        assert_eq!(s.seconds, 6 * 3600, "an extra hour in the night");
    }

    #[test]
    fn rejects() {
        for t in ["", "tea", "0", "0m", "-5", "25:00", "13pm", "8d", "h", "1x", "nan", "inf", "1000h"] {
            assert_eq!(p(t), None, "{t:?}");
        }
    }

    #[test]
    fn formatting() {
        assert_eq!(length(5400), "1 h 30 min");
        assert_eq!(length(600), "10 min");
        assert_eq!(length(90), "1 min 30 s");
        assert_eq!(left(462), "8 min");
        assert_eq!(left(42), "42 s");
        assert_eq!(left(3600 + 61), "1 h 2 min");
        let n = now();
        let ts = n.timestamp().as_second();
        assert_eq!(clock_time(ts + 600, &n), "10:10");
        assert_eq!(clock_time(ts + 86400, &n), "Mon 10:00");
        assert_eq!(clock_time(ts + 10 * 86400, &n), "Nov 4 10:00");
    }
}
