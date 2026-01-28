use anyhow::{Context, Result};
use clap::Parser;
use crossbeam_channel::{bounded, select, unbounded};
use ears::audio;
#[cfg(feature = "hooks")]
use ears::config::DictationHooksConfig;
use ears::config::{AppConfig, DictationNotificationConfig};
#[cfg(feature = "llm-correct")]
use ears::llm_correct::{LlmCorrectConfig, SentenceCorrector};
use ears::virtual_keyboard::{create_virtual_keyboard, VirtualKeyboard, SpecialKey};
use futures_util::{SinkExt, StreamExt};
use notifica::notify;
use rdev::{EventType, listen};
use serde_json::Value;
use std::fs;
#[cfg(feature = "hooks")]
use std::process::Command as ProcessCommand;
use std::sync::{Arc, Mutex};
use std::thread;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};

const PID_FILE_NAME: &str = "dictation.pid";

#[derive(Clone, Debug, clap::ValueEnum)]
enum EngineArg {
    Kyutai,
    #[cfg(feature = "parakeet")]
    Parakeet,
}

impl EngineArg {
    fn as_str(&self) -> &'static str {
        match self {
            EngineArg::Kyutai => "kyutai",
            #[cfg(feature = "parakeet")]
            EngineArg::Parakeet => "parakeet",
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum DictationEvent {
    Started,
    Paused,
    Stopped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DictationState {
    Listening,
    Suspended,
    Inactive,
}

#[derive(Debug, Parser)]
#[command(name = "ears-dictation", about = "Dictation client for eaRS")]
struct Args {
    #[arg(
        long,
        help = "Set the transcription language (e.g., 'en', 'de', 'es', 'fr', 'ja')"
    )]
    lang: Option<String>,

    #[arg(long, value_enum, help = "Select transcription engine (kyutai|parakeet)")]
    engine: Option<EngineArg>,

    #[arg(
        long,
        env = "EARS_SERVER_URL",
        help = "WebSocket URL of the eaRS server (e.g., ws://192.168.1.100:8765)"
    )]
    server: Option<String>,

    #[cfg(feature = "llm-correct")]
    #[arg(
        long,
        env = "EARS_OLLAMA_URL",
        help = "Ollama API endpoint for LLM correction (e.g., http://192.168.1.100:11434)"
    )]
    ollama_url: Option<String>,

    #[cfg(feature = "llm-correct")]
    #[arg(
        long,
        env = "EARS_OLLAMA_MODEL",
        default_value = "qwen2.5:7b",
        help = "Ollama model for text correction"
    )]
    ollama_model: String,
}

fn get_pid_file() -> std::path::PathBuf {
    let state_dir = if let Ok(xdg_state) = std::env::var("XDG_STATE_HOME") {
        if !xdg_state.is_empty() {
            std::path::PathBuf::from(xdg_state)
        } else {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            std::path::PathBuf::from(home).join(".local/state")
        }
    } else {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        std::path::PathBuf::from(home).join(".local/state")
    };
    state_dir.join("ears").join(PID_FILE_NAME)
}

fn write_pid_file() -> Result<()> {
    let pid_file = get_pid_file();
    if let Some(parent) = pid_file.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&pid_file, std::process::id().to_string())?;
    Ok(())
}

