# Timer

Timers and alarms that end with a desktop notification and a sound. Id `timer`, trigger `timer `.

## Commands

| Type | Does |
|---|---|
| `timer 10m tea` | A 10-minute timer labelled "tea". |
| `timer 1h30`, `timer 1h 30m call` | Hours and minutes. |
| `timer 25` | A bare number is minutes (setting `default_unit`). |
| `timer 14:30 standup`, `timer 7pm`, `timer at 11` | An alarm at the next such time, in local time. |
| `timer` | Running timers, soonest first, then the last five you started. |

Everything after the time is the label.

On a running timer: **Enter** cancels it; **Tab** offers **Restart** and **+5 min**. A timer whose
process was gone (logout, reboot) is listed as **Missed** until dismissed.

## Settings

| Key | Type | Default | Meaning |
|---|---|---|---|
| `default_unit` | choice | `"min"` | What a bare number means: `min` or `s`. |
| `sound` | bool | `true` | Play a sound when a timer ends. |
| `sticky` | bool | `true` | Keep the notification until you dismiss it. |

## Notes

- Each timer runs in a small process of its own that keeps running when the plugin
  stops while idle.
- Linux uses `notify-send` (or `gdbus`) and `canberra-gtk-play` / `paplay` / `pw-play`; Windows a
  toast; macOS `osascript`.
- "8 min left" is computed when you type; the row does not count down.
