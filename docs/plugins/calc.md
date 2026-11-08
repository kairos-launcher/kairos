# Calculator

Math, unit and currency conversion. Id `calc`, trigger `=`, also answers plain searches that look
like math. Uses [fend](https://printfn.github.io/fend/).

## Commands

| Type | Does |
|---|---|
| `= 2+2` | Calculate. |
| `5 km to miles` | Convert units (`to`, `in` or `as`). |
| `20% of 150`, `sin(pi/2)` | Percentages, functions, constants. |
| `100 usd to chf`, `10 € in £` | Convert currencies (about 30, at the European Central Bank's daily rates). |

| Key | Does |
|---|---|
| **Enter** | Copy the result. |
| **Tab** | Also: copy `expression = result`. |

Plain searches only get an answer when they look like math (a digit plus an operator or `to` /
`in` / `as`), so typing app names stays quiet.

## Settings

| Key | Type | Default | Meaning |
|---|---|---|---|
| `currency` | bool | `true` | Currency conversion. Downloads the ECB exchange rates (no account needed) the first time you convert money, then at most twice a day. |

## Notes

Nothing is downloaded until you first convert money; only that first conversion shows "Loading
exchange rates…". The rates are cached in `<cache>/kairos/calc/` and refreshed in the background,
so queries never wait for the network. No query text is ever sent.
