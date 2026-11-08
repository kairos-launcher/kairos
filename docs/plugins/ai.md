# AI

Ask an AI and get the answer streamed into the answer panel, with follow-up questions. Id `ai`,
trigger `ai `. Works with the **Anthropic** Messages API or any **OpenAI-compatible** chat API
(OpenAI, OpenRouter, Ollama, LM Studio, …).

## Commands

| Type | Does |
|---|---|
| `ai what is a lifetime` | Ask a question. |
| anything | Once set up, every plain search ends with an "Ask AI: …" row. Nothing is sent until you choose it. |

In the answer panel:

| Key | Does |
|---|---|
| type + **Enter** | Ask a follow-up in the same conversation. |
| **Enter** on an empty box | Copy the last answer. |
| **Ctrl+C** | Stop the answer, then go back to the results. |

## Setup

Nothing is preset. Until the plugin is configured, `ai …` shows what is missing and Enter opens its
settings page. Set:

| Provider | `api` | `base_url` | `model` (example) |
|---|---|---|---|
| Anthropic | `anthropic` | `https://api.anthropic.com` | `claude-opus-5-5` |
| OpenAI | `openai` | `https://api.openai.com/v1` | `gpt-5` |
| Ollama | `openai` | `http://localhost:11434/v1` | `llama3.2` |

…and the API key (optional for local servers). Plain `http://` is only accepted for this machine.

## Settings

| Key | Type | Default | Meaning |
|---|---|---|---|
| `api` | choice | | Required. `anthropic` or `openai` (also OpenAI-compatible servers). |
| `base_url` | string | | Required. API address, see above. |
| `model` | string | | Required. Model id. |
| `api_key` | secret | | Required for Anthropic; optional for OpenAI-compatible servers. Stored privately, never in `config.toml`; see [Secrets](/guide/secrets). |
| `system_prompt` | string | `""` | Empty: answer briefly, in plain text. |
| `effort` | choice | `""` | Anthropic only: `low`, `medium`, `high`, `xhigh`, `max`. Empty: the model's default. |
| `max_tokens` | number | `0` | Longest answer. `0`: 16000 for Anthropic, the server's default for OpenAI. |
| `secrets` | secrets | | Named tokens for MCP servers, used as `${name}`. |
| `mcp_servers` | table | `{}` | Tools the AI may use, see below. |

## Tools (MCP)

The AI can use tools from [MCP](https://modelcontextprotocol.io) servers: local ones it starts
(`command`) and remote ones over HTTP (`url`).

```toml
[plugins.ai.mcp_servers.files]                  # its tools are shown as files.<tool>
command = ["npx", "-y", "@modelcontextprotocol/server-filesystem", "/home/me/notes"]
env = { LOG_LEVEL = "warn" }                    # optional, added to the environment
allow = ["read_text_file", "list_directory"]    # optional: these run without asking ("*" = all)

[plugins.ai.mcp_servers.github]
url = "https://api.githubcopilot.com/mcp/"      # https://, or http:// on this machine
headers = { Authorization = "Bearer ${github_token}" }
```

Store the token with `plugin set ai secrets github_token <token>` (or under **Secrets** on the
settings page), see [named secrets](/guide/secrets#named-secrets).

| Field | Meaning |
|---|---|
| `command` | A local server: the program, then its arguments. |
| `url` | Or a remote server. |
| `env` | Added to a local server's environment; `${name}` uses a secret. |
| `headers` | Sent to a remote server. |
| `allow` | Tools that run without asking; `*` allows all. |

- **Asking first.** A tool not in `allow` pauses the answer: the panel shows the call and Enter
  runs it; Tab offers **Always allow** (until the plugin stops) and **Don't run**.
- **When servers run.** They start with the first question, never while you type, and stop with
  the plugin (10 minutes after the last use). Local servers on Node can use a lot of memory
  while they run.
- **Limits.** A tool call has 45 seconds; results over 32 KiB are cut off; one question makes at
  most 10 rounds of tool calls.

### Signing in (OAuth)

Many remote servers (Linear, Notion, Sentry, …) let you sign in in the browser
instead of pasting a token. Add them with just the url:

```toml
[plugins.ai.mcp_servers.tracker]
url = "https://tracker.example.com/mcp"
```

The first question that needs the server shows *⚠ MCP server tracker: sign in to use its
tools* and a **Sign in to tracker** action (Tab). It opens the browser; approve there, and the
next question can use the server. `ai sign in` lists every such server with **Sign in** or
**Sign out**, to sign in ahead of time or switch accounts.

- Tokens are kept in the launcher's private folder for the plugin
  (`~/.local/state/kairos/plugins/ai/oauth.json`, readable only by you) and renewed on their own.
- A server with an `Authorization` header in its config uses that header and never signs in.
- The server must let apps register themselves (dynamic client registration). Servers that
  need an OAuth app you create first (GitHub, Slack) are not supported yet: use a token in
  `headers` for those.

### From Claude

Servers you already set up for Claude (`.mcp.json`, `claude_desktop_config.json`) can be
pasted as they are:

```
plugin set ai mcp_servers {"mcpServers": {"files": {"command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "/home/me/notes"]}}}
```

or paste the JSON into the **MCP servers** box on the settings page and press Add. They are
converted to the TOML above (`command` and `args` become one `command` list; `env`, `url` and
`headers` are kept) and added next to the servers you have; one with the same name is
replaced. VS Code's `{"servers": …}` works too. SSE servers are not supported.

## Notes

Conversations live in the plugin's process: they are gone once it stops (10 minutes after the last
use). A follow-up to a lost conversation starts a new one.
