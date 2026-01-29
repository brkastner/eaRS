//! Preview overlay window using egui for buffer-first dictation.
//!
//! Displays the preview buffer with committed sections (dimmed) and active section (bright),
//! receives updates via channels, and handles checkpoint/commit commands.

use crate::preview_buffer::{DisplaySection, DisplayStyle, PreviewBuffer};
use anyhow::Result;
use eframe::egui::{self, Color32, RichText, ViewportBuilder};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;

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

/// Handle to communicate with the overlay
pub struct OverlayHandle {
    /// Send commands to the overlay
    pub command_tx: Sender<OverlayCommand>,
    /// Receive responses from the overlay
    pub response_rx: Receiver<OverlayResponse>,
    /// Thread handle for the overlay
    thread_handle: Option<thread::JoinHandle<()>>,
}

impl OverlayHandle {
    /// Send a word to the overlay
    pub fn send_word(&self, word: String) -> Result<()> {
        self.command_tx
            .send(OverlayCommand::Word(word))
            .map_err(|e| anyhow::anyhow!("Failed to send word to overlay: {}", e))
    }

    /// Send a correction to the overlay
    pub fn send_correction(&self, corrected: String) -> Result<()> {
        self.command_tx
            .send(OverlayCommand::Correction(corrected))
            .map_err(|e| anyhow::anyhow!("Failed to send correction to overlay: {}", e))
    }

    /// Request a checkpoint (paste and continue)
    pub fn checkpoint(&self) -> Result<()> {
        self.command_tx
            .send(OverlayCommand::Checkpoint)
            .map_err(|e| anyhow::anyhow!("Failed to send checkpoint to overlay: {}", e))
    }

    /// Request a commit (paste all and close)
    pub fn commit(&self) -> Result<()> {
        self.command_tx
            .send(OverlayCommand::Commit)
            .map_err(|e| anyhow::anyhow!("Failed to send commit to overlay: {}", e))
    }

    /// Update the status indicator
    pub fn set_status(&self, status: OverlayStatus) -> Result<()> {
        self.command_tx
            .send(OverlayCommand::Status(status))
            .map_err(|e| anyhow::anyhow!("Failed to send status to overlay: {}", e))
    }

    /// Close the overlay
    pub fn close(&self) -> Result<()> {
        self.command_tx
            .send(OverlayCommand::Close)
            .map_err(|e| anyhow::anyhow!("Failed to send close to overlay: {}", e))
    }

    /// Check for a response (non-blocking)
    pub fn try_recv(&self) -> Option<OverlayResponse> {
        match self.response_rx.try_recv() {
            Ok(response) => Some(response),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(OverlayResponse::Closed),
        }
    }

    /// Wait for the overlay thread to finish
    pub fn join(mut self) -> Result<()> {
        if let Some(handle) = self.thread_handle.take() {
            handle.join().map_err(|_| anyhow::anyhow!("Overlay thread panicked"))?;
        }
        Ok(())
    }
}

/// Spawn the overlay window in a separate thread
pub fn spawn_overlay(window_width: u32, window_height: u32) -> Result<OverlayHandle> {
    let (command_tx, command_rx) = mpsc::channel();
    let (response_tx, response_rx) = mpsc::channel();

    let thread_handle = thread::spawn(move || {
        if let Err(e) = run_overlay(command_rx, response_tx, window_width, window_height) {
            eprintln!("Overlay error: {}", e);
        }
    });

    Ok(OverlayHandle {
        command_tx,
        response_rx,
        thread_handle: Some(thread_handle),
    })
}

/// Run the overlay window (called from spawned thread)
fn run_overlay(
    command_rx: Receiver<OverlayCommand>,
    response_tx: Sender<OverlayResponse>,
    window_width: u32,
    window_height: u32,
) -> Result<()> {
    let options = eframe::NativeOptions {
        viewport: ViewportBuilder::default()
            .with_title("eaRS Preview")
            .with_inner_size([window_width as f32, window_height as f32])
            .with_always_on_top()
            .with_decorations(true)
            .with_resizable(true),
        ..Default::default()
    };

    let app = PreviewApp::new(command_rx, response_tx);

    eframe::run_native("eaRS Preview", options, Box::new(|_cc| Ok(Box::new(app))))
        .map_err(|e| anyhow::anyhow!("Failed to run overlay: {}", e))
}

