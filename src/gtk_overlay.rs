//! GTK4 layer-shell overlay for buffer-first dictation.
//!
//! Uses Wayland layer-shell protocol for a non-focusable overlay
//! that displays transcribed text without stealing keyboard focus.

use crate::preview_buffer::{DisplaySection, DisplayStyle, PreviewBuffer};
use anyhow::Result;
use async_channel::{Receiver as AsyncReceiver, Sender as AsyncSender};
use gtk4::glib;
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

/// Handle to communicate with the overlay from the main thread
pub struct OverlayHandle {
    /// Send commands to the overlay (uses async_channel for GTK thread)
    command_tx: AsyncSender<OverlayCommand>,
    /// Receive responses from the overlay (standard mpsc for main thread)
    pub response_rx: Receiver<OverlayResponse>,
}

impl OverlayHandle {
    /// Send a word to the overlay
    pub fn send_word(&self, word: String) -> Result<()> {
        self.command_tx
            .send_blocking(OverlayCommand::Word(word))
            .map_err(|e| anyhow::anyhow!("Failed to send word to overlay: {}", e))
    }

    /// Send a correction to the overlay
    pub fn send_correction(&self, corrected: String) -> Result<()> {
        self.command_tx
            .send_blocking(OverlayCommand::Correction(corrected))
            .map_err(|e| anyhow::anyhow!("Failed to send correction to overlay: {}", e))
    }

    /// Request a checkpoint (paste and continue)
    pub fn checkpoint(&self) -> Result<()> {
        self.command_tx
            .send_blocking(OverlayCommand::Checkpoint)
            .map_err(|e| anyhow::anyhow!("Failed to send checkpoint to overlay: {}", e))
    }

    /// Request a commit (paste all and close)
    pub fn commit(&self) -> Result<()> {
        self.command_tx
            .send_blocking(OverlayCommand::Commit)
            .map_err(|e| anyhow::anyhow!("Failed to send commit to overlay: {}", e))
    }

    /// Update the status indicator
    pub fn set_status(&self, status: OverlayStatus) -> Result<()> {
        self.command_tx
            .send_blocking(OverlayCommand::Status(status))
            .map_err(|e| anyhow::anyhow!("Failed to send status to overlay: {}", e))
    }

    /// Close the overlay
    pub fn close(&self) -> Result<()> {
        self.command_tx
            .send_blocking(OverlayCommand::Close)
            .map_err(|e| anyhow::anyhow!("Failed to send close to overlay: {}", e))
    }

    /// Check for a response (non-blocking)
    pub fn try_recv(&self) -> Option<OverlayResponse> {
        match self.response_rx.try_recv() {
            Ok(response) => Some(response),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(OverlayResponse::Closed),
        }
    }
}

const OVERLAY_CSS: &str = r#"
window {
    background-color: rgba(30, 30, 35, 0.95);
    border-radius: 8px;
}

.content-box {
    padding: 12px;
}

.committed-text {
    color: rgba(128, 128, 128, 0.9);
    font-size: 14px;
}

.active-text {
    color: rgba(240, 240, 240, 1.0);
    font-size: 16px;
}

.waiting-text {
    color: rgba(100, 100, 100, 0.8);
    font-style: italic;
}

.status-listening {
    color: rgba(100, 200, 100, 1.0);
    font-size: 12px;
}

.status-correcting {
    color: rgba(200, 200, 100, 1.0);
    font-size: 12px;
}

.status-paused {
    color: rgba(150, 150, 150, 1.0);
    font-size: 12px;
}

.word-count {
    color: rgba(100, 100, 100, 0.8);
    font-size: 11px;
}

separator {
    background-color: rgba(80, 80, 80, 0.5);
    min-height: 1px;
    margin: 8px 0;
}
"#;

fn load_css() {
    let provider = CssProvider::new();
    provider.load_from_data(OVERLAY_CSS);

    gtk4::style_context_add_provider_for_display(
        &gtk4::gdk::Display::default().expect("Could not get default display"),
        &provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

/// Internal state for the overlay UI
struct OverlayState {
    buffer: PreviewBuffer,
    status: OverlayStatus,
    response_tx: Sender<OverlayResponse>,
    content_box: GtkBox,
    status_label: Label,
    word_count_label: Label,
}

impl OverlayState {
    fn update_display(&self) {
        // Clear existing children
        while let Some(child) = self.content_box.first_child() {
            self.content_box.remove(&child);
        }

        let sections = self.buffer.display_sections();

        if sections.is_empty() {
            let label = Label::new(Some("Waiting for speech..."));
            label.add_css_class("waiting-text");
            label.set_wrap(true);
            label.set_xalign(0.0);
            self.content_box.append(&label);
        } else {
            for section in &sections {
                match section.style {
                    DisplayStyle::Committed => {
                        let label = Label::new(Some(&section.text));
                        label.add_css_class("committed-text");
                        label.set_wrap(true);
                        label.set_xalign(0.0);
                        self.content_box.append(&label);
                    }
                    DisplayStyle::Active => {
                        let label = Label::new(Some(&section.text));
                        label.add_css_class("active-text");
                        label.set_wrap(true);
                        label.set_xalign(0.0);
                        self.content_box.append(&label);
                    }
                    DisplayStyle::Separator => {
                        let sep = gtk4::Separator::new(Orientation::Horizontal);
                        self.content_box.append(&sep);
                    }
                }
            }
        }

        // Update word count
        self.word_count_label
            .set_label(&format!("{} words", self.buffer.active_word_count()));
    }

    fn update_status_display(&self) {
        // Remove old status classes
        self.status_label.remove_css_class("status-listening");
        self.status_label.remove_css_class("status-correcting");
        self.status_label.remove_css_class("status-paused");

        // Add new status class
        let class = match self.status {
            OverlayStatus::Listening => "status-listening",
            OverlayStatus::Correcting => "status-correcting",
            OverlayStatus::Paused => "status-paused",
        };
        self.status_label.add_css_class(class);
        self.status_label.set_label(self.status.as_str());
    }
}
