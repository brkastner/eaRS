# GTK4-Layer-Shell Overlay Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Replace the egui-based preview overlay with a gtk4-layer-shell overlay that doesn't steal focus, properly integrates with Wayland compositors, and can run as a standalone local binary connecting to remote STT/Ollama servers.

**Architecture:**
1. **New binary `ears-overlay`** - Standalone GTK4 overlay that connects to ears-server via WebSocket and Ollama via HTTP. Runs locally on the laptop.
2. **Separation of concerns** - Audio capture + STT on remote (athena), overlay + LLM correction requests on local (fw)
3. **Layer-shell integration** - Non-focusable overlay positioned at bottom-right
4. **Signal-based control** - SIGUSR1 (toggle), SIGUSR2 (checkpoint), auto-commit on 10s silence

**Tech Stack:** gtk4, gtk4-layer-shell, glib, tokio (async), reqwest (HTTP), tokio-tungstenite (WebSocket)

---

## Background

The current egui/eframe overlay has critical issues:
- Steals focus when clicked or interacted with
- Requires workarounds for non-main-thread event loops
- SIGHUP signal handling fails (terminates process)
- Can't commit without focusing the window
- Tightly coupled to ears-dictation (can't run standalone)

gtk4-layer-shell solves these by using Wayland's layer-shell protocol which allows:
- `KeyboardMode::None` - window never receives keyboard focus
- Proper compositor integration (no hacks)
- Native positioning via anchors

**New architecture benefit:** Running the overlay as a separate local process allows:
- Local GTK4 rendering (no X11 forwarding needed)
- Remote STT processing on powerful hardware (athena)
- Remote Ollama on GPU-equipped machine (athena)
- Lower latency display (overlay runs locally)

---

## Deployment Model

```
┌─────────────────────────────────────────────────────────────────┐
│ Laptop (fw) - Local                                             │
│  ┌──────────────────┐    ┌──────────────────────────────────┐  │
│  │ ears-overlay     │◄───│ Microphone audio via PipeWire    │  │
│  │ (GTK4 layer-shell│    │ (captured locally, sent to athena)│  │
│  │  overlay)        │    └──────────────────────────────────┘  │
│  └────────┬─────────┘                                           │
│           │ WebSocket (ws://athena:8765)                        │
│           │ HTTP (http://athena:11434) for Ollama               │
└───────────┼─────────────────────────────────────────────────────┘
            │
┌───────────▼─────────────────────────────────────────────────────┐
│ Desktop (athena) - Remote                                       │
│  ┌──────────────────┐    ┌──────────────────────────────────┐  │
│  │ ears-server      │    │ Ollama                            │  │
│  │ (STT processing) │    │ (LLM correction)                  │  │
│  │ Parakeet/Kyutai  │    │ qwen2.5:7b                        │  │
│  └──────────────────┘    └──────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
```

---

## Task 0: Create New Binary Structure

**Files:**
- Create: `src/bin/ears-overlay.rs`
- Modify: `Cargo.toml`

**Step 1: Add new binary to Cargo.toml**

```toml
[[bin]]
name = "ears-overlay"
path = "src/bin/ears-overlay.rs"
required-features = ["preview-overlay"]
```

**Step 2: Create minimal binary skeleton**

Create `src/bin/ears-overlay.rs`:

```rust
//! Standalone GTK4 layer-shell overlay for eaRS dictation.
//!
//! This binary runs locally and connects to:
//! - Remote ears-server for STT (WebSocket)
//! - Remote Ollama for LLM correction (HTTP)
//!
//! Usage:
//!   ears-overlay --server ws://athena:8765 --ollama http://athena:11434

use anyhow::{Context, Result};
use clap::Parser;

#[derive(Debug, Parser)]
#[command(name = "ears-overlay", about = "Standalone dictation overlay for eaRS")]
struct Args {
    #[arg(
        long,
        env = "EARS_SERVER_URL",
        default_value = "ws://localhost:8765",
        help = "WebSocket URL of the eaRS server"
    )]
    server: String,

    #[arg(
        long,
        env = "EARS_OLLAMA_URL",
        default_value = "http://localhost:11434",
        help = "Ollama API endpoint for LLM correction"
    )]
    ollama_url: String,

    #[arg(
        long,
        env = "EARS_OLLAMA_MODEL",
        default_value = "qwen2.5:7b",
        help = "Ollama model for text correction"
    )]
    ollama_model: String,

    #[arg(long, default_value = "400", help = "Overlay window width")]
    width: u32,

    #[arg(long, default_value = "200", help = "Overlay window height")]
    height: u32,
}

fn main() -> Result<()> {
    let args = Args::parse();
    eprintln!("ears-overlay starting...");
    eprintln!("Server: {}", args.server);
    eprintln!("Ollama: {} ({})", args.ollama_url, args.ollama_model);

    // TODO: Initialize GTK4 and layer-shell overlay
    // TODO: Connect to WebSocket server
    // TODO: Set up signal handlers
    // TODO: Run main loop

    Ok(())
}
```

**Step 3: Verify it compiles**

Run: `cargo check --features preview-overlay`
Expected: Compiles with warnings about unused code

**Step 4: Commit**

```bash
git add Cargo.toml src/bin/ears-overlay.rs
git commit -m "feat: add ears-overlay binary skeleton"
```

---

## Task 1: Update Dependencies

**Files:**
- Modify: `Cargo.toml`

**Step 1: Replace egui dependencies with gtk4**

In `Cargo.toml`, replace the preview-overlay dependencies:

```toml
# Remove these from [dependencies]:
# eframe = { version = "0.30", optional = true, ... }
# winit = { version = "0.30", optional = true }

# Add these:
gtk4 = { version = "0.8", optional = true, features = ["v4_10"] }
gtk4-layer-shell = { version = "0.7", optional = true }
glib = { version = "0.19", optional = true }

# Update feature:
preview-overlay = ["dep:gtk4", "dep:gtk4-layer-shell", "dep:glib", "dep:wl-clipboard-rs", "dep:arboard", "llm-correct"]
```

**Step 2: Verify dependencies resolve**

Run: `cargo check --features preview-overlay,parakeet`
Expected: Compiles without errors (may have warnings about unused code)

**Step 3: Commit**

```bash
git add Cargo.toml
git commit -m "deps: replace egui with gtk4-layer-shell for overlay"
```

---

## Task 2: Create GTK4 Overlay Module Structure

**Files:**
- Create: `src/gtk_overlay.rs`
- Modify: `src/lib.rs`

**Step 1: Create the module file with types**

Create `src/gtk_overlay.rs`:

```rust
//! GTK4 layer-shell overlay for buffer-first dictation.
//!
//! Uses Wayland layer-shell protocol for a non-focusable overlay
//! that displays transcribed text without stealing keyboard focus.

use crate::preview_buffer::{DisplaySection, DisplayStyle, PreviewBuffer};
use anyhow::Result;
use glib::MainContext;
use gtk4::prelude::*;
use gtk4::{Application, ApplicationWindow, Box as GtkBox, CssProvider, Label, Orientation, ScrolledWindow};
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
```

**Step 2: Add module to lib.rs**

In `src/lib.rs`, add:

```rust
#[cfg(feature = "preview-overlay")]
pub mod gtk_overlay;
```

**Step 3: Verify it compiles**

Run: `cargo check --features preview-overlay,parakeet`
Expected: Compiles (warnings about unused imports OK for now)

**Step 4: Commit**

```bash
git add src/gtk_overlay.rs src/lib.rs
git commit -m "feat: add gtk4 overlay module structure"
```

---

## Task 3: Implement OverlayHandle

**Files:**
- Modify: `src/gtk_overlay.rs`

**Step 1: Add OverlayHandle struct and methods**

Append to `src/gtk_overlay.rs`:

```rust
/// Handle to communicate with the overlay from the main thread
pub struct OverlayHandle {
    /// Send commands to the overlay (uses glib channel sender)
    command_tx: glib::Sender<OverlayCommand>,
    /// Receive responses from the overlay (standard mpsc for main thread)
    pub response_rx: Receiver<OverlayResponse>,
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
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(OverlayResponse::Closed),
        }
    }
}
```

**Step 2: Verify it compiles**

Run: `cargo check --features preview-overlay,parakeet`
Expected: Compiles with warnings about unused fields

**Step 3: Commit**

```bash
git add src/gtk_overlay.rs
git commit -m "feat: add OverlayHandle for gtk4 overlay communication"
```

---

## Task 4: Implement CSS Styling

**Files:**
- Modify: `src/gtk_overlay.rs`

**Step 1: Add CSS loading function**

Append to `src/gtk_overlay.rs`:

```rust
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
    provider.load_from_string(OVERLAY_CSS);

    gtk4::style_context_add_provider_for_display(
        &gtk4::gdk::Display::default().expect("Could not get default display"),
        &provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}
```

**Step 2: Verify it compiles**

Run: `cargo check --features preview-overlay,parakeet`
Expected: Compiles

**Step 3: Commit**

```bash
git add src/gtk_overlay.rs
git commit -m "feat: add CSS styling for gtk4 overlay"
```

---

## Task 5: Implement Overlay UI Builder

**Files:**
- Modify: `src/gtk_overlay.rs`

**Step 1: Add UI state struct and builder**

Append to `src/gtk_overlay.rs`:

```rust
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
        self.word_count_label.set_label(&format!("{} words", self.buffer.active_word_count()));
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
```

**Step 2: Verify it compiles**

Run: `cargo check --features preview-overlay,parakeet`
Expected: Compiles

**Step 3: Commit**

```bash
git add src/gtk_overlay.rs
git commit -m "feat: add overlay UI state and display update logic"
```

---

## Task 6: Implement Window Builder with Layer-Shell

**Files:**
- Modify: `src/gtk_overlay.rs`

**Step 1: Add window building function**

Append to `src/gtk_overlay.rs`:

```rust
fn build_window(
    app: &Application,
    window_width: u32,
    window_height: u32,
    command_rx: glib::Receiver<OverlayCommand>,
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
    window.set_layer_shell_margin(gtk4_layer_shell::Edge::Bottom, 32);
    window.set_layer_shell_margin(gtk4_layer_shell::Edge::Right, 32);

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

    // Handle commands from main thread
    let state_clone = state.clone();
    let window_clone = window.clone();
    command_rx.attach(None, move |cmd| {
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
            }
            OverlayCommand::Close => {
                let _ = state.response_tx.send(OverlayResponse::Closed);
                window_clone.close();
            }
            OverlayCommand::Status(status) => {
                state.status = status;
                state.update_status_display();
            }
        }
        glib::ControlFlow::Continue
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
```

**Step 2: Verify it compiles**

Run: `cargo check --features preview-overlay,parakeet`
Expected: Compiles

**Step 3: Commit**

```bash
git add src/gtk_overlay.rs
git commit -m "feat: add layer-shell window builder with command handling"
```

---

## Task 7: Implement spawn_overlay Function

**Files:**
- Modify: `src/gtk_overlay.rs`

**Step 1: Add spawn function**

Append to `src/gtk_overlay.rs`:

```rust
/// Spawn the overlay in a separate thread
///
/// Returns a handle to communicate with the overlay.
/// The overlay runs its own GTK main loop in a dedicated thread.
pub fn spawn_overlay(window_width: u32, window_height: u32) -> Result<OverlayHandle> {
    // Check if layer-shell is supported
    if !gtk4_layer_shell::is_supported() {
        anyhow::bail!("Layer-shell not supported (are you running on Wayland?)");
    }

    // Channel for sending commands TO the overlay (glib channel for GTK thread)
    let (command_tx, command_rx) = MainContext::channel(glib::Priority::DEFAULT);

    // Channel for receiving responses FROM the overlay (standard mpsc for main thread)
    let (response_tx, response_rx) = mpsc::channel();

    // Spawn GTK thread
    std::thread::spawn(move || {
        let app = Application::builder()
            .application_id("com.ears.dictation.overlay")
            .build();

        let response_tx_clone = response_tx.clone();
        app.connect_activate(move |app| {
            load_css();
            build_window(app, window_width, window_height, command_rx.clone(), response_tx_clone.clone());
        });

        // Run with empty args (we don't use GTK's arg parsing)
        app.run_with_args::<&str>(&[]);

        // Send closed response when GTK exits
        let _ = response_tx.send(OverlayResponse::Closed);
    });

    Ok(OverlayHandle {
        command_tx,
        response_rx,
    })
}
```

**Step 2: Verify it compiles**

Run: `cargo check --features preview-overlay,parakeet`
Expected: May have errors about `command_rx.clone()` - glib::Receiver doesn't implement Clone

**Step 3: Fix the channel issue**

The glib channel receiver can't be cloned. We need to pass it directly. Update the spawn function:

```rust
/// Spawn the overlay in a separate thread
pub fn spawn_overlay(window_width: u32, window_height: u32) -> Result<OverlayHandle> {
    // Check if layer-shell is supported
    if !gtk4_layer_shell::is_supported() {
        anyhow::bail!("Layer-shell not supported (are you running on Wayland?)");
    }

    // Channel for sending commands TO the overlay (glib channel for GTK thread)
    let (command_tx, command_rx) = MainContext::channel(glib::Priority::DEFAULT);

    // Channel for receiving responses FROM the overlay (standard mpsc for main thread)
    let (response_tx, response_rx) = mpsc::channel();

    // Spawn GTK thread
    std::thread::spawn(move || {
        let app = Application::builder()
            .application_id("com.ears.dictation.overlay")
            .build();

        // Store these in thread-local storage for the activate callback
        let command_rx = std::cell::RefCell::new(Some(command_rx));
        let response_tx = response_tx.clone();

        app.connect_activate(move |app| {
            load_css();
            // Take the receiver (can only activate once)
            if let Some(rx) = command_rx.borrow_mut().take() {
                build_window(app, window_width, window_height, rx, response_tx.clone());
            }
        });

        app.run_with_args::<&str>(&[]);
    });

    Ok(OverlayHandle {
        command_tx,
        response_rx,
    })
}
```

**Step 4: Verify it compiles**

Run: `cargo check --features preview-overlay,parakeet`
Expected: Compiles

**Step 5: Commit**

```bash
git add src/gtk_overlay.rs
git commit -m "feat: add spawn_overlay function for gtk4 layer-shell"
```

---

## Task 8: Update Dictation Client Imports

**Files:**
- Modify: `src/bin/ears-dictation.rs`

**Step 1: Update imports to use new module**

Change the imports from:
```rust
#[cfg(feature = "preview-overlay")]
use ears::preview_overlay::{spawn_overlay, OverlayHandle, OverlayResponse, OverlayStatus};
```

To:
```rust
#[cfg(feature = "preview-overlay")]
use ears::gtk_overlay::{spawn_overlay, OverlayHandle, OverlayResponse, OverlayStatus};
```

**Step 2: Verify it compiles**

Run: `cargo check --features preview-overlay,parakeet`
Expected: Compiles

**Step 3: Commit**

```bash
git add src/bin/ears-dictation.rs
git commit -m "refactor: use gtk_overlay module in dictation client"
```

---

## Task 9: Add Auto-Commit on Extended Silence

**Files:**
- Modify: `src/bin/ears-dictation.rs`

**Step 1: Find the pause detection code**

Look for the existing pause detection that triggers LLM correction (around 1.5-2 second pause). We'll add a longer timeout (10 seconds) that triggers auto-commit.

**Step 2: Add auto-commit constant and tracking**

Near the top of the main function, after `last_word_time` is defined, add:

```rust
#[cfg(all(feature = "preview-overlay", feature = "llm-correct"))]
const AUTO_COMMIT_PAUSE_SECS: u64 = 10;
```

**Step 3: Add auto-commit check in the main loop**

In the default timeout handler (around line 730-740), after the existing pause detection for correction, add:

```rust
// Auto-commit after extended silence (10+ seconds)
#[cfg(feature = "preview-overlay")]
if preview_mode && overlay_handle.is_some() {
    let silence_duration = last_word_time.elapsed().as_secs();
    if silence_duration >= AUTO_COMMIT_PAUSE_SECS && correction_buffer.word_count() > 0 {
        eprintln!("[AUTO-COMMIT] {} seconds of silence, committing", silence_duration);
        if let Some(ref handle) = overlay_handle {
            let _ = handle.commit();
        }
        *capturing.lock().unwrap() = false;
    }
}
```

**Step 4: Verify it compiles**

Run: `cargo check --features preview-overlay,parakeet`
Expected: Compiles

**Step 5: Commit**

```bash
git add src/bin/ears-dictation.rs
git commit -m "feat: add auto-commit after 10 seconds of silence"
```

---

## Task 10: Remove Old egui Overlay Module

**Files:**
- Delete: `src/preview_overlay.rs`
- Modify: `src/lib.rs`

**Step 1: Remove old module from lib.rs**

Remove or comment out:
```rust
#[cfg(feature = "preview-overlay")]
pub mod preview_overlay;
```

**Step 2: Delete the old file**

```bash
rm src/preview_overlay.rs
```

**Step 3: Remove winit import from preview_overlay.rs references**

Search for any remaining references to `preview_overlay` or `winit` and remove them.

**Step 4: Verify it compiles**

Run: `cargo build --release --features preview-overlay,parakeet,amd`
Expected: Builds successfully

**Step 5: Commit**

```bash
git add -A
git commit -m "refactor: remove old egui overlay module"
```

---

## Task 11: Integration Testing

**Files:** None (manual testing)

**Step 1: Build the release binary**

Run: `cargo build --release --features preview-overlay,parakeet,amd`
Expected: Builds successfully

**Step 2: Install and restart service**

```bash
systemctl --user stop ears-dictation-remote
cp target/release/ears-dictation ~/.cargo/bin/
systemctl --user start ears-dictation-remote
```

**Step 3: Test overlay spawning**

Press XF86Tools to toggle on.
Expected: Overlay appears at bottom-right, doesn't steal focus

**Step 4: Test dictation flow**

1. Speak some words while focused on another window
2. Verify words appear in overlay
3. Press numpad - for checkpoint
4. Verify text pastes to focused window
5. Speak more words
6. Wait 10 seconds
7. Verify auto-commit triggers

**Step 5: Test toggle off/on cycle**

1. Press XF86Tools to toggle off
2. Overlay should close
3. Press XF86Tools to toggle on
4. Fresh overlay should appear

**Step 6: Commit final state**

```bash
git add -A
git commit -m "test: verify gtk4 layer-shell overlay integration"
```

---

## Task 12: Update Documentation

**Files:**
- Modify: `CLAUDE.md`

**Step 1: Update architecture section**

Add note about gtk4-layer-shell overlay:

```markdown
### Preview Overlay

The preview overlay uses gtk4-layer-shell for a non-focusable Wayland overlay:
- Displays transcribed text without stealing keyboard focus
- Positioned at bottom-right via layer-shell anchors
- Signals: SIGUSR1 (toggle), SIGUSR2 (checkpoint)
- Auto-commits after 10 seconds of silence
- Requires: gtk4, gtk4-layer-shell system libraries
```

**Step 2: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: update architecture for gtk4 layer-shell overlay"
```

---

## Task 13: Create Systemd Service for ears-overlay

**Files:**
- Create: `contrib/systemd/ears-overlay.service`

**Step 1: Create service file**

Create `contrib/systemd/ears-overlay.service`:

```ini
[Unit]
Description=eaRS dictation overlay (local GTK4 layer-shell)
After=graphical-session.target

[Service]
Type=simple
# Connect to remote ears-server and ollama on athena
Environment=EARS_SERVER_URL=ws://athena:8765
Environment=EARS_OLLAMA_URL=http://athena:11434
Environment=EARS_OLLAMA_MODEL=qwen2.5:7b
ExecStart=%h/.cargo/bin/ears-overlay
Restart=on-failure
RestartSec=2

[Install]
WantedBy=graphical-session.target
```

**Step 2: Install and enable**

```bash
cp contrib/systemd/ears-overlay.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable ears-overlay
```

**Step 3: Commit**

```bash
git add contrib/systemd/ears-overlay.service
git commit -m "feat: add systemd service for standalone ears-overlay"
```

---

## Task 14: Wire Up Complete ears-overlay Binary

**Files:**
- Modify: `src/bin/ears-overlay.rs`

**Step 1: Implement full binary with WebSocket, signals, and GTK**

This is the main integration task. The binary needs to:

1. Initialize GTK4 application with layer-shell
2. Connect to ears-server via WebSocket
3. Handle SIGUSR1 (toggle), SIGUSR2 (checkpoint) signals
4. Capture audio locally and send to server
5. Receive transcription words and display in overlay
6. Send text to Ollama for correction
7. Handle checkpoint/commit/auto-commit

See `src/bin/ears-dictation.rs` for reference on:
- WebSocket connection
- Audio capture
- LLM correction
- Signal handling

Key difference: GTK4 runs its own main loop, so async operations need to integrate with glib::MainContext.

**Step 2: Verify it compiles and runs**

```bash
cargo build --release --features preview-overlay
./target/release/ears-overlay --server ws://athena:8765 --ollama http://athena:11434
```

**Step 3: Commit**

```bash
git add src/bin/ears-overlay.rs
git commit -m "feat: implement complete ears-overlay binary"
```

---

## Summary

After completing all tasks:

1. **New binary:** `ears-overlay` - standalone local overlay connecting to remote servers
2. **Dependencies changed:** egui/eframe/winit → gtk4/gtk4-layer-shell/glib
3. **New module:** `src/gtk_overlay.rs` (layer-shell based overlay)
4. **Removed:** `src/preview_overlay.rs` (old egui overlay)
5. **New feature:** Auto-commit after 10 seconds silence
6. **Behavior:** Overlay no longer steals focus, runs locally

**Architecture:**
```
Laptop (fw):                    Desktop (athena):
┌──────────────┐               ┌──────────────┐
│ ears-overlay │◄──WebSocket──►│ ears-server  │
│ (GTK4 local) │               │ (STT)        │
│              │◄────HTTP─────►│ Ollama       │
│ [Microphone] │               │ (LLM)        │
└──────────────┘               └──────────────┘
```

**Keybinds:**
- XF86Tools: Toggle dictation on/off (spawns/closes overlay)
- Numpad -: Checkpoint (paste buffer, continue)
- Auto-commit: 10 seconds of silence

**Services:**
- `ears-overlay.service` - Runs on laptop, connects to athena
- `ears-server.service` - Runs on athena (unchanged)

**Testing checklist:**
- [ ] Overlay appears without stealing focus
- [ ] Words appear in overlay while typing elsewhere
- [ ] Checkpoint pastes to focused window (Ctrl+Shift+V for terminal)
- [ ] Auto-commit triggers after 10 seconds silence
- [ ] Toggle cycle works (off/on spawns fresh overlay)
- [ ] Service survives signals (no unexpected termination)
- [ ] LLM correction works via remote Ollama
- [ ] Committed sections show dimmed, active shows bright

**Migration from ears-dictation-remote:**
1. Stop old service: `systemctl --user stop ears-dictation-remote`
2. Disable old service: `systemctl --user disable ears-dictation-remote`
3. Start new service: `systemctl --user start ears-overlay`
4. Enable new service: `systemctl --user enable ears-overlay`
