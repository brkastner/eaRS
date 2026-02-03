//! GTK4 layer-shell overlay for buffer-first dictation.
//!
//! Uses Wayland layer-shell protocol for a non-focusable overlay
//! that displays transcribed text without stealing keyboard focus.

use crate::preview_buffer::{DisplayStyle, PreviewBuffer};
use anyhow::Result;
use async_channel::{Receiver as AsyncReceiver, Sender as AsyncSender};
use gtk4::gdk;
use gtk4::glib;
use gtk4::glib::prelude::CastNone;
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
    /// LLM correction completed (full replacement for final paragraph correction)
    Correction(String),
    /// Chunk correction: splice corrected words into active_words at a range
    ChunkCorrection {
        corrected: String,
        start: usize,
        original_len: usize,
    },
    /// Checkpoint requested (paste current buffer, continue)
    Checkpoint,
    /// Commit requested (paste all, close)
    Commit,
    /// Close the overlay without committing
    Close,
    /// Show the overlay window
    Show,
    /// Hide the overlay window
    Hide,
    /// Update the status indicator
    Status(OverlayStatus),
    /// Update the info text
    Info(String),
    /// Show review menu with candidate options
    Review { options: Vec<ReviewOption>, selected: usize },
}

/// Status indicator for the overlay
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OverlayStatus {
    #[default]
    Listening,
    Correcting,
    Paused,
    Review,
}

impl OverlayStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            OverlayStatus::Listening => "listening",
            OverlayStatus::Correcting => "correcting...",
            OverlayStatus::Paused => "paused",
            OverlayStatus::Review => "review (↑/↓ select, → paste)",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewChoice {
    Raw,
    Final,
    Accuracy,
    Cancel,
}

#[derive(Debug, Clone)]
pub struct ReviewOption {
    pub choice: ReviewChoice,
    pub text: String,
    pub safe: bool,
}

