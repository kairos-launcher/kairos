//! Calculator plugin: arithmetic, percentages and unit conversion via `fend-core`.
//! Answers `= <expr>` always, and plain searches that look like math.
//! JSON-RPC 2.0 over stdio, one message per line (see docs/develop/protocol.md).

mod rates;

use std::io::{BufRead, Write};
use std::sync::Arc;
use std::time::{Duration, Instant};

use fend_core::{Context, Interrupt};
use serde_json::{Value, json};

/// Longest a single evaluation may run (guards against inputs like `9^9^9^9`).
const DEADLINE: Duration = Duration::from_millis(100);
const MAX_RESULT_LEN: usize = 120;
/// Constant on purpose: frecency keys on the result id, and one id per calculation
/// would grow `frecency.json` forever. `execute` re-evaluates the query instead.
const RESULT_ID: &str = "calc";

struct Deadline(Instant);

impl Interrupt for Deadline {
    fn should_interrupt(&self) -> bool {
        Instant::now() >= self.0
    }
}

#[derive(Default)]
struct Calc {
    /// Created on first use. Each evaluation runs on a clone, so assignments such as
    /// `x = 5` never leak into later keystrokes.
    base: Option<Context>,
    /// Exchange rates; `None` means currencies are reported as unavailable.
    rates: Option<rates::Source>,
}

impl Calc {
    /// Apply the settings from `initialize`. Only `currency` (default on) exists.
    fn configure(&mut self, settings: &Value) {
        let currency = settings["currency"].as_bool().unwrap_or(true);
        self.rates = currency.then(|| rates::Source::new(rates::default_cache_file(), Arc::new(rates::fetch_ecb)));
        self.base = None;
    }

    /// The result text, plus the exchange rates date when it converted money.
    fn evaluate(&mut self, expr: &str) -> Result<(String, Option<String>), String> {
        let rates = self.rates.clone();
        let mut ctx = self
            .base
            .get_or_insert_with(|| {
                let mut c = Context::new();
                if let Some(r) = rates {
                    c.set_exchange_rate_handler_v2(r);
                }
                c
            })
            .clone();
        let deadline = Deadline(Instant::now() + DEADLINE);
        let out = fend_core::evaluate_with_interrupt(expr, &mut ctx, &deadline);
        // Taken even on error, so a failed evaluation never marks the next one as money.
        let rates_date = self.rates.as_ref().and_then(rates::Source::take_used);
        let out = out?;
        let main = out.get_main_result().trim();
        if main.is_empty() || main.contains('\n') || main.len() > MAX_RESULT_LEN {
            return Err("no displayable result".into());
        }
        let main = if rates_date.is_some() { round_money(main) } else { main.to_string() };
        Ok((main, rates_date))
    }

    /// Results for one query; `prefixed` is true when the `=` trigger was used.
    fn query(&mut self, text: &str, prefixed: bool) -> Vec<Value> {
        let text = clean(text);
        if text.is_empty() || (!prefixed && !looks_like_math(text)) {
            return vec![];
        }
        let item = |title: &str, subtitle: String, score: f64, actions: Value| json!({ "id": RESULT_ID, "title": title, "subtitle": subtitle, "icon": { "glyph": "=" }, "score": score, "actions": actions });
        match self.evaluate(text) {
            // An echo (`5` -> `5`) is not an answer in a plain search.
            Ok((r, _)) if !prefixed && r == text => vec![],
            Ok((r, rates_date)) => vec![item(
                &r,
                format!("{text} ={}", rates_date.map(|d| format!(" · ECB rates {d}")).unwrap_or_default()),
                if prefixed { 1.0 } else { 0.95 },
                json!([
                    { "id": "copy", "title": "Copy result", "default": true },
                    { "id": "copy_full", "title": "Copy expression and result", "default": false },
                ]),
            )],
            // Only the very first conversion ever waits for a download; say so even in a
            // plain search, or currency would look unsupported.
            Err(e) if e.contains(rates::LOADING) => {
                vec![item(
                    "Loading exchange rates…",
                    "Type again in a moment".into(),
                    if prefixed { 1.0 } else { 0.95 },
                    json!([]),
                )]
            }
            Err(e) if e.contains(rates::UNAVAILABLE) => {
                let why = e.split(rates::UNAVAILABLE).nth(1).map(first_line).unwrap_or_default();
                vec![item(
                    "Exchange rates unavailable",
                    format!("{why} · check `proxy` in config.toml"),
                    if prefixed { 1.0 } else { 0.95 },
                    json!([]),
                )]
            }
            Err(e) if prefixed => vec![item("Invalid expression", first_line(&e), 1.0, json!([]))],
            Err(_) => vec![],
        }
    }

