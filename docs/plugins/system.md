# System

Lock, sleep, log out, restart, shut down, empty the trash. Id `builtin-system`, trigger `sys `,
also answers plain searches.

## Commands

| Type | Does |
|---|---|
| `sys lock` | Lock screen. |
| `sys sleep` | Sleep. |
| `sys hibernate` | Hibernate. |
| `sys log out` | Log out. Asks first. |
| `sys restart` | Restart. Asks first. |
| `sys shut down` | Shut down. Asks first. |
| `sys empty trash` | Empty the trash. Asks first. |
| `shut`, `lock` | Plain searches of three letters or more also find these. |

"Asks first" means Enter turns the search into `sys restart!`, whose single row runs it.

## Settings

| Key | Type | Default | Meaning |
|---|---|---|---|
| `confirm` | bool | `true` | Ask before logging out, restarting, shutting down or emptying the trash. |

## Notes

Linux uses `loginctl` / `systemctl` and empties `~/.local/share/Trash`. Windows uses `shutdown`,
`rundll32` and `Clear-RecycleBin`.
