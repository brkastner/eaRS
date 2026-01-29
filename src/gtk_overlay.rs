//! GTK4 layer-shell overlay for buffer-first dictation.
//!
//! Uses Wayland layer-shell protocol for a non-focusable overlay
//! that displays transcribed text without stealing keyboard focus.

use crate::preview_buffer::{DisplaySection, DisplayStyle, PreviewBuffer};
use anyhow::Result;
use glib::MainContext;
use gtk4::prelude::*;
use gtk4::{
    Application, ApplicationWindow, Box as GtkBox, CssProvider, Label, Orientation, ScrolledWindow,
};
use gtk4_layer_shell::LayerShell;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender};

/// Commands that can be sent to the overlay
#[derive(Debug, Clone)]
pub enum OverlayCommand {
    /// A new word arrived from STT
    Word(String),
    /// LLM correction completed
    Correction(String),
    /// Checkpoint requested (paste current buffer, continue)
    Checkpoint,
    /// Commit requested (paste all, close)
    Commit,
    /// Close the overlay without committing
    Close,
    /// Update the status indicator
    Status(OverlayStatus),
}

/// Status indicator for the overlay
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OverlayStatus {
    #[default]
    Listening,
    Correcting,
    Paused,
}

impl OverlayStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            OverlayStatus::Listening => "listening",
            OverlayStatus::Correcting => "correcting...",
            OverlayStatus::Paused => "paused",
        }
    }
}

/// Response from the overlay (sent back to main thread)
#[derive(Debug, Clone)]
pub enum OverlayResponse {
    /// Text to paste (from checkpoint or commit)
    PasteText(String),
    /// Overlay was closed
    Closed,
}
