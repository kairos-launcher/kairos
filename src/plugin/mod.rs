pub mod error;
pub mod external;
pub mod import;
pub mod manifest;
#[cfg(feature = "remote")]
pub mod remote;
pub mod secrets;

use std::sync::Arc;

use serde::{Deserialize, Serialize};

pub use error::{Error, Result};

#[derive(Deserialize, Serialize, Clone, Debug, Default)]
pub struct Icon {
    pub path: Option<String>,
    pub base64: Option<String>,
    pub mime: Option<String>,
    pub glyph: Option<String>,
    /// Ask for the image later: the host calls the plugin's `icon` method with this id, only
    /// for results that are actually shown. Keeps query replies small.
    pub id: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Action {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub default: bool,
    /// What the host does when the action is chosen; `call` asks the plugin (`execute`).
    #[serde(default, skip_serializing_if = "ActionKind::is_call")]
    pub kind: ActionKind,
    /// `open_url`: the link. `copy` / `set_query`: the text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// The action shows output (`ExecOutcome::Output`): the launcher opens the output panel
    /// at once instead of waiting for the reply. Only a hint; a reply with `output` opens it
    /// either way.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub output: bool,
}

/// Actions the host performs itself, without a round trip to the plugin. This is how a
/// remote plugin (which cannot run anything locally) gets things done, and it is the only
/// way it can: there is no "run command" or "open file" kind.
#[derive(Deserialize, Serialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    #[default]
    Call,
    OpenUrl,
    Copy,
    SetQuery,
    /// Open the settings page of the plugin that returned the result.
    Settings,
}

impl ActionKind {
    #[allow(clippy::trivially_copy_pass_by_ref, reason = "serde's skip_serializing_if passes a reference")]
    fn is_call(&self) -> bool {
        *self == Self::Call
    }
}

impl Action {
    /// Perform a host-side action. `None` for `call`, which the plugin handles.
    pub fn run_host(&self) -> Option<Result<ExecOutcome>> {
        let value = self.value.as_deref().unwrap_or_default();
        Some(match self.kind {
            ActionKind::Call => return None,
            ActionKind::OpenUrl => {
                crate::core::process::open_url(value).map(|()| ExecOutcome::Close).map_err(Error::from)
            }
            ActionKind::Copy => crate::core::clipboard::copy(value).map(|()| ExecOutcome::Close).map_err(Error::from),
            ActionKind::SetQuery => Ok(ExecOutcome::complete(value)),
            // The engine fills in which plugin (see `Engine::execute`).
            ActionKind::Settings => Ok(ExecOutcome::Settings(String::new())),
        })
    }
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct ResultItem {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub subtitle: String,
    #[serde(default)]
    pub icon: Option<Icon>,
    /// 0..1, plugin-relative. The engine adds the frecency boost.
    #[serde(default = "default_score")]
    pub score: f64,
    #[serde(default)]
    pub actions: Vec<Action>,
    /// If set, **Tab** replaces the whole search box with this text (trigger included) and
    /// re-runs the search, instead of opening the actions menu. Lets a plugin choose what
    /// Tab means for each of its results: autocomplete vs. a menu.
    #[serde(default)]
    pub complete: Option<String>,
    /// A catch-all row such as "Ask AI: …": always listed after the other results, and never
    /// boosted by how often it was used.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fallback: bool,
    /// Filled in by the engine with the owning provider's id.
    #[serde(skip)]
    pub provider: String,
    /// The query text this provider received (trigger stripped); passed back to `execute`.
    #[serde(skip)]
    pub query: String,
}

fn default_score() -> f64 {
    0.5
}

/// Size caps for one result, whoever sent it: a buggy or hostile plugin must not flood the
/// list or the launcher's memory.
const MAX_TEXT: usize = 1024;
const MAX_VALUE: usize = 8192;
const MAX_ACTIONS: usize = 16;
const MAX_GLYPH: usize = 16;

/// Shorten `s` to at most `max` bytes, at a character boundary.
fn truncate_to(s: &mut String, max: usize) {
    if s.len() > max {
        let mut end = max;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
    }
}

impl ResultItem {
    pub fn default_action(&self) -> Option<&Action> {
        self.actions.iter().find(|a| a.default).or(self.actions.first())
    }

