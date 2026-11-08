# Shell

Run a shell command. Id `builtin-shell`, trigger `>`.

## Commands

| Type | Does |
|---|---|
| `> htop` | Run the command in a terminal. |
| `>` | List recent commands; **Tab** puts one back in the box to edit. |

Actions: **Run in terminal** (Enter), **Run without a terminal**, **Copy command**.

The terminal stays open with a shell after the command ends (setting `keep_open`).

## Settings

| Key | Type | Default | Meaning |
|---|---|---|---|
| `terminal` | string | `""` | Terminal command that runs what follows it, e.g. `"kitty"` or `"gnome-terminal --"`. Empty: detect. |
| `keep_open` | bool | `true` | Leave a shell running in the terminal after the command ends. |
| `history` | number | `50` | Recent commands to remember (`0`: none). |

## Notes

- History is kept in `<state>/shell_history.json` (Linux `~/.local/state/kairos/`), readable only
  by you.
- Detected terminals: `$TERMINAL`, then x-terminal-emulator, kitty, alacritty, foot, wezterm,
  ghostty, gnome-terminal, konsole, xterm; on Windows, Windows Terminal or cmd.
