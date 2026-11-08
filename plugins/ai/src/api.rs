//! The two wire formats: Anthropic Messages API and OpenAI-compatible chat completions.
//! Both stream server-sent events; `Stream` turns their lines into `Event`s and assembles
//! the assistant message, tool calls included.

use serde_json::{Value, json};

use crate::config::{ANTHROPIC_MAX_TOKENS, Api, Config};

/// One entry of a conversation.
#[derive(Debug, Clone, PartialEq)]
pub enum Turn {
    User(String),
    Assistant {
        /// The answer text, as shown (with notes such as a cut-off).
        text: String,
        calls: Vec<ToolCall>,
        /// Anthropic: the content blocks exactly as received. Thinking blocks and their
        /// signatures must go back unchanged when the model continues after a tool call.
        raw: Option<Value>,
    },
    /// The results for the calls of the assistant turn before, in the same order.
    ToolResults(Vec<ToolResult>),
}

impl Turn {
    pub fn answer(text: impl Into<String>) -> Self {
        Turn::Assistant { text: text.into(), calls: vec![], raw: None }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: String,
    /// The name the model knows (`<server>__<tool>`).
    pub name: String,
    /// `Null` when the model's arguments were not valid JSON.
    pub input: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResult {
    pub id: String,
    pub text: String,
    pub is_error: bool,
}

/// A tool offered to the model.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    /// JSON Schema of the arguments (an object schema).
    pub schema: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Value,
}

/// One meaningful piece of a streamed answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Text(String),
    /// Shown after the answer (e.g. that it was cut off).
    Note(String),
    Error(String),
    Done,
}

/// The HTTP request asking `cfg`'s model to continue `turns` (which end with a user turn or
/// tool results), offering `tools`.
pub fn request(cfg: &Config, turns: &[Turn], tools: &[ToolDef]) -> Request {
    let mut headers = vec![("content-type".to_string(), "application/json".to_string())];
    match cfg.api {
        Some(Api::Anthropic) => {
            if let Some(k) = &cfg.api_key {
                headers.push(("x-api-key".into(), k.clone()));
            }
            headers.push(("anthropic-version".into(), "2023-06-01".into()));
            let max = if cfg.max_tokens == 0 { ANTHROPIC_MAX_TOKENS } else { cfg.max_tokens };
            let mut body = json!({
                "model": cfg.model,
                "max_tokens": max,
                "stream": true,
                "system": cfg.system,
                "messages": anthropic_messages(turns),
            });
            if let Some(e) = &cfg.effort {
                body["output_config"] = json!({"effort": e});
            }
            if !tools.is_empty() {
                body["tools"] = tools
                    .iter()
                    .map(|t| json!({"name": t.name, "description": t.description, "input_schema": t.schema}))
                    .collect();
            }
            Request { url: format!("{}/v1/messages", cfg.base_url), headers, body }
        }
        Some(Api::OpenAi) | None => {
            if let Some(k) = &cfg.api_key {
                headers.push(("authorization".into(), format!("Bearer {k}")));
            }
            let mut body = json!({"model": cfg.model, "stream": true, "messages": openai_messages(&cfg.system, turns)});
            if cfg.max_tokens > 0 {
                body["max_tokens"] = json!(cfg.max_tokens);
            }
            if !tools.is_empty() {
                body["tools"] = tools
                    .iter()
                    .map(|t| {
                        json!({"type": "function",
                               "function": {"name": t.name, "description": t.description, "parameters": t.schema}})
                    })
                    .collect();
            }
            Request { url: format!("{}/chat/completions", cfg.base_url), headers, body }
        }
    }
}

fn anthropic_messages(turns: &[Turn]) -> Vec<Value> {
    let mut out: Vec<Value> = vec![];
    for t in turns {
        let (role, content) = match t {
            Turn::User(text) => ("user", json!(text)),
            Turn::Assistant { raw: Some(raw), .. } => ("assistant", raw.clone()),
            Turn::Assistant { text, .. } => ("assistant", json!(text)),
            Turn::ToolResults(rs) => (
                "user",
                rs.iter()
                    .map(|r| json!({"type": "tool_result", "tool_use_id": r.id, "content": r.text, "is_error": r.is_error}))
                    .collect(),
            ),
        };
        // Tool results followed by a question become one user message.
        match out.last_mut() {
            Some(last) if last["role"] == role => {
                let mut blocks = blocks(last["content"].take());
                blocks.extend(self::blocks(content));
                last["content"] = Value::Array(blocks);
            }
            _ => out.push(json!({"role": role, "content": content})),
        }
    }
    out
}