    /// A row with no icon or actions yet; add them with the builder methods below.
    pub fn new(id: impl Into<String>, title: impl Into<String>, subtitle: impl Into<String>, score: f64) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            subtitle: subtitle.into(),
            icon: None,
            score,
            actions: vec![],
            complete: None,
            fallback: false,
            provider: String::new(),
            query: String::new(),
        }
    }

    pub fn with_icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Add an action; the first one added is the default (Enter).
    pub fn with_action(mut self, id: &str, title: &str) -> Self {
        let default = self.actions.is_empty();
        self.actions.push(Action { id: id.into(), title: title.into(), default, ..Default::default() });
        self
    }

    /// Apply the size caps (`MAX_TEXT`, `MAX_VALUE`, ...).
    pub fn clamp_sizes(&mut self) {
        truncate_to(&mut self.id, MAX_VALUE);
        truncate_to(&mut self.title, MAX_TEXT);
        truncate_to(&mut self.subtitle, MAX_TEXT);
        if let Some(c) = &mut self.complete {
            truncate_to(c, MAX_VALUE);
        }
        if let Some(icon) = &mut self.icon {
            if let Some(g) = &mut icon.glyph {
                truncate_to(g, MAX_GLYPH);
            }
            if let Some(id) = &mut icon.id {
                truncate_to(id, MAX_TEXT);
            }
        }
        clamp_actions(&mut self.actions);
    }

    pub fn with_complete(mut self, text: impl Into<String>) -> Self {
        self.complete = Some(text.into());
        self
    }
}

/// Apply the size caps to a list of actions (a result's, or an output panel's).
pub(crate) fn clamp_actions(actions: &mut Vec<Action>) {
    actions.truncate(MAX_ACTIONS);
    for a in actions {
        truncate_to(&mut a.id, MAX_TEXT);
        truncate_to(&mut a.title, MAX_TEXT);
        if let Some(v) = &mut a.value {
            truncate_to(v, MAX_VALUE);
        }
    }
}

pub struct Query<'a> {
    /// Text with any trigger prefix stripped.
    pub text: &'a str,
    pub trigger: Option<&'a str>,
    pub limit: usize,
}

/// What the launcher does after an action ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecOutcome {
    /// Done: hide the launcher.
    Close,
    /// Stay open as is (e.g. a window was closed and the list refreshes).
    KeepOpen,
    /// Stay open and replace the search box text, re-running the search (e.g. to complete a
    /// command).
    SetQuery(String),
    /// Stay open and show this in the output panel; more may follow through the `OutputSink`
    /// when `streaming` is set.
    Output(Box<OutputStart>),
    /// Stay open and show the settings page of this plugin (its id).
    Settings(String),
}

/// The first part of an action's output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OutputStart {
    pub title: String,
    pub text: String,
    /// `text` (default) or `markdown` (rendered; plain text can be selected, markdown cannot).
    pub format: String,
    /// More arrives as `OutputEvent`s until `Done`.
    pub streaming: bool,
    /// Actions for the panel (Enter runs the default). Empty: the launcher offers Copy.
    pub actions: Vec<Action>,
    /// The search box takes text for the plugin while the panel is open (follow-ups).
    pub input: Option<OutputInput>,
}

/// Text input offered in the output panel: Enter runs `action` with what was typed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OutputInput {
    pub placeholder: String,
    pub action: String,
}

/// A later piece of a streaming output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputEvent {
    Append(String),
    /// Replace all text shown so far.
    Replace(String),
    Actions(Vec<Action>),
    /// Offer (or, with `None`, withdraw) the panel's text input.
    Input(Option<OutputInput>),
    /// The stream ended; `Some` with the reason when it failed.
    Done(Option<String>),
}

/// Receives the output events of one execution, on whatever thread they arrive.
pub type OutputSink = Arc<dyn Fn(OutputEvent) + Send + Sync>;

/// Context for one execution: its id (`exec` in the protocol) and where streamed output goes.
#[derive(Clone)]
pub struct ExecCtx {
    pub exec: u64,
    pub sink: OutputSink,
    /// Text typed into the output panel's input (sent as `input`).
    pub input: Option<String>,
}

