//! The plugin's settings (from `initialize`) and what is still missing before it can ask.

use serde_json::{Map, Value};

pub const DEFAULT_SYSTEM: &str = "You answer questions typed into a desktop launcher. Be brief and direct. \
     Use Markdown sparingly (bold, lists, inline code, code blocks); no tables or images.";
/// `max_tokens` for the Anthropic API when the setting is 0 (it is required there).
pub const ANTHROPIC_MAX_TOKENS: u64 = 16_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Api {
    Anthropic,
    OpenAi,
}

/// An MCP server from `mcp_servers`, whose tools the model may call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerConfig {
    /// The key in `mcp_servers`; prefixes its tools' names.
    pub name: String,
    pub kind: ServerKind,
    /// Tools that run without asking first (`*`: all).
    pub allow: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerKind {
    /// A program speaking MCP over stdin/stdout; `env` is added to the inherited environment.
    Stdio { command: Vec<String>, env: Vec<(String, String)> },
    /// A Streamable HTTP endpoint, with fixed request headers (e.g. a bearer token).
    Http { url: String, headers: Vec<(String, String)> },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    pub api: Option<Api>,
    api_raw: String,
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    pub system: String,
    pub effort: Option<String>,
    /// 0: the default (see `ANTHROPIC_MAX_TOKENS`).
    pub max_tokens: u64,
    pub servers: Vec<ServerConfig>,
    /// Servers left out because their entry is wrong, in words for the user.
    pub server_problems: Vec<String>,
}

impl Config {
    pub fn from_settings(v: &Value) -> Self {
        Self::from_settings_with(v, &|k| std::env::var(k).ok())
    }

    /// `from_settings`, with `${name}` in MCP server entries looked up in the `secrets`
    /// setting (stored privately by the launcher) and then by `env`.
    pub fn from_settings_with(v: &Value, env: &dyn Fn(&str) -> Option<String>) -> Self {
        let lookup = |k: &str| v["secrets"][k].as_str().map(str::to_string).or_else(|| env(k));
        let (servers, server_problems) = servers(&v["mcp_servers"], &lookup);
        let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().trim().to_string();
        let api_raw = s("api");
        let system = s("system_prompt");
        let effort = s("effort");
        Self {
            api: match api_raw.as_str() {
                "anthropic" => Some(Api::Anthropic),
                "openai" => Some(Api::OpenAi),
                _ => None,
            },
            api_raw,
            base_url: s("base_url").trim_end_matches('/').to_string(),
            model: s("model"),
            api_key: Some(s("api_key")).filter(|k| !k.is_empty()),
            system: if system.is_empty() { DEFAULT_SYSTEM.into() } else { system },
            effort: Some(effort).filter(|e| !e.is_empty()),
            max_tokens: v.get("max_tokens").and_then(Value::as_u64).unwrap_or(0),
            servers,
            server_problems,
        }
    }

    /// What must still be set before asking, in words for the user. Empty: ready.
    pub fn problems(&self) -> Vec<String> {
        let mut out = vec![];
        match self.api {
            None if self.api_raw.is_empty() => out.push("api type (anthropic or openai)".into()),
            None => out.push(format!("api {:?} is neither anthropic nor openai", self.api_raw)),
            Some(_) => {}
        }
        if self.base_url.is_empty() {
            out.push("base_url".into());
        } else if let Err(e) = check_url(&self.base_url) {
            out.push(format!("base_url {e}"));
        }
        if self.model.is_empty() {
            out.push("model".into());
        }
        if self.api == Some(Api::Anthropic) && self.api_key.is_none() {
            out.push("api_key".into());
        }
        out
    }
}

