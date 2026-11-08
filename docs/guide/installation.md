# Installation

## Quick install

These fetch the current release and install it for your user only (no admin rights).

### Linux

```sh
mkdir -p ~/.local/share && curl -fsSL https://github.com/kairos-launcher/kairos/releases/latest/download/kairos-linux-x86_64.tar.gz | tar xz -C ~/.local/share && ~/.local/share/kairos/kairos --install-autostart
```

This unpacks Kairos into `~/.local/share/kairos` and makes it start hidden at sign-in. Start it now
with `~/.local/share/kairos/kairos`. To run it as `kairos`:

```sh
ln -sf ~/.local/share/kairos/kairos ~/.local/bin/kairos
```

On Wayland, bind the hotkey in your desktop (`kairos --install-shortcut` does it on GNOME, see
[Usage](./usage#wayland)).

### Windows

In PowerShell:

```powershell
irm https://github.com/kairos-launcher/kairos/releases/latest/download/kairos-setup.exe -OutFile "$env:TEMP\kairos-setup.exe"; & "$env:TEMP\kairos-setup.exe" /SILENT
```

This installs Kairos to `%LOCALAPPDATA%\Programs\Kairos` with a Start Menu shortcut, and makes it
start at sign-in. Start it from the Start Menu.

Running a one-liner again installs the current release over the old one. They skip the checksum
check; download by hand (below) to verify the files.

## Download a release

The current release's files always have the same address:

| File | For |
|---|---|
| [kairos-linux-x86_64.tar.gz](https://github.com/kairos-launcher/kairos/releases/latest/download/kairos-linux-x86_64.tar.gz) | Linux (x86_64): unpack and run `kairos`. |
| [kairos-setup.exe](https://github.com/kairos-launcher/kairos/releases/latest/download/kairos-setup.exe) | Windows installer. |
| [kairos-windows-x86_64-portable.zip](https://github.com/kairos-launcher/kairos/releases/latest/download/kairos-windows-x86_64-portable.zip) | Windows, portable: unpack and run `kairos.exe`. |
| [SHA256SUMS](https://github.com/kairos-launcher/kairos/releases/latest/download/SHA256SUMS) | Checksums of the files above. |

Older versions are on the [Releases page](https://github.com/kairos-launcher/kairos/releases).

Check a download against its checksum and build provenance attestation:

```sh
sha256sum --check --ignore-missing SHA256SUMS
gh attestation verify <file> --repo kairos-launcher/kairos
```

Then see [Usage](./usage) for starting it at sign-in and binding the hotkey.

## Updates

Release builds update themselves. Once a day they look for a new release, download it, check it
against the release's checksums, install it the next time the launcher hides, and restart.

- Type `update` to see the state and check now.
- Turn off **Install automatically** in the [Update plugin's](/plugins/update) settings to only be told.
- Disable the Update plugin to never check.

Builds from source never update themselves, and neither does an install in a folder you cannot
write to (Windows installed for all users): those only point to the release page.

## Portable mode

A file named `portable` next to the executable keeps everything (config, history, plugins, cache)
in a `data` folder beside it, instead of your user profile. Setting `KAIROS_HOME=<dir>` does the
same with another folder. See [Files and folders](./files).

## Build from source

Needs Rust 1.92 or newer. On Linux also the fontconfig headers (`libfontconfig1-dev` on
Debian/Ubuntu).

```sh
git clone https://github.com/kairos-launcher/kairos.git
cd kairos
cargo build --release          # target/release/kairos
scripts/dist.sh                # kairos plus the shipped plugins in dist/
```

`target/release/kairos` alone runs without the [shipped plugins](/plugins/#shipped-plugins)
(calculator, AI, bookmarks, …); `dist/` has the launcher with all of them.
