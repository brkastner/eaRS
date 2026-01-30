//! Clipboard operations for preview overlay mode.
//!
//! Uses wl-clipboard-rs for Wayland clipboard, with arboard as fallback.

use anyhow::{Context, Result};
use std::thread;
use std::time::Duration;

#[cfg(feature = "preview-overlay")]
use wl_clipboard_rs::copy::{MimeType, Options, Source};

/// Copy text to the system clipboard
#[cfg(feature = "preview-overlay")]
pub fn copy_to_clipboard(text: &str) -> Result<()> {
    let opts = Options::new();
    opts.copy(
        Source::Bytes(text.as_bytes().into()),
        MimeType::Text,
    )
    .context("Failed to copy text to Wayland clipboard")?;
    Ok(())
}

/// Copy text to clipboard using arboard (fallback for non-Wayland)
#[cfg(not(feature = "preview-overlay"))]
pub fn copy_to_clipboard(_text: &str) -> Result<()> {
    anyhow::bail!("Clipboard support requires preview-overlay feature")
}

/// Copy text to clipboard and paste via configurable hotkey
///
/// This is more reliable and faster than typing character-by-character.
#[cfg(feature = "preview-overlay")]
pub fn copy_and_paste(
    text: &str,
    keyboard: &mut dyn crate::virtual_keyboard::VirtualKeyboard,
    paste_hotkey: &str,
) -> Result<()> {
    use crate::virtual_keyboard::Modifier;

    // Copy to clipboard
    copy_to_clipboard(text)?;

    // Small delay to ensure clipboard is available
    thread::sleep(Duration::from_millis(50));

    // Parse hotkey string (e.g., "ctrl+v", "ctrl+shift+v")
    let parts: Vec<&str> = paste_hotkey.split('+').collect();
    let key_char = parts.last()
        .and_then(|s| s.chars().next())
        .context("paste_hotkey has no key character")?;
    let modifiers: Vec<Modifier> = parts[..parts.len() - 1]
        .iter()
        .filter_map(|s| match s.to_lowercase().as_str() {
            "ctrl" | "control" => Some(Modifier::Ctrl),
            "shift" => Some(Modifier::Shift),
            "alt" => Some(Modifier::Alt),
            "super" | "meta" => Some(Modifier::Super),
            _ => None,
        })
        .collect();

    keyboard.send_chord(&modifiers, key_char)?;

    Ok(())
}

#[cfg(not(feature = "preview-overlay"))]
pub fn copy_and_paste(
    _text: &str,
    _keyboard: &mut dyn crate::virtual_keyboard::VirtualKeyboard,
    _paste_hotkey: &str,
) -> Result<()> {
    anyhow::bail!("Clipboard support requires preview-overlay feature")
}
