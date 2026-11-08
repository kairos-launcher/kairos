# Remote plugins (HTTPS)

**Example:** [`examples/plugins/echo-remote`](https://github.com/kairos-launcher/kairos/tree/main/examples/plugins/echo-remote), a small Python server with the matching `plugin.toml`.

A plugin can be a web service instead of a local program: `run.url` replaces `run.exec`, and the same JSON-RPC messages travel as HTTPS `POST`s. Nothing is installed or executed locally, so a remote plugin cannot harm your machine, but it **receives the text you type** into it.

```toml
id = "wiki"
name = "Company wiki"
version = "0.1.0"

[run]
url = "https://wiki.example.com/launcher/rpc"   # https only, no credentials in the URL

[remote]
auth = "bearer"        # bearer (Authorization: Bearer <token>) | header | none
# header = "X-Api-Key" # header name for auth = "header"

[query]
triggers = ["wiki "]
# global = true        # default false: only when its trigger is typed
# timeout_ms = 800     # default for remote plugins (150 for local ones)
# min_chars = 2        # send nothing for shorter queries
```

- **Token.** Never in the manifest or `config.toml`. The launcher reads `KAIROS_TOKEN_<ID>` (id upper-cased, `-` becomes `_`) or `<config dir>/secrets/<id>`, which must not be readable by others. Store one with `printf %s "$TOKEN" | kairos --set-token <id>`. It is sent only as a header, never in a body.
- **Private CA.** Servers are verified against the OS trust store, so a company CA installed on the machine (or listed in `SSL_CERT_FILE` on Linux) just works. To trust a CA only for remote plugins, set `ca_file = "/path/to/ca.pem"` in `[plugins."<id>"]` in `config.toml` (it is not a manifest field, so a plugin cannot choose its own trust). A top-level `ca_file = "..."` in `config.toml` applies to all remote plugins; a plugin's own setting wins. The file's certificates then replace the OS trust store for that plugin; if it cannot be read, every request fails with that error.
- **Global search.** Remote plugins are trigger-only by default. Set `global = true` in `[plugins."<id>"]` in `config.toml` to send every global search there too.
- **Requests.** `POST <url>` with a JSON-RPC body, exactly the methods of the protocol reference (`initialize` is sent once, then `query`, `execute`, `icon`). A reply is either `application/json` (one response) or `application/x-ndjson`: any number of `results` notifications (`{"method":"results","params":{"qid":..,"results":[..],"done":..}}`), one per line, then the response. Results shown as they arrive. A `Kairos-Session` response header is sent back on later requests.
- **Typing.** The launcher waits 120 ms after a keystroke and drops the request if you typed again; a newer query also abandons an older one that is still streaming.
- **Limits.** Replies are capped at 1 MiB (each line, for NDJSON); at most `limit` results are used; a result's `icon.path` is ignored (inline `base64`, `glyph` or an `id` fetched with the `icon` request are fine). Redirects are not followed. After a failure the plugin is skipped for 2, 4, 8, ... up to 60 seconds. HTTP 401/403 report a token problem.
- **Actions.** A remote plugin cannot run local commands. Instead an action can carry a `kind`, which the launcher performs itself without a round trip:

  | `kind` | `value` | Effect |
  |---|---|---|
  | `call` (default) | | `execute` request to the plugin |
  | `open_url` | link | open in the browser; only `https`, `http`, `mailto` |
  | `copy` | text | put on the clipboard |
  | `set_query` | text | replace the search box text (drill-down, or a confirmation step) |
  | `settings` | | open this plugin's settings page (e.g. from a "not set up" row) |

  The `execute` reply also accepts `"open_url": "<link>"` besides `copy` and `hide`. `kind` works for local plugins too. Example: `examples/plugins/echo-remote/`.
