# Bookmarks

Open browser bookmarks. Id `bookmarks`, trigger `bm `, also answers plain searches. Reads Firefox
(every profile, also snap/flatpak and LibreWolf), Chrome, Chromium, Brave, Edge and Vivaldi.

## Commands

| Type | Does |
|---|---|
| `bm rust docs` | Bookmarks containing every typed word in the title or address. |
| `bm` | All bookmarks. |
| `github` | Plain searches (two letters at least) also list bookmarks, below apps. |

Actions: **Open** in the default browser (Enter), **Copy link** (Tab).

The same address saved in several browsers is listed once.

## Settings

| Key | Type | Default | Meaning |
|---|---|---|---|
| `history` | bool | `false` | Also offer pages from the browsing history (the 2000 most visited per profile). |

## Notes

Bookmarks are read in the background when the plugin starts and again when a browser saved
changes. Everything stays on your machine.