impl ReviewOption {
    pub fn label(&self) -> &'static str {
        match self.choice {
            ReviewChoice::Raw => "RAW",
            ReviewChoice::Final => "FINAL",
            ReviewChoice::Accuracy => "ACCURACY",
            ReviewChoice::Cancel => "CANCEL",
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
    /// Review was canceled
    Cancel,
    /// Review selection
    ReviewSelection { choice: ReviewChoice, text: String, type_output: bool },
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

    /// Send a correction to the overlay (full replacement for final paragraph correction)
    pub fn send_correction(&self, corrected: String) -> Result<()> {
        self.command_tx
            .send_blocking(OverlayCommand::Correction(corrected))
            .map_err(|e| anyhow::anyhow!("Failed to send correction to overlay: {}", e))
    }

    /// Send a chunk correction that splices corrected words into a range of active_words
    pub fn send_chunk_correction(&self, corrected: &str, start: usize, original_len: usize) -> Result<()> {
        self.command_tx
            .send_blocking(OverlayCommand::ChunkCorrection {
                corrected: corrected.to_string(),
                start,
                original_len,
            })
            .map_err(|e| anyhow::anyhow!("Failed to send chunk correction to overlay: {}", e))
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

    /// Update the info text
    pub fn set_info(&self, info: String) -> Result<()> {
        self.command_tx
            .send_blocking(OverlayCommand::Info(info))
            .map_err(|e| anyhow::anyhow!("Failed to send info to overlay: {}", e))
    }

    /// Show review options
    pub fn show_review(&self, options: Vec<ReviewOption>, selected: usize) -> Result<()> {
        self.command_tx
            .send_blocking(OverlayCommand::Review { options, selected })
            .map_err(|e| anyhow::anyhow!("Failed to send review options to overlay: {}", e))
    }


    /// Close the overlay
    pub fn close(&self) -> Result<()> {
        self.command_tx
            .send_blocking(OverlayCommand::Close)
            .map_err(|e| anyhow::anyhow!("Failed to send close to overlay: {}", e))
    }

    /// Show the overlay window
    pub fn show(&self) -> Result<()> {
        self.command_tx
            .send_blocking(OverlayCommand::Show)
            .map_err(|e| anyhow::anyhow!("Failed to send show to overlay: {}", e))
    }

    /// Hide the overlay window
    pub fn hide(&self) -> Result<()> {
        self.command_tx
            .send_blocking(OverlayCommand::Hide)
            .map_err(|e| anyhow::anyhow!("Failed to send hide to overlay: {}", e))
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
    font-size: 18px;
}

.active-text {
    color: rgba(240, 240, 240, 1.0);
    font-size: 20px;
}

.waiting-text {
    color: rgba(100, 100, 100, 0.8);
    font-size: 18px;
    font-style: italic;
}

.status-listening {
    color: rgba(100, 200, 100, 1.0);
    font-size: 14px;
}

.status-correcting {
    color: rgba(200, 200, 100, 1.0);
    font-size: 14px;
}

.status-paused {
    color: rgba(150, 150, 150, 1.0);
    font-size: 14px;
}

.status-review {
    color: rgba(220, 180, 80, 1);
    font-size: 14px;
}

.word-count {
    color: rgba(100, 100, 100, 0.8);
    font-size: 13px;
}


.info-text {
    color: rgba(160, 160, 160, 0.9);
    font-size: 12px;
}

.review-item {
    padding: 6px 8px;
    border-radius: 6px;
}

.review-selected {
    background-color: rgba(80, 100, 160, 0.35);
}

.review-label {
    color: rgba(200, 200, 200, 0.9);
    font-size: 12px;
    font-weight: 600;
}

.review-unsafe {
    color: rgba(255, 140, 140, 1.0);
}

.review-text {
    color: rgba(230, 230, 230, 0.95);
    font-size: 14px;
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
    review_active: bool,
    review_options: Vec<ReviewOption>,
    review_selected: usize,
    pending_selection: Option<ReviewOption>,
    pending_type_output: bool,
    response_tx: Sender<OverlayResponse>,
    scrolled: ScrolledWindow,
    content_box: GtkBox,
    status_label: Label,
    info_label: Label,
    word_count_label: Label,
}

impl OverlayState {
    fn update_display(&self) {
        // Clear existing children
        while let Some(child) = self.content_box.first_child() {
            self.content_box.remove(&child);
        }

        if self.review_active {
            self.update_review_display();
            if self.pending_selection.is_some() && self.pending_type_output {
                self.word_count_label
                    .set_label("release shift to type");
            } else {
                self.word_count_label
                    .set_label("↑/↓ select · → paste · Esc cancel");
            }
            return;
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
        self.scroll_to_bottom();
    }

    fn update_review_display(&self) {
        if self.review_options.is_empty() {
            let label = Label::new(Some("No review options"));
            label.add_css_class("waiting-text");
            label.set_wrap(true);
            label.set_xalign(0.0);
            self.content_box.append(&label);
            return;
        }

        for (idx, option) in self.review_options.iter().enumerate() {
            let item = GtkBox::new(Orientation::Vertical, 2);
            item.add_css_class("review-item");
            if idx == self.review_selected {
                item.add_css_class("review-selected");
            }

            let label_text = if option.safe || matches!(option.choice, ReviewChoice::Raw | ReviewChoice::Cancel) {
                option.label().to_string()
            } else {
                format!("{} (rejected)", option.label())
            };
            let label = Label::new(Some(&label_text));
            label.add_css_class("review-label");
            if !option.safe && !matches!(option.choice, ReviewChoice::Raw | ReviewChoice::Cancel) {
                label.add_css_class("review-unsafe");
            }
            label.set_halign(gtk4::Align::Start);
            label.set_xalign(0.0);

            let text = Label::new(Some(option.text.as_str()));
            text.add_css_class("review-text");
            text.set_wrap(true);
            text.set_xalign(0.0);

            item.append(&label);
            item.append(&text);
            self.content_box.append(&item);
        }
        self.scroll_to_bottom();
    }

    fn scroll_to_bottom(&self) {
        let adjustment = self.scrolled.vadjustment();
        let upper = adjustment.upper();
        let page_size = adjustment.page_size();
        let lower = adjustment.lower();
        let target = (upper - page_size).max(lower);
        adjustment.set_value(target);
    }

    fn update_status_display(&self) {
        // Remove old status classes
        self.status_label.remove_css_class("status-listening");
        self.status_label.remove_css_class("status-correcting");
        self.status_label.remove_css_class("status-paused");
        self.status_label.remove_css_class("status-review");

        // Add new status class
        let class = match self.status {
            OverlayStatus::Listening => "status-listening",
            OverlayStatus::Correcting => "status-correcting",
            OverlayStatus::Paused => "status-paused",
            OverlayStatus::Review => "status-review",
        };
        self.status_label.add_css_class(class);
        self.status_label.set_label(self.status.as_str());
    }

    fn update_info(&self, info: &str) {
        self.info_label.set_label(info);
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

    let base_width = (window_width as i32) * 2;
    let base_height = (window_height as i32) * 2;
    let mut monitor_width = base_width + 64;
    let mut monitor_height = base_height + 64;

    if let Some(display) = gtk4::gdk::Display::default() {
        let monitor = display.monitors().item(0).and_downcast::<gtk4::gdk::Monitor>();
        if let Some(monitor) = monitor {
            let geometry = monitor.geometry();
            monitor_width = geometry.width();
            monitor_height = geometry.height();
        }
    }

    let max_default_width = (monitor_width - 40).max(200);
    let max_default_height = (monitor_height - 40).max(200);
    let default_width = base_width.min(max_default_width);
    let default_height = base_height.min(max_default_height);

    let review_width = ((monitor_width as f32) * 0.30) as i32;
    let review_height = ((monitor_height as f32) * 0.30) as i32;

    let center_window = move |window: &ApplicationWindow, width: i32, height: i32| {
        let offset_x = ((monitor_width - width) / 2).max(0);
        let offset_y = ((monitor_height - height) / 2).max(0);
        window.set_default_size(width, height);
        window.set_anchor(gtk4_layer_shell::Edge::Top, true);
        window.set_anchor(gtk4_layer_shell::Edge::Left, true);
        window.set_anchor(gtk4_layer_shell::Edge::Bottom, false);
        window.set_anchor(gtk4_layer_shell::Edge::Right, false);
        window.set_margin(gtk4_layer_shell::Edge::Left, offset_x);
        window.set_margin(gtk4_layer_shell::Edge::Top, offset_y);
    };

    center_window(&window, default_width, default_height);

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

    // Info line
    let info_label = Label::new(Some(""));
    info_label.add_css_class("info-text");
    info_label.set_halign(gtk4::Align::Start);
    info_label.set_xalign(0.0);
    info_label.set_wrap(true);
    info_label.set_margin_start(12);
    info_label.set_margin_end(12);
    info_label.set_margin_top(4);
    info_label.set_margin_bottom(2);

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
    main_box.append(&info_label);
    main_box.append(&status_bar);
    window.set_child(Some(&main_box));

    // Create state
    let response_tx_clone = response_tx.clone();
    let state = Rc::new(RefCell::new(OverlayState {
        buffer: PreviewBuffer::new(),
        status: OverlayStatus::Listening,
        review_active: false,
        review_options: Vec::new(),
        review_selected: 0,
        pending_selection: None,
        pending_type_output: false,
        response_tx,
        scrolled: scrolled.clone(),
        content_box,
        status_label,
        info_label,
        word_count_label,
    }));

    // Initial display
    state.borrow().update_display();
    state.borrow().update_status_display();

    // Key handling for review mode
    let state_key = state.clone();
    let window_key = window.clone();
    let key_controller = gtk4::EventControllerKey::new();
    let response_tx_key = response_tx_clone.clone();
    key_controller.connect_key_pressed(move |_, key, _, modifiers| {
        let mut state = state_key.borrow_mut();
        if !state.review_active {
            return glib::Propagation::Proceed;
        }
        if state.pending_selection.is_some() && state.pending_type_output {
            return glib::Propagation::Stop;
        }

        let option_count = state.review_options.len();
        let type_output = modifiers.contains(gdk::ModifierType::SHIFT_MASK);
        match key {
            gdk::Key::Up => {
                if option_count > 0 {
                    state.review_selected = (state.review_selected + option_count - 1) % option_count;
                    state.update_display();
                }
                glib::Propagation::Stop
            }
            gdk::Key::Down => {
                if option_count > 0 {
                    state.review_selected = (state.review_selected + 1) % option_count;
                    state.update_display();
                }
                glib::Propagation::Stop
            }
            gdk::Key::Right | gdk::Key::Return | gdk::Key::KP_Enter => {
                let selected = state.review_options.get(state.review_selected).cloned();
                if type_output {
                    state.pending_selection = selected;
                    state.pending_type_output = true;
                    state.update_display();
                    return glib::Propagation::Stop;
                }

                state.review_active = false;
                state.review_options.clear();
                state.pending_selection = None;
                state.pending_type_output = false;
                state.status = OverlayStatus::Paused;
                state.update_status_display();
                state.update_display();
                drop(state);

                window_key.set_keyboard_mode(gtk4_layer_shell::KeyboardMode::None);
                window_key.set_visible(false);
                center_window(&window_key, default_width, default_height);

                if let Some(option) = selected {
                    match option.choice {
                        ReviewChoice::Cancel => {
                            let _ = response_tx_key.send(OverlayResponse::Cancel);
                        }
                        _ => {
                            let _ = response_tx_key.send(OverlayResponse::ReviewSelection {
                                choice: option.choice,
                                text: option.text,
                                type_output,
                            });
                        }
                    }
                }
                glib::Propagation::Stop
            }
            gdk::Key::Escape => {
                state.review_active = false;
                state.review_options.clear();
                state.pending_selection = None;
                state.pending_type_output = false;
                state.status = OverlayStatus::Paused;
                state.update_status_display();
                state.update_display();
                drop(state);

                window_key.set_keyboard_mode(gtk4_layer_shell::KeyboardMode::None);
                window_key.set_visible(false);
                center_window(&window_key, default_width, default_height);

                let _ = response_tx_key.send(OverlayResponse::Cancel);
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        }
    });
    let state_release = state.clone();
    let window_release = window.clone();
    let response_tx_release = response_tx_clone.clone();
    key_controller.connect_key_released(move |_, key, _, modifiers| {
        let mut state = state_release.borrow_mut();
        if !state.review_active || state.pending_selection.is_none() {
            return glib::Propagation::Proceed;
        }
        if !matches!(key, gdk::Key::Shift_L | gdk::Key::Shift_R) {
            return glib::Propagation::Stop;
        }
        if modifiers.contains(gdk::ModifierType::SHIFT_MASK) {
            return glib::Propagation::Stop;
        }
        let selected = state.pending_selection.take();
        state.pending_type_output = false;
        state.review_active = false;
        state.review_options.clear();
        state.status = OverlayStatus::Paused;
        state.update_status_display();
        state.update_display();
        drop(state);

        window_release.set_keyboard_mode(gtk4_layer_shell::KeyboardMode::None);
        window_release.set_visible(false);
        center_window(&window_release, default_width, default_height);

        if let Some(option) = selected {
            match option.choice {
                ReviewChoice::Cancel => {
                    let _ = response_tx_release.send(OverlayResponse::Cancel);
                }
                _ => {
                    let _ = response_tx_release.send(OverlayResponse::ReviewSelection {
                        choice: option.choice,
                        text: option.text,
                        type_output: true,
                    });
                }
            }
        }
        glib::Propagation::Stop
    });
    window.add_controller(key_controller);

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
                OverlayCommand::ChunkCorrection { corrected, start, original_len } => {
                    state.buffer.apply_chunk_correction(start, original_len, &corrected);
                    state.update_display();
                }
                OverlayCommand::Checkpoint => {
                    if let Some(text) = state.buffer.checkpoint() {
                        let _ = state.response_tx.send(OverlayResponse::PasteText(text));
                    }
                    state.update_display();
                }
                OverlayCommand::Commit => {
                    state.review_active = false;
                    state.review_options.clear();
                    center_window(&window_clone, default_width, default_height);
                    if let Some(text) = state.buffer.commit() {
                        let _ = state.response_tx.send(OverlayResponse::PasteText(text));
                    }
                    state.update_display();
                    drop(state);
                    window_clone.set_visible(false);
                }
                OverlayCommand::Close => {
                    state.review_active = false;
                    state.review_options.clear();
                    center_window(&window_clone, default_width, default_height);
                    state.buffer.clear();
                    state.update_display();
                    drop(state);
                    window_clone.set_visible(false);
                }
                OverlayCommand::Status(status) => {
                    state.status = status;
                    state.update_status_display();
                }
                OverlayCommand::Info(info) => {
                    state.update_info(&info);
                }
                OverlayCommand::Review { options, selected } => {
                    state.review_active = true;
                    state.review_options = options;
                    state.review_selected = selected.min(state.review_options.len().saturating_sub(1));
                    state.status = OverlayStatus::Review;
                    state.update_status_display();
                    state.update_display();
                    drop(state);
                    center_window(&window_clone, review_width.max(default_width), review_height.max(default_height));
                    window_clone.set_keyboard_mode(gtk4_layer_shell::KeyboardMode::Exclusive);
                    window_clone.set_visible(true);
                    window_clone.present();
                }
                OverlayCommand::Show => {
                    drop(state);
                    window_clone.set_visible(true);
                    window_clone.present();
                }
                OverlayCommand::Hide => {
                    drop(state);
                    window_clone.set_visible(false);
                }
            }
        }
    });

    // Handle window close
    let state_close = state.clone();
    let window_close = window.clone();
    window.connect_close_request(move |_| {
        let mut state = state_close.borrow_mut();
        state.review_active = false;
        state.review_options.clear();
        state.buffer.clear();
        state.update_display();
        drop(state);
        window_close.set_visible(false);
        glib::Propagation::Stop
    });

    window.present();
}

/// Spawn the overlay in a separate thread
///
/// Returns a handle to communicate with the overlay.
/// The overlay runs its own GTK main loop in a dedicated thread.
pub fn spawn_overlay(window_width: u32, window_height: u32) -> Result<OverlayHandle> {
    // Channel for sending commands TO the overlay (async_channel for GTK thread)
    let (command_tx, command_rx) = async_channel::unbounded();

    // Channel for receiving responses FROM the overlay (standard mpsc for main thread)
    let (response_tx, response_rx) = mpsc::channel();

    // Spawn GTK thread
    std::thread::spawn(move || {
        // Check if layer-shell is supported (must be done after GTK init)
        let app = Application::builder()
            .application_id("com.ears.dictation.overlay")
            .flags(gtk4::gio::ApplicationFlags::NON_UNIQUE)
            .build();

        // Store command_rx in a RefCell to transfer into the activate callback
        let command_rx = std::cell::RefCell::new(Some(command_rx));
        let response_tx = response_tx.clone();

        app.connect_activate(move |app| {
            // Check layer-shell support after GTK is initialized
            if !gtk4_layer_shell::is_supported() {
                eprintln!("Layer-shell not supported (are you running on Wayland?)");
                return;
            }

            load_css();

            // Take the receiver (can only activate once)
            if let Some(rx) = command_rx.borrow_mut().take() {
                build_window(app, window_width, window_height, rx, response_tx.clone());
            }
        });

        // Run with empty args (we don't use GTK's arg parsing)
        app.run_with_args::<&str>(&[]);
    });

    Ok(OverlayHandle {
        command_tx,
        response_rx,
    })
}
