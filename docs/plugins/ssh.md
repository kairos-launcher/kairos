# SSH

Connect to hosts from your SSH config in a terminal. Id `builtin-ssh`, trigger `ssh `.

## Commands

| Type | Does |
|---|---|
| `ssh web1` | Hosts from `~/.ssh/config` matching `web1`, shown as `user@host:port`. Enter connects. |
| `ssh me@10.0.0.5` | An address not in the config connects to it directly. |

## Settings

| Key | Type | Default | Meaning |
|---|---|---|---|
| `terminal` | string | `""` | Terminal command that runs what follows it, e.g. `"kitty"` or `"gnome-terminal --"`. Empty: detect. |
| `config` | path | `"~/.ssh/config"` | File to read hosts from. |

## Notes

- `Include` lines are followed; wildcard patterns are skipped.
- The plugin turns itself off (no trigger) when no `ssh` client is installed.
