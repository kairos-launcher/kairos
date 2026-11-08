# Applications

Fuzzy-search installed applications and launch them. Id `builtin-apps`, answers plain searches.

## Commands

| Type | Does |
|---|---|
| `firefox`, `vsc`, `firfox` | Find an app by name, initials (`vsc` → Visual Studio Code) or with one typo. |
| `browser` | Linux: apps are also found by their keywords, ranked below name matches. |
| `chrome github.com`, `evince ~/x.pdf` | Text after the app name is passed to it (`~` is expanded for files). |
| `private window`, `firefox private` | An app's desktop actions (such as Firefox's New Private Window) as rows of their own. |

| Key | Does |
|---|---|
| **Enter** | Launch the app. |
| **Tab** | Complete the app's name plus a space, or open its desktop actions if it has any. |

Apps you launch often and recently rank higher.

## Notes

- **Linux** reads `.desktop` files; icons come from your icon theme.
- **Windows** reads Start Menu shortcuts.
- Passing text works for Linux apps whose `Exec=` line has a `%f`, `%F`, `%u` or `%U` field code,
  and for every Windows shortcut.
- The app list is re-read when an app folder changes, or after 30 minutes.

This plugin has no settings.