impl ExecCtx {
    /// No output wanted: events are dropped.
    pub fn none() -> Self {
        Self { exec: 0, sink: Arc::new(|_| {}), input: None }
    }
}

impl ExecOutcome {
    /// Stay open and put `query` in the search box.
    pub fn complete(query: impl Into<String>) -> Self {
        Self::SetQuery(query.into())
    }

    pub fn keeps_open(&self) -> bool {
        !matches!(self, Self::Close)
    }

    /// The new search box text, if any.
    pub fn query(&self) -> Option<&str> {
        match self {
            Self::SetQuery(q) => Some(q),
            Self::Close | Self::KeepOpen | Self::Output(_) | Self::Settings(_) => None,
        }
    }
}

/// Implemented by bundled plugins (in-process) and by `ExternalPlugin` (stdio JSON-RPC).
/// Routing (triggers, global) comes from the plugin's manifest, not from this trait.
pub trait Provider: Send + Sync {
    fn id(&self) -> &str;
    fn query(&self, q: &Query) -> Vec<ResultItem>;
    /// Like `query`, but reports results in batches as they become available (the engine
    /// publishes each one). The default reports everything once.
    fn query_stream(&self, q: &Query, emit: &mut dyn FnMut(Vec<ResultItem>)) {
        emit(self.query(q));
    }
    fn execute(&self, result_id: &str, action_id: &str, query: &str) -> Result<ExecOutcome>;
    /// `execute` for providers that can stream output (`ExecOutcome::Output` with
    /// `streaming`): later events go to `ctx.sink`. The default ignores `ctx`.
    fn execute_with(&self, result_id: &str, action_id: &str, query: &str, ctx: &ExecCtx) -> Result<ExecOutcome> {
        let _ = ctx;
        self.execute(result_id, action_id, query)
    }
    /// Stop streaming the output of execution `exec` (the panel was closed).
    fn cancel_output(&self, _exec: u64) {}
    /// Called shortly after the launcher hides; stop idle on-demand processes.
    fn reap_idle(&self) {}
    /// Image for a lazy icon id (`Icon::id`). `None` if there is none.
    fn icon(&self, _icon_id: &str) -> Option<Icon> {
        None
    }
    /// Called at startup for plugins that want to be warm.
    fn warm_up(&self) {}
    /// Re-scan data sources (e.g. the app index). Called off the UI thread when
    /// `needs_refresh` is true, typically as the launcher is shown.
    fn refresh(&self) {}
    fn needs_refresh(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_actions_run_without_the_plugin() {
        let a: Action =
            serde_json::from_str(r#"{"id":"x","title":"X","kind":"set_query","value":"jira PROJ-1"}"#).unwrap();
        let out = a.run_host().unwrap().unwrap();
        assert_eq!(out, ExecOutcome::SetQuery("jira PROJ-1".into()));
        let call: Action = serde_json::from_str(r#"{"id":"x","title":"X"}"#).unwrap();
        assert!(call.run_host().is_none(), "default kind is a call to the plugin");
        let bad: Action =
            serde_json::from_str(r#"{"id":"x","title":"X","kind":"open_url","value":"file:///etc/passwd"}"#).unwrap();
        assert!(bad.run_host().unwrap().is_err());
        assert!(serde_json::from_str::<Action>(r#"{"id":"x","title":"X","kind":"run_command"}"#).is_err());
    }

    #[test]
    fn result_json_with_and_without_complete() {
        let r: ResultItem = serde_json::from_str(r#"{"id":"1","title":"t","complete":"kill firefox"}"#).unwrap();
        assert_eq!(r.complete.as_deref(), Some("kill firefox"));
        let r: ResultItem = serde_json::from_str(r#"{"id":"1","title":"t"}"#).unwrap();
        assert_eq!(r.complete, None, "absent means Tab opens the actions menu");
    }

    #[test]
    #[allow(clippy::float_cmp, reason = "the default is an exact constant")]
    fn result_json_defaults_and_engine_fields_are_not_serialized() {
        let r: ResultItem = serde_json::from_str(r#"{"id":"1","title":"t"}"#).unwrap();
        assert_eq!(r.subtitle, "");
        assert_eq!(r.score, 0.5);
        assert!(r.actions.is_empty());
        assert!(r.icon.is_none());

        let mut r = ResultItem::new("1", "t", "", 0.1);
        r.provider = "p".into();
        r.query = "q".into();
        let v = serde_json::to_value(&r).unwrap();
        assert!(v.get("provider").is_none() && v.get("query").is_none(), "{v}");
        let back: ResultItem =
            serde_json::from_value(serde_json::json!({"id":"1","title":"t","provider":"x","query":"y"})).unwrap();
        assert_eq!((back.provider.as_str(), back.query.as_str()), ("", ""), "a plugin cannot set engine fields");
    }

    #[test]
    fn default_action_prefers_flag_then_first() {
        let r: ResultItem = serde_json::from_str(
            r#"{"id":"1","title":"t","actions":[{"id":"a","title":"A"},{"id":"b","title":"B","default":true}]}"#,
        )
        .unwrap();
        assert_eq!(r.default_action().unwrap().id, "b");
        let r: ResultItem =
            serde_json::from_str(r#"{"id":"1","title":"t","actions":[{"id":"a","title":"A"},{"id":"b","title":"B"}]}"#)
                .unwrap();
        assert_eq!(r.default_action().unwrap().id, "a");
        assert!(ResultItem::new("1", "t", "", 0.5).default_action().is_none());
    }

    #[test]
    fn builder_marks_only_first_action_default() {
        let r = ResultItem::new("id", "Title", "Sub", 0.7)
            .with_icon(Icon { glyph: Some("G".into()), ..Default::default() })
            .with_action("a", "A")
            .with_action("b", "B")
            .with_complete("x ");
        assert_eq!((r.id.as_str(), r.title.as_str(), r.subtitle.as_str(), r.score), ("id", "Title", "Sub", 0.7));
        assert_eq!(r.actions.iter().map(|a| a.default).collect::<Vec<_>>(), [true, false]);
        assert_eq!(r.icon.unwrap().glyph.as_deref(), Some("G"));
        assert_eq!(r.complete.as_deref(), Some("x "));
    }

    #[test]
    fn oversized_results_are_clamped_at_char_boundaries() {
        let mut r = ResultItem::new("1", "é".repeat(MAX_TEXT), "s", 0.5);
        for i in 0..40 {
            r = r.with_action(&format!("a{i}"), "A");
        }
        r.clamp_sizes();
        assert!(r.title.len() <= MAX_TEXT && r.title.chars().all(|c| c == 'é'));
        assert_eq!(r.actions.len(), MAX_ACTIONS);
        let mut small = ResultItem::new("1", "t", "s", 0.5).with_action("a", "A");
        let before = format!("{small:?}");
        small.clamp_sizes();
        assert_eq!(format!("{small:?}"), before, "normal results are untouched");
    }

    #[test]
    fn exec_outcome_constructors() {
        assert!(!ExecOutcome::Close.keeps_open() && ExecOutcome::Close.query().is_none());
        assert!(ExecOutcome::KeepOpen.keeps_open() && ExecOutcome::KeepOpen.query().is_none());
        let q = ExecOutcome::complete("kill ");
        assert!(q.keeps_open());
        assert_eq!(q.query(), Some("kill "));
    }

    #[test]
    fn icon_parses_every_documented_shape() {
        let i: Icon = serde_json::from_str(r#"{"glyph":"D"}"#).unwrap();
        assert_eq!(i.glyph.as_deref(), Some("D"));
        let i: Icon = serde_json::from_str(r#"{"path":"icon.png"}"#).unwrap();
        assert_eq!(i.path.as_deref(), Some("icon.png"));
        let i: Icon = serde_json::from_str(r#"{"base64":"AAAA","mime":"image/png"}"#).unwrap();
        assert_eq!((i.base64.as_deref(), i.mime.as_deref()), (Some("AAAA"), Some("image/png")));
        let i: Icon = serde_json::from_str(r#"{"id":"blue"}"#).unwrap();
        assert_eq!(i.id.as_deref(), Some("blue"));
        let i: Icon = serde_json::from_str("{}").unwrap();
        assert!(i.glyph.is_none() && i.path.is_none() && i.base64.is_none() && i.id.is_none());
    }
}
