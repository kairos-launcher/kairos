# The settings file

All settings, the launcher's and every plugin's, are in one [TOML](https://toml.io) file:

| System | Path |
|---|---|
| Linux | `~/.config/kairos/config.toml` |
| Windows | `%APPDATA%\kairos\config.toml` |
| Portable | `data\config.toml` next to the executable |

The tray icon's **Settings** entry opens it, and creates it first if needed with every option
commented out at its default. Edits apply the next time the launcher window opens.

## Example

```toml
hotkey = "ctrl+space"
theme = "dark"
max_results = 10

# A plugin's section is named after its id.
[plugins.builtin-files]
roots = ["~", "/data/projects"]
max_depth = 6

[plugins.builtin-web]
engine = "g"

[plugins.builtin-web.engines]
sp = "https://www.startpage.com/do/search?q={q}"

[plugins.ai]
api = "anthropic"
base_url = "https://api.anthropic.com"
model = "claude-opus-5-5"
# The API key is not here: see Secrets.

[plugins.builtin-ssh]
enabled = false
```

## Launcher settings

| Key | Default | Meaning |
|---|---|---|
| `hotkey` | `"alt+space"` | Global hotkey. Windows reserves Win+Space. On Wayland, bind `kairos --toggle` in your desktop instead (see [Usage](./usage#wayland)). |
| `max_results` | `8` | Results shown at most. |
| `tray` | `true` | System tray icon, see [below](#tray-icon). |
| `theme` | `"auto"` | `"auto"` follows the system, or force `"dark"` / `"light"`. |
| `position` | `"mouse"` | Monitor to open on: `"mouse"` (under the cursor), `"active"` (the focused window's) or `"primary"`. X11 and Windows only; on Wayland the desktop decides. Needs a restart. |
| `animations` | `true` | Fade in late results and show a progress bar while a plugin is still answering. Typing and selection are never animated. |
| `keep_search_s` | `90` | Reopening within this many seconds brings back an unfinished search or answer. `0`: always start empty. |
| `start_on_boot` | unset | `true` starts the launcher hidden at sign-in, `false` removes that; unset leaves it alone. |
| `plugins_dir` | unset | Extra folder with plugins. Relative paths are relative to `config.toml`; `~` is your home folder. |
| `proxy` | `"auto"` | Proxy for plugins that go online, see [Proxy](#proxy). |
| `no_proxy` | `""` | Extra hosts that skip the proxy, e.g. `"localhost,.corp.example"`. |
| `ca_file` | unset | PEM file with the CA certificates trusted by [remote plugins](/develop/remote-plugins). |

## Plugin settings

Each plugin has a section `[plugins.<id>]`; `kairos --list-plugins` or `plugin` in the launcher
shows the ids. `enabled = false` turns a plugin off; its settings go in the same section. Settings
that are tables get a section of their own (`[plugins.builtin-web.engines]`).

- **Defaults are written for you.** When the launcher starts, and after a plugin is installed, it
  adds a section for every plugin that has settings but no section yet, with each setting
  **commented out at its default** and its description above it. Uncomment a line to change it.
  Sections that are already there (commented or not) are never touched.
- **Wrong values** (a text where a number belongs, a choice that is not one of its options) are
  reported and the default is used, so one typo does not break the file.
- **Secrets** are not allowed here and are ignored with a warning; see [Secrets](./secrets).

Each [plugin's page](/plugins/) lists its settings.

## Tray icon

With `tray = true` the launcher shows a tray icon: double-click shows it, right-click offers
**Show**, **Settings** and **Quit**.

On Linux it works on X11 and Wayland (StatusNotifierItem over D-Bus). Stock GNOME hides tray icons
unless the AppIndicator extension is enabled (Ubuntu enables it by default). On Windows it uses the
notification area.

## Proxy

Plugins that go online (exchange rates, AI, remote plugins) use the proxy from `proxy`:

- `"auto"` (default): the `HTTPS_PROXY` / `HTTP_PROXY` / `ALL_PROXY` and `NO_PROXY` variables the
  launcher was started with, else the system settings: Windows Internet Options (including setup
  scripts and automatic detection), GNOME or KDE (manual proxies).
- `"none"`: connect directly.
- A URL, used as is: `"http://proxy:8080"`, `"http://user:password@proxy:8080"`,
  `"socks5://127.0.0.1:1080"`.

External plugins get the result as `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY` and `NO_PROXY`.
Proxies that need Windows (NTLM/Kerberos) sign-in are not supported. The system settings are read
again every 30 minutes.
