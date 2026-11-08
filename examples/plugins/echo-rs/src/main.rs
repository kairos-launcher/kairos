//! Reference launcher plugin in Rust: JSON-RPC 2.0, one message per line on stdio. Logs go to stderr.
use std::io::{BufRead, Write};

use serde_json::{Value, json};

fn main() {
    let stdout = std::io::stdout();
    let reply = |id: &Value, body: Value| {
        let mut out = stdout.lock();
        let _ = writeln!(out, "{}", json!({ "jsonrpc": "2.0", "id": id, "result": body }));
        let _ = out.flush();
    };

    // stdin closing means: exit
    for line in std::io::stdin().lock().lines().map_while(Result::ok) {
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        let (method, id, params) = (msg["method"].as_str().unwrap_or(""), &msg["id"], &msg["params"]);
        match method {
            "initialize" => reply(id, json!({ "api": "0.1", "capabilities": { "streaming": false, "actions": true } })),
            "query" => {
                let text = params["text"].as_str().unwrap_or("");
                let results = if text.is_empty() {
                    vec![]
                } else {
                    vec![json!({
                        "id": format!("echo:{text}"),
                        "title": text,
                        "subtitle": "Echo (Rust)",
                        "icon": { "glyph": "R" },
                        "score": 0.9,
                        "actions": [{ "id": "noop", "title": "Do nothing", "default": true }],
                    })]
                };
                reply(id, json!({ "results": results }));
            }
            "execute" => {
                eprintln!("executed {params}");
                reply(id, json!({ "ok": true }));
            }
            "shutdown" => {
                reply(id, json!({}));
                break;
            }
            _ if !id.is_null() => {
                let err =
                    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": "unknown method" } });
                println!("{err}");
            }
            _ => {}
        }
    }
}