/// `https://…`, or `http://` to this machine only: a key must not cross the network in clear.
pub fn check_url(url: &str) -> Result<(), String> {
    if let Some(rest) = url.strip_prefix("https://") {
        return if rest.is_empty() { Err("has no host".into()) } else { Ok(()) };
    }
    let Some(rest) = url.strip_prefix("http://") else {
        return Err("must start with https:// (or http:// for localhost)".into());
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = if authority.starts_with('[') {
        authority.split(']').next().map(|h| format!("{h}]")).unwrap_or_default()
    } else {
        authority.split(':').next().unwrap_or("").to_string()
    };
    if ["localhost", "127.0.0.1", "[::1]"].contains(&host.as_str()) {
        Ok(())
    } else {
        Err("must use https:// unless it is on this machine (localhost)".into())
    }
}

/// The servers in `mcp_servers`, and why any were left out.
fn servers(v: &Value, env: &dyn Fn(&str) -> Option<String>) -> (Vec<ServerConfig>, Vec<String>) {
    let (mut ok, mut problems) = (vec![], vec![]);
    let Some(table) = v.as_object() else {
        if !v.is_null() {
            problems.push("mcp_servers must be a table of servers".into());
        }
        return (ok, problems);
    };
    for (name, entry) in table {
        match server(name, entry, env) {
            Ok(s) => ok.push(s),
            Err(e) => problems.push(format!("MCP server {name}: {e}")),
        }
    }
    (ok, problems)
}

fn server(name: &str, v: &Value, env: &dyn Fn(&str) -> Option<String>) -> Result<ServerConfig, String> {
    // Tools are named `<server>__<tool>`; without `__` in the server name or a `_` at its end,
    // the first `__` always splits them, so the approval prompt names the right tool.
    if name.is_empty()
        || name.len() > 32
        || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        || name.contains("__")
        || name.ends_with('_')
    {
        return Err("the name may only use letters, digits, - and single _ inside (at most 32)".into());
    }
    let strings = |key: &str| -> Result<Vec<String>, String> {
        match v.get(key) {
            None => Ok(vec![]),
            Some(Value::Array(a)) => a
                .iter()
                .map(|x| x.as_str().map(str::to_string).ok_or(format!("{key} must be a list of strings")))
                .collect(),
            Some(_) => Err(format!("{key} must be a list of strings")),
        }
    };
    let pairs = |key: &str| -> Result<Vec<(String, String)>, String> {
        let Some(m) = v.get(key) else { return Ok(vec![]) };
        let m: &Map<String, Value> = m.as_object().ok_or(format!("{key} must be a table of strings"))?;
        m.iter()
            .map(|(k, x)| {
                let x = x.as_str().ok_or(format!("{key}.{k} must be a string"))?;
                Ok((k.clone(), expand(x, env)?))
            })
            .collect()
    };
    let allow = strings("allow")?;
    let kind = match (v.get("command"), v.get("url")) {
        (Some(_), Some(_)) => return Err("set either command or url, not both".into()),
        (Some(_), None) => {
            let command = strings("command")?;
            if command.first().is_none_or(|c| c.trim().is_empty()) {
                return Err("command is empty".into());
            }
            ServerKind::Stdio { command, env: pairs("env")? }
        }
        (None, Some(u)) => {
            let url = u.as_str().ok_or("url must be a string")?.trim().to_string();
            check_url(&url).map_err(|e| format!("url {e}"))?;
            ServerKind::Http { url, headers: pairs("headers")? }
        }
        (None, None) => return Err("needs a command (a local server) or a url (a remote one)".into()),
    };
    Ok(ServerConfig { name: name.to_string(), kind, allow })
}

/// Replace `${name}` with a secret (`plugin set ai secrets <name> <value>`) or an environment
/// variable, so tokens need not be in `config.toml`.
fn expand(s: &str, env: &dyn Fn(&str) -> Option<String>) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("${") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        let end = after.find('}').ok_or("an unclosed ${ in a value")?;
        let var = &after[..end];
        out.push_str(&env(var).ok_or(format!(
            "{var} is not set: store it with `plugin set ai secrets {var} <value>` (or set the environment variable)"
        ))?);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn nothing_is_preset() {
        let c = Config::from_settings(&json!({}));
        assert_eq!(c.problems(), ["api type (anthropic or openai)", "base_url", "model"]);
        assert_eq!(c.system, DEFAULT_SYSTEM);
    }

    #[test]
    fn anthropic_needs_a_key_openai_does_not() {
        let base = json!({"api": "anthropic", "base_url": "https://api.anthropic.com/", "model": "m"});
        let c = Config::from_settings(&base);
        assert_eq!(c.problems(), ["api_key"]);
        assert_eq!(c.base_url, "https://api.anthropic.com", "trailing slash dropped");
        let mut with_key = base.clone();
        with_key["api_key"] = json!("sk");
        assert!(Config::from_settings(&with_key).problems().is_empty());
        let local = json!({"api": "openai", "base_url": "http://localhost:11434/v1", "model": "llama3.2"});
        assert!(Config::from_settings(&local).problems().is_empty());
    }

    #[test]
    fn plain_http_only_to_this_machine() {
        assert!(check_url("http://127.0.0.1:8080/v1").is_ok());
        assert!(check_url("http://[::1]:1234/v1").is_ok());
        assert!(check_url("http://example.com/v1").is_err());
        assert!(check_url("http://localhost.evil.com/v1").is_err());
        assert!(check_url("ftp://x").is_err());
        assert!(check_url("https://").is_err());
        let c = Config::from_settings(&json!({"api": "gpt", "base_url": "http://x.com", "model": "m"}));
        let p = c.problems();
        assert!(p[0].contains("neither") && p[1].starts_with("base_url") && p[1].contains("https://"), "{p:?}");
    }

    fn env(k: &str) -> Option<String> {
        (k == "TOKEN").then(|| "t0k".to_string())
    }

    #[test]
    fn mcp_servers() {
        let v = json!({"mcp_servers": {
            "fs": {"command": ["npx", "-y", "srv"], "env": {"A": "b"}, "allow": ["read_file"]},
            "gh": {"url": "https://h/mcp", "headers": {"Authorization": "Bearer ${TOKEN}"}},
            "both": {"command": ["x"], "url": "https://h"},
            "none": {},
            "plain": {"url": "http://example.com/mcp"},
            "nokey": {"url": "https://h", "headers": {"X": "${MISSING}"}},
            "bad name": {"command": ["x"]},
            "a__b": {"command": ["x"]},
            "a_": {"command": ["x"]},
            "my_fs": {"command": ["x"]},
            "empty": {"command": []},
        }});
        let c = Config::from_settings_with(&v, &env);
        assert_eq!(
            c.servers,
            [
                ServerConfig {
                    name: "fs".into(),
                    kind: ServerKind::Stdio {
                        command: vec!["npx".into(), "-y".into(), "srv".into()],
                        env: vec![("A".into(), "b".into())]
                    },
                    allow: vec!["read_file".into()],
                },
                ServerConfig {
                    name: "gh".into(),
                    kind: ServerKind::Http {
                        url: "https://h/mcp".into(),
                        headers: vec![("Authorization".into(), "Bearer t0k".into())]
                    },
                    allow: vec![],
                },
                ServerConfig {
                    name: "my_fs".into(),
                    kind: ServerKind::Stdio { command: vec!["x".into()], env: vec![] },
                    allow: vec![],
                },
            ]
        );
        let p = c.server_problems.join("\n");
        for want in [
            "bad name: the name",
            "a__b: the name",
            "a_: the name",
            "both: set either",
            "empty: command is empty",
            "none: needs a command",
            "plain: url must use https://",
            "nokey: MISSING is not set: store it with `plugin set ai secrets MISSING",
        ] {
            assert!(p.contains(want), "{want} missing in {p}");
        }
        assert!(Config::from_settings(&json!({})).server_problems.is_empty());
        assert_eq!(Config::from_settings(&json!({"mcp_servers": 3})).server_problems.len(), 1);
    }

    #[test]
    fn secrets_win_over_the_environment() {
        let v = json!({
            "secrets": {"TOKEN": "from-store", "gh": "g1"},
            "mcp_servers": {"a": {"url": "https://h", "headers": {"A": "${TOKEN}", "B": "${gh}"}}},
        });
        let c = Config::from_settings_with(&v, &env);
        let ServerKind::Http { headers, .. } = &c.servers[0].kind else { panic!() };
        assert_eq!(headers, &[("A".into(), "from-store".into()), ("B".into(), "g1".into())]);
    }

    #[test]
    fn expands_variables() {
        assert_eq!(expand("a ${TOKEN} b ${TOKEN}", &env).unwrap(), "a t0k b t0k");
        assert_eq!(expand("no vars $HOME", &env).unwrap(), "no vars $HOME");
        assert!(expand("${TOKEN", &env).is_err());
    }

    #[test]
    fn optional_settings() {
        let c = Config::from_settings(&json!({"effort": "low", "system_prompt": " Be terse ", "max_tokens": 500}));
        assert_eq!((c.effort.as_deref(), c.system.as_str(), c.max_tokens), (Some("low"), "Be terse", 500));
        let c = Config::from_settings(&json!({"effort": ""}));
        assert_eq!(c.effort, None);
    }
}
