//! Web search: `<keyword> <text>` searches that engine, `web <text>` the default one, a typed
//! URL or domain opens directly, and a plain search ends with a "search the web" row.

use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

use crate::core::url::encode;
use crate::core::{icons, process};
use crate::plugin;
use crate::plugin::{Error, ExecOutcome, Icon, Provider, Query, ResultItem};

const ID: &str = "builtin-web";
const WEB: &str = "web ";

#[derive(Deserialize, Debug, Clone)]
#[serde(default)]
pub struct Settings {
    pub engine: String,
    pub browser: String,
    pub fallback: bool,
    /// Sorted by keyword so triggers and listings are stable.
    pub engines: BTreeMap<String, String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self { engine: "ddg".into(), browser: String::new(), fallback: true, engines: BTreeMap::default() }
    }
}

pub struct WebProvider {
    s: Settings,
}

/// `https://www.example.com/x?q={q}` -> `example.com`.
fn host(template: &str) -> &str {
    let rest = template.split_once("://").map_or(template, |(_, r)| r);
    let h = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    h.strip_prefix("www.").unwrap_or(h)
}

/// File names look like domains (`notes.txt`): these endings only count with a path after them.
const NOT_TLDS: [&str; 24] = [
    "txt", "md", "pdf", "png", "jpg", "jpeg", "gif", "svg", "rs", "py", "js", "ts", "toml", "json", "yaml", "yml",
    "sh", "zip", "gz", "tar", "doc", "docx", "csv", "log",
];

/// The URL to open if `text` is one (`https://…`) or looks like a domain (`example.com/x`).
pub fn as_url(text: &str) -> Option<String> {
    let t = text.trim();
    if t.is_empty() || t.contains(char::is_whitespace) {
        return None;
    }
    let lower = t.to_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return (t.len() > lower.find("://")? + 3).then(|| t.to_string());
    }
    let host = t.split(['/', '?', '#']).next()?;
    let host_only = host.split(':').next()?;
    let labels: Vec<&str> = host_only.split('.').collect();
    let tld = labels.last()?;
    let valid = labels.len() >= 2
        && labels.iter().all(|l| !l.is_empty() && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        && (2..=12).contains(&tld.len())
        && tld.chars().all(|c| c.is_ascii_alphabetic())
        // `main.rs` is a file, `docs.rs/slint` a page.
        && (!NOT_TLDS.contains(&tld.to_lowercase().as_str()) || host.len() < t.len());
    valid.then(|| format!("https://{t}"))
}

impl WebProvider {
    pub fn new(settings: &Value) -> Self {
        let mut s: Settings = serde_json::from_value(settings.clone()).unwrap_or_default();
        s.engines.retain(|k, v| !k.trim().is_empty() && !k.contains(char::is_whitespace) && !v.is_empty());
        if !s.engines.contains_key(&s.engine)
            && let Some(first) = s.engines.keys().next()
        {
            log::warn!("web: unknown engine {:?}; using {first}", s.engine);
            s.engine = first.clone();
        }
        Self { s }
    }

    /// `web ` plus one trigger per engine keyword.
    pub fn triggers(&self) -> Vec<String> {
        std::iter::once(WEB.to_string()).chain(self.s.engines.keys().map(|k| format!("{k} "))).collect()
    }

    fn engine_for(&self, trigger: Option<&str>) -> Option<(&str, &str)> {
        let key = match trigger.map(str::trim_end) {
            Some(t) if t != WEB.trim_end() => t,
            _ => &self.s.engine,
        };
        self.s.engines.get_key_value(key).map(|(k, v)| (k.as_str(), v.as_str()))
    }

    fn open(&self, url: &str) -> Result<(), String> {
        process::open_with(&self.s.browser, url)
    }

    fn icon() -> Icon {
        icons::themed(&["web-browser", "applications-internet", "internet-web-browser"], "W")
    }

    fn search_row(key: &str, template: &str, text: &str, score: f64) -> ResultItem {
        // One id per engine keeps frecency small; `execute` rebuilds the URL from the query.
        ResultItem::new(
            format!("search:{key}"),
            format!("Search {} for \u{201c}{text}\u{201d}", host(template)),
            format!("{key} · {}", template.replace("{q}", &encode(text))),
            score,
        )
        .with_icon(Self::icon())
        .with_action("open", "Search")
        .with_action("copy", "Copy URL")
    }
}

impl Provider for WebProvider {
    fn id(&self) -> &str {
        ID
    }

