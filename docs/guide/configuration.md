# Configuration

There are two ways to change settings, and both change the same values:

- **In the launcher**: every plugin has a settings page, and short commands change one value as you
  type. This is the easiest way, and covers everything plugins offer.
- **In the [settings file](./config-file)** `config.toml`: the launcher's own settings (hotkey,
  theme, proxy, …), and every plugin setting as well.

API keys and tokens are kept apart from both, see [Secrets](./secrets). Where everything is
stored is listed in [Files and folders](./files).

Changes made either way apply the next time the launcher window opens. Changes made in the
launcher keep the comments and formatting of `config.toml`. Plugins whose settings changed are
restarted; the others keep running.

## Turning plugins on and off

Type `plugin` to list every plugin with its state. **Enter** on a plugin enables or disables it.
A disabled plugin gets no searches and is never started.

In `config.toml`:

```toml
[plugins.builtin-ssh]
enabled = false
```

## The settings page

Each plugin with settings has a page that lists them all as fields. Open it with:

- `plugin`, select the plugin, **Tab** → **Settings**;
- `plugin config <id>`, first row (for example `plugin config ai`);
- **Enter** on a plugin's "needs setup" row, which a plugin with a required setting that is still
  empty (such as the AI plugin's API) shows in place of its results.

| Setting type | Field |
|---|---|
| Text, path, number | A text field. |
| Yes / no | A toggle. |
| Choice | A button per option. |
| List of text | One item per line. |
| Table (search engines, MCP servers, …) | TOML text, with an example. |
| Secret (API key) | A password field, see [Secrets](./secrets). |

| Key | Does |
|---|---|
| **Ctrl+S** | Save all fields at once. Nothing is saved while a field is invalid; it shows why. |
| **Esc** | Go back. Asks once if there are unsaved changes. |

## Changing one value by typing

| Type | Does |
|---|---|
| `plugin config <id>` | One row per setting with its current value. **Enter** toggles a yes/no setting, cycles a choice, or fills in `plugin set …` to edit the others. **Tab** offers **Reset to default** and **Edit config.toml**. |
| `plugin set <id> <key> <value>` | Shows the parsed value, or why it is invalid. **Enter** saves it. |

Examples:

```text
plugin set builtin-files max_depth 4
plugin set calc currency false
plugin set builtin-web engine g
```

Lists and tables (`roots`, `engines`, …) are edited on the settings page or in `config.toml`.

## Launcher settings

The launcher's own settings (theme, hotkey, number of results, tray icon, monitor, proxy, …) are
on the **Settings** page. Open it from the tray icon's **Settings** entry, by searching for
`settings`, or with `kairos --settings` (handy as a desktop shortcut where there is no tray).

- Settings are grouped in sections; the filter box at the top shows only those (and the plugins)
  that contain every word you type.
- The **Plugins** list has a switch for every plugin, which turns it on or off at once. Click a
  plugin with settings to open its page; Esc comes back.
- Save with Ctrl+S. Everything applies at once, including a new hotkey and the tray icon; if
  another app holds the hotkey, the page says so and the old one keeps working.

The same options are in [`config.toml`](./config-file#launcher-settings), whose path the page
shows with a link to edit it.

Each [plugin's page](/plugins/) lists its settings, with their keys for `plugin set` and
`config.toml`.
