//! Pasted configuration from other apps, converted into a setting's own shape. A table setting
//! opts in with `import = "<format>"` in its manifest (see `Setting::import_entries`).

use serde_json::{Map, Value};

/// Convert `raw` (pasted text) in format `format` into entries of the setting's table.
pub fn entries(format: &str, raw: &str) -> Result<toml::Table, String> {
    match format {
        "mcp" => mcp_servers(raw),
        _ => Err(format!("this launcher cannot import {format:?}")),
    }
}

/// MCP server JSON as Claude (`.mcp.json`, `claude_desktop_config.json`) or VS Code writes it,
/// as the AI plugin's `mcp_servers`: `command` + `args` become one `command` list; `env`, `url`
/// and `headers` stay; other keys are dropped.
fn mcp_servers(raw: &str) -> Result<toml::Table, String> {
    let root: Value = serde_json::from_str(raw.trim()).map_err(|e| format!("not valid JSON: {e}"))?;
    let root = root.as_object().ok_or("expected a JSON object of servers")?;
    let servers = ["mcpServers", "servers"].iter().find_map(|k| root.get(*k)?.as_object()).unwrap_or(root);
    if servers.is_empty() {
        return Err("no servers in it".into());
    }
    servers.iter().map(|(name, s)| Ok((name.clone(), mcp_server(s).map_err(|e| format!("{name}: {e}"))?))).collect()
}

fn mcp_server(v: &Value) -> Result<toml::Value, String> {
    let s = v.as_object().ok_or("expected an object")?;
    match s.get("type").and_then(Value::as_str) {
        None | Some("stdio" | "http" | "streamable-http" | "streamableHttp") => {}
        Some("sse") => return Err("SSE servers are not supported; use its Streamable HTTP url".into()),
        Some(t) => return Err(format!("unknown type {t:?}")),
    }
    let mut out = toml::Table::new();
    let mut command = match s.get("command") {
        None => vec![],
        Some(Value::String(c)) => vec![c.clone()],
        Some(Value::Array(a)) => strings(a, "command")?,
        Some(_) => return Err("command must be a string".into()),
    };
    match s.get("args") {
        None => {}
        Some(Value::Array(a)) => command.extend(strings(a, "args")?),
        Some(_) => return Err("args must be a list of strings".into()),
    }
    if !command.is_empty() {
        out.insert("command".into(), toml::Value::Array(command.into_iter().map(toml::Value::String).collect()));
    }
    if let Some(url) = s.get("url") {
        out.insert("url".into(), toml::Value::String(url.as_str().ok_or("url must be a string")?.into()));
    }
    for key in ["env", "headers"] {
        if let Some(m) = s.get(key) {
            out.insert(key.into(), toml::Value::Table(pairs(m.as_object().ok_or(format!("{key} must be an object"))?)));
        }
    }
    if !out.contains_key("command") && !out.contains_key("url") {
        return Err("needs a command or a url".into());
    }
    Ok(toml::Value::Table(out))
}

fn strings(a: &[Value], key: &str) -> Result<Vec<String>, String> {
    a.iter().map(|x| x.as_str().map(str::to_string).ok_or(format!("{key} must be a list of strings"))).collect()
}

/// Names to strings; numbers and booleans are written as text (an environment has no types).
fn pairs(m: &Map<String, Value>) -> toml::Table {
    m.iter()
        .filter(|(_, v)| !v.is_null())
        .map(|(k, v)| (k.clone(), toml::Value::String(v.as_str().map_or_else(|| v.to_string(), str::to_string))))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLAUDE: &str = r#"{"mcpServers": {
        "files": {"command": "npx", "args": ["-y", "srv", "/home/me"], "env": {"A": "b", "N": 1}},
        "github": {"type": "http", "url": "https://h/mcp/", "headers": {"Authorization": "Bearer ${GITHUB_TOKEN}"}}
    }, "globalShortcut": ""}"#;

    #[test]
    fn claude_json_becomes_mcp_servers() {
        let want: toml::Table = toml::from_str(
            r#"
            [files]
            command = ["npx", "-y", "srv", "/home/me"]
            env = { A = "b", N = "1" }
            [github]
            url = "https://h/mcp/"
            headers = { Authorization = "Bearer ${GITHUB_TOKEN}" }
            "#,
        )
        .unwrap();
        assert_eq!(entries("mcp", CLAUDE).unwrap(), want);
        // Just the servers, or VS Code's `servers`, work too.
        let bare = r#"{"x": {"command": ["a", "b"]}}"#;
        assert_eq!(entries("mcp", bare).unwrap()["x"]["command"], toml::Value::try_from(["a", "b"]).unwrap());
        assert!(entries("mcp", r#"{"servers": {"x": {"url": "https://h"}}}"#).unwrap().contains_key("x"));
    }

    #[test]
    fn errors_name_the_server() {
        let e = |raw: &str| entries("mcp", raw).unwrap_err();
        assert!(e(r#"{"a": {"type": "sse", "url": "https://h"}}"#).starts_with("a: SSE"));
        assert_eq!(e(r#"{"a": {"env": {}}}"#), "a: needs a command or a url");
        assert!(e("{nope").starts_with("not valid JSON"));
        assert_eq!(e(r#"{"mcpServers": {}}"#), "no servers in it");
        assert!(entries("other", "{}").is_err());
    }
}