    fn query(&self, q: &Query) -> Vec<ResultItem> {
        let text = q.text.trim();
        let mut out = vec![];
        if let Some(trigger) = q.trigger {
            let Some((key, template)) = self.engine_for(Some(trigger)) else { return out };
            if text.is_empty() {
                out.push(
                    ResultItem::new(format!("home:{key}"), format!("Open {}", host(template)), "Type to search", 1.0)
                        .with_icon(Self::icon())
                        .with_action("open", "Open"),
                );
            } else {
                out.push(Self::search_row(key, template, text, 1.0));
            }
            return out;
        }
        if let Some(url) = as_url(text) {
            out.push(
                ResultItem::new("url", format!("Open {url}"), "Web page", 0.92)
                    .with_icon(Self::icon())
                    .with_action("open", "Open")
                    .with_action("copy", "Copy URL"),
            );
        }
        // A fallback row: listed after every real match, never lifted by frecency.
        if self.s.fallback
            && !text.is_empty()
            && let Some((key, template)) = self.engine_for(None)
        {
            let mut row = Self::search_row(key, template, text, 0.01);
            row.fallback = true;
            out.push(row);
        }
        out
    }

    fn execute(&self, result_id: &str, action_id: &str, query: &str) -> plugin::Result<ExecOutcome> {
        let text = query.trim();
        let url = match result_id.split_once(':') {
            Some(("search", key)) => self.s.engines.get(key).ok_or("engine gone")?.replace("{q}", &encode(text)),
            Some(("home", key)) => {
                let t = self.s.engines.get(key).ok_or("engine gone")?;
                t.split_once("://")
                    .map(|(s, r)| format!("{s}://{}", r.split('/').next().unwrap_or(r)))
                    .ok_or("bad engine URL")?
            }
            _ if result_id == "url" => as_url(text).ok_or("not a URL")?,
            _ => return Err(Error::UnknownResult(result_id.into())),
        };
        match action_id {
            "copy" => crate::core::clipboard::copy(&url)?,
            _ => self.open(&url)?,
        }
        Ok(ExecOutcome::Close)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::manifest::Manifest;

    fn provider(user: &str) -> WebProvider {
        let m = Manifest::parse(include_str!("plugin.toml"), "web").unwrap();
        let user: toml::Table = toml::from_str(user).unwrap();
        WebProvider::new(&m.effective_settings(Some(&user)))
    }

    #[test]
    fn detects_urls_but_not_file_names_or_numbers() {
        assert_eq!(as_url("example.com").as_deref(), Some("https://example.com"));
        assert_eq!(as_url("docs.rs/slint").as_deref(), Some("https://docs.rs/slint"));
        assert_eq!(as_url("http://localhost:8080").as_deref(), Some("http://localhost:8080"));
        assert_eq!(as_url("example.com/main.rs").as_deref(), Some("https://example.com/main.rs"));
        for no in ["notes.txt", "3.14", "firefox", "two words.com", "https://", "a..com", "main.rs"] {
            assert_eq!(as_url(no), None, "{no}");
        }
    }

    #[test]
    fn triggers_come_from_engines() {
        let p = provider(
            "[engines]\nddg = \"https://duckduckgo.com/?q={q}\"\nsp = \"https://startpage.com/do/search?q={q}\"\n",
        );
        assert_eq!(p.triggers(), ["web ", "ddg ", "sp "], "user table replaces the defaults");
        let r = p.query(&Query { text: "rust lang", trigger: Some("sp "), limit: 8 });
        assert!(r[0].subtitle.contains("https://startpage.com/do/search?q=rust%20lang"), "{}", r[0].subtitle);
    }

    #[test]
    fn plain_search_gets_url_and_low_fallback() {
        let p = provider("");
        let r = p.query(&Query { text: "example.com", trigger: None, limit: 8 });
        assert_eq!(r.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["url", "search:ddg"]);
        assert!(r[1].score < 0.05 && r[1].fallback && !r[0].fallback);
        let p = provider("fallback = false\nengine = \"g\"\n");
        assert!(p.query(&Query { text: "firefox", trigger: None, limit: 8 }).is_empty());
        let r = p.query(&Query { text: "x", trigger: Some("web "), limit: 8 });
        assert_eq!(r[0].id, "search:g", "web uses the configured default engine");
    }

    #[test]
    fn unknown_default_engine_falls_back_to_a_known_one() {
        let p = provider("engine = \"nope\"\n");
        assert!(p.s.engines.contains_key(&p.s.engine));
    }
}
