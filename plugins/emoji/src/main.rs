//! Emoji plugin: find an emoji by its name or CLDR keywords and copy it.
//! JSON-RPC 2.0 over stdio, one message per line (see docs/develop/protocol.md).
//!
//! The table (`data/emoji.tsv`, built by `data/generate.py` from Unicode data) is compiled in.
//! Copying is done by the launcher (`kind: "copy"`), so `execute` is never needed.

// Scores mix small counts and floats on purpose (as in the launcher's own lints).
#![allow(clippy::cast_precision_loss)]

mod image;

use std::io::{BufRead, Write};

use base64::Engine as _;
use serde_json::{Value, json};

const TABLE: &str = include_str!("../data/emoji.tsv");
const TONES: [&str; 5] = ["light", "medium-light", "medium", "medium-dark", "dark"];

struct Emoji<'a> {
    /// The emoji itself.
    #[allow(clippy::struct_field_names)]
    emoji: &'a str,
    name: &'a str,
    group: &'a str,
    keywords: Vec<&'a str>,
    /// The five skin-tone variants, light to dark, if it has them.
    tones: Vec<&'a str>,
}

fn parse(table: &str) -> Vec<Emoji<'_>> {
    table
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
        .filter_map(|l| {
            let mut f = l.split('\t');
            let (emoji, name, group) = (f.next()?, f.next()?, f.next()?);
            let keywords = f.next().unwrap_or("").split('|').filter(|k| !k.is_empty()).collect();
            let tones: Vec<&str> = f.next().unwrap_or("").split(' ').filter(|t| !t.is_empty()).collect();
            Some(Emoji { emoji, name, group, keywords, tones: if tones.len() == 5 { tones } else { vec![] } })
        })
        .collect()
}

/// How well one typed word matches: a word of the name beats a keyword, a whole word beats a
/// prefix, a prefix beats text inside a word.
fn word_score(w: &str, e: &Emoji) -> Option<f64> {
    let name_words = || e.name.split([' ', '-', ':', ',']).map(str::to_lowercase);
    if name_words().any(|n| n == w) {
        return Some(1.0);
    }
    if name_words().any(|n| n.starts_with(w)) {
        return Some(0.9);
    }
    let kw = || e.keywords.iter().map(|k| k.to_lowercase());
    if kw().any(|k| k == w) {
        return Some(0.85);
    }
    if kw().any(|k| k.starts_with(w)) {
        return Some(0.75);
    }
    (w.len() >= 3 && e.name.to_lowercase().contains(w)).then_some(0.6)
}

/// Every typed word must match; the score is their average, with a small bonus for short names
/// so `heart` puts ❤️ before `heart with arrow`.
fn score(text: &str, e: &Emoji) -> Option<f64> {
    let words: Vec<String> = text.to_lowercase().split_whitespace().map(str::to_string).collect();
    if words.is_empty() {
        return None;
    }
    let mut total = 0.0;
    for w in &words {
        total += word_score(w, e)?;
    }
    let brevity = 0.05 / (1.0 + e.name.split(' ').count() as f64);
    Some((total / words.len() as f64 * 0.95 + brevity).min(1.0))
}

struct Plugin {
    table: Vec<Emoji<'static>>,
    /// Index into `TONES`, if a skin tone is set.
    tone: Option<usize>,
    /// The colour emoji font, if this system has one with images; rows then carry lazy image
    /// icons instead of the emoji as text.
    font: Option<image::EmojiFont>,
}

impl Plugin {
    fn new() -> Self {
        Self { table: parse(TABLE), tone: None, font: None }
    }

    fn configure(&mut self, settings: &Value) {
        let t = settings["skin_tone"].as_str().unwrap_or("");
        self.tone = TONES.iter().position(|x| *x == t);
    }

