# Bundled plugins in Rust

Bundled plugins run in the launcher process. Use this for things that need to be instant or touch launcher internals, such as `builtin-apps` (a small one worth reading first: `src/builtin/system/`).

1. Create `src/builtin/<name>/mod.rs` and `src/builtin/<name>/plugin.toml`. The manifest uses the same schema as above and has no `[run]` section:

   ```toml
   id = "builtin-clipboard"
   name = "Clipboard"
   version = "0.1.0"

   [query]
   triggers = ["clip "]
   global = false
   ```

2. Implement `Provider` (`src/plugin/mod.rs`). `id()` must equal the manifest id, which starts with `builtin-` (both checked by a test). Triggers and `global` come from the manifest, so the engine routes bundled and external plugins identically.

   ```rust
   pub struct ClipboardProvider { /* state */ }

   impl Provider for ClipboardProvider {
       fn id(&self) -> &str { "clipboard" }
       fn query(&self, q: &Query) -> Vec<ResultItem> { /* q.text, q.limit */ vec![] }
       fn execute(&self, result_id: &str, action_id: &str, query: &str)
           -> Result<ExecOutcome, String> { Ok(ExecOutcome::close()) }
       // Also: ExecOutcome::keep_open(true), or ExecOutcome::complete("text ") to stay open
       // and rewrite the search box.
       // Optional: warm_up(), refresh()/needs_refresh(), reap_idle()
   }
   ```

3. Register it in `all()` in `src/builtin/mod.rs`. The closure receives the plugin's effective settings (a JSON object; deserialize what you need) and may adjust the manifest, e.g. `builtin-web` turns its configured engine keywords into triggers:

   ```rust
   bundle(ctx, include_str!("clipboard/plugin.toml"), "builtin/clipboard/plugin.toml",
          |_manifest, settings| Arc::new(clipboard::ClipboardProvider::new(settings))),
   ```

Score text with `fuzzy::score(query, target)` (0..1, `None` = no match). It ignores case and Latin accents (`cafe` finds `Café`) and ranks prefix > substring at a word start > other substring > acronym (`vsc` → Visual Studio Code, camelCase counts as word starts) > scattered letters > one typo (`firfox`, only for queries of 4+ letters). A query with spaces also matches word by word in any order (`code visual`).

Build rows with `ResultItem::new(id, title, subtitle, score).with_icon(..).with_action(id, title)` (the first action is the default). `crate::core` has helpers: `icons::themed(&["folder"], "F")` (a freedesktop theme icon, else a letter), `process::{open, open_with, spawn, which, run_in_terminal}` and `clipboard::copy`. When settings change the provider is rebuilt, so read them once in the constructor. `execute` runs on a worker thread, never the UI thread.

`query` runs on a worker thread, possibly concurrently with other plugins: keep it fast (no blocking I/O; read from an in-memory index you build in `warm_up`/`refresh`) and make the provider `Send + Sync`. `refresh` is called off the UI thread when the window is shown and `needs_refresh()` is true (the app index re-scans when an app folder changed, or after 30 minutes).
