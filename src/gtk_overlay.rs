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

fn build_window(
    app: &Application,
    window_width: u32,
    window_height: u32,
    command_rx: AsyncReceiver<OverlayCommand>,
    response_tx: Sender<OverlayResponse>,
) {
    let window = ApplicationWindow::builder()
        .application(app)
        .title("eaRS Preview")
        .default_width(window_width as i32)
        .default_height(window_height as i32)
        .build();

    // Initialize layer-shell BEFORE presenting window
    window.init_layer_shell();
    window.set_layer(gtk4_layer_shell::Layer::Overlay);
    window.set_keyboard_mode(gtk4_layer_shell::KeyboardMode::None);

    // Position at bottom-right with margins
    window.set_anchor(gtk4_layer_shell::Edge::Bottom, true);
    window.set_anchor(gtk4_layer_shell::Edge::Right, true);
    window.set_margin(gtk4_layer_shell::Edge::Bottom, 32);
    window.set_margin(gtk4_layer_shell::Edge::Right, 32);

    // Set namespace for compositor identification
    window.set_namespace("eaRS-dictation-overlay");

    // Main vertical layout
    let main_box = GtkBox::new(Orientation::Vertical, 0);

    // Scrolled window for content
    let scrolled = ScrolledWindow::builder()
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .vscrollbar_policy(gtk4::PolicyType::Automatic)
        .vexpand(true)
        .build();

    // Content box for text sections
    let content_box = GtkBox::new(Orientation::Vertical, 4);
    content_box.add_css_class("content-box");
    scrolled.set_child(Some(&content_box));

    // Status bar at bottom
    let status_bar = GtkBox::new(Orientation::Horizontal, 8);
    status_bar.set_margin_start(12);
    status_bar.set_margin_end(12);
    status_bar.set_margin_bottom(8);

    let status_label = Label::new(Some("listening"));
    status_label.add_css_class("status-listening");
    status_label.set_halign(gtk4::Align::Start);
    status_label.set_hexpand(true);

    let word_count_label = Label::new(Some("0 words"));
    word_count_label.add_css_class("word-count");
    word_count_label.set_halign(gtk4::Align::End);

    status_bar.append(&status_label);
    status_bar.append(&word_count_label);

    main_box.append(&scrolled);
    main_box.append(&status_bar);
    window.set_child(Some(&main_box));

    // Create state
    let state = Rc::new(RefCell::new(OverlayState {
        buffer: PreviewBuffer::new(),
        status: OverlayStatus::Listening,
        response_tx,
        content_box,
        status_label,
        word_count_label,
    }));

    // Initial display
    state.borrow().update_display();
    state.borrow().update_status_display();

    // Handle commands from main thread using spawn_local
    let state_clone = state.clone();
    let window_clone = window.clone();
    glib::spawn_future_local(async move {
        while let Ok(cmd) = command_rx.recv().await {
            let mut state = state_clone.borrow_mut();
            match cmd {
                OverlayCommand::Word(word) => {
                    state.buffer.add_word(word);
                    state.update_display();
                }
                OverlayCommand::Correction(corrected) => {
                    state.buffer.apply_correction(corrected);
                    state.update_display();
                }
                OverlayCommand::Checkpoint => {
                    if let Some(text) = state.buffer.checkpoint() {
                        let _ = state.response_tx.send(OverlayResponse::PasteText(text));
                    }
                    state.update_display();
                }
                OverlayCommand::Commit => {
                    if let Some(text) = state.buffer.commit() {
                        let _ = state.response_tx.send(OverlayResponse::PasteText(text));
                    }
                    let _ = state.response_tx.send(OverlayResponse::Closed);
                    window_clone.close();
                    break;
                }
                OverlayCommand::Close => {
                    let _ = state.response_tx.send(OverlayResponse::Closed);
                    window_clone.close();
                    break;
                }
                OverlayCommand::Status(status) => {
                    state.status = status;
                    state.update_status_display();
                }
            }
        }
    });

    // Handle window close
    let state_close = state.clone();
    window.connect_close_request(move |_| {
        let state = state_close.borrow();
        let _ = state.response_tx.send(OverlayResponse::Closed);
        glib::Propagation::Proceed
    });

    window.present();
}