/// Message content as a list of blocks (a plain string is one text block).
fn blocks(content: Value) -> Vec<Value> {
    match content {
        Value::Array(a) => a,
        Value::String(s) => vec![json!({"type": "text", "text": s})],
        _ => vec![],
    }
}

fn openai_messages(system: &str, turns: &[Turn]) -> Vec<Value> {
    let mut out = vec![json!({"role": "system", "content": system})];
    for t in turns {
        match t {
            Turn::User(text) => out.push(json!({"role": "user", "content": text})),
            Turn::Assistant { text, calls, .. } if calls.is_empty() => {
                out.push(json!({"role": "assistant", "content": text}));
            }
            Turn::Assistant { text, calls, .. } => out.push(json!({
                "role": "assistant",
                "content": if text.is_empty() { Value::Null } else { json!(text) },
                "tool_calls": calls.iter().map(|c| json!({
                    "id": c.id, "type": "function",
                    "function": {"name": c.name, "arguments": if c.input.is_null() { "{}".into() } else { c.input.to_string() }},
                })).collect::<Vec<_>>(),
            })),
            Turn::ToolResults(rs) => out.extend(
                rs.iter().map(|r| json!({"role": "tool", "tool_call_id": r.id, "content": tool_text(r)})),
            ),
        }
    }
    out
}

/// OpenAI's tool messages have no error flag; say it in the text.
fn tool_text(r: &ToolResult) -> String {
    if r.is_error { format!("Error: {}", r.text) } else { r.text.clone() }
}

/// More tool calls in one answer are ignored.
const MAX_CALLS: usize = 64;

/// Turns the lines of one streamed response into events, and assembles the whole message.
pub struct Stream {
    api: Api,
    text: String,
    /// Anthropic: the content blocks by index; tool inputs arrive as JSON pieces in `inputs`.
    blocks: Vec<Value>,
    inputs: Vec<String>,
    /// Blocks whose tool input was not valid JSON (sent back as `{}`).
    bad_input: Vec<bool>,
    /// OpenAI: tool calls by index (id, name, arguments so far).
    calls: Vec<(String, String, String)>,
    /// The answer stopped at `max_tokens`.
    cut_off: bool,
}

impl Stream {
    pub fn new(api: Api) -> Self {
        Self {
            api,
            text: String::new(),
            blocks: vec![],
            inputs: vec![],
            bad_input: vec![],
            calls: vec![],
            cut_off: false,
        }
    }

    /// The event in this line, if any (`event:` lines, comments and blanks carry none).
    pub fn line(&mut self, line: &str) -> Option<Event> {
        let data = line.strip_prefix("data:")?.trim();
        if self.api == Api::OpenAi && data == "[DONE]" {
            return Some(Event::Done);
        }
        let v: Value = serde_json::from_str(data).ok()?;
        if let Some(e) = v.get("error") {
            return Some(Event::Error(error_message(e)));
        }
        let event = match self.api {
            Api::Anthropic => self.anthropic(&v),
            Api::OpenAi => self.openai(&v),
        };
        match &event {
            Some(Event::Text(t)) => self.text.push_str(t),
            Some(Event::Note(n)) => {
                self.cut_off = true;
                self.text.push_str(&format!("\n\n{n}"));
            }
            _ => {}
        }
        event
    }

