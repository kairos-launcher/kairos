# Controls

Media playback, volume, Wi-Fi, Bluetooth, dark mode, screen brightness and Do Not Disturb. Id
`controls`, triggers `media ` and `toggle `, also answers plain searches.

## Commands

| Type | Does |
|---|---|
| `pause`, `play`, `next`, `previous`, `stop` | Control the player that is playing. The row shows the track. |
| `media` | Every player with what it plays. **Enter** plays or pauses it; **Tab** offers next, previous, stop. |
| `vol 40`, `vol up`, `vol -10`, `mute` | Volume of the default output. Turning it up unmutes. |
| `wifi`, `wifi off`, `bt on` | Turn Wi-Fi or Bluetooth on or off. The row shows the current state. |
| `dark`, `light mode` | Switch dark mode. |
| `bright 60`, `bright up` | Brightness of the built-in screen. |
| `dnd` | Turn Do Not Disturb on until you turn it off, or off again. |
| `dnd 30m`, `dnd 1h30`, `dnd until 14:30` | Do Not Disturb with an end time; it turns itself off then. |
| `media `, `toggle ` | List the media or the system controls. |

Plain searches need the whole name (`bt`, `dnd`) or its first three letters (`blu`). Controls this
computer does not have (no Wi-Fi card, no backlight) are left out.

Times read like the [Timer](./timer)'s: `25` is minutes, `14:30` and `7pm` are clock times. The
end time works when the launcher is closed: a small process waits for it, as a timer does.

## Settings

| Key | Type | Default | Meaning |
|---|---|---|---|
| `player` | string | `""` | Player that `pause` and `next` control when several run, e.g. `spotify`. Empty: the one playing. |
| `volume_step` | number | `5` | Percent that `vol up/down` and `bright up/down` change. |

## How it works

| | Linux | Windows | macOS |
|---|---|---|---|
| Media | MPRIS (D-Bus) | System media sessions | Music and Spotify (AppleScript) |
| Volume | `wpctl` (PipeWire) or `pactl` | Core Audio | AppleScript |
| Wi-Fi | NetworkManager, else `rfkill` | Radio settings | `networksetup` |
| Bluetooth | BlueZ and `rfkill` | Radio settings | [`blueutil`](https://github.com/toy/blueutil) |
| Brightness | logind (no root needed) | WMI (built-in screens) | [`brightness`](https://github.com/nriley/brightness) |
| Dark mode | GNOME/GTK `color-scheme`, Plasma colour scheme | App and system theme | Appearance |
| Do Not Disturb | GNOME, Plasma, SwayNC, dunst, mako | "Get notifications" switch | Shortcuts, see below |

- **mako** needs a `[mode=do-not-disturb]` section with `invisible=1` in its config.
- **macOS** has no way for apps to set Focus. Create two Shortcuts named **Kairos DND On** and
  **Kairos DND Off** that turn Focus on and off; the plugin runs them.
- **Windows** Do Not Disturb turns off the "Get notifications from apps and other senders" switch.