    /// The text to put on the clipboard. The launcher does the copying.
    fn execute(&mut self, result_id: &str, action_id: &str, query: &str) -> Result<String, String> {
        if result_id != RESULT_ID {
            return Err(format!("unknown result {result_id}"));
        }
        let expr = clean(query);
        let (result, _) = self.evaluate(expr)?;
        copy_text(action_id, expr, &result).ok_or_else(|| format!("unknown action {action_id}"))
    }

    /// One request -> its `result` (Ok) or error message (Err). `None` for notifications.
    fn handle(&mut self, method: &str, params: &Value) -> Option<Result<Value, String>> {
        let s = |k: &str| params[k].as_str().unwrap_or("");
        Some(Ok(match method {
            "initialize" => {
                self.configure(&params["settings"]);
                json!({ "api": "0.1", "capabilities": { "streaming": false, "actions": true } })
            }
            "query" => json!({ "results": self.query(s("text"), !params["trigger"].is_null()) }),
            "execute" => match self.execute(s("result_id"), s("action_id"), s("query")) {
                Ok(text) => json!({ "ok": true, "copy": text }),
                Err(e) => json!({ "ok": false, "message": e }),
            },
            "shutdown" => json!({}),
            _ => return Some(Err("unknown method".into())),
        }))
    }
}

/// Plain searches only count as math if they plausibly are, so typing app names stays quiet.
fn looks_like_math(s: &str) -> bool {
    let has_digit = s.chars().any(|c| c.is_ascii_digit());
    let has_op = s.chars().any(|c| "+-*/^%()".contains(c));
    let lower = s.to_lowercase();
    let has_conversion = [" to ", " in ", " as "].iter().any(|w| lower.contains(w));
    has_digit && (has_op || has_conversion)
}

/// Remove a leading `=` (if the caller did not strip the trigger) and surrounding space.
fn clean(query: &str) -> &str {
    query.trim().trim_start_matches('=').trim()
}

/// `approx. 82.3927363361 CHF` -> `82.39 CHF`. ECB reference rates are not exact anyway,
/// so `approx.` adds nothing. Below 1, four significant digits keep tiny amounts readable.
/// Anything not shaped like `[symbol]number[ unit]` is returned unchanged.
fn round_money(result: &str) -> String {
    let r = result.strip_prefix("approx. ").unwrap_or(result);
    let start = r.find(|c: char| c.is_ascii_digit() || c == '-').unwrap_or(r.len());
    let end = r[start..].find(' ').map_or(r.len(), |i| start + i);
    let (symbol, num, unit) = (&r[..start], &r[start..end], &r[end..]);
    let Ok(x) = num.parse::<f64>() else { return result.to_string() };
    if symbol.chars().any(|c| c.is_alphanumeric() || c.is_whitespace()) {
        return result.to_string();
    }
    let decimals = if x.abs() >= 1.0 || x == 0.0 {
        2
    } else {
        // 0.005712 -> 3 leading zeros after the point would need 5 decimals for 4 digits.
        usize::try_from(3 - x.abs().log10().floor() as i64).unwrap_or(2)
    };
    let mut n = format!("{x:.decimals$}");
    if decimals > 2 && n.contains('.') {
        n = n.trim_end_matches('0').trim_end_matches('.').to_string();
    }
    format!("{symbol}{n}{unit}")
}

fn first_line(e: &str) -> String {
    let line = e.lines().next().unwrap_or("").trim();
    line.chars().take(MAX_RESULT_LEN).collect()
}

fn copy_text(action_id: &str, expr: &str, result: &str) -> Option<String> {
    match action_id {
        "copy" => Some(result.to_string()),
        "copy_full" => Some(format!("{expr} = {result}")),
        _ => None,
    }
}

