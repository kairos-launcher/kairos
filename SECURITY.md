# Security

## Reporting a vulnerability

Please report security problems privately through GitHub's "Report a vulnerability" button on
the repository's Security tab, not in a public issue. You should get an answer within a week.

## Threat model

What the launcher trusts, and what it defends against:

| Source | Trust | What protects you |
|---|---|---|
| Bundled plugins | Part of the launcher | Code review |
| External (stdio) plugins | **Trusted code, running as you, no sandbox** | You decide what to install; `plugin install` shows the command a plugin runs before installing it |
| Remote (HTTPS) plugins and their replies | **Untrusted** | HTTPS only, no redirects (the API token cannot leave the host), host-performed actions limited to `open_url` (http/https/mailto), `copy` and `set_query`, local icon paths dropped, size limits |
| Results from any plugin | Untrusted data | Size caps per result, per batch and per reply line |
| Other users on the same machine | Untrusted | Control socket in a private (0700) folder, history files 0600, world-writable plugin folders ignored |
| Release downloads (self-update) | Trusted once checked | HTTPS to GitHub; the file must match the release's `SHA256SUMS`. Both come from the same release, so this guards against broken or swapped downloads, not a compromised GitHub account; the provenance attestation is not checked. Only release-workflow builds update, only into a folder you can write |
| Plugin sources given to `plugin install` | Semi-trusted | Only `https://`/SSH git sources; the URL cannot become a git option; clones are reviewed before install |

Files and secrets:

- API tokens for remote plugins: `KAIROS_TOKEN_<ID>` or `<config>/secrets/<id>` (0600, checked
  on read). They are sent only in request headers and are not passed on to launched programs.
- `secret` plugin settings (API keys, e.g. the AI plugin's): `<config>/secrets/<id>.<key>`, and
  `<id>.<key>.<name>` for named `secrets` entries (0600, checked on read), set with `plugin set`,
  never read from or written to `config.toml`. Each one reaches only its own plugin, inside
  `initialize`, not through the environment.
- The AI plugin sends your questions and its key only to the `base_url` you configure, over
  `https://` (plain `http://` only to localhost), and only for the row you choose: typing alone
  sends nothing.
- MCP servers configured for the AI plugin (`mcp_servers`) run as you, like plugins, and what
  their tools return goes to the model, which may be steered by it (prompt injection). So a tool
  asks before it runs unless it is listed in `allow`; allow only tools that cannot do harm.
  Remote servers get the headers you configure; `${name}` (a named secret, or an environment
  variable) keeps tokens out of `config.toml`.
- Launch and command history (`frecency.json`, `query_picks.json`, `shell_history.json`) live in the state folder
  (`~/.local/state/kairos`), readable only by you, since typed commands can contain secrets.

Known limits:

- External plugins are not sandboxed.
- `KAIROS_PLUGINS` and `KAIROS_HOME` change where plugins and config are loaded from;
  anything that can set your environment can already run code as you.
- On Windows, custom actions with `shell = true` run through `cmd`, whose quoting cannot stop
  `%VAR%` expansion: do not use `{}` placeholders with `shell = true` there.
