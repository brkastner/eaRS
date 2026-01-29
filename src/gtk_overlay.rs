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
