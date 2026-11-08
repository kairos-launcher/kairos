# Files and folders

Kairos keeps your settings, what it learns, and caches in three folders.

## Config

Settings, secrets, your plugins and scripts. Back this one up.

| Linux | Windows | Portable |
|---|---|---|
| `~/.config/kairos/` | `%APPDATA%\kairos\` | `data\` next to the executable |

| Inside | Holds |
|---|---|
| `config.toml` | [The settings file](./config-file). |
| `secrets/` | [Secrets](./secrets): API keys and tokens, one file each. |
| `plugins/` | Plugins you installed (`plugin install`), one folder each. |
| `scripts/` | Your [script commands](/plugins/scripts). |

## State

What the launcher learns as you use it. Safe to delete: it starts again from scratch.

| Linux | Windows | Portable |
|---|---|---|
| `~/.local/state/kairos/` | `%LOCALAPPDATA%\kairos\` | `data\` next to the executable |

| Inside | Holds |
|---|---|
| `frecency.json` | How often and how recently you ran each result, for ranking. |
| `query_picks.json` | The result you last picked for each search text. |
| `shell_history.json` | Recent [shell](/plugins/shell) commands (readable only by you). |
| `timers.json` | Running and recent [timers](/plugins/timer). |
| `update.json` | When the [updater](/plugins/update) last found nothing new. |

## Cache

Rebuilt when needed. Safe to delete.

| Linux | Windows | Portable |
|---|---|---|
| `~/.cache/kairos/` | `%LOCALAPPDATA%\kairos\` | `data\cache\` (icons) |

| Inside | Holds |
|---|---|
| `icons/` | Icons sent by plugins. |
| `calc/` | The [calculator's](/plugins/calc) exchange rates. |
| `answer.*` | The last answer opened in an editor. |

## Next to the executable

| Inside | Holds |
|---|---|
| `plugins/` | The [shipped plugins](/plugins/#shipped-plugins) (calculator, AI, …). |
| `portable` | An empty file that switches on portable mode, see below. |
| `.update/` | Downloaded updates, until they are installed. |

## Where plugins are loaded from

1. `plugins/` in the config folder (plugins you installed).
2. `plugins/` next to the executable (shipped plugins).
3. The folder named by `plugins_dir` in [`config.toml`](./config-file#launcher-settings).
4. The folders in the `KAIROS_PLUGINS` environment variable (separated by `:`, `;` on Windows).

A plugin folder that every user on the machine can write to is skipped.

## Portable mode

An empty file named `portable` next to the executable keeps config, state and icon cache in a
`data` folder beside it, so the whole install can live on a USB stick or in a synced folder. The
Windows portable zip comes with this file. Setting `KAIROS_HOME=<folder>` does the same with
another folder.

## Starting over

Quit the launcher (`kairos --quit`), then delete the state folder to forget rankings and history,
or the config folder too to reset every setting, secret and installed plugin.
