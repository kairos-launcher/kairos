# Script commands

The lightest way to add a command: a script with a comment header. Id `builtin-scripts`.
[Raycast script commands](#raycast-script-commands) work too.

## Commands

| Type | Does |
|---|---|
| `scripts create restart audio` | Writes `restart-audio.sh` (`.ps1` on Windows) from a template and opens it in the editor. Give an extension (`clean.py`) for another language. |
| `scripts` | Also offers **Open scripts folder**. |
| `restart audio`, `restart-audio` | A script shows up in plain searches by its title and by its keyword (the file name without extension); typing the keyword ranks it first. |
| `jira PROJ-1` | A script with `argument` takes what is typed after its keyword. |

**Tab** on a script offers **Edit script**. An existing file is opened by `scripts create`, never
overwritten.

## Writing a script

Put it into the scripts folder (Linux `~/.config/kairos/scripts`, Windows
`%APPDATA%\kairos\scripts`, or a folder listed in the `folders` setting):

```sh
#!/bin/bash
# @kairos.title Restart audio
# @kairos.mode silent
# @kairos.description Restarts PipeWire
systemctl --user restart pipewire
```

Keys go in comments (`#`, `//`, `--`, `;`, `::`, `'` or `REM`) within the first 50 lines:

| Key | Meaning |
|---|---|
| `title` | Required; a file without it is not a command. |
| `mode` | `silent` (default): run in the background. `output`: stream its output into the answer panel (Ctrl+C stops the script). `terminal`: run in a terminal window. |
| `argument` | Takes what is typed after the keyword, passed as `$1`. |
| `keyword` | One word instead of the file name. |
| `description` | The row's subtitle. |
| `icon` | An image file next to the script, one character, a theme icon name, or an installed app's name (`firefox`). |
| `format` | `markdown` renders `output` as Markdown. |

[`examples/scripts/`](https://github.com/kairos-launcher/kairos/tree/main/examples/scripts) has a few to
copy. Changes to the folder apply the next time the launcher opens.

## How scripts run

- A script runs in its own folder.
- **Linux:** an executable file runs directly (its `#!` line decides); otherwise the extension picks
  the interpreter (`sh`, `bash`, `py` → `python3`, `js` → `node`, `rb`, `pl`, `ps1` → `pwsh`).
- **Windows:** `.bat`, `.cmd` and `.exe` run directly, `.ps1` through PowerShell, `.py` through
  `python`.
- Typed text is handed over as arguments, never through a shell.
- Scripts run as you; a script or folder that every user can write to is ignored.

## Raycast script commands

`@raycast.title`, `mode` (`fullOutput`, `inline` and `compact` show output, `silent` runs quietly),
`argument1` … `argument3`, `icon`, `description`, `packageName` and `currentDirectoryPath` are
read; `needsConfirmation` and `refreshTime` are ignored. Scripts written for macOS still need macOS
commands.

## Settings

| Key | Type | Default | Meaning |
|---|---|---|---|
| `folders` | list | `[]` | More folders with scripts; `<config>/scripts` is always used. |
| `editor` | string | `""` | Command that edits scripts, e.g. `"code"`. Empty: the default app (Notepad on Windows). |
| `terminal` | string | `""` | Terminal for `mode terminal`, e.g. `"kitty"`. Empty: detect. |
