//! Dev utilities plugin: values to copy, one trigger per tool.
//! JSON-RPC 2.0 over stdio, one message per line (see docs/develop/protocol.md).
//!
//! `uuid`, `genpw [length]`, `urlencode <text>`, `urldecode <text>`, `base64 encode|decode <text>`,
//! `hash sha256|sha512|sha1|md5 <text>`. Copying is done by the launcher (`kind: "copy"`), so
//! `execute` is never needed.

use std::io::{BufRead, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use sha2::Digest as _;

const LOWER: &[u8] = b"abcdefghijklmnopqrstuvwxyz";
const UPPER: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ";
const DIGITS: &[u8] = b"0123456789";
const SYMBOLS: &[u8] = b"!#$%&*+-.:;=?@^_~";
const PW_MIN: usize = 4;
const PW_MAX: usize = 256;

const BASE64_MODES: [(&str, &str); 2] = [("encode", "text to Base64"), ("decode", "Base64 to text")];
const HASH_TYPES: [(&str, &str); 4] = [
    ("sha256", "SHA-256"),
    ("sha512", "SHA-512"),
    ("sha1", "SHA-1 (legacy, e.g. git)"),
    ("md5", "MD5 (legacy checksums)"),
];

/// A result row whose Enter copies `value`.
fn copy_row(id: &str, title: &str, subtitle: &str, glyph: &str, value: &str) -> Value {
    json!({
        "id": id,
        "title": title,
        "subtitle": subtitle,
        "icon": { "glyph": glyph },
        "actions": [{ "id": "copy", "title": "Copy", "default": true, "kind": "copy", "value": value }],
    })
}

/// A row that only explains what to type.
fn hint_row(id: &str, title: &str, subtitle: &str, glyph: &str) -> Value {
    json!({ "id": id, "title": title, "subtitle": subtitle, "icon": { "glyph": glyph }, "actions": [] })
}

/// A row that fills the search box with `text` (Enter and Tab), for picking a mode or type.
fn choice_row(id: &str, title: &str, subtitle: &str, glyph: &str, text: &str) -> Value {
    json!({
        "id": id,
        "title": title,
        "subtitle": subtitle,
        "icon": { "glyph": glyph },
        "complete": text,
        "actions": [{ "id": "pick", "title": "Choose", "default": true, "kind": "set_query", "value": text }],
    })
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    // The OS generator does not fail on supported systems; all zeros would still be well-formed.
    let _ = getrandom::fill(&mut b);
    b
}

/// Random index below `n` without modulo bias.
fn random_below(n: usize) -> usize {
    let n = n as u64;
    let zone = u64::MAX - u64::MAX % n;
    loop {
        let r = getrandom::u64().unwrap_or(0);
        if r < zone {
            return usize::try_from(r % n).unwrap_or(0);
        }
    }
}

fn format_uuid(b: &[u8; 16]) -> String {
    let h = hex(b);
    format!("{}-{}-{}-{}-{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..])
}

fn uuid_v4(mut b: [u8; 16]) -> String {
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    format_uuid(&b)
}

/// RFC 9562 v7: 48-bit Unix milliseconds, then random bits; sorts by creation time.
fn uuid_v7(ms: u64, mut b: [u8; 16]) -> String {
    b[..6].copy_from_slice(&ms.to_be_bytes()[2..]);
    b[6] = (b[6] & 0x0f) | 0x70;
    b[8] = (b[8] & 0x3f) | 0x80;
    format_uuid(&b)
}

fn uuid_rows() -> Vec<Value> {
    let ms = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(0));
    let (v4, v7) = (uuid_v4(random_bytes()), uuid_v7(ms, random_bytes()));
    vec![
        copy_row("uuid-v4", &v4, "UUID v4 (random)", "U", &v4),
        copy_row("uuid-v7", &v7, "UUID v7 (time-ordered)", "U", &v7),
    ]
}

/// A password from `sets`, with at least one character of each set (so site rules are met).
fn password(len: usize, sets: &[&[u8]]) -> String {
    let all: Vec<u8> = sets.concat();
    let mut pw: Vec<u8> = sets.iter().map(|s| s[random_below(s.len())]).collect();
    while pw.len() < len {
        pw.push(all[random_below(all.len())]);
    }
    // Shuffle so the guaranteed characters are not always at the start.
    for i in (1..pw.len()).rev() {
        pw.swap(i, random_below(i + 1));
    }
    String::from_utf8(pw).unwrap_or_default()
}

fn genpw_rows(text: &str, default_len: usize) -> Vec<Value> {
    let len = if text.is_empty() {
        default_len
    } else {
        match text.parse::<usize>() {
            Ok(n) if (PW_MIN..=PW_MAX).contains(&n) => n,
            _ => {
                return vec![hint_row(
                    "genpw-hint",
                    &format!("Type a length from {PW_MIN} to {PW_MAX}"),
                    "e.g. genpw 32",
                    "*",
                )];
            }
        }
    };
    let strong = password(len, &[LOWER, UPPER, DIGITS, SYMBOLS]);
    let plain = password(len, &[LOWER, UPPER, DIGITS]);
    vec![
        copy_row("genpw-symbols", &strong, &format!("{len} characters with symbols"), "*", &strong),
        copy_row("genpw-alnum", &plain, &format!("{len} letters and digits"), "*", &plain),
    ]
}

/// Percent-encodes everything but the RFC 3986 unreserved characters.
fn url_encode(s: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// `None` for a broken `%XX` or bytes that are not UTF-8. `+` stays `+`.
fn url_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let h = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(h, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn urlencode_rows(text: &str) -> Vec<Value> {
    if text.is_empty() {
        return vec![hint_row("urlencode-hint", "Type text to percent-encode", "e.g. urlencode a b&c", "%")];
    }
    let enc = url_encode(text);
    vec![copy_row("urlencode", &enc, "URL-encoded", "%", &enc)]
}

fn urldecode_rows(text: &str) -> Vec<Value> {
    if text.is_empty() {
        return vec![hint_row("urldecode-hint", "Type text to decode", "e.g. urldecode a%20b%26c", "%")];
    }
    match url_decode(text) {
        Some(dec) => vec![copy_row("urldecode", &dec, "URL-decoded", "%", &dec)],
        None => {
            vec![hint_row("urldecode-invalid", "Not valid URL encoding", "a % must be followed by two hex digits", "%")]
        }
    }
}

/// Splits `word rest` and says which `choices` the first word means: exactly one (`Ok`), or the
/// rows to pick from (`Err`), keeping the rest of the text in what they fill in.
fn pick<'a>(
    trigger: &str,
    text: &'a str,
    choices: &[(&'a str, &str)],
    glyph: &str,
) -> Result<(&'a str, &'a str), Vec<Value>> {
    let (word, rest) = text.split_once(char::is_whitespace).map_or((text, ""), |(w, r)| (w, r.trim_start()));
    let word_lc = word.to_lowercase();
    if let Some((name, _)) = choices.iter().find(|(name, _)| *name == word_lc) {
        return Ok((name, rest));
    }
    // A prefix of a choice narrows the list; anything else is text to keep.
    let (matching, keep): (Vec<_>, &str) = if rest.is_empty() && choices.iter().any(|(n, _)| n.starts_with(&word_lc)) {
        (choices.iter().filter(|(n, _)| n.starts_with(&word_lc)).collect(), "")
    } else {
        (choices.iter().collect(), text)
    };
    Err(matching
        .into_iter()
        .map(|(name, desc)| {
            let fill = format!("{trigger}{name} {keep}");
            choice_row(&format!("{}-{name}", trigger.trim_end()), &format!("{trigger}{name}"), desc, glyph, &fill)
        })
        .collect())
}

fn base64_decode(s: &str) -> Option<String> {
    let s: String = s.split_whitespace().collect();
    let bytes = [STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD].iter().find_map(|e| e.decode(&s).ok())?;
    String::from_utf8(bytes).ok()
}

fn base64_rows(trigger: &str, text: &str) -> Vec<Value> {
    let (mode, rest) = match pick(trigger, text, &BASE64_MODES, "B") {
        Ok(p) => p,
        Err(rows) => return rows,
    };
    if rest.is_empty() {
        return vec![hint_row(&format!("base64-{mode}-hint"), &format!("Type text to {mode}"), "", "B")];
    }
    if mode == "encode" {
        let std = STANDARD.encode(rest);
        let url = URL_SAFE_NO_PAD.encode(rest);
        let mut row = copy_row("base64-encode", &std, "Base64", "B", &std);
        row["actions"]
            .as_array_mut()
            .expect("actions is an array")
            .push(json!({ "id": "copy_url", "title": "Copy URL-safe, no padding", "kind": "copy", "value": url }));
        vec![row]
    } else {
        match base64_decode(rest) {
            Some(dec) => vec![copy_row("base64-decode", &dec, "Decoded Base64", "B", &dec)],
            None => {
                vec![hint_row("base64-invalid", "Not valid Base64 text", "or it does not decode to UTF-8 text", "B")]
            }
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

fn digest(kind: &str, data: &[u8]) -> String {
    match kind {
        "sha512" => hex(&sha2::Sha512::digest(data)),
        "sha1" => hex(&sha1::Sha1::digest(data)),
        "md5" => hex(&md5::Md5::digest(data)),
        _ => hex(&sha2::Sha256::digest(data)),
    }
}

fn hash_rows(trigger: &str, text: &str) -> Vec<Value> {
    let (kind, rest) = match pick(trigger, text, &HASH_TYPES, "#") {
        Ok(p) => p,
        Err(rows) => return rows,
    };
    let label = HASH_TYPES.iter().find(|(n, _)| *n == kind).map_or(kind, |(_, d)| d);
    if rest.is_empty() {
        return vec![hint_row(&format!("hash-{kind}-hint"), "Type text to hash", label, "#")];
    }
    let d = digest(kind, rest.as_bytes());
    vec![copy_row(&format!("hash-{kind}"), &d, label, "#", &d)]
}

struct Plugin {
    password_length: usize,
}

impl Plugin {
    fn new() -> Self {
        Self { password_length: 20 }
    }

    fn configure(&mut self, settings: &Value) {
        let n = settings["password_length"].as_f64().unwrap_or(20.0);
        // Clamped to PW_MIN..=PW_MAX first, so the cast is exact.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let n = n.round().clamp(4.0, 256.0) as usize;
        self.password_length = n.clamp(PW_MIN, PW_MAX);
    }

    fn query(&self, trigger: &str, text: &str) -> Vec<Value> {
        let text = text.trim();
        match trigger {
            "uuid " => uuid_rows(),
            "genpw " => genpw_rows(text, self.password_length),
            "urlencode " => urlencode_rows(text),
            "urldecode " => urldecode_rows(text),
            "base64 " => base64_rows(trigger, text),
            "hash " => hash_rows(trigger, text),
            _ => vec![],
        }
    }

    fn handle(&mut self, method: &str, params: &Value) -> Result<Value, String> {
        Ok(match method {
            "initialize" => {
                self.configure(&params["settings"]);
                json!({ "api": "0.1", "capabilities": { "streaming": false, "actions": true } })
            }
            "query" => {
                let trigger = params["trigger"].as_str().unwrap_or("");
                json!({ "results": self.query(trigger, params["text"].as_str().unwrap_or("")) })
            }
            // Every action is a launcher-side copy or set_query.
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

    fn q(trigger: &str, text: &str) -> Vec<Value> {
        Plugin::new().query(trigger, text)
    }

    fn value(row: &Value) -> &str {
        row["actions"][0]["value"].as_str().unwrap()
    }

    #[test]
    fn uuids_are_well_formed() {
        let rows = q("uuid ", "");
        assert_eq!(rows.len(), 2);
        for (row, version) in rows.iter().zip(['4', '7']) {
            let u = value(row);
            assert_eq!(u, row["title"]);
            assert_eq!(u.len(), 36);
            assert_eq!(u.split('-').map(str::len).collect::<Vec<_>>(), [8, 4, 4, 4, 12]);
            assert_eq!(u.as_bytes()[14], version as u8);
            assert!("89ab".contains(u.as_bytes()[19] as char), "{u}");
        }
        assert_ne!(value(&q("uuid ", "")[0]), value(&rows[0]), "random");
    }

    #[test]
    fn uuid_v7_sorts_by_time() {
        let a = uuid_v7(1_700_000_000_000, [0xff; 16]);
        let b = uuid_v7(1_700_000_000_001, [0; 16]);
        assert!(a < b, "{a} {b}");
        assert!(a.starts_with("018bcfe5-6800-7"), "{a}");
    }

    #[test]
    fn passwords_have_the_length_and_every_character_kind() {
        let rows = q("genpw ", "32");
        let strong = value(&rows[0]);
        assert_eq!(strong.len(), 32);
        for set in [LOWER, UPPER, DIGITS, SYMBOLS] {
            assert!(strong.bytes().any(|b| set.contains(&b)), "{strong}");
        }
        assert!(value(&rows[1]).bytes().all(|b| b.is_ascii_alphanumeric()));
        assert_eq!(value(&q("genpw ", "")[0]).len(), 20, "default length");
        assert_eq!(q("genpw ", "3")[0]["id"], "genpw-hint");
        assert_eq!(q("genpw ", "abc")[0]["id"], "genpw-hint");

        let mut p = Plugin::new();
        p.configure(&json!({ "password_length": 12 }));
        assert_eq!(value(&p.query("genpw ", "")[0]).len(), 12);
    }

    #[test]
    fn url_encoding_round_trips() {
        assert_eq!(value(&q("urlencode ", "a b&c/ä")[0]), "a%20b%26c%2F%C3%A4");
        assert_eq!(value(&q("urldecode ", "a%20b%26c%2F%C3%A4")[0]), "a b&c/ä");
        assert_eq!(value(&q("urldecode ", "1+1")[0]), "1+1");
        assert_eq!(q("urldecode ", "50%")[0]["id"], "urldecode-invalid");
        assert_eq!(q("urldecode ", "%zz")[0]["id"], "urldecode-invalid");
        assert_eq!(q("urldecode ", "%ff")[0]["id"], "urldecode-invalid", "not UTF-8");
        assert_eq!(q("urlencode ", "")[0]["id"], "urlencode-hint");
    }

    #[test]
    fn base64_encodes_and_decodes() {
        let row = &q("base64 ", "encode hello?")[0];
        assert_eq!(value(row), "aGVsbG8/");
        assert_eq!(row["actions"][1]["value"], "aGVsbG8_");
        assert_eq!(value(&q("base64 ", "decode aGVsbG8=")[0]), "hello");
        assert_eq!(value(&q("base64 ", "decode aGVsbG8")[0]), "hello", "padding optional");
        assert_eq!(value(&q("base64 ", "DECODE aGVsbG8_")[0]), "hello?", "URL-safe, any case");
        assert_eq!(q("base64 ", "decode !!!")[0]["id"], "base64-invalid");
        assert_eq!(q("base64 ", "decode")[0]["id"], "base64-decode-hint");
    }

    #[test]
    fn modes_and_types_are_offered_until_chosen() {
        let ids = |rows: Vec<Value>| rows.iter().map(|r| r["id"].as_str().unwrap().to_string()).collect::<Vec<_>>();
        assert_eq!(ids(q("base64 ", "")), ["base64-encode", "base64-decode"]);
        assert_eq!(ids(q("base64 ", "de")), ["base64-decode"]);
        assert_eq!(ids(q("hash ", "sh")), ["hash-sha256", "hash-sha512", "hash-sha1"]);
        let rows = q("hash ", "hello world");
        assert_eq!(rows.len(), 4, "not a type: every type, keeping the text");
        assert_eq!(rows[0]["complete"], "hash sha256 hello world");
        assert_eq!(rows[0]["actions"][0]["kind"], "set_query");
        assert_eq!(q("base64 ", "")[0]["complete"], "base64 encode ");
    }

    #[test]
    fn hashes_match_known_vectors() {
        let h = |t: &str| value(&q("hash ", &format!("{t} abc"))[0]).to_string();
        assert_eq!(h("sha256"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(h("sha1"), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(h("md5"), "900150983cd24fb0d6963f7d28e17f72");
        assert!(h("sha512").starts_with("ddaf35a193617aba"));
        assert_eq!(q("hash ", "md5")[0]["id"], "hash-md5-hint");
    }

    #[test]
    fn protocol_round_trip() {
        let mut p = Plugin::new();
        assert_eq!(p.handle("initialize", &json!({ "settings": {} })).unwrap()["api"], "0.1");
        let r = p.handle("query", &json!({ "text": "", "trigger": "uuid ", "limit": 8 })).unwrap();
        assert_eq!(r["results"][0]["actions"][0]["kind"], "copy");
        assert!(p.handle("nope", &Value::Null).is_err());
    }
}