    /// Nothing arrived: no text and no tool call.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
            && self.blocks.iter().all(|b| b["type"] != "tool_use")
            && self.calls.iter().all(|c| c.1.is_empty())
    }

    /// The assistant turn received so far. An answer cut off at `max_tokens` keeps only its
    /// text: its last block (thinking without a signature, a tool call's input) may be
    /// incomplete, and the API refuses such blocks when they are sent back.
    pub fn into_turn(self) -> Turn {
        if self.cut_off {
            return Turn::answer(self.text);
        }
        match self.api {
            Api::Anthropic => {
                let blocks = self.blocks.iter().enumerate().filter(|(_, b)| !b.is_null());
                let calls = blocks
                    .clone()
                    .filter(|(_, b)| b["type"] == "tool_use")
                    .map(|(i, b)| ToolCall {
                        id: b["id"].as_str().unwrap_or_default().into(),
                        name: b["name"].as_str().unwrap_or_default().into(),
                        input: if self.bad_input.get(i) == Some(&true) { Value::Null } else { b["input"].clone() },
                    })
                    .collect();
                let raw: Vec<Value> = blocks.map(|(_, b)| b.clone()).collect();
                Turn::Assistant { text: self.text.clone(), calls, raw: (!raw.is_empty()).then_some(Value::Array(raw)) }
            }
            Api::OpenAi => Turn::Assistant {
                text: self.text.clone(),
                calls: self
                    .calls
                    .iter()
                    .enumerate()
                    .filter(|(_, (_, name, _))| !name.is_empty())
                    .map(|(i, (id, name, args))| ToolCall {
                        id: if id.is_empty() { format!("call_{i}") } else { id.clone() },
                        name: name.clone(),
                        input: if args.trim().is_empty() {
                            json!({})
                        } else {
                            serde_json::from_str(args).unwrap_or(Value::Null)
                        },
                    })
                    .collect(),
                raw: None,
            },
        }
    }

    fn anthropic(&mut self, v: &Value) -> Option<Event> {
        let index = v["index"].as_u64().unwrap_or(0) as usize;
        match v.get("type")?.as_str()? {
            "content_block_start" => {
                if self.blocks.len() <= index {
                    self.blocks.resize(index + 1, Value::Null);
                    self.inputs.resize(index + 1, String::new());
                    self.bad_input.resize(index + 1, false);
                }
                self.blocks[index] = v["content_block"].clone();
                None
            }
            "content_block_delta" => {
                let d = &v["delta"];
                let field = match d["type"].as_str()? {
                    "text_delta" => "text",
                    "thinking_delta" => "thinking",
                    "signature_delta" => "signature",
                    "input_json_delta" => {
                        if let Some(s) = self.inputs.get_mut(index) {
                            s.push_str(d["partial_json"].as_str().unwrap_or_default());
                        }
                        return None;
                    }
                    _ => return None,
                };
                let piece = d[field].as_str().unwrap_or_default();
                if let Some(b) = self.blocks.get_mut(index).filter(|b| b.is_object()) {
                    let joined = format!("{}{piece}", b[field].as_str().unwrap_or_default());
                    b[field] = json!(joined);
                }
                // Thinking and other block types carry no answer text.
                (field == "text").then(|| Event::Text(piece.into()))
            }
            "content_block_stop" => {
                let input = self.inputs.get(index).cloned().unwrap_or_default();
                if let Some(b) = self.blocks.get_mut(index).filter(|b| b["type"] == "tool_use") {
                    match input.trim() {
                        "" => b["input"] = json!({}),
                        s => match serde_json::from_str::<Value>(s) {
                            Ok(i) => b["input"] = i,
                            Err(_) => {
                                b["input"] = json!({});
                                self.bad_input[index] = true;
                            }
                        },
                    }
                }
                None
            }
            "message_delta" => match v["delta"]["stop_reason"].as_str()? {
                "max_tokens" => Some(Event::Note("(answer cut off: max_tokens)".into())),
                "refusal" => Some(Event::Error("The model declined to answer".into())),
                _ => None,
            },
            "message_stop" => Some(Event::Done),
            _ => None,
        }
    }

    fn openai(&mut self, v: &Value) -> Option<Event> {
        let choice = v.get("choices")?.get(0)?;
        for c in choice["delta"]["tool_calls"].as_array().into_iter().flatten() {
            let id = c["id"].as_str().filter(|id| !id.is_empty());
            let i = match (c["index"].as_u64(), id) {
                (Some(i), _) => usize::try_from(i).unwrap_or(usize::MAX),
                // No index (some compatible servers): a new id is a new call, else the last one.
                (None, Some(id)) => self.calls.iter().position(|x| x.0 == id).unwrap_or(self.calls.len()),
                (None, None) => self.calls.len().saturating_sub(1),
            };
            if i >= MAX_CALLS {
                continue;
            }
            if self.calls.len() <= i {
                self.calls.resize(i + 1, Default::default());
            }
            let call = &mut self.calls[i];
            if let Some(id) = id {
                call.0 = id.into();
            }
            let f = &c["function"];
            // The name comes whole (some servers repeat it in every piece); only the
            // arguments arrive in pieces.
            if let Some(name) = f["name"].as_str().filter(|n| !n.is_empty()) {
                call.1 = name.into();
            }
            match &f["arguments"] {
                Value::String(a) => call.2.push_str(a),
                // Some local servers send the arguments as an object.
                a @ Value::Object(_) => call.2 = a.to_string(),
                _ => {}
            }
        }
        if let Some(t) = choice["delta"]["content"].as_str().filter(|t| !t.is_empty()) {
            return Some(Event::Text(t.into()));
        }
        (choice["finish_reason"] == "length").then(|| Event::Note("(answer cut off: max_tokens)".into()))
    }
}