    /// The emoji to copy: with the chosen skin tone when it has one.
    fn shown<'a>(&self, e: &'a Emoji) -> &'a str {
        match self.tone {
            Some(i) if !e.tones.is_empty() => e.tones[i],
            _ => e.emoji,
        }
    }

    fn query(&self, text: &str, limit: usize) -> Vec<Value> {
        let text = text.trim();
        let mut hits: Vec<(f64, &Emoji)> = if text.is_empty() {
            // Nothing typed yet: the first ones (smileys); frecency lifts the ones you use.
            self.table.iter().take(limit).map(|e| (0.5, e)).collect()
        } else {
            self.table.iter().filter_map(|e| score(text, e).map(|s| (s, e))).collect()
        };
        // Stable: equal scores keep the Unicode order.
        hits.sort_by(|a, b| b.0.total_cmp(&a.0));
        hits.into_iter().take(limit.max(1)).map(|(s, e)| self.item(e, s)).collect()
    }

    fn item(&self, e: &Emoji, score: f64) -> Value {
        let emoji = self.shown(e);
        let codes: Vec<String> = emoji.chars().map(|c| format!("U+{:04X}", c as u32)).collect();
        let keywords: Vec<&str> = e.keywords.iter().take(6).copied().collect();
        let subtitle =
            if keywords.is_empty() { e.group.to_string() } else { format!("{} · {}", e.group, keywords.join(", ")) };
        json!({
            // The base emoji: the same row whichever skin tone is set, for frecency.
            "id": e.emoji,
            "title": e.name,
            "subtitle": subtitle,
            // An image keeps the launcher from searching its fonts for one that has the emoji.
            "icon": if self.font.is_some() { json!({ "id": emoji }) } else { json!({ "glyph": emoji }) },
            "score": score,
            "actions": [
                { "id": "copy", "title": "Copy emoji", "default": true, "kind": "copy", "value": emoji },
                { "id": "copy_name", "title": "Copy name", "kind": "copy", "value": e.name },
                { "id": "copy_code", "title": "Copy code points", "kind": "copy", "value": codes.join(" ") },
            ],
        })
    }

    fn handle(&mut self, method: &str, params: &Value) -> Result<Value, String> {
        Ok(match method {
            "initialize" => {
                self.configure(&params["settings"]);
                if self.font.is_none() {
                    self.font = image::EmojiFont::find();
                }
                json!({ "api": "0.1", "capabilities": { "streaming": false, "actions": true } })
            }
            "query" => {
                let limit = usize::try_from(params["limit"].as_u64().unwrap_or(8)).unwrap_or(8);
                json!({ "results": self.query(params["text"].as_str().unwrap_or(""), limit) })
            }
            "icon" => {
                let id = params["icon_id"].as_str().unwrap_or("");
                // Only emoji from the table, so this never renders arbitrary text.
                let known = self.table.iter().any(|e| e.emoji == id || e.tones.contains(&id));
                let size = params["size"].as_u64().map_or(image::ICON_PX, |s| s.clamp(16, 128) as u32);
                match self.font.as_ref().filter(|_| known).and_then(|f| f.png(id, size)) {
                    Some(png) => {
                        json!({ "base64": base64::engine::general_purpose::STANDARD.encode(png), "mime": "image/png" })
                    }
                    None => Value::Null,
                }
            }
            // Every action is a launcher-side copy.
            "execute" => json!({ "ok": false, "message": "unknown action" }),
            "shutdown" => json!({}),
            _ => return Err("unknown method".into()),
        })
    }
}

fn main() {
    let mut plugin = Plugin::new();
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

    fn first(p: &Plugin, text: &str) -> String {
        p.query(text, 8)[0]["actions"][0]["value"].as_str().unwrap().to_string()
    }

    #[test]
    fn table_parses_completely() {
        let t = parse(TABLE);
        assert!(t.len() > 1800, "{}", t.len());
        assert!(t.iter().all(|e| !e.emoji.is_empty() && !e.name.is_empty()));
        assert!(t.iter().filter(|e| !e.tones.is_empty()).count() > 250);
    }

    #[test]
    fn finds_by_name_and_keyword() {
        let p = Plugin::new();
        assert_eq!(first(&p, "thumbs up"), "👍");
        assert_eq!(first(&p, "thumbs"), "👍");
        assert_eq!(first(&p, "red heart"), "❤️");
        assert_eq!(first(&p, "tada"), "🎉", "a CLDR keyword");
        assert_eq!(p.query("zzzzqqq", 8).len(), 0);
        assert_eq!(p.query("", 5).len(), 5, "something to pick from at once");
    }

    #[test]
    fn skin_tone_setting_applies_where_possible() {
        let mut p = Plugin::new();
        p.configure(&json!({ "skin_tone": "medium-dark" }));
        let row = &p.query("thumbs up", 8)[0];
        assert_eq!(row["icon"]["glyph"], "👍🏾", "no font loaded in this test: the emoji as text");
        assert_eq!(row["id"], "👍", "same row for frecency");
        assert_eq!(row["actions"][0]["value"], "👍🏾");
        assert_eq!(row["actions"][2]["value"], "U+1F44D U+1F3FE");
        assert_eq!(first(&p, "grinning face"), "😀", "no tones: unchanged");
    }

    #[test]
    fn icons_are_images_when_the_font_has_them() {
        let mut p = Plugin::new();
        p.handle("initialize", &json!({ "settings": {} })).unwrap();
        let icon = p.handle("icon", &json!({ "icon_id": "🚀", "size": 64 })).unwrap();
        if p.font.is_none() {
            assert!(icon.is_null());
            return;
        }
        assert_eq!(p.query("rocket", 1)[0]["icon"]["id"], "🚀");
        assert_eq!(icon["mime"], "image/png");
        assert!(p.handle("icon", &json!({ "icon_id": "<svg>", "size": 64 })).unwrap().is_null(), "only table emoji");
    }

    #[test]
    fn protocol_round_trip() {
        let mut p = Plugin::new();
        assert_eq!(p.handle("initialize", &json!({ "settings": {} })).unwrap()["api"], "0.1");
        let r = p.handle("query", &json!({ "text": "rocket", "trigger": ":", "limit": 3 })).unwrap();
        assert_eq!(r["results"][0]["actions"][0]["kind"], "copy");
        assert!(p.handle("nope", &Value::Null).is_err());
    }
}
