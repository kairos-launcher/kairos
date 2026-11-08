//! Copy text to the system clipboard.

use std::sync::Mutex;

/// Kept alive: on X11/Wayland the clipboard contents are served by the owning process.
static CLIPBOARD: Mutex<Option<arboard::Clipboard>> = Mutex::new(None);

pub fn copy(text: &str) -> Result<(), String> {
    let mut g = crate::core::sync::lock(&CLIPBOARD);
    let clipboard = match g.take() {
        Some(c) => c,
        None => arboard::Clipboard::new().map_err(|e| format!("clipboard: {e}"))?,
    };
    g.insert(clipboard).set_text(text.to_string()).map_err(|e| format!("clipboard: {e}"))
}