fn main() {
    let mut calc = Calc::default();
    let mut out = std::io::stdout().lock();
    // stdin closing means: exit
    for line in std::io::stdin().lock().lines().map_while(Result::ok) {
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        let (method, id) = (msg["method"].as_str().unwrap_or(""), &msg["id"]);
        if id.is_null() {
            continue; // notifications: nothing to do
        }
        let reply = match calc.handle(method, &msg["params"]) {
            Some(Ok(result)) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Some(Err(message)) => {
                json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": message } })
            }
            None => continue,
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

    fn plain(text: &str) -> Vec<Value> {
        Calc::default().query(text, false)
    }

    fn prefixed(text: &str) -> Vec<Value> {
        Calc::default().query(text, true)
    }

    fn title(items: &[Value]) -> &str {
        items[0]["title"].as_str().unwrap()
    }

    #[test]
    fn arithmetic_and_functions() {
        assert_eq!(title(&plain("2+2")), "4");
        assert_eq!(title(&plain("sqrt(16)")), "4");
        assert_eq!(title(&plain("(1+2)*3")), "9");
        assert_eq!(title(&prefixed("2^10")), "1024");
    }

    #[test]
    fn unit_conversion() {
        let r = plain("5 km to miles");
        assert_eq!(title(&r).strip_prefix("approx. ").map(|t| &t[..4]), Some("3.10"), "{}", title(&r));
        assert!(title(&r).ends_with("miles"), "{}", title(&r));
        assert_eq!(title(&plain("1 hour in minutes")), "60 minutes");
    }

    #[test]
    fn inexact_results_are_marked() {
        assert!(title(&plain("1/3")).starts_with("approx."), "{}", title(&plain("1/3")));
    }

    #[test]
    fn plain_search_ignores_non_math() {
        for s in ["firefox", "7-zip", "5", "2 3", "visual studio code", "", "   "] {
            assert!(plain(s).is_empty(), "{s:?} should not produce a calculator result");
        }
    }

    #[test]
    fn prefix_always_answers_and_reports_errors() {
        assert_eq!(title(&prefixed("  5 ")), "5");
        let e = prefixed("2 +* 3 )");
        assert_eq!(title(&e), "Invalid expression");
        assert!(e[0]["actions"].as_array().unwrap().is_empty() && !e[0]["subtitle"].as_str().unwrap().is_empty());
        assert!(prefixed("").is_empty());
    }

    #[test]
    fn no_state_leaks_between_queries() {
        let mut c = Calc::default();
        assert_eq!(title(&c.query("x = 5", true)), "5");
        assert_eq!(title(&c.query("x", true)), "Invalid expression");
    }

    #[test]
    fn runaway_expressions_hit_the_deadline() {
        let start = Instant::now();
        let r = prefixed("9^9^9^9");
        assert!(start.elapsed() < Duration::from_secs(2), "took {:?}", start.elapsed());
        assert!(r.is_empty() || title(&r) == "Invalid expression" || title(&r).contains('e'), "{}", title(&r));
    }

    #[test]
    fn result_shape_and_constant_id() {
        let (a, b) = (plain("2+2"), plain("3+3"));
        assert_eq!(a[0]["id"], b[0]["id"], "frecency must not grow per expression");
        assert_eq!(a[0]["subtitle"], "2+2 =");
        assert_eq!(a[0]["actions"][0]["id"], "copy");
        assert_eq!(a[0]["actions"][0]["default"], true);
        assert!(plain("2+2")[0]["score"].as_f64() < prefixed("2+2")[0]["score"].as_f64());
    }

    #[test]
    fn copy_text_per_action() {
        assert_eq!(copy_text("copy", "2+2", "4").as_deref(), Some("4"));
        assert_eq!(copy_text("copy_full", "2+2", "4").as_deref(), Some("2+2 = 4"));
        assert_eq!(copy_text("nope", "2+2", "4"), None);
    }

    #[test]
    fn execute_replies_with_text_to_copy_or_an_error() {
        let mut c = Calc::default();
        let ok = c.handle("execute", &json!({ "result_id": "calc", "action_id": "copy_full", "query": "6*7" }));
        assert_eq!(ok.unwrap().unwrap(), json!({ "ok": true, "copy": "6*7 = 42" }));
        for (r, a, q) in [("other", "copy", "2+2"), ("calc", "bogus", "2+2"), ("calc", "copy", "2 +* )")] {
            let v = c.handle("execute", &json!({ "result_id": r, "action_id": a, "query": q })).unwrap().unwrap();
            assert_eq!(v["ok"], false, "{r} {a} {q}");
        }
    }

    #[test]
    fn query_uses_trigger_to_decide_prefixed() {
        let mut c = Calc::default();
        let plain = c.handle("query", &json!({ "qid": 1, "text": "5", "trigger": null, "limit": 8 })).unwrap().unwrap();
        assert!(plain["results"].as_array().unwrap().is_empty());
        let pre = c.handle("query", &json!({ "qid": 2, "text": "5", "trigger": "=", "limit": 8 })).unwrap().unwrap();
        assert_eq!(pre["results"][0]["title"], "5");
        assert!(c.handle("bogus", &json!({})).unwrap().is_err());
    }

    fn with_rates() -> Calc {
        Calc { rates: Some(rates::loaded(rates::FIXTURE)), ..Calc::default() }
    }

    #[test]
    fn currency_conversion_uses_ecb_rates() {
        let mut c = with_rates();
        let r = c.query("100 usd to chf", false);
        // 100 USD = 100/1.1 EUR = 100/1.1*0.935 CHF = 85
        assert_eq!(title(&r), "85.00 CHF");
        assert_eq!(r[0]["subtitle"], "100 usd to chf = · ECB rates 2026-10-20");
        let r = c.query("1 eur in jpy", true);
        assert_eq!(title(&r), "160.00 JPY");
        assert_eq!(title(&c.query("10 € to £", true)), "£8.50");
        assert_eq!(title(&c.query("1 jpy to eur", true)), "0.00625 EUR");
        // Plain math right after keeps the plain subtitle.
        assert_eq!(c.query("2+2", false)[0]["subtitle"], "2+2 =");
    }

    #[test]
    fn currency_without_a_rate_or_disabled_is_not_answered() {
        let mut c = with_rates();
        assert!(c.query("5 sek to eur", false).is_empty());
        assert_eq!(title(&c.query("5 sek to eur", true)), "Invalid expression");
        let mut off = Calc::default();
        off.configure(&json!({ "currency": false }));
        assert!(off.rates.is_none());
        assert!(off.query("5 usd to eur", false).is_empty());
        assert_eq!(title(&off.query("2*3", false)), "6");
    }

    #[test]
    fn currency_while_loading_says_so() {
        let mut c =
            Calc { rates: Some(rates::Source::new(None, Arc::new(|| Err("offline".into())))), ..Calc::default() };
        assert_eq!(title(&c.query("5 usd to eur", false)), "Loading exchange rates…");
        assert!(c.query("2+2", false)[0]["subtitle"] == "2+2 =");
    }

    #[test]
    fn currency_after_a_failed_download_says_why() {
        let mut c = Calc {
            rates: Some(rates::Source::new(None, Arc::new(|| Err("proxy refused the connection".into())))),
            ..Calc::default()
        };
        let mut rows = c.query("5 usd to eur", false);
        for _ in 0..200 {
            if title(&rows) != "Loading exchange rates…" {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            rows = c.query("5 usd to eur", false);
        }
        assert_eq!(title(&rows), "Exchange rates unavailable");
        let sub = rows[0]["subtitle"].as_str().unwrap();
        assert!(sub.contains("proxy refused the connection"), "{sub}");
    }

    #[test]
    fn money_is_rounded() {
        assert_eq!(round_money("approx. 82.3927363361 CHF"), "82.39 CHF");
        assert_eq!(round_money("£8.5"), "£8.50");
        assert_eq!(round_money("160 JPY"), "160.00 JPY");
        assert_eq!(round_money("approx. 0.0057123 EUR"), "0.005712 EUR");
        assert_eq!(round_money("-3.14159 USD"), "-3.14 USD");
        assert_eq!(round_money("weird output"), "weird output");
    }

    #[test]
    fn clean_strips_prefix() {
        assert_eq!(clean("= 2+2 "), "2+2");
        assert_eq!(clean("2+2"), "2+2");
    }
}
