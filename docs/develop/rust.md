# Rust

A compiled plugin starts in a few milliseconds, uses little memory, and needs nothing installed on
the user's machine. The shipped plugins (calculator, AI, bookmarks, …) are written this way.

- **Needs:** nothing for users; you build one binary per system.
- **Example:** [`examples/plugins/echo-rs`](https://github.com/kairos-launcher/kairos/tree/main/examples/plugins/echo-rs),
  a standalone crate using only `serde_json`.
- **Bigger examples:** the shipped plugins in
  [`plugins/`](https://github.com/kairos-launcher/kairos/tree/main/plugins), for example
  [`devtools`](https://github.com/kairos-launcher/kairos/tree/main/plugins/devtools).

## The example

`[run.exec]` names the binary per system, next to `plugin.toml`.

::: code-group

<<< @/../examples/plugins/echo-rs/plugin.toml [plugin.toml]

<<< @/../examples/plugins/echo-rs/src/main.rs [src/main.rs]

<<< @/../examples/plugins/echo-rs/Cargo.toml [Cargo.toml]

:::

The empty `[workspace]` keeps the crate out of any surrounding Cargo workspace.

## Build and try it

```sh
cd examples/plugins/echo-rs
cargo build --release && cp target/release/echo-rs .     # echo-rs.exe on Windows
cd ../../.. && KAIROS_PLUGINS=$PWD/examples/plugins kairos   # then type: echors hello
```

Without the launcher:

```sh
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \
              '{"jsonrpc":"2.0","id":2,"method":"query","params":{"qid":1,"text":"hi","limit":8}}' | ./echo-rs
```

## Streaming output

Reply to `execute`, then send `output` notifications from a thread while the main loop keeps
reading stdin. `io::stdout()` is shared and locks per write, so whole lines never interleave:

```rust
use std::sync::{Arc, Mutex};
use std::collections::HashSet;

let cancelled = Arc::new(Mutex::new(HashSet::<u64>::new()));

// in the "execute" arm:
let exec = params["exec"].as_u64().unwrap_or(0);
reply(id, json!({ "ok": true, "output": { "title": "Echo", "streaming": true } }));
let (text, cancelled) = (params["query"].as_str().unwrap_or("").to_owned(), cancelled.clone());
std::thread::spawn(move || {
    for word in text.split_whitespace() {
        if cancelled.lock().unwrap().contains(&exec) { return; }
        std::thread::sleep(std::time::Duration::from_millis(150));
        println!("{}", json!({ "jsonrpc": "2.0", "method": "output", "params": { "exec": exec, "append": format!("{word} ") } }));
    }
    println!("{}", json!({ "jsonrpc": "2.0", "method": "output", "params": { "exec": exec, "done": true } }));
});

// in a "$/cancel" arm:
if let Some(exec) = params["exec"].as_u64() { cancelled.lock().unwrap().insert(exec); }
```

## Tips

- **Release profile** for small, fast binaries:

  ```toml
  [profile.release]
  opt-level = "s"
  lto = true
  codegen-units = 1
  panic = "abort"
  strip = true
  ```

- **Windows from Linux:** `rustup target add x86_64-pc-windows-gnu`, install MinGW
  (`gcc-mingw-w64-x86-64`), then `cargo build --release --target x86_64-pc-windows-gnu`.
- **Windows Defender** sometimes flags small unsigned binaries with generic names; give yours a
  distinctive name (the shipped plugins are `kairos-<name>.exe`).
- **HTTP:** `ureq` is small and blocking, which suits the one-request-at-a-time protocol. Honour
  `HTTPS_PROXY`, which the launcher sets from its proxy setting.
- **Settings** arrive in `initialize` as `params["settings"]`.
- Writing a plugin compiled **into** the launcher instead: [Bundled plugins in Rust](./bundled-plugins).
