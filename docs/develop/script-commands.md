# Script commands

Not every command needs a plugin. A **script command** is any script with a comment header: no
protocol, no `plugin.toml`, nothing to install. Kairos lists it by its title and runs it when you
press Enter.

## Script or plugin?

| | Script command | Plugin |
|---|---|---|
| You write | A script with a few `@kairos.` comments | A program that speaks the [protocol](./protocol) |
| Answers while you type | No: one row, run on Enter | Yes: rows for every keystroke |
| Takes typed text | Yes, as `$1` | Yes, the whole query |
| Shows output | In the answer panel, or a terminal | Rows, the answer panel, streamed text |
| Settings page | No | Yes, from `[[settings]]` |
| Good for | Restart a service, open a ticket, run a backup | Search, lookups, converters, anything interactive |

Start with a script; turn it into a plugin when you want results as you type.

## A script in one minute

Type `scripts create restart audio` in the launcher. It writes `restart-audio.sh` (`.ps1` on
Windows) into your scripts folder and opens it in your editor. Fill it in:

```sh
#!/bin/bash
# @kairos.title Restart audio
# @kairos.mode silent
# @kairos.description Restarts PipeWire
systemctl --user restart pipewire
```

Save it, open the launcher and type `restart audio`.

To take what is typed after the keyword, add `@kairos.argument`:

```sh
#!/bin/bash
# @kairos.title Jira issue
# @kairos.keyword jira
# @kairos.argument Issue key
# @kairos.mode silent
xdg-open "https://jira.example.com/browse/$1"
```

Now `jira PROJ-1` opens that issue. With `@kairos.mode output` instead, whatever the script
prints is shown in the launcher's answer panel.

Any language works: the extension picks the interpreter (`.py`, `.js`, `.ps1`, …), or the `#!`
line on Linux. Raycast script commands work too.

## Next

- [Script commands reference](/plugins/scripts): every `@kairos.` key, modes, where scripts are
  kept, and how they run on Linux and Windows.
- [`examples/scripts/`](https://github.com/kairos-launcher/kairos/tree/main/examples/scripts): scripts to
  copy.
