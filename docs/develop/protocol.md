# Protocol reference

JSON-RPC 2.0, one JSON message per line, UTF-8, over the plugin's **stdin/stdout**. A message must not contain a raw newline. **stdout is protocol only**; write logs and debugging output to **stderr** (the launcher passes it through to its own stderr). Non-JSON lines on stdout are ignored.

The examples on this page are formatted for reading. On the wire, each message must be sent as one line.

The launcher sends requests; the plugin replies with a result (or an `error` object) carrying the same `id`. The plugin only sends notifications for streaming results.

### `initialize` (request, first message)

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "initialize",
  "params": {
    "api": "0.1",
    "host": "kairos",
    "settings": {
      "sides": 6
    },
    "locale": "en"
  }
}
```

`data_dir` (when present) is a folder for the plugin's own files, readable only by the user
(`<state>/plugins/<id>`, e.g. `~/.local/state/kairos/plugins/<id>`): keep what must survive a
restart there, such as sign-in tokens. It is outside the plugin folder, so updates keep it.

Reply with anything, conventionally `{"api":"0.1","capabilities":{"streaming":false,"actions":true}}`. The launcher waits up to 3 seconds for the reply. `settings` holds the effective settings: the manifest defaults overridden by the user's values from `config.toml`. When the user changes a setting, the plugin is stopped and started again with the new values, so reading them once in `initialize` is enough.

### `query` (request)

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "query",
  "params": {
    "qid": 7,
    "text": "20",
    "trigger": "roll ",
    "limit": 8
  }
}
```

| Field | Meaning |
|---|---|
| `qid` | Increases with each query. Echo it in streamed `results` notifications. |
| `text` | What the user typed, with any trigger removed. |
| `trigger` | The trigger that matched, or `null` for a global search. |
| `limit` | Soft cap on useful results. |

Reply: `{"results":[Result, ...]}`. If the reply is not in within the query's `timeout_ms`, the launcher shows results without it for that keystroke (the plugin keeps running).

**Streaming** (for slow sources): reply `{"results":[],"streaming":true}`, then send notifications until you are done:

```json
{
  "jsonrpc": "2.0",
  "method": "results",
  "params": {
    "qid": 7,
    "results": [...],
    "done": false
  }
}

{
  "jsonrpc": "2.0",
  "method": "results",
  "params": {
    "qid": 7,
    "results": [...],
    "done": true
  }
}
```

Everything must arrive within the query's `timeout_ms`; notifications for an old `qid` are ignored.

### `$/cancel` (notification)

```json
{
  "jsonrpc": "2.0",
  "method": "$/cancel",
  "params": {
    "qid": 7
  }
}
```

Sent when the user typed again while query `qid` was still running. The launcher has already stopped waiting for it: any reply or `results` you still send for it are ignored, and the next `query` follows right away. Stop the work for that `qid` if you can. Ignoring the notification is allowed, but then the next query waits until your old work is done.

### Result

```json
{
  "id": "roll:20:13",
  "title": "🎲 13",
  "subtitle": "d20",
  "icon": {
    "glyph": "D"
  },
  "score": 0.9,
  "actions": [
    {
      "id": "copy",
      "title": "Copy result",
      "default": true
    },
    {
      "id": "again",
      "title": "Roll again"
    }
  ]
}
```