/// `{"message": "…"}`, or a bare string as some compatible servers send.
fn error_message(e: &Value) -> String {
    e.get("message").and_then(Value::as_str).or(e.as_str()).unwrap_or("the API reported an error").to_string()
}

/// What to tell the user about a failed HTTP request.
pub fn http_error(status: u16, body: &str) -> String {
    let detail = serde_json::from_str::<Value>(body).ok().and_then(|v| v.get("error").map(error_message));
    let hint = match status {
        401 | 403 => " Check the API key (plugin set ai api_key …).",
        404 => " Check base_url and model.",
        _ => "",
    };
    match detail {
        Some(d) => format!("{d} (HTTP {status}).{hint}"),
        None => format!("HTTP {status}.{hint}"),
    }
}

/// Worth one retry: rate limits, overload and server errors.
pub fn retryable(status: u16) -> bool {
    status == 429 || status == 529 || (500..600).contains(&status)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(api: &str) -> Config {
        Config::from_settings(&json!({"api": api, "base_url": "https://h/v1", "model": "m", "api_key": "k"}))
    }

    fn turns() -> Vec<Turn> {
        vec![Turn::User("q1".into()), Turn::answer("a1"), Turn::User("q2".into())]
    }

    fn tool_turns() -> Vec<Turn> {
        let raw = json!([
            {"type": "thinking", "thinking": "", "signature": "sig"},
            {"type": "tool_use", "id": "t1", "name": "fs__read", "input": {"p": "a"}},
        ]);
        vec![
            Turn::User("q".into()),
            Turn::Assistant {
                text: String::new(),
                calls: vec![ToolCall { id: "t1".into(), name: "fs__read".into(), input: json!({"p": "a"}) }],
                raw: Some(raw),
            },
            Turn::ToolResults(vec![ToolResult { id: "t1".into(), text: "nope".into(), is_error: true }]),
            Turn::User("then?".into()),
        ]
    }

    fn tools() -> Vec<ToolDef> {
        vec![ToolDef { name: "fs__read".into(), description: "Read".into(), schema: json!({"type": "object"}) }]
    }

    fn header<'a>(r: &'a Request, k: &str) -> Option<&'a str> {
        r.headers.iter().find(|(h, _)| h == k).map(|(_, v)| v.as_str())
    }

    #[test]
    fn anthropic_request() {
        let r = request(&cfg("anthropic"), &turns(), &[]);
        assert_eq!(r.url, "https://h/v1/v1/messages");
        assert_eq!(header(&r, "x-api-key"), Some("k"));
        assert_eq!(header(&r, "anthropic-version"), Some("2023-06-01"));
        assert_eq!(header(&r, "authorization"), None);
        assert_eq!(r.body["max_tokens"], ANTHROPIC_MAX_TOKENS);
        assert_eq!(r.body["stream"], true);
        assert_eq!(r.body["system"], crate::config::DEFAULT_SYSTEM);
        assert_eq!(r.body["messages"][1], json!({"role": "assistant", "content": "a1"}));
        assert!(r.body.get("output_config").is_none() && r.body.get("thinking").is_none());
        assert!(r.body.get("tools").is_none(), "no tools unless offered");
        let mut c = cfg("anthropic");
        c.effort = Some("low".into());
        assert_eq!(request(&c, &turns(), &[]).body["output_config"], json!({"effort": "low"}));
    }

    #[test]
    fn openai_request() {
        let r = request(&cfg("openai"), &turns(), &[]);
        assert_eq!(r.url, "https://h/v1/chat/completions");
        assert_eq!(header(&r, "authorization"), Some("Bearer k"));
        assert_eq!(r.body["messages"][0]["role"], "system");
        assert_eq!(r.body["messages"].as_array().unwrap().len(), 4);
        assert!(r.body.get("max_tokens").is_none(), "server default unless set");
        let mut c = cfg("openai");
        c.api_key = None;
        c.max_tokens = 100;
        let r = request(&c, &turns(), &[]);
        assert_eq!(header(&r, "authorization"), None, "local servers need no key");
        assert_eq!(r.body["max_tokens"], 100);
    }

    #[test]
    fn anthropic_tool_round_trip() {
        let r = request(&cfg("anthropic"), &tool_turns(), &tools());
        assert_eq!(
            r.body["tools"],
            json!([{"name": "fs__read", "description": "Read", "input_schema": {"type": "object"}}])
        );
        let m = &r.body["messages"];
        assert_eq!(m.as_array().unwrap().len(), 3, "results and the next question merge: {m}");
        assert_eq!(m[1]["content"][0]["signature"], "sig", "thinking goes back unchanged");
        assert_eq!(
            m[2]["content"],
            json!([
                {"type": "tool_result", "tool_use_id": "t1", "content": "nope", "is_error": true},
                {"type": "text", "text": "then?"},
            ])
        );
    }

    #[test]
    fn openai_tool_round_trip() {
        let r = request(&cfg("openai"), &tool_turns(), &tools());
        assert_eq!(r.body["tools"][0]["function"]["parameters"], json!({"type": "object"}));
        let m = &r.body["messages"];
        assert_eq!(m[2]["content"], Value::Null);
        assert_eq!(m[2]["tool_calls"][0]["function"], json!({"name": "fs__read", "arguments": "{\"p\":\"a\"}"}));
        assert_eq!(m[3], json!({"role": "tool", "tool_call_id": "t1", "content": "Error: nope"}));
        assert_eq!(m[4]["role"], "user");
    }

    fn turn(api: Api, sse: &str) -> (Vec<Event>, Turn) {
        let mut s = Stream::new(api);
        let ev = sse.lines().filter_map(|l| s.line(l)).collect();
        (ev, s.into_turn())
    }

    #[test]
    fn anthropic_stream_assembles_tool_calls() {
        let sse = r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}
