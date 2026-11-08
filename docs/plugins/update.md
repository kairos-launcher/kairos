# Update

Keeps Kairos up to date from its GitHub releases. Id `builtin-update`, trigger `update `.

## Commands

| Type | Does |
|---|---|
| `update` | Show the state and check now. When a new version is downloaded and auto-install is off, the row offers **Update now**. |

## How it works

A minute after start, and then once a day, it asks GitHub for the latest release. A newer
release's file for your install (Linux tarball, Windows portable zip or installer) is downloaded
and checked against the release's `SHA256SUMS`; a file that does not match is deleted. The update
is installed the next time the launcher has been hidden a while, then the launcher restarts.

Only release builds update themselves. Builds from source, and installs in a folder you cannot
write to, show the releases page instead.

## Settings

| Key | Type | Default | Meaning |
|---|---|---|---|
| `auto_install` | bool | `true` | Download new versions and install them while the launcher is hidden. Off: only show that one is available. |

Disable the plugin to never check.
