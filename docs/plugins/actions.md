# Custom actions

Your own keyword commands, defined in `config.toml`. Id `builtin-actions`. Each keyword becomes a
command; the settings page edits them one by one.

## Commands

You define them. With the example below:

| Type | Does |
|---|---|
| `gh rust-lang/rust` | Opens `https://github.com/rust-lang/rust`. |
| `downloads` | Opens your Downloads folder (no parameter: found in plain searches). |
| `logs` / `logs sshd` | Follows a service's log in a terminal (`nginx` when nothing is typed). |

Actions: **Run** (Enter) and **Copy command**. The row shows the exact command that will run.

## Defining actions

```toml
[plugins.builtin-actions.actions.gh]
title = "GitHub repository"                  # optional, shown in the row (default: the keyword)
command = "xdg-open https://github.com/{}"   # required

[plugins.builtin-actions.actions.downloads]
command = "xdg-open ~/Downloads"             # no placeholder: runs on the keyword alone

[plugins.builtin-actions.actions.logs]
command = "journalctl -f -u {1}"
terminal = true                              # run in a terminal window
default = "nginx"                            # used when nothing is typed after the keyword
icon = "utilities-system-monitor"            # theme icon, app name ("firefox") or image path

[plugins.builtin-actions.actions.count]
command = "echo {} | wc -c > /tmp/count"
shell = true                                 # through sh -c: pipes, &&, $VARS, redirects
```

The table name (`gh`, `downloads`, …) is the keyword: one word.

| Field | Meaning |
|---|---|
| `command` | Required. The command to run, with placeholders. |
| `title` | Shown in the list; empty: the keyword. |
| `default` | Used when nothing is typed after the keyword. |
| `terminal` | Run in a terminal window. |
| `shell` | Run through `sh -c` (pipes, `&&`, `$VARS`); what you type is quoted. |
| `icon` | Theme icon name, app name or image path. |

| Placeholder | Becomes |
|---|---|
| `{}` or `{0}` | Everything typed after the keyword. |
| `{url}` | The same, URL-encoded (`a b&c` → `a%20b%26c`). |
| `{1}` … `{9}` | The 1st … 9th word; quotes group words (`"new york" 3`). |
| <code v-pre>{{</code> <code v-pre>}}</code> | Literal braces. |

- An action **with** a placeholder is reached through `<keyword> `. Without a parameter (and
  without `default`) it waits for one.
- An action **without** a placeholder appears in plain searches: typing its keyword ranks it
  first, so Enter runs it.
- Your keywords take precedence over the [Web Search](./web) engine keywords.

::: tip Typed text is never run by a shell
The command is split into arguments before placeholders are filled, so `gh foo; rm -rf ~` passes
`https://github.com/foo; rm -rf ~` to `xdg-open` as one argument. With `shell = true` the
parameter is quoted for the shell.
:::

**Windows:** `start <program> …` works, and programs that are not on PATH (browsers such as
`msedge`, URLs, documents) are opened through the shell API, so typed text is still not
interpreted: `command = "start msedge https://www.bing.com/search?q={url}"`.

## Settings

| Key | Type | Default | Meaning |
|---|---|---|---|
| `actions` | table | `{}` | Your keyword commands, as above. |
| `terminal` | string | `""` | Terminal command for actions with `terminal = true`, e.g. `"kitty"`. Empty: detect. |

An entry with a missing keyword or command, a keyword with spaces, an unknown placeholder or a
repeated keyword is skipped with a message on stderr.