data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hm"}}
data: {"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"abc"}}
data: {"type":"content_block_stop","index":0}
data: {"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}
data: {"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Looking"}}
data: {"type":"content_block_stop","index":1}
data: {"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"tu1","name":"fs__read","input":{}}}
data: {"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"p\":"}}
data: {"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"\"x\"}"}}
data: {"type":"content_block_stop","index":2}
data: {"type":"content_block_start","index":3,"content_block":{"type":"tool_use","id":"tu2","name":"fs__list","input":{}}}
data: {"type":"content_block_stop","index":3}
data: {"type":"content_block_start","index":4,"content_block":{"type":"tool_use","id":"tu3","name":"fs__list","input":{}}}
data: {"type":"content_block_delta","index":4,"delta":{"type":"input_json_delta","partial_json":"{\"p\""}}
data: {"type":"content_block_stop","index":4}
data: {"type":"message_delta","delta":{"stop_reason":"tool_use"}}
data: {"type":"message_stop"}"#;
        let (ev, t) = turn(Api::Anthropic, sse);
        assert_eq!(ev, [Event::Text("Looking".into()), Event::Done], "tool_use is no error");
        let Turn::Assistant { text, calls, raw } = t else { panic!() };
        assert_eq!(text, "Looking");
        assert_eq!(
            calls,
            [
                ToolCall { id: "tu1".into(), name: "fs__read".into(), input: json!({"p": "x"}) },
                ToolCall { id: "tu2".into(), name: "fs__list".into(), input: json!({}) },
                ToolCall { id: "tu3".into(), name: "fs__list".into(), input: Value::Null },
            ]
        );
        let raw = raw.unwrap();
        assert_eq!(raw[0], json!({"type": "thinking", "thinking": "hm", "signature": "abc"}));
        assert_eq!(raw[1], json!({"type": "text", "text": "Looking"}));
        assert_eq!(
            raw[4],
            json!({"type": "tool_use", "id": "tu3", "name": "fs__list", "input": {}}),
            "valid to send back"
        );
    }

    #[test]
    fn openai_tool_calls_without_index_or_with_repeated_names() {
        let sse = r#"data: {"choices":[{"delta":{"tool_calls":[{"id":"a","function":{"name":"fs__read","arguments":"{}"}}]}}]}
data: {"choices":[{"delta":{"tool_calls":[{"id":"b","function":{"name":"fs__list","arguments":"{\"d\":"}}]}}]}
data: {"choices":[{"delta":{"tool_calls":[{"function":{"name":"fs__list","arguments":"1}"}}]}}]}
data: {"choices":[{"delta":{"tool_calls":[{"index":99999999999,"id":"x","function":{"name":"big"}}]}}]}
data: [DONE]"#;
        let Turn::Assistant { calls, .. } = turn(Api::OpenAi, sse).1 else { panic!() };
        assert_eq!(
            calls,
            [
                ToolCall { id: "a".into(), name: "fs__read".into(), input: json!({}) },
                ToolCall { id: "b".into(), name: "fs__list".into(), input: json!({"d": 1}) },
            ]
        );
    }

    #[test]
    fn a_cut_off_answer_keeps_only_its_text() {
        let sse = r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}
