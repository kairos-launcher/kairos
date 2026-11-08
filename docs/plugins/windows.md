# Windows

Switch to an open window or close it. Id `builtin-windows`, trigger `win `, also answers plain
searches.

## Commands

| Type | Does |
|---|---|
| `win firefox` | Windows matching `firefox`. Enter switches to it. |
| `win` | Every window, most recently focused first, the current one last. |
| `firefox` | Plain searches (two letters at least) also list windows, below apps. |

Actions: **Switch to** (Enter), **Close window** (Tab).

## Desktop support

| Desktop | Needs |
|---|---|
| GNOME (Wayland) | The [Window Calls](https://extensions.gnome.org/extension/4724/window-calls/) extension. Until it is installed, `win ` shows an install link. |
| Hyprland | `hyprctl` |
| Sway | `swaymsg` |
| niri | `niri msg` |
| X11 | `wmctrl` |
| Windows | Built in. |

KDE on Wayland is not supported yet. The window list is fetched when the launcher is shown, never
while it is hidden.

This plugin has no settings.