/// The egui application for the preview overlay
struct PreviewApp {
    buffer: PreviewBuffer,
    command_rx: Receiver<OverlayCommand>,
    response_tx: Sender<OverlayResponse>,
    status: OverlayStatus,
    should_close: bool,
}

impl PreviewApp {
    fn new(command_rx: Receiver<OverlayCommand>, response_tx: Sender<OverlayResponse>) -> Self {
        Self {
            buffer: PreviewBuffer::new(),
            command_rx,
            response_tx,
            status: OverlayStatus::Listening,
            should_close: false,
        }
    }

    fn process_commands(&mut self) {
        // Process all pending commands
        loop {
            match self.command_rx.try_recv() {
                Ok(command) => self.handle_command(command),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.should_close = true;
                    break;
                }
            }
        }
    }

    fn handle_command(&mut self, command: OverlayCommand) {
        match command {
            OverlayCommand::Word(word) => {
                self.buffer.add_word(word);
            }
            OverlayCommand::Correction(corrected) => {
                self.buffer.apply_correction(corrected);
            }
            OverlayCommand::Checkpoint => {
                if let Some(text) = self.buffer.checkpoint() {
                    let _ = self.response_tx.send(OverlayResponse::PasteText(text));
                }
            }
            OverlayCommand::Commit => {
                if let Some(text) = self.buffer.commit() {
                    let _ = self.response_tx.send(OverlayResponse::PasteText(text));
                }
                self.should_close = true;
            }
            OverlayCommand::Close => {
                self.should_close = true;
            }
            OverlayCommand::Status(status) => {
                self.status = status;
            }
        }
    }

    fn render_section(&self, ui: &mut egui::Ui, section: &DisplaySection) {
        match section.style {
            DisplayStyle::Committed => {
                ui.label(
                    RichText::new(&section.text)
                        .color(Color32::from_rgb(128, 128, 128))
                        .size(14.0),
                );
            }
            DisplayStyle::Active => {
                ui.label(
                    RichText::new(&section.text)
                        .color(Color32::from_rgb(240, 240, 240))
                        .size(16.0),
                );
            }
            DisplayStyle::Separator => {
                ui.separator();
            }
        }
    }
}

impl eframe::App for PreviewApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Process any pending commands
        self.process_commands();

        // Request continuous repaints to check for new commands
        ctx.request_repaint_after(std::time::Duration::from_millis(50));

        // Close if requested
        if self.should_close {
            let _ = self.response_tx.send(OverlayResponse::Closed);
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        // Dark background
        let frame = egui::Frame::central_panel(&ctx.style())
            .fill(Color32::from_rgb(30, 30, 35));

        egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
            // Scrollable area for content
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let sections = self.buffer.display_sections();

                    if sections.is_empty() {
                        ui.label(
                            RichText::new("Waiting for speech...")
                                .color(Color32::from_rgb(100, 100, 100))
                                .italics(),
                        );
                    } else {
                        for section in &sections {
                            self.render_section(ui, section);
                        }
                    }
                });

            // Status bar at bottom
            ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                ui.horizontal(|ui| {
                    let status_color = match self.status {
                        OverlayStatus::Listening => Color32::from_rgb(100, 200, 100),
                        OverlayStatus::Correcting => Color32::from_rgb(200, 200, 100),
                        OverlayStatus::Paused => Color32::from_rgb(150, 150, 150),
                    };
                    ui.label(RichText::new(self.status.as_str()).color(status_color).size(12.0));

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!("{} words", self.buffer.active_word_count()))
                                .color(Color32::from_rgb(100, 100, 100))
                                .size(11.0),
                        );
                    });
                });
            });
        });
    }
}
