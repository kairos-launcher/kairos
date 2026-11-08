# Contributing

Thanks for helping. Bug reports, plugins and pull requests are all welcome.

## Before you open a pull request

Run what CI runs:

```sh
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --no-default-features -- -D warnings
cargo test
cargo deny check            # optional locally: cargo install cargo-deny
```

The protocol tests start the example plugins, so they need `python3` on `PATH`. Without it they
are skipped locally (with a note) and fail on CI.

Windows-only code can be checked from Linux with
`rustup target add x86_64-pc-windows-gnu` and `cargo clippy --target x86_64-pc-windows-gnu`.

## Code guidelines

- **No new `unsafe`.** The crate denies `unsafe_code`; the few FFI calls that need it live in
  small modules with `#[allow(unsafe_code)]` and a `// SAFETY:` comment on every block.
- **Memory matters, binary size does not.** The launcher should stay under 20 MB RSS while hidden
  and use no CPU then; plugin processes do not count, so features with heavy dependencies or
  data belong in an external plugin. If a change could affect that, measure with
  `scripts/rss.sh [-- query ...]` (several runs) and mention the numbers.
- **Types over strings.** Errors the code reacts to are enum variants (`plugin::Error`), not
  message text; settings and ids that have a fixed set of values are enums.
- **Everything a plugin sends is input.** Remote plugin replies are untrusted; keep the size
  caps and checks in `plugin::ResultItem::clamp_sizes`, `core::process::check_url` and
  `plugin/remote.rs` intact.
- Keep the docs in [`docs/`](docs/) in step with protocol and behaviour changes (`cd docs && npm run docs:dev` previews them).
- Prefer a focused commit per change, with a message that says why.

## Layout

| Path | What |
|---|---|
| `src/main.rs`, `src/cli.rs` | Entry point and command-line flags |
| `src/ui/` | Window, callbacks and result list (`ui/main.slint` is the layout) |
| `src/engine.rs` | Routing queries to plugins, ranking, running actions |
| `src/plugin/` | The `Provider` trait, manifests, stdio and HTTPS plugin hosts |
| `src/builtin/` | Bundled plugins |
| `src/core/` | Shared helpers: processes, paths, locking, file writes, fuzzy matching |
| `plugins/` | Plugins shipped with releases (built by `scripts/dist.sh`) |
| `examples/plugins/` | Example plugins, not shipped |
