# Usage

## Starting

| Command | Does |
|---|---|
| `kairos` | Start with the window open. |
| `kairos --background` | Start hidden. |
| `kairos --install-autostart` | Start hidden at every sign-in (`--uninstall-autostart` undoes it). |

The default hotkey is **Alt+Space**; change it with `hotkey` in the
[configuration](./configuration).

### Wayland

On Wayland, applications cannot grab global keys. Bind `kairos --toggle` to a shortcut in your
desktop's keyboard settings instead. For GNOME, `kairos --install-shortcut` does this for you
(`--uninstall-shortcut` removes it).

## Keys

| Key | Does |
|---|---|
| **Enter** | Run the selected result's default action. |
| **Tab** | Open the result's other actions, or complete a command (`ki` → `kill `). |
| **Up / Down** | Move through results. |
| **Ctrl+C** | Step back without closing: close the actions menu, stop an answer, clear the search, or leave the answer panel. |
| **Esc** | Close the launcher. |

Close the launcher without running anything and reopen it within 90 seconds, and your search (or
answer) is still there, selected so typing starts a new one.

Plugin actions that answer with text (an AI answer, a lookup) show it in a panel in place of the
list, streamed as it arrives. **Ctrl+C** stops it or goes back to the results.


## Command-line flags

| Flag | Does |
|---|---|
| `--background` | Start hidden (used by autostart). |
| `--toggle` | Show or hide the running launcher. |
| `--show` / `--hide` | Show or hide the running launcher. |
| `--query <text>` | Show the running launcher with `<text>` in the search box. |
| `--quit` | Stop the running launcher. |
| `--list-plugins` | List plugins with their state and triggers. |
| `--set-token <plugin>` | Store a remote plugin's API token (read from stdin). |
| `--install-shortcut` / `--uninstall-shortcut` | Bind the hotkey to `kairos --toggle` in the desktop (GNOME). |
| `--install-autostart` / `--uninstall-autostart` | Start hidden at sign-in, or stop doing so. |
| `--help` | Show the flags. |

`--query` makes handy desktop shortcuts, for example one bound to `kairos --query "ai "`.