fn remove_pid_file() {
    let pid_file = get_pid_file();
    let _ = fs::remove_file(pid_file);
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let config = AppConfig::load().unwrap_or_default();
    let url = args.server.clone().unwrap_or_else(|| {
        let port = config.server.websocket_port;
        format!("ws://127.0.0.1:{}", port)
    });

    write_pid_file()?;

    let running = Arc::new(Mutex::new(true));
    let capturing = Arc::new(Mutex::new(false));
    let dictation_state = Arc::new(Mutex::new(DictationState::Inactive));

    let (stop_tx, stop_rx) = bounded::<()>(1);
    let running_clone = running.clone();

    ctrlc::set_handler(move || {
        *running_clone.lock().unwrap() = false;
        let _ = stop_tx.send(());
    })
    .context("Failed to set Ctrl+C handler")?;

    // SIGUSR1 toggles capturing state for external hotkey integration
    {
        let sig_capturing = capturing.clone();
        let sig_state = dictation_state.clone();
        let sig_notif = config.dictation.notifications.clone();
        #[cfg(feature = "hooks")]
        let sig_hooks = config.dictation.hooks.clone();
        tokio::spawn(async move {
            let mut sigusr1 = signal(SignalKind::user_defined1())
                .expect("failed to register SIGUSR1 handler");
            loop {
                sigusr1.recv().await;
                let mut c = sig_capturing.lock().unwrap();
                *c = !*c;
                let is_active = *c;
                eprintln!("SIGUSR1: audio capture {}", if is_active { "started" } else { "stopped" });
                drop(c);
                let event = if is_active {
                    DictationState::Listening
                } else {
                    DictationState::Suspended
                };
                #[cfg(feature = "hooks")]
                apply_state_change(&sig_state, event, &sig_notif, &sig_hooks);
                #[cfg(not(feature = "hooks"))]
                apply_state_change(&sig_state, event, &sig_notif);
            }
        });
    }

    let hotkey_running = running.clone();
    let hotkey_capturing = capturing.clone();
    let hotkey_config = config.hotkeys.clone();
    let notification_config = config.dictation.notifications.clone();
    #[cfg(feature = "hooks")]
    let hook_config = config.dictation.hooks.clone();

    if hotkey_config.enable_internal {
        eprintln!("Initializing hotkey listener for: {}", hotkey_config.toggle);
        let dictation_state_thread = dictation_state.clone();
        #[cfg(feature = "hooks")]
        let hook_config_thread = hook_config.clone();
        let notification_config_thread = notification_config.clone();
        thread::spawn(move || {
            let toggle_combo = hotkey_config.toggle.to_lowercase();
            let (t_ctrl, t_shift, t_alt, t_key) = parse_combo(&toggle_combo);
            eprintln!(
                "Parsed combo - ctrl:{} shift:{} alt:{} key:{:?}",
                t_ctrl, t_shift, t_alt, t_key
            );

            if let Err(e) = listen(move |event| -> () {
                static mut CTRL: bool = false;
                static mut SHIFT: bool = false;
                static mut ALT: bool = false;

                match event.event_type {
                    EventType::KeyPress(rdev::Key::ControlLeft)
                    | EventType::KeyPress(rdev::Key::ControlRight) => unsafe {
                        CTRL = true;
                    },
                    EventType::KeyRelease(rdev::Key::ControlLeft)
                    | EventType::KeyRelease(rdev::Key::ControlRight) => unsafe {
                        CTRL = false;
                    },
                    EventType::KeyPress(rdev::Key::ShiftLeft)
                    | EventType::KeyPress(rdev::Key::ShiftRight) => unsafe {
                        SHIFT = true;
                    },
                    EventType::KeyRelease(rdev::Key::ShiftLeft)
                    | EventType::KeyRelease(rdev::Key::ShiftRight) => unsafe {
                        SHIFT = false;
                    },
                    EventType::KeyPress(rdev::Key::Alt) | EventType::KeyPress(rdev::Key::AltGr) => unsafe {
                        ALT = true;
                    },
                    EventType::KeyRelease(rdev::Key::Alt)
                    | EventType::KeyRelease(rdev::Key::AltGr) => unsafe {
                        ALT = false;
                    },
                    EventType::KeyRelease(k) => unsafe {
                        if !*hotkey_running.lock().unwrap() {
                            return;
                        }
                        if CTRL == t_ctrl && SHIFT == t_shift && ALT == t_alt && k == t_key {
                            let mut c = hotkey_capturing.lock().unwrap();
                            *c = !*c;
                            let is_active = *c;
                            eprintln!(
                                "Audio capture {}",
                                if is_active { "started" } else { "stopped" }
                            );
                            drop(c);
                            let event = if is_active {
                                DictationState::Listening
                            } else {
                                DictationState::Suspended
                            };
                            #[cfg(feature = "hooks")]
                            apply_state_change(
                                &dictation_state_thread,
                                event,
                                &notification_config_thread,
                                &hook_config_thread,
                            );
                            #[cfg(not(feature = "hooks"))]
                            apply_state_change(
                                &dictation_state_thread,
                                event,
                                &notification_config_thread,
                            );
                        }
                    },
                    _ => {}
                }
            }) {
                eprintln!("Hotkey listener error: {:?}", e);
            }
        });
    }

    #[cfg(feature = "hooks")]
    apply_state_change(
        &dictation_state,
        DictationState::Listening,
        &notification_config,
        &hook_config,
    );
    #[cfg(not(feature = "hooks"))]
    apply_state_change(
        &dictation_state,
        DictationState::Listening,
        &notification_config,
    );

    let (audio_tx, audio_rx) = unbounded();
    let device_index = None;

    thread::spawn(move || {
        if let Err(e) = audio::start_audio_capture(audio_tx, device_index) {
            eprintln!("Audio capture error: {}", e);
        }
    });

    eprintln!("ears-dictation started");
    eprintln!("Connecting to {}...", url);
    eprintln!("Hotkey: {} to toggle pause/resume", config.hotkeys.toggle);
    eprintln!("Press Ctrl+C to stop\n");

    loop {
        let is_running = *running.lock().unwrap();
        if !is_running {
            break;
        }

        match connect_async(&url).await {
            Ok((ws_stream, _)) => {
                eprintln!("Connected to transcription server");
                let (mut write, mut read) = ws_stream.split();
                let mut keyboard = create_virtual_keyboard()
                    .context("Failed to initialize virtual keyboard. \
                              On Linux/Wayland, ensure you are in the 'input' group.")?;

                #[cfg(feature = "llm-correct")]
                let mut corrector = {
                    let ollama_url = args.ollama_url.clone()
                        .unwrap_or_else(|| "http://localhost:11434".to_string());
                    let config = LlmCorrectConfig {
                        endpoint: ollama_url.clone(),
                        model: args.ollama_model.clone(),
                        timeout_secs: 10,
                    };
                    eprintln!("LLM correction enabled: {} ({})", config.model, ollama_url);
                    SentenceCorrector::new(config)?
                };
                #[cfg(feature = "llm-correct")]
                let mut sentence_buffer = SentenceBuffer::new();

                let (writer_tx, mut writer_rx) = mpsc::unbounded_channel::<WriterCommand>();

                if let Some(ref lang) = args.lang {
                    eprintln!("Setting language to: {}", lang);
                    let lang_cmd = serde_json::json!({
                        "type": "setlanguage",
                        "lang": lang
                    })
                    .to_string();
                    if let Err(e) = writer_tx.send(WriterCommand::Text(lang_cmd)) {
                        eprintln!("Failed to send language command: {}", e);
                    }
                }

                if let Some(ref engine) = args.engine {
                    eprintln!("Selecting engine: {}", engine.as_str());
                    let engine_cmd = serde_json::json!({
                        "type": "setengine",
                        "engine": engine.as_str(),
                    })
                    .to_string();
                    let _ = writer_tx.send(WriterCommand::Text(engine_cmd));
                }

                let audio_writer = writer_tx.clone();
                let audio_rx_clone = audio_rx.clone();
                let audio_capturing = capturing.clone();
                thread::spawn(move || {
                    while let Ok(chunk) = audio_rx_clone.recv() {
                        if *audio_capturing.lock().unwrap() {
                            if audio_writer
                                .send(WriterCommand::Audio(encode_chunk(&chunk)))
                                .is_err()
                            {
                                break;
                            }
                        }
                    }
                });

                let writer_handle = tokio::spawn(async move {
                    while let Some(cmd) = writer_rx.recv().await {
                        match cmd {
                            WriterCommand::Audio(bytes) => {
                                if write.send(Message::binary(bytes)).await.is_err() {
                                    break;
                                }
                            }
                            WriterCommand::Text(text) => {
                                if write.send(Message::text(text)).await.is_err() {
                                    break;
                                }
                            }
                            WriterCommand::Stop => {
                                // Send close frame to properly terminate the WebSocket
                                let _ = write.send(Message::Close(None)).await;
                                break;
                            }
                        }
                    }
                });

                loop {
                    select! {
                        recv(stop_rx) -> _ => {
                            break;
                        }
                        default => {
                            if let Some(message) = read.next().await {
                                match message {
                                    Ok(Message::Text(text)) => {
                                        // eprintln!("[WS RECEIVED] {}", text);
                                        if let Ok(json) = serde_json::from_str::<Value>(&text) {
                                            #[cfg(feature = "llm-correct")]
                                            {
                                                let is_capturing = *capturing.lock().unwrap();
                                                if is_capturing {
                                                    if let Some(word) = json.get("word").and_then(|v| v.as_str()) {
                                                        if let Err(e) = handle_word_with_correction(
                                                            word,
                                                            &mut keyboard,
                                                            &mut sentence_buffer,
                                                            &mut corrector,
                                                        ).await {
                                                            eprintln!("[ERROR] {}", e);
                                                        }
                                                    }
                                                }
                                            }
                                            #[cfg(not(feature = "llm-correct"))]
                                            {
                                                handle_message(&json, &mut keyboard, &capturing)?;
                                            }
                                        } else {
                                            eprintln!("[ERROR] Failed to parse JSON");
                                        }
                                    }
                                    Ok(Message::Binary(data)) => {
                                        eprintln!("[WS BINARY] {} bytes", data.len());
                                    }
                                    Ok(Message::Close(_)) => {
                                        eprintln!("WebSocket closed");
                                        break;
                                    }
                                    Err(e) => {
                                        eprintln!("WebSocket error: {}", e);
                                        break;
                                    }
                                    _ => {}
                                }
                            } else {
                                break;
                            }
                        }
                    }
                }

                let _ = writer_tx.send(WriterCommand::Stop);
                let _ = writer_handle.await;

                let is_running = *running.lock().unwrap();
                if !is_running {
                    break;
                }

                eprintln!("Disconnected, reconnecting in 2s...");
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            Err(e) => {
                eprintln!("Failed to connect: {} (retrying in 2s)", e);
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }
    }

    remove_pid_file();
    #[cfg(feature = "hooks")]
    apply_state_change(
        &dictation_state,
        DictationState::Inactive,
        &notification_config,
        &hook_config,
    );
    #[cfg(not(feature = "hooks"))]
    apply_state_change(
        &dictation_state,
        DictationState::Inactive,
        &notification_config,
    );
    eprintln!("ears-dictation stopped");
    Ok(())
}

/// Tracks typed words for sentence-level correction
#[cfg(feature = "llm-correct")]
struct SentenceBuffer {
    words: Vec<String>,
    /// Total characters typed (including spaces)
    char_count: usize,
}

#[cfg(feature = "llm-correct")]
impl SentenceBuffer {
    fn new() -> Self {
        Self { words: Vec::new(), char_count: 0 }
    }

    fn add_word(&mut self, word: &str) {
        if !self.words.is_empty() {
            self.char_count += 1; // space
        }
        self.char_count += word.len();
        self.words.push(word.to_string());
    }

    fn take_sentence(&mut self) -> (String, usize) {
        let sentence = self.words.join(" ");
        let chars = self.char_count;
        self.words.clear();
        self.char_count = 0;
        (sentence, chars)
    }

    fn is_sentence_end(word: &str) -> bool {
        let trimmed = word.trim();
        trimmed.ends_with('.') || trimmed.ends_with('?') || trimmed.ends_with('!')
    }
}

#[cfg(not(feature = "llm-correct"))]
fn handle_message(
    json: &Value,
    keyboard: &mut Box<dyn VirtualKeyboard>,
    capturing: &Arc<Mutex<bool>>
) -> Result<()> {
    let is_capturing = *capturing.lock().unwrap();

    if let Some(event_type) = json.get("type").and_then(|v| v.as_str()) {
        match event_type {
            "word" if is_capturing => {
                if let Some(word) = json.get("word").and_then(|v| v.as_str()) {
                    // Skip empty words and punctuation-only words
                    let has_alphanumeric = word.chars().any(|c| c.is_alphanumeric());
                    if !word.is_empty() && has_alphanumeric {
                        // eprintln!("[TYPING WORD] {}", word);
                        keyboard.type_text(word)?;
                        keyboard.press_key(SpecialKey::Space)?;
                    }
                }
            }
            "final" if is_capturing => {
                if let Some(text) = json.get("text").and_then(|v| v.as_str()) {
                    eprintln!("[FINAL] {}", text);
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Handle a word with optional LLM correction
#[cfg(feature = "llm-correct")]
async fn handle_word_with_correction(
    word: &str,
    keyboard: &mut Box<dyn VirtualKeyboard>,
    buffer: &mut SentenceBuffer,
    corrector: &mut SentenceCorrector,
) -> Result<()> {
    // Skip empty words and punctuation-only words
    let has_alphanumeric = word.chars().any(|c| c.is_alphanumeric());
    if word.is_empty() || !has_alphanumeric {
        return Ok(());
    }

    // Type the word immediately
    keyboard.type_text(word)?;
    keyboard.press_key(SpecialKey::Space)?;
    buffer.add_word(word);

    // Check if this is end of sentence
    if SentenceBuffer::is_sentence_end(word) && buffer.words.len() >= 2 {
        let (original, char_count) = buffer.take_sentence();

        // Get correction from LLM
        match corrector.correct_sentence(&original).await {
            Ok(corrected) if corrected != original => {
                eprintln!("[CORRECTION] '{}' -> '{}'", original, corrected);

                // Backspace the original text (including trailing space)
                for _ in 0..=char_count {
                    keyboard.press_key(SpecialKey::Backspace)?;
                }

                // Type corrected text
                keyboard.type_text(&corrected)?;
                keyboard.press_key(SpecialKey::Space)?;
            }
            Ok(_) => {
                // No correction needed
            }
            Err(e) => {
                eprintln!("[CORRECTION ERROR] {}", e);
            }
        }
    }

    Ok(())
}

#[cfg(not(feature = "hooks"))]
fn apply_state_change(
    state: &Arc<Mutex<DictationState>>,
    new_state: DictationState,
    notifications: &DictationNotificationConfig,
) {
    let event = match new_state {
        DictationState::Listening => DictationEvent::Started,
        DictationState::Suspended => DictationEvent::Paused,
        DictationState::Inactive => DictationEvent::Stopped,
    };

    let mut guard = state.lock().unwrap();
    if *guard == new_state {
        return;
    }
    *guard = new_state;
    drop(guard);

    handle_toggle_side_effects(event, notifications);
}

#[cfg(feature = "hooks")]
fn apply_state_change(
    state: &Arc<Mutex<DictationState>>,
    new_state: DictationState,
    notifications: &DictationNotificationConfig,
    hooks: &DictationHooksConfig,
) {
    let event = match new_state {
        DictationState::Listening => DictationEvent::Started,
        DictationState::Suspended => DictationEvent::Paused,
        DictationState::Inactive => DictationEvent::Stopped,
    };

    let mut guard = state.lock().unwrap();
    if *guard == new_state {
        return;
    }
    *guard = new_state;
    drop(guard);

    handle_toggle_side_effects(event, notifications, hooks);
}

fn send_toggle_notification(event: DictationEvent, notifications: &DictationNotificationConfig) {
    if !notifications.enabled {
        return;
    }

    let message = match event {
        DictationEvent::Started => notifications.start_message.as_str(),
        DictationEvent::Paused => notifications.pause_message.as_str(),
        DictationEvent::Stopped => notifications.stop_message.as_str(),
    };

    if message.trim().is_empty() {
        return;
    }

    if let Err(err) = notify("eaRS Dictation", message) {
        eprintln!("Failed to send dictation notification: {}", err);
    }
}

#[cfg(not(feature = "hooks"))]
fn handle_toggle_side_effects(event: DictationEvent, notifications: &DictationNotificationConfig) {
    send_toggle_notification(event, notifications);
}

#[cfg(feature = "hooks")]
fn handle_toggle_side_effects(
    event: DictationEvent,
    notifications: &DictationNotificationConfig,
    hooks: &DictationHooksConfig,
) {
    send_toggle_notification(event, notifications);
    if let Err(err) = run_hook_command(event, hooks) {
        eprintln!("Failed to run dictation hook command: {}", err);
    }
}

#[cfg(feature = "hooks")]
fn run_hook_command(event: DictationEvent, hooks: &DictationHooksConfig) -> Result<()> {
    let command = match event {
        DictationEvent::Started => hooks.start_command.as_deref(),
        DictationEvent::Paused => hooks.pause_command.as_deref(),
        DictationEvent::Stopped => hooks.stop_command.as_deref(),
    };

    let command = match command {
        Some(cmd) if !cmd.trim().is_empty() => cmd.trim(),
        _ => return Ok(()),
    };

    #[cfg(target_os = "windows")]
    {
        ProcessCommand::new("cmd")
            .arg("/C")
            .arg(command)
            .spawn()
            .with_context(|| format!("failed to spawn hook command '{}'", command))?;
    }

    #[cfg(not(target_os = "windows"))]
    {
        ProcessCommand::new("sh")
            .arg("-c")
            .arg(command)
            .spawn()
            .with_context(|| format!("failed to spawn hook command '{}'", command))?;
    }

    Ok(())
}

fn parse_combo(s: &str) -> (bool, bool, bool, rdev::Key) {
    let mut ctrl = false;
    let mut shift = false;
    let mut alt = false;
    let mut key = rdev::Key::Unknown(0);

    for part in s.split('+') {
        match part.trim() {
            "ctrl" | "control" => ctrl = true,
            "shift" => shift = true,
            "alt" => alt = true,
            k if k.len() == 1 => {
                if let Some(ch) = k.chars().next() {
                    key = match ch {
                        'a' => rdev::Key::KeyA,
                        'b' => rdev::Key::KeyB,
                        'c' => rdev::Key::KeyC,
                        'd' => rdev::Key::KeyD,
                        'e' => rdev::Key::KeyE,
                        'f' => rdev::Key::KeyF,
                        'g' => rdev::Key::KeyG,
                        'h' => rdev::Key::KeyH,
                        'i' => rdev::Key::KeyI,
                        'j' => rdev::Key::KeyJ,
                        'k' => rdev::Key::KeyK,
                        'l' => rdev::Key::KeyL,
                        'm' => rdev::Key::KeyM,
                        'n' => rdev::Key::KeyN,
                        'o' => rdev::Key::KeyO,
                        'p' => rdev::Key::KeyP,
                        'q' => rdev::Key::KeyQ,
                        'r' => rdev::Key::KeyR,
                        's' => rdev::Key::KeyS,
                        't' => rdev::Key::KeyT,
                        'u' => rdev::Key::KeyU,
                        'v' => rdev::Key::KeyV,
                        'w' => rdev::Key::KeyW,
                        'x' => rdev::Key::KeyX,
                        'y' => rdev::Key::KeyY,
                        'z' => rdev::Key::KeyZ,
                        _ => rdev::Key::Unknown(0),
                    }
                }
            }
            _ => {}
        }
    }
    (ctrl, shift, alt, key)
}

enum WriterCommand {
    Audio(Vec<u8>),
    Text(String),
    Stop,
}

fn encode_chunk(chunk: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(chunk.len() * 4);
    for sample in chunk {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    bytes
}
