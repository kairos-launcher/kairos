# Local patch of softbuffer 0.4.8

Slint's software renderer presents frames through softbuffer. softbuffer 0.4.x creates its
Wayland `wl_shm` buffers as `Xrgb8888`, which makes the compositor ignore the alpha channel:
every transparent pixel is drawn black, so the launcher's rounded corners showed a dark backdrop.

The first change is `Xrgb8888` -> `Argb8888` in `src/backends/wayland/buffer.rs` (two places).
Slint already renders premultiplied alpha, which is what `Argb8888` expects.

The second: `WaylandBuffer::resize` now resets `age` to 0. It kept the old age, so after the
window grew Slint took the new, larger buffer for one that still held the last frame and only
repainted what changed; the rest showed leftovers (the result list under the settings page).

Wired in through `[patch.crates-io]` in the root `Cargo.toml`. Drop the patch once a softbuffer
release (and the Slint version using it) presents alpha on Wayland.

Tracking upstream: transparency landed in softbuffer master via
https://github.com/rust-windowing/softbuffer/pull/321 (`AlphaMode`), not released as of 2026-10.
Remove this patch when Slint depends on a softbuffer release that includes it and sets a
premultiplied alpha mode.