| Field | Required | Notes |
|---|---|---|
| `id` | yes | Unique within the plugin; also the frecency key, so keep it stable for "the same thing". |
| `title` | yes | Main line. |
| `subtitle` | no | Second line. |
| `icon` | no | How to draw the row's icon; one of `{"glyph":"D"}` (a character in a tile), `{"path":"icon.png"}` (PNG, JPEG or SVG file, relative to the plugin folder), `{"base64":"…","mime":"image/png"}` (inline PNG or JPEG), or `{"id":"blue"}` (lazy, see below). No icon: the first letter of the title. See [Icons](./writing-plugins#icons). |
| `score` | no | 0 to 1, relative to your own results (default 0.5). The launcher adds the frecency boost. |
| `actions` | no | The action flagged `"default": true` (or the first one) runs on Enter. An action with `"output": true` opens the [output panel](#output-panel) as soon as it is chosen. |
| `fallback` | no | `true` for a catch-all row such as "Ask AI: …" or "Search the web for …": it is listed after every other result, keeps its place when the list is full, and is never moved up by how often it is used. |
| `complete` | no | What **Tab** does for this result. Absent: Tab opens a menu of all `actions` (Up/Down, Enter to run, Tab/Left/Ctrl+C to close). Present: Tab replaces the whole search box with this text (include your trigger, e.g. `"kill firefox"`) and re-runs the search, like shell autocomplete. Each plugin chooses per result; the launcher's own command suggestions use it. |

### `icon` (request, only for lazy icons)

Inline `base64` icons are decoded once and cached by content, so repeating the same icon on every keystroke is cheap for the launcher, but it still makes every reply bigger. For many or large icons use a lazy one: give the result `"icon": {"id": "blue"}` and the launcher asks for the image separately, after the results are on screen, once per distinct id and only for the rows shown:

```json
{
  "jsonrpc": "2.0",
  "id": 4,
  "method": "icon",
  "params": {
    "icon_id": "blue",
    "size": 64
  }
}
```

Reply with `{"base64":"…","mime":"image/png"}` or `{"path":"blue.png"}`, or `null` if there is none (the row keeps its letter tile). You have 1 second. The answer is cached for the session, so later queries show the icon at once.

### `execute` (request)

Sent when the user activates a result.

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "execute",
  "params": {
    "result_id": "roll:20:13",
    "action_id": "copy",
    "query": "20"
  }
}
```

Reply `{"ok":true}`. Optional fields: `"copy": "text"` puts text on the clipboard (the launcher owns it, so it survives the plugin being stopped); `"hide": false` keeps the launcher open; `"output": {…}` shows text in the [output panel](#output-panel); `"ok": false` with a `"message"` reports a failure: the message is shown in the output panel (and logged to stderr). You have 10 seconds for the reply; streamed output may take longer. A successful execute also counts toward the result's frecency.

`params.exec` numbers the execution; it only matters for streamed output. Older plugins can ignore it.

### Output panel

An action can answer with text instead of closing the launcher: an AI answer, a lookup, a log. The result list makes way for a panel showing the text; it grows with the text up to the height of eight rows and then scrolls (Up/Down, PgUp/PgDn). Enter runs the panel's default action, Tab lists its actions, Ctrl+C stops a stream and then goes back to the results, and Esc closes the launcher (the panel, still streaming, comes back if it is reopened soon). The text is plain text (selectable with the mouse) or Markdown, at most 256 KiB.

Reply to `execute` with `output`:

```json
{
  "ok": true,
  "output": {
    "title": "Claude",
    "text": "Lifetimes tell…",
    "streaming": false,
    "actions": [
      {
        "id": "again",
        "title": "Regenerate",
        "default": true
      }
    ]
  }
}
```

| Field | Notes |
|---|---|
| `title` | Small heading above the text (default: the result's title). |
| `text` | The text so far. |
| `format` | `"text"` (default) or `"markdown"`. Markdown is rendered: bold, italics, strikethrough, inline code, links (opened in the browser on click: `https`, `http`, `mailto`), and lists as such; headings as bold lines, code blocks as code lines, quotes with a bar, tables as `a · b` rows, HTML as text. Markdown text cannot be selected with the mouse, so offer a copy action. Escape text you did not write (`\*`). Streamed text is re-rendered as it arrives, so half-finished markup is fine. |
| `streaming` | `true`: more follows as `output` notifications. |
| `actions` | The panel's actions, same shape as a result's. `copy`, `open_url` and `set_query` run in the launcher; a plugin action runs `execute` again (same `result_id` and `query`, new `exec`) and its output replaces the panel's. A **Copy** action (everything shown) is always added; it is the default when you give none. |

Mark the action `"output": true` in your results so the panel opens (showing "Working…") the moment it is chosen, instead of after your reply.

**Follow-ups.** Add `"input": {"placeholder": "Ask a follow-up…", "action": "followup:7"}` to `output` (or send it later in an `output` notification; `"input": null` removes it). The search box then takes text for you: Enter with text runs `execute` with `action_id` set to `input.action` and the text in `params.input`; Enter on an empty box runs the panel's default action as before. Encode whatever you need to continue (a conversation id) in the action id. The reply replaces the panel like any other output, so send the whole transcript as `text`; the old text stays on screen until it arrives.

**Streaming.** After replying with `"streaming": true`, send notifications carrying the `exec` from the request:

```json
{
  "jsonrpc": "2.0",
  "method": "output",
  "params": {
    "exec": 12,
    "append": "more text "
  }
}

{
  "jsonrpc": "2.0",
  "method": "output",
  "params": {
    "exec": 12,
    "text": "replaces everything so far"
  }
}

{
  "jsonrpc": "2.0",
  "method": "output",
  "params": {
    "exec": 12,
    "actions": [...]
  }
}

{
  "jsonrpc": "2.0",
  "method": "output",
  "params": {
    "exec": 12,
    "done": true
  }
}

{
  "jsonrpc": "2.0",
  "method": "output",
  "params": {
    "exec": 12,
    "error": "quota exceeded"
  }
}
```

`error` ends the stream like `done` and is shown in the panel's footer. When the user stops it (Ctrl+C), goes back, or closes the launcher and does not reopen it within `keep_search_s` (90 s), you get `{"jsonrpc":"2.0","method":"$/cancel","params":{"exec":12}}`; stop the work, later notifications are ignored. A stream with no notification for 60 seconds, or one running for 10 minutes, is stopped the same way. Notifications for any other `exec` are ignored.

Keep reading stdin while you stream (send from a thread, with a lock around stdout): the launcher keeps querying the same process, and the cancel must reach you. `examples/plugins/echo-py` ("Stream back") is a complete example.

Remote plugins can reply with `output`, but not stream it yet (`streaming` is ignored).

### `shutdown` (request)

Reply `{}` and exit. The launcher kills the process shortly afterwards regardless.

### Errors

Reply with `{"jsonrpc":"2.0","id":N,"error":{"code":-32601,"message":"unknown method"}}` for anything you do not support. Ignore unknown methods that have no `id`. An error reply, malformed protocol use, or the process exiting makes the launcher restart the plugin on its next query.

### Planned, not implemented yet

Enforcement of `[permissions]`.