data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hmm"}}
data: {"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}
data: {"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Partial"}}
data: {"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"t","name":"fs__read","input":{}}}
data: {"type":"message_delta","delta":{"stop_reason":"max_tokens"}}
data: {"type":"message_stop"}"#;
        let t = turn(Api::Anthropic, sse).1;
        assert_eq!(t, Turn::answer("Partial\n\n(answer cut off: max_tokens)"));
    }

    #[test]
    fn openai_stream_assembles_tool_calls() {
        let sse = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","type":"function","function":{"name":"fs__read","arguments":""}}]}}]}
data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"p\":"}}]}}]}
data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"x\"}"}}]}}]}
data: {"choices":[{"delta":{"tool_calls":[{"index":1,"function":{"name":"fs__list","arguments":{"d":1}}}]}}]}
data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}
data: [DONE]"#;
        let (ev, t) = turn(Api::OpenAi, sse);
        assert_eq!(ev, [Event::Done]);
        let Turn::Assistant { calls, raw, .. } = t else { panic!() };
        assert_eq!(raw, None);
        assert_eq!(
            calls,
            [
                ToolCall { id: "c1".into(), name: "fs__read".into(), input: json!({"p": "x"}) },
                ToolCall { id: "call_1".into(), name: "fs__list".into(), input: json!({"d": 1}) },
            ]
        );
    }

    fn events(api: Api, sse: &str) -> Vec<Event> {
        let mut s = Stream::new(api);
        sse.lines().filter_map(|l| s.line(l)).collect()
    }

    #[test]
    fn anthropic_stream() {
        let sse = r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_1"}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}
data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hmm"}}
event: ping
data: {"type": "ping"}
data: {"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Hel"}}
data: {"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"lo"}}
data: not json
data: {"type":"message_delta","delta":{"stop_reason":"max_tokens"},"usage":{"output_tokens":3}}
data: {"type":"message_stop"}"#;
        assert_eq!(
            events(Api::Anthropic, sse),
            [
                Event::Text("Hel".into()),
                Event::Text("lo".into()),
                Event::Note("(answer cut off: max_tokens)".into()),
                Event::Done
            ]
        );
        let refusal = r#"data: {"type":"message_delta","delta":{"stop_reason":"refusal"}}"#;
        assert!(matches!(events(Api::Anthropic, refusal)[..], [Event::Error(_)]));
        let err = r#"data: {"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#;
        assert_eq!(events(Api::Anthropic, err), [Event::Error("Overloaded".into())]);
    }

    #[test]
    fn openai_stream() {
        let sse = r#"data: {"choices":[{"delta":{"role":"assistant"}}]}
data: {"choices":[{"delta":{"content":"Hi"}}]}
: keep-alive
data: {"choices":[{"delta":{"content":" there"},"finish_reason":null}]}
data: {"choices":[{"delta":{},"finish_reason":"length"}]}
data: [DONE]"#;
        assert_eq!(
            events(Api::OpenAi, sse),
            [
                Event::Text("Hi".into()),
                Event::Text(" there".into()),
                Event::Note("(answer cut off: max_tokens)".into()),
                Event::Done
            ]
        );
        assert_eq!(
            events(Api::OpenAi, r#"data: {"error":"model not found"}"#),
            [Event::Error("model not found".into())]
        );
    }

    #[test]
    fn http_errors_say_what_to_check() {
        let m = http_error(
            401,
            r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#,
        );
        assert!(m.starts_with("invalid x-api-key (HTTP 401).") && m.contains("api_key"), "{m}");
        assert_eq!(http_error(500, "<html>"), "HTTP 500.");
        assert!(http_error(404, "{}").contains("base_url and model"));
        assert!(retryable(429) && retryable(529) && retryable(503) && !retryable(400) && !retryable(404));
    }
}
