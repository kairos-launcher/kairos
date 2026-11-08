# Processes

Find and stop your own running processes. Id `builtin-processes`, trigger `kill `.

## Commands

| Type | Does |
|---|---|
| `kill firefox` | One row for the app and all its helper processes (`kill java` finds IntelliJ). |
| `kill 1234` | One process by PID. |
| `kill` | The top memory users. |

| Key | Does |
|---|---|
| **Enter** | Terminate. If something is still running two seconds later, the row offers **Force kill**. |
| **Tab** | **Terminate (graceful)** and **Force kill**. |

An app none of whose processes has a window is marked **No window**: helpers left behind after the
window closed. This needs the same window list as the [Windows](./windows) plugin.

## Notes

- **Linux** groups processes by the app that started them (GNOME, KDE, snap and flatpak scopes),
  then by name. Terminate sends SIGTERM to the top processes first so an app can close its helpers
  itself; Force kill sends SIGKILL to all of them.
- **Windows** lists your session's processes. Terminate asks their windows to close; Force kill
  ends the process.
- PID 1 and the launcher itself are never offered, and the list is re-read before signalling.

This plugin has no settings.
