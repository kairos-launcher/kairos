# Dev utilities

Values to copy: UUIDs, passwords, encodings and hashes. Id `devtools`. Enter copies; nothing is
sent anywhere.

## Commands

| Type | Does |
|---|---|
| `uuid` | A random v4 and a time-ordered v7 UUID. |
| `genpw`, `genpw 32` | A password with symbols, and one of letters and digits only. Length 4 to 256 (setting `password_length`). |
| `urlencode a b&c` | Percent-encode everything but the RFC 3986 unreserved characters. |
| `urldecode a%20b` | Reverse it (`+` stays `+`). |
| `base64 encode hello` | Base64-encode. **Tab** also offers the URL-safe form. |
| `base64 decode aGVsbG8=` | Decode standard or URL-safe, with or without padding. |
| `hash sha256 hello` | Hash the UTF-8 text: `sha256`, `sha512`, `sha1` or `md5`. |

For `base64` and `hash` the first word picks the mode or type. Until it does, rows list the
choices and Enter or Tab fills one in, keeping text you already typed.

## Settings

| Key | Type | Default | Meaning |
|---|---|---|---|
| `password_length` | number | `20` | Used when you type just `genpw`. |
