# Plugins

Everything in the result list comes from a plugin. Each plugin answers to a **trigger** (a prefix
such as `f ` or `kill `), to **plain searches** (no prefix), or both.

- **Built in**: compiled into the launcher, they answer instantly and cost nothing extra.
- **Shipped**: separate programs that come with the release in the `plugins` folder. They start
  when first needed and stop again after sitting idle, so they use no memory while unused.
- **Your own**: any program that speaks the [plugin protocol](/develop/writing-plugins), in any
  language, or an HTTPS service.

## Built-in plugins

| Plugin | Id | Trigger | Does |
|---|---|---|---|
| [Applications](./apps) | `builtin-apps` | plain search | Launch installed applications |
| [Files](./files) | `builtin-files` | `f ` | Find and browse files and folders |
| [Web Search](./web) | `builtin-web` | `web `, `ddg `, `g `, `w `, `gh `, `yt `, plain search | Search engines and typed URLs |
| [Shell](./shell) | `builtin-shell` | `>` | Run a shell command in a terminal |
| [SSH](./ssh) | `builtin-ssh` | `ssh ` | Connect to hosts from `~/.ssh/config` |
| [Windows](./windows) | `builtin-windows` | `win `, plain search | Switch to or close open windows |
| [Processes](./processes) | `builtin-processes` | `kill ` | Stop your own processes |
| [System](./system) | `builtin-system` | `sys `, plain search | Lock, sleep, log out, restart, shut down |
| [Custom actions](./actions) | `builtin-actions` | your keywords, plain search | Keyword commands from `config.toml` |
| [Script commands](./scripts) | `builtin-scripts` | `scripts`, plain search | Your scripts as commands |
| [Update](./update) | `builtin-update` | `update `, plain search | Install new versions |
| Plugins | `builtin-plugins` | `plugin ` | Manage plugins, [see below](#managing-plugins) |

## Shipped plugins

| Plugin | Id | Trigger | Does |
|---|---|---|---|
| [Calculator](./calc) | `calc` | `=`, plain search | Math, units and currencies |
| [AI](./ai) | `ai` | `ai `, plain search | Ask an AI, with follow-ups and MCP tools |
| [Bookmarks](./bookmarks) | `bookmarks` | `bm `, plain search | Browser bookmarks |
| [Emoji](./emoji) | `emoji` | `:`, `emoji ` | Find and copy emoji |
| [Timer](./timer) | `timer` | `timer ` | Timers and alarms |
| [Dev utilities](./devtools) | `devtools` | `uuid `, `genpw `, `urlencode `, `urldecode `, `base64 `, `hash ` | UUIDs, passwords, encodings, hashes |

## Managing plugins

Type `plugin` to list every plugin with its state, kind, version and triggers.

| Type | Does |
|---|---|
| `plugin` / `plugin <name>` | **Enter** enables or disables the plugin. **Tab** opens more: Settings, Open folder, Uninstall… |
| `plugin config <id>` | First row opens the settings page, then one row per setting with its value. Enter toggles a yes/no setting, cycles a choice, or fills in `plugin set …` to edit. |
| `plugin set <id> <key> <value>` | Shows the parsed value (or why it is invalid); Enter saves it. |
| `plugin install <folder>` | Shows the plugin's name, version and the command it runs; Enter copies it into `<config>/plugins/<id>/` and loads it. |
| `plugin install <git URL>` | Same, from a git repository (`https://` or SSH; needs `git`). It is fetched first and shown before anything is installed. |
| `plugin uninstall <id>` | Deletes a plugin installed in `<config>/plugins/`. Built-in and shipped plugins can only be disabled. |
| `plugin reload` | Rescan plugin folders and `config.toml` now, and rebuild indexes (apps, ssh hosts, …). |

### Settings page

Every plugin with settings gets a settings page (`plugin`, **Tab** → **Settings**); the same
values can be set in [`config.toml`](/guide/config-file#plugin-settings). See
[Configuration](/guide/configuration) for how the page works, and [Secrets](/guide/secrets) for
API keys.

### From the command line

`kairos --list-plugins` prints each plugin's id, kind, state, version and triggers: the quickest
way to check that a plugin loaded.

::: warning Trust
External plugins are ordinary programs running as you, without a sandbox. Install only plugins you
trust. Remote plugins see what you type after their trigger.
:::
