# Web Search

Search the web and open typed URLs. Id `builtin-web`.

## Commands

| Type | Does |
|---|---|
| `web rust` | Search with the default engine (`engine` setting). |
| `ddg rust` | DuckDuckGo. |
| `g rust` | Google. |
| `w rust` | Wikipedia. |
| `gh rust` | GitHub. |
| `yt rust` | YouTube. |
| `docs.rs/slint` | A plain search that looks like an address offers to open it. |
| anything | Every plain search ends with a "Search …" row (setting `fallback`). |

Each engine keyword in the `engines` setting becomes its own trigger, so adding an engine adds a
command. File names such as `notes.txt` or `main.rs` are not mistaken for addresses.

## Settings

| Key | Type | Default | Meaning |
|---|---|---|---|
| `engine` | string | `"ddg"` | Engine keyword used by `web ` and the fallback row. |
| `browser` | string | `""` | Command to open pages with, e.g. `"firefox --private-window"`. Empty: the system default. |
| `fallback` | bool | `true` | Offer a web search at the bottom of every plain search. |
| `engines` | table | see below | Keyword = URL with `{q}` where the search text goes. |

```toml
[plugins.builtin-web.engines]
ddg = "https://duckduckgo.com/?q={q}"
g = "https://www.google.com/search?q={q}"
w = "https://en.wikipedia.org/w/index.php?search={q}"
gh = "https://github.com/search?q={q}"
yt = "https://www.youtube.com/results?search_query={q}"
sp = "https://www.startpage.com/do/search?q={q}"   # your own
```
