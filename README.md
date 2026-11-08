<img src="assets/icons/kairos-128.png" alt="Kairos icon" width="96" align="right">

# Kairos

Kairos is a keyboard launcher for Linux and Windows: press a hotkey, type, press Enter.
It starts applications, finds files, runs commands, does maths, switches windows, and is extended
with plugins written in any language.

It is built to stay out of the way: a single native binary with a software-rendered UI and low
resource usage.

Kairos is the Greek god of the opportune moment: it appears when called, does one thing, and is
gone.

**Documentation:** [kairos-launcher.github.io/kairos](https://kairos-launcher.github.io/kairos/): installation,
configuration, and every plugin's commands and settings.

[![Made with Slint](https://raw.githubusercontent.com/slint-ui/slint/master/logo/MadeWithSlint-logo-light.svg)](https://slint.dev)

## Features

| Type | Does | Plugin |
|---|---|---|
| `firefox` | Launch applications, ranked by how often and how recently you use them | Applications |
| `f report` | Find files and folders; `f ~/Documents/` browses | Files |
| `= 2+2`, `5 km to miles`, `100 usd to chf` | Calculator, unit and currency conversion | Calculator |
| `ai what is a lifetime` | Ask an AI (Anthropic or OpenAI-compatible, your own key), with follow-ups and tools from MCP servers | AI |
| `> htop` | Run a shell command in a terminal | Shell |
| `ssh web1` | Connect to hosts from `~/.ssh/config` | SSH |
| `scripts create backup`, `jira PROJ-1` | Your own scripts as commands (Raycast script commands work too) | Script commands |
| `win firefox` | Switch to or close open windows | Windows |
| `sys lock` | Lock, sleep, log out, restart, shut down | System |
| `kill firefox` | Terminate or force-kill your own processes | Processes |
| `web rust`, `g rust` | Web searches and typed URLs | Web Search |
| `:thumbs`, `emoji party` | Find an emoji and copy it | Emoji |
| `uuid`, `genpw 32`, `hash sha256 hi` | Generate UUIDs and passwords, URL/Base64 encode and decode, hash text | Dev utilities |
| `github`, `bm rust docs` | Open browser bookmarks (Firefox, Chrome, Chromium, Brave, Edge, Vivaldi) | Bookmarks |
| `plugin` | Enable, configure, install and remove plugins | Plugins |
| `update` | Check for a new version and install it | Update |

**Tab** opens a result's other actions (or completes a command), **Esc** closes. **Ctrl+C** steps
back without closing: it closes the actions menu, stops an answer, clears what you typed, or
leaves the answer panel.
Close it without running anything and reopen it within 90 seconds, and your search (or answer)
is still there, selected so typing starts a new one.
Plugin actions that answer with text (an AI answer, a lookup) show it in a panel in place of
the list, streamed as it arrives; **Ctrl+C** stops it or goes back to the results.

## Install

**Linux** (into `~/.local/share/kairos`, starts at sign-in):

```sh
mkdir -p ~/.local/share && curl -fsSL https://github.com/kairos-launcher/kairos/releases/latest/download/kairos-linux-x86_64.tar.gz | tar xz -C ~/.local/share && ~/.local/share/kairos/kairos --install-autostart
```

**Windows** (PowerShell, per-user install):

```powershell
irm https://github.com/kairos-launcher/kairos/releases/latest/download/kairos-setup.exe -OutFile "$env:TEMP\kairos-setup.exe"; & "$env:TEMP\kairos-setup.exe" /SILENT
```

Or download from the Releases page: `kairos-linux-x86_64.tar.gz` (unpack and run `kairos`),
`kairos-setup.exe`, or `kairos-windows-x86_64-portable.zip`. Each release lists SHA-256
checksums and a build provenance attestation
(`gh attestation verify <file> --repo kairos-launcher/kairos`). See the
[installation guide](https://kairos-launcher.github.io/kairos/guide/installation) for details.

Release builds update themselves: once a day they look for a new release, download it, check
it against the release's checksums and install it the next time the launcher hides, then
restart. `update` shows the state and checks now. Turn off **Install automatically** in the
Update plugin's settings to only be told, or disable the plugin to never check. Builds from
source never update themselves, and neither does an install in a folder you cannot write to
(Windows installed for all users): those only point to the release page.

### Build from source

Needs Rust 1.92 or newer.

```sh
cargo build --release          # target/release/kairos
scripts/dist.sh                # kairos plus the shipped plugins in dist/
```

## Usage

`kairos` starts it with the window open; `kairos --background` starts it hidden, and
`kairos --install-autostart` does that at every sign-in. The default hotkey is **Alt+Space**.

On **Wayland** applications cannot grab global keys. Bind `kairos --toggle` to a shortcut in
your desktop; `kairos --install-shortcut` does this for GNOME.

`kairos --help` lists all flags.

## Configuration

Settings live in `~/.config/kairos/config.toml` (Windows: `%APPDATA%\kairos\config.toml`).
The file explains every option; plugins add their settings to it, commented out at their
defaults. Changes apply the next time the window opens. The tray icon's **Settings** entry opens
the file.

A `portable` file next to the executable keeps everything (config, history, plugins, cache) in a
`data` folder beside it.

## Plugins

Everything in the list comes from a plugin. External plugins are programs in any language that
talk JSON-RPC over stdin/stdout; remote plugins are HTTPS services. See [Writing a plugin](https://kairos-launcher.github.io/kairos/develop/writing-plugins)
for the architecture, the protocol, and how to write one. `examples/plugins/` has small plugins in
Node.js, Rust, Go and Python.

External plugins run as you, without a sandbox: install only plugins you trust. Remote plugins
see what you type in their trigger. See [SECURITY.md](SECURITY.md).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

Licensed under the [MIT license](LICENSE). Contributions are accepted under the same terms.

The user interface uses [Slint](https://slint.dev) under the
[Slint Royalty-free Desktop, Mobile, and Web Applications License](https://github.com/slint-ui/slint/blob/master/LICENSES/LicenseRef-Slint-Royalty-free-2.0.md),
which requires the attribution above. `vendor/softbuffer` is a patched copy of
[softbuffer](https://github.com/rust-windowing/softbuffer) (MIT OR Apache-2.0); see its
`PATCH.md`. The emoji plugin's table is built from Unicode data under the Unicode License v3
(`plugins/emoji/data/LICENSE-UNICODE.txt`).
