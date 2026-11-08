# Writing a plugin

Everything the launcher shows is a plugin's result. A plugin is a folder with a `plugin.toml` and a program in any language that reads and writes lines of JSON on stdin/stdout. Pick a [language](#pick-a-language) to start; the [protocol reference](./protocol) has every message.

## How a plugin works

The launcher starts your program when its trigger is first typed, and talks to it with
[JSON-RPC 2.0](https://www.jsonrpc.org/specification) messages, one JSON object per line:

1. `initialize`: sent once, with the plugin's [settings](#plugin-settings). Reply with anything.
2. `query`: sent for every keystroke, with the typed text. Reply with result rows.
3. `execute`: the user pressed Enter (or picked a Tab action) on one of your rows.
4. `shutdown`, or stdin closing: exit. On-demand plugins are stopped after sitting idle.

Read requests from **stdin**, write replies to **stdout** (and flush after each line), write logs
to **stderr**. Every message is in the [protocol reference](./protocol).

## Pick a language

Anything that can read and write lines of JSON works. These have a page and an example plugin:

| Language | Needs on the user's machine | Good for | Example |
|---|---|---|---|
| [Node.js](./nodejs) | Node.js (or Deno, Bun) | Web APIs, npm packages, TypeScript. | [`echo-node`](https://github.com/kairos-launcher/kairos/tree/main/examples/plugins/echo-node) |
| [Rust](./rust) | nothing (you ship a binary) | Fast start, small memory, heavy work. | [`echo-rs`](https://github.com/kairos-launcher/kairos/tree/main/examples/plugins/echo-rs) |
| [Go](./go) | nothing (you ship a binary) | Same as Rust with a simpler toolchain; cross-compiles in one command. | [`echo-go`](https://github.com/kairos-launcher/kairos/tree/main/examples/plugins/echo-go) |
| [Python](./python) | Python 3 | Quick scripts, the standard library does a lot. Linux has it; Windows users install it. | [`echo-py`](https://github.com/kairos-launcher/kairos/tree/main/examples/plugins/echo-py) |
| [Remote (HTTPS)](./remote-plugins) | nothing (it runs on a server) | Company services; any server language. | [`echo-remote`](https://github.com/kairos-launcher/kairos/tree/main/examples/plugins/echo-remote) |

Compiled plugins start fastest and need nothing installed; interpreted ones are easier to change.
Either way, an on-demand plugin keeps running while the user types, so start time is paid once.

For a single command that does not need to answer as you type, a
[script command](./script-commands) is simpler: a script with a comment header, no protocol.

## Install and test

Install a plugin folder from the launcher: type `plugin install ~/my-plugin` and press Enter (the
row shows the plugin's name and the command it will run first). Or copy it by hand and rescan:

```sh
cp -r my-plugin ~/.config/kairos/plugins/     # Windows: %APPDATA%\kairos\plugins\
kairos --list-plugins                         # should list it as external/enabled
```

then type `plugin reload` in the launcher.

While developing, point the launcher at a folder of plugins instead:

```sh
KAIROS_PLUGINS=$PWD/examples/plugins kairos
```

You can also test a plugin without the launcher by piping lines into it (each language page has
the exact command):

```sh
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \
              '{"jsonrpc":"2.0","id":2,"method":"query","params":{"qid":1,"text":"hi","limit":8}}' | python3 echo.py
```

The examples are covered by the launcher's tests: `cargo test echo_plugin_roundtrip`, and
`cargo test plugin::` for the protocol behaviour (streaming, timeouts, restarts, lazy icons, idle
shutdown).

## Plugin folder and `plugin.toml`

```
<plugins dir>/
  dice/                 one folder per plugin; the folder name does not matter
    plugin.toml
    dice.py             whatever `run.exec` starts
```

Plugin directories scanned at startup: `<config>/plugins` (Linux `~/.config/kairos/plugins`, Windows `%APPDATA%\kairos\plugins`), the `plugins` folder next to the executable (bundled plugins), the `plugins_dir` set in `config.toml` (relative paths are relative to the config folder), and the folders in `$KAIROS_PLUGINS` (a path list, `:`-separated, `;` on Windows). Folders are rescanned by `plugin install`, `plugin uninstall`, `plugin reload` and whenever `config.toml` changed since the last time the window opened. A folder with an unparsable manifest is skipped with a message on stderr, and so is a plugin folder (or plugins root) that every user on the machine can write to. Portable mode: a file named `portable` next to the executable (or `KAIROS_HOME=<dir>`) keeps config, history, user plugins and the icon cache in `<exe dir>/data` instead of the user profile. `kairos --install-autostart` (undo: `--uninstall-autostart`) makes it start hidden at sign-in (`.desktop` file in `~/.config/autostart` on Linux, `HKCU\...\Run` on Windows). Or set `start_on_boot = true` in `config.toml` (`false` removes it; unset leaves it alone).

```toml
id = "dice"                      # required. Unique; letters, digits, `-` and `_` only (it is a
                                 #   key in config.toml, so no quotes needed). `builtin-` is
                                 #   reserved. A clash with an earlier plugin id is skipped.
name = "Dice"                    # required. Shown in listings.
version = "0.1.0"                # required.
api = "0.1"                      # protocol version this plugin targets (default "0.1")
description = "Roll dice"        # optional

[run]                            # required for external plugins
exec = ["python3", "dice.py"]    # argv. Relative paths with a slash (./bin) resolve against
                                 #   the plugin folder; the working directory is the plugin folder.
mode = "on-demand"               # "on-demand" (default) | "resident"
idle_timeout_s = 30              # on-demand only: stop after this much idle time

# Different command per OS (use instead of the array above):
# [run.exec]
# windows = ["dice.exe"]
# linux   = ["./dice"]
# macos   = ["./dice"]

[query]
triggers = ["roll "]             # literal prefixes that route a query only to this plugin
global = true                    # also receive plain (unprefixed) searches (default true)
# global_requires = "0123456789" # plain searches only if they contain one of these characters,
                                 # so an on-demand plugin is not started for every search
timeout_ms = 150                 # budget for one query (default 150)

[[settings]]                     # optional, repeatable; see "Plugin settings"
key = "sides"
type = "number"                  # string | number | bool | choice | path | list | table | secret | secrets
label = "Default sides"
description = "used when you type just `roll`"
default = 6
# options = ["a", "b"]           # allowed values of a `choice`
# placeholder = "6"              # hint in the empty field on the settings page
# required = true                # not started until this has a value (see [Plugin settings](#plugin-settings))
# import = "mcp"                 # a table with fields: pasted MCP server JSON (Claude, VS Code) is added as entries
```

A `[permissions]` table is accepted but ignored (plugins are not sandboxed, see [Trust](#trust-and-performance)).

## Plugin settings

A plugin declares its settings in `plugin.toml` with `[[settings]]` (key, `type`, `label`, `description`, `default`, `options` for a `choice`, and optionally `placeholder` and `required`). The plugin writes no UI: the launcher builds a **settings page** from this list, one field per setting in file order (`plugin`, Tab → Settings, or the first row of `plugin config <id>`). Every setting is edited in place: text and number fields, a toggle for `bool`, buttons for a `choice`, a password field for a `secret`, one item per line for a list of strings, TOML text for tables and lists of tables. Changes are saved together with Ctrl+S (nothing is saved if one field is invalid; it shows why), so the plugin restarts once. Esc goes back, asking once if there are unsaved changes.

Users can also set values in `config.toml`, in the same section as `enabled`:

```toml
[plugins.builtin-web]
browser = "firefox --private-window"
engine = "g"

[plugins.builtin-web.engines]
g = "https://www.google.com/search?q={q}"
sp = "https://www.startpage.com/do/search?q={q}"
```

- **Effective settings** = the manifest defaults, overridden by these values. A value of the wrong type (or not one of a `choice`'s options) is reported on stderr and the default is used. Keys the plugin does not declare are passed through unchanged.
- **Discoverability.** When the launcher starts, and after an install, it appends a section for every plugin that has settings but no section yet, with each setting **commented out at its default** and its label/description as a comment. Uncomment a line to change it. Nothing is written for plugins that already have a section (commented or not), and existing text is never changed, so a plugin update can change its defaults without old values sticking in your file.
- **Types:** `string`, `path`, `number`, `bool`, `choice` (one of `options`), `list` (TOML array), `table` (TOML table, e.g. web engines), `secret`, `secrets`. All of them can be edited on the settings page; all but `list` and `table` also with `plugin set`. An unknown type is shown read-only with a link to `config.toml`.
- **Required:** while a `required = true` setting is empty (unset, `""`, `[]` or `{}`), the plugin is not started: its trigger answers with one "needs setup" row whose Enter opens the settings page, and plain searches skip it. A plugin can offer the page itself with an action of `kind = "settings"`.
- **Secrets** (API keys, tokens): a `secret` setting is never read from or written to `config.toml`. The settings page (or `plugin set <id> <key> <value>`) stores it in `<config>/secrets/<id>.<key>`, a file only you can read (an empty value removes it); `plugin config <id>` shows only whether it is set. The plugin receives it like any other setting (in `initialize`, or the constructor for bundled plugins), so there is nothing extra to implement. A value found in `config.toml` is ignored with a warning, and a secret file others can read counts as unset.
- **Named secrets:** a `secrets` setting holds any number of them, named by the user (tokens that other settings refer to by name, as the AI plugin's `${name}`). `plugin set <id> <key> <name> <value>` stores one in `<config>/secrets/<id>.<key>.<name>` (no value removes it); `plugin config <id>` lists only the names. The plugin receives an object of names to values, e.g. `"secrets": {"github_token": "…"}`.
- Bundled plugins get the effective settings in their constructor; external plugins in `initialize`. Either way a change rebuilds the plugin.

## Icons

Rows show an icon tile on the left. Applications get theirs from the desktop's icon theme (the `Icon=` of the `.desktop` file, looked up in your theme, the themes it inherits, `hicolor` and `pixmaps`; within a theme a PNG of any size is taken before an SVG, which is drawn once at 64 px). Plugins choose one of four ways:

| `icon` value | Use when | Cost |
|---|---|---|
| `{"glyph":"G"}` | a character is enough | nothing, if the UI font has the character; otherwise the launcher searches its fonts for one that does, which can map large font files (emoji, CJK). Prefer images for symbols. |
| `{"path":"icon.png"}` | one image shipped with the plugin (PNG, JPEG or SVG) | file read, cached |
| `{"base64":"…"}` | a few small images made on the fly | decoded once, cached on disk by content (`<cache>/kairos/icons`, at most 300 files, 512 KB each, PNG/JPEG detected by content) |
| `{"id":"blue"}` | many or large images | one `icon` request per id, after the results show; never delays results |

## Architecture

```
                 keystroke
                     │
   ┌─────────────────▼──────────────────┐
   │ UI (Slint)  query-changed(text)    │
   └─────────────────┬──────────────────┘
                     │ worker thread per query, stale answers dropped
   ┌─────────────────▼──────────────────┐
   │ Engine                             │
   │  route ─ trigger prefix? ──► one plugin
   │        └ otherwise ─────────► all global plugins (in parallel)
   │  merge ─ score + 0.3 × frecency + pick, sort, keep max_results
   └───────┬───────────────┬────────────┘
           │ Provider trait│
   ┌───────▼──────┐  ┌─────▼─────────────────────────────┐
   │ Bundled      │  │ External (ExternalPlugin)         │
   │ in-process   │  │ child process, JSON-RPC over stdio│
   │ apps │  │ any language, folder + plugin.toml│
   └──────────────┘  └───────────────────────────────────┘
```

Every plugin is a **`Provider`** (`src/plugin/mod.rs`): it answers `query`, and runs an action on `execute`. There are two kinds, described by the same `plugin.toml` schema:

| | Bundled | External |
|---|---|---|
| Runs | in the launcher process | as a child process |
| Written in | Rust, compiled in | anything that reads and writes lines of JSON |
| Added by | registering it in `src/builtin/mod.rs` | `plugin install <folder or git URL>` in the launcher, or copying a folder into the plugins directory |
| Cost while hidden | none | none for `on-demand` plugins once idle; one resident process for `resident` plugins |
| Example | `builtin-apps` (applications), `builtin-system` | `plugins/calc` (shipped calculator), `examples/plugins/echo-py` |

The launcher is meant to keep its resource usage low and use no CPU while hidden (`scripts/rss.sh` measures its memory; plugin processes are not counted). That is why the app launcher is bundled (it must answer instantly and an extra process would cost memory), why features with big dependencies or data (a database driver, an emoji table) are external plugins, and why external plugins default to `on-demand` and are stopped after they sit idle.

Plugins that ship with the launcher but are not compiled in live in `plugins/` (today: the calculator, `plugins/calc/`, a Rust crate with `fend-core`). Moving it out of the main binary keeps `fend-core` out of the launcher's memory; its process only starts when a search contains a digit or uses `=`, and stops 30 s after it was last used. `scripts/dist.sh` builds them and puts them in `dist/plugins/`, next to `dist/launcher`, where the launcher finds them. During development: `scripts/dist.sh && KAIROS_PLUGINS=$PWD/dist/plugins cargo run`.

### Query routing

1. If the text starts with a plugin's **trigger** (e.g. `echo `), only that plugin gets the query, with the trigger stripped (and following whitespace trimmed). Triggers are matched literally, so include a trailing space if you want one. The first matching plugin wins.
2. Otherwise every enabled plugin with `global = true` gets the full text, in parallel.
3. Results are ranked by `score + 0.3 × frecency + pick` (frecency is keyed by plugin id + result id, grows with launch count and decays with a 14-day half-life; `pick` is 1 for the result last run for the same typed text, ignoring case and surrounding spaces, so it comes first next time, and decays with a 30-day half-life), then cut to `max_results` (default 8). Fallback rows get neither boost. Picks are kept in `<state>/query_picks.json`; a new pick for a text replaces the old one.

An empty query returns nothing.

**Command suggestions.** Before the user has typed the space after a trigger, the launcher offers the command itself: typing `ki` or `kill` shows a `kill` result (with the plugin's name and description). Enter, a click or Tab completes the search box to `kill ` straight away (no menu in between) and the plugin's results for the bare trigger appear, so a plugin should answer an empty `text` usefully (`builtin-processes` lists the top memory users). This is automatic for every plugin that declares `triggers`, so write a good `description` in `plugin.toml`; it is what users see there.

### Lifecycle of an external plugin

- `mode = "resident"`: started in the background when the launcher starts.
- `mode = "on-demand"` (default): started on the first query that reaches it. About 35 seconds after the window is hidden, the launcher stops it if it has been idle for at least `idle_timeout_s` (default 30).
- A plugin that crashes, exits, or sends an error is killed and started again on its next query. A plugin that merely misses `timeout_ms` is left running; its late reply is skipped.
- Closing the plugin's stdin means "exit". The launcher also sends `shutdown` first when stopping an idle plugin.

## Trust and performance

- Remote plugins (`run.url`) are not programs on your machine, but they see what you type (see [Remote plugins](./remote-plugins)).
- The bookmarks plugin reads your browsers' bookmark files (and with `history = true` the browsing history) locally; nothing leaves the machine.
- The bundled calculator downloads exchange rates from `www.ecb.europa.eu` when you convert money (no query text is sent). `[plugins.calc] currency = false` stops that.
- Results are capped whatever their source: at most `limit` per batch, text fields at 1 KiB, action values at 8 KiB, 16 actions per result. A stdio reply line over 1 MiB stops the plugin (it restarts on the next query).
- Plugins and launched programs do not inherit `KAIROS_TOKEN_*` variables.
- External plugins are **ordinary programs running as you, with no sandbox**. Only install plugins you trust. `[permissions]` in a manifest is documentation only.
- Keep `query` well under `timeout_ms` (150 ms default). Slow plugins are skipped for that keystroke.
- Prefer `on-demand`. Use `resident` only when startup is slow (loading a big index), because it keeps a process alive while the launcher is hidden.
- Do your own caching; the launcher sends a query on every keystroke.
- Be idle when idle: block on stdin rather than polling.

## Current limitations

- Results appear as each plugin answers; a slow plugin never delays the others. The selected row stays selected when later results arrive.
- Queries to one plugin are serialized. A newer query cancels the older one (`$/cancel`), but a plugin that ignores the notification still finishes its old work before it reads the new query.
- A crashing plugin restarts on the next query without backoff.
- One action runs at a time; pressing Enter again while a slow action (up to its 10 s limit) is still running does nothing. A streaming output keeps going after its action returned and does not block other actions.
- Only one output streams per plugin at a time; starting another stops the first.
- The AI plugin keeps conversations in its process: they are gone once it idles out (10 minutes after the last use) or restarts. A follow-up to a lost conversation starts a new one.
- Remote plugins cannot stream output, and nothing changes the result rows after `timeout_ms`.
- The timer plugin's "8 min left" is computed when you type; the row does not count down. Timers do not survive logging out (they are listed as Missed afterwards).
