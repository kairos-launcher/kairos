# Files

Find files and folders by name. Id `builtin-files`, trigger `f `.

## Commands

| Type | Does |
|---|---|
| `f report` | Find files and folders whose name matches. |
| `f proj/notes` | Narrow by folder: `notes` inside a folder matching `proj`. |
| `f ~/Documents/` | Browse a folder; **Tab** enters a subfolder. |

Actions: **Open** (Enter), **Open containing folder**, **Copy path**.

## Settings

| Key | Type | Default | Meaning |
|---|---|---|---|
| `roots` | list | `["~"]` | Folders to look in. |
| `excludes` | list | `node_modules`, `target`, `.git`, `.cache`, `__pycache__`, `.venv`, `venv`, `build`, `dist`, `snap` | Folder names never entered. |
| `hidden` | bool | `false` | Include names starting with a dot. |
| `max_depth` | number | `8` | Folder levels below each root. |
| `max_entries` | number | `100000` | Stop indexing after this many entries. |

## Notes

The index is built in the background on the first `f ` search (a home folder takes about half a
second), refreshed after ten minutes, and dropped when the launcher has been hidden a while, so it
uses no memory at rest.
