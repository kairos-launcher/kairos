# Secrets

API keys and tokens are never stored in `config.toml`, so the file can be shared or kept in a
dotfiles repository. Each secret is kept in a file of its own that only you can read.

A secret found in `config.toml` is ignored, with a warning.

## Setting a secret

**On the settings page** (`plugin`, **Tab** → **Settings**): secrets are password fields. Type the
value and press **Ctrl+S**. Clearing the field and saving removes it.

**By typing** in the launcher:

```text
plugin set ai api_key sk-ant-…
```

An empty value removes it. `plugin config ai` shows only whether a secret is set, never its value.

## Named secrets

Some plugins take any number of tokens that other settings refer to by name. The AI plugin's
**Secrets** are used as `${name}` in its MCP servers' `env` and `headers`:

```toml
[plugins.ai.mcp_servers.github]
url = "https://api.githubcopilot.com/mcp/"
headers = { Authorization = "Bearer ${github_token}" }
```

Store `github_token` under **Secrets** on the AI settings page, or:

```text
plugin set ai secrets github_token ghp_…
```

`plugin set ai secrets github_token` with no value removes it. `plugin config ai` lists only the
names.

## Remote plugin tokens

[Remote plugins](/develop/remote-plugins) that need a token get it from the command line, read
from stdin so it does not end up in your shell history:

```sh
printf %s "$TOKEN" | kairos --set-token wiki
```

Or set the environment variable `KAIROS_TOKEN_<ID>` (the id upper-cased, `-` becomes `_`, for
example `KAIROS_TOKEN_WIKI`) before starting the launcher; it wins over the stored token. Programs
the launcher starts never inherit these variables.

## Where secrets are stored

In the `secrets` folder of the config folder (see [Files and folders](./files)):

| File | Holds |
|---|---|
| `secrets/<id>.<key>` | A plugin's secret setting, e.g. `secrets/ai.api_key`. |
| `secrets/<id>.<key>.<name>` | One named secret, e.g. `secrets/ai.secrets.github_token`. |
| `secrets/<id>` | A remote plugin's token. |

Each file holds the value on one line. On Linux the folder is readable only by you (`700`) and the
files too (`600`); a secret file that others can read counts as unset (fix it with `chmod 600`).
On Windows the files are in your user profile, which other users cannot read.

To move your secrets to another machine, copy the folder. They are plain files: anyone who can
sign in as you can read them.
