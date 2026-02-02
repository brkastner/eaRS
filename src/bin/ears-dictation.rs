use anyhow::{Context, Result};
use clap::Parser;
use crossbeam_channel::{bounded, select, unbounded};
use ears::audio;
#[cfg(feature = "hooks")]
use ears::config::DictationHooksConfig;
use ears::config::{AppConfig, DictationNotificationConfig};
#[cfg(feature = "llm-correct")]
use ears::llm_correct::{CorrectionProfile, LlmCorrectConfig, SentenceCorrector};
use ears::virtual_keyboard::{
    create_virtual_keyboard_with_timing,
    KeyboardTiming,
    VirtualKeyboard,
    SpecialKey,
};
#[cfg(feature = "preview-overlay")]
use ears::clipboard::copy_and_paste;
#[cfg(feature = "preview-overlay")]
use ears::gtk_overlay::{spawn_overlay, OverlayHandle, OverlayResponse, OverlayStatus};
use futures_util::{SinkExt, StreamExt};
use notifica::notify;
use rdev::{EventType, listen};
use serde_json::Value;
use std::fs;
#[cfg(feature = "hooks")]
use std::process::Command as ProcessCommand;
#[cfg(feature = "llm-correct")]
use std::collections::HashSet;
use std::time::{Duration, Instant};
use std::sync::{Arc, Mutex};
use std::thread;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};

const PID_FILE_NAME: &str = "dictation.pid";
const STATUS_FILE_NAME: &str = "status.json";

#[cfg(all(feature = "preview-overlay", feature = "llm-correct"))]
const AUTO_COMMIT_PAUSE_SECS: u64 = 10;

#[derive(Clone, Debug, clap::ValueEnum)]
enum EngineArg {
    Kyutai,
    #[cfg(feature = "parakeet")]
    Parakeet,
}

#[cfg(feature = "llm-correct")]
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum CorrectionProfileArg {
    Journal,
    Technical,
}

#[cfg(feature = "llm-correct")]
impl From<CorrectionProfileArg> for CorrectionProfile {
    fn from(value: CorrectionProfileArg) -> Self {
        match value {
            CorrectionProfileArg::Journal => CorrectionProfile::Journal,
            CorrectionProfileArg::Technical => CorrectionProfile::Technical,
        }
    }
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
        default_value = "qwen2.5:14b",
        help = "Ollama model for text correction"
    )]
    ollama_model: String,

    #[cfg(feature = "llm-correct")]
    #[arg(
        long,
        env = "EARS_OLLAMA_MODEL_FAST",
        help = "Ollama model for fast, live correction"
    )]
    ollama_model_fast: Option<String>,

    #[cfg(feature = "llm-correct")]
    #[arg(
        long,
        env = "EARS_OLLAMA_MODEL_FINAL",
        help = "Ollama model for final paragraph correction"
    )]
    ollama_model_final: Option<String>,

    #[cfg(feature = "llm-correct")]
    #[arg(
        long,
        env = "EARS_OLLAMA_NUM_PREDICT_FAST",
        default_value_t = 128,
        help = "Max tokens for fast correction"
    )]
    ollama_num_predict_fast: i32,

    #[cfg(feature = "llm-correct")]
    #[arg(
        long,
        env = "EARS_OLLAMA_NUM_PREDICT_FINAL",
        default_value_t = 512,
        help = "Max tokens for final correction"
    )]
    ollama_num_predict_final: i32,

    #[cfg(feature = "llm-correct")]
    #[arg(
        long,
        env = "EARS_CORRECTION_PROFILE",
        value_enum,
        default_value = "journal",
        help = "Correction profile (journal|technical)"
    )]
    correction_profile: CorrectionProfileArg,

    #[arg(
        long,
        env = "EARS_TYPE_DELAY_US",
        default_value_t = 500,
        help = "Delay between typed characters (microseconds)"
    )]
    type_delay_us: u64,

    #[arg(
        long,
        env = "EARS_KEY_DELAY_US",
        default_value_t = 500,
        help = "Delay after non-backspace key presses (microseconds)"
    )]
    key_delay_us: u64,

    #[arg(
        long,
        env = "EARS_BACKSPACE_DELAY_MS",
        default_value_t = 15,
        help = "Delay after backspace presses (milliseconds)"
    )]
    backspace_delay_ms: u64,

    #[arg(
        long,
        env = "EARS_DELETE_WORD_DELAY_MS",
        default_value_t = 10,
        help = "Delay between Ctrl+Backspace word deletions (milliseconds)"
    )]
    delete_word_delay_ms: u64,

    #[arg(
        long,
        env = "EARS_CHORD_DELAY_MS",
        default_value_t = 5,
        help = "Delay after modifier chords (milliseconds)"
    )]
    chord_delay_ms: u64,

    #[arg(
        long,
        env = "EARS_CAPTURE_GRACE_MS",
        default_value_t = 500,
        help = "Grace period to keep capturing after toggle-off (milliseconds)"
    )]
    capture_grace_ms: u64,

    #[cfg(feature = "preview-overlay")]
    #[arg(
        long,
        help = "Enable preview overlay mode (buffer-first with popup window)"
    )]
    preview: bool,
}

fn get_state_dir() -> std::path::PathBuf {
    if let Ok(xdg_state) = std::env::var("XDG_STATE_HOME") {
        if !xdg_state.is_empty() {
            return std::path::PathBuf::from(xdg_state).join("ears");
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    std::path::PathBuf::from(home).join(".local/state/ears")
}

fn get_pid_file() -> std::path::PathBuf {
    get_state_dir().join(PID_FILE_NAME)
}

fn get_status_file() -> std::path::PathBuf {
    get_state_dir().join(STATUS_FILE_NAME)
}

fn write_status(state: &str) {
    let status_file = get_status_file();
    if let Some(parent) = status_file.parent() {
        if fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    let json = format!(r#"{{"state":"{}"}}"#, state);
    let tmp_file = status_file.with_extension("tmp");
    if fs::write(&tmp_file, &json).is_ok() {
        let _ = fs::rename(&tmp_file, &status_file);
    }
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
    let capture_grace_until = Arc::new(Mutex::new(None::<Instant>));
    let dictation_state = Arc::new(Mutex::new(DictationState::Inactive));

    let (stop_tx, stop_rx) = bounded::<()>(1);
    // Channel to signal "trigger final correction now" from toggle handlers
    #[cfg(feature = "llm-correct")]
    let (final_correct_tx, final_correct_rx) = bounded::<()>(1);
    #[cfg(not(feature = "llm-correct"))]
    let (_, final_correct_rx) = bounded::<()>(1);

    // Channels for preview overlay commands (checkpoint/commit/respawn)
    // Always created but only used when preview-overlay feature is enabled
    let (checkpoint_tx, checkpoint_rx) = bounded::<()>(1);
    let (commit_tx, commit_rx) = bounded::<()>(1);
    let (respawn_overlay_tx, respawn_overlay_rx) = bounded::<()>(1);
    let (discard_tx, discard_rx) = bounded::<()>(1);

    // Preview mode flag (set by --preview arg)
    #[cfg(feature = "preview-overlay")]
    let preview_mode = args.preview;
    #[cfg(not(feature = "preview-overlay"))]
    let preview_mode = false;

    // Store preview config for respawning overlay
    #[cfg(feature = "preview-overlay")]
    let preview_window_width = config.dictation.preview.window_width;
    #[cfg(feature = "preview-overlay")]
    let preview_window_height = config.dictation.preview.window_height;

    // Store paste hotkey for clipboard operations
    #[cfg(feature = "preview-overlay")]
    let paste_hotkey = config.dictation.preview.paste_hotkey.clone();
    #[cfg(not(feature = "preview-overlay"))]
    let paste_hotkey = "ctrl+v".to_string();

    let running_clone = running.clone();

    ctrlc::set_handler(move || {
        *running_clone.lock().unwrap() = false;
        let _ = stop_tx.send(());
    })
    .context("Failed to set Ctrl+C handler")?;

    // SIGUSR1 toggles capturing state for external hotkey integration
    #[cfg(feature = "llm-correct")]
    let sig_final_tx = final_correct_tx.clone();
    let sig_respawn_tx = respawn_overlay_tx.clone();
    let sig_preview_mode = preview_mode;
    {
        let sig_capturing = capturing.clone();
        let sig_grace = capture_grace_until.clone();
        let sig_grace_ms = args.capture_grace_ms;
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
                if is_active {
                    let mut grace = sig_grace.lock().unwrap();
                    *grace = None;
                } else {
                    let mut grace = sig_grace.lock().unwrap();
                    *grace = Some(Instant::now() + Duration::from_millis(sig_grace_ms));
                }
                eprintln!("SIGUSR1: audio capture {}", if is_active { "started" } else { "stopped" });
                drop(c);
                let event = if is_active {
                    DictationState::Listening
                } else {
                    DictationState::Suspended
                };
                // Trigger final correction when toggling OFF
                #[cfg(feature = "llm-correct")]
                if !is_active {
                    let _ = sig_final_tx.send(());
                }
                // Trigger overlay respawn when toggling ON in preview mode
                if is_active && sig_preview_mode {
                    let _ = sig_respawn_tx.send(());
                }
                #[cfg(feature = "hooks")]
                apply_state_change(&sig_state, event, &sig_notif, &sig_hooks);
                #[cfg(not(feature = "hooks"))]
                apply_state_change(&sig_state, event, &sig_notif);
            }
        });
    }

    // SIGUSR2 triggers checkpoint (paste current buffer, continue)
    #[cfg(feature = "preview-overlay")]
    {
        let sig_checkpoint_tx = checkpoint_tx.clone();
        tokio::spawn(async move {
            let mut sigusr2 = signal(SignalKind::user_defined2())
                .expect("failed to register SIGUSR2 handler");
            loop {
                sigusr2.recv().await;
                eprintln!("SIGUSR2: checkpoint requested");
                let _ = sig_checkpoint_tx.send(());
            }
        });
    }

    // SIGRTMIN discards overlay session (close without paste, reset buffers)
    #[cfg(feature = "preview-overlay")]
    {
        let sig_discard_tx = discard_tx.clone();
        tokio::spawn(async move {
            let mut sigrtmin = signal(SignalKind::from_raw(libc::SIGRTMIN()))
                .expect("failed to register SIGRTMIN handler");
            loop {
                sigrtmin.recv().await;
                eprintln!("SIGRTMIN: discard overlay session");
                let _ = sig_discard_tx.send(());
            }
        });
    }

    // Note: Commit is triggered by pressing Enter in the focused overlay window
    // SIGHUP is not used because it terminates the process before handler registers

            let hotkey_running = running.clone();
            let hotkey_capturing = capturing.clone();
            let hotkey_grace = capture_grace_until.clone();
            let hotkey_grace_ms = args.capture_grace_ms;
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
        #[cfg(feature = "llm-correct")]
        let hotkey_final_tx = final_correct_tx.clone();
        let hotkey_checkpoint_tx = checkpoint_tx.clone();
        let hotkey_commit_tx = commit_tx.clone();
        #[cfg(feature = "preview-overlay")]
        let preview_config_hotkeys = config.dictation.preview.clone();
        let _hotkey_preview_mode = preview_mode;
        #[cfg(feature = "preview-overlay")]
        let hotkey_preview_mode = _hotkey_preview_mode;
        thread::spawn(move || {
            let toggle_combo = hotkey_config.toggle.to_lowercase();
            let (t_ctrl, t_shift, t_alt, t_key) = parse_combo(&toggle_combo);
            eprintln!(
                "Parsed combo - ctrl:{} shift:{} alt:{} key:{:?}",
                t_ctrl, t_shift, t_alt, t_key
            );

            // Parse preview hotkeys
            #[cfg(feature = "preview-overlay")]
            let (cp_ctrl, cp_shift, cp_alt, cp_key) = if hotkey_preview_mode {
                let combo = parse_combo(&preview_config_hotkeys.checkpoint_hotkey);
                eprintln!("Checkpoint hotkey: {} -> ctrl:{} shift:{} alt:{} key:{:?}",
                         preview_config_hotkeys.checkpoint_hotkey, combo.0, combo.1, combo.2, combo.3);
                combo
            } else {
                (false, false, false, rdev::Key::Unknown(0))
            };
            #[cfg(feature = "preview-overlay")]
            let (cm_ctrl, cm_shift, cm_alt, cm_key) = if hotkey_preview_mode {
                let combo = parse_combo(&preview_config_hotkeys.commit_hotkey);
                eprintln!("Commit hotkey: {} -> ctrl:{} shift:{} alt:{} key:{:?}",
                         preview_config_hotkeys.commit_hotkey, combo.0, combo.1, combo.2, combo.3);
                combo
            } else {
                (false, false, false, rdev::Key::Unknown(0))
            };

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
                        // Debug: log numpad and special keys to diagnose hotkey issues
                        #[cfg(feature = "preview-overlay")]
                        if hotkey_preview_mode {
                            match k {
                                rdev::Key::KpPlus | rdev::Key::KpMinus | rdev::Key::KpReturn |
                                rdev::Key::KpMultiply | rdev::Key::KpDivide => {
                                    eprintln!("[KEY DEBUG] Numpad key: {:?} (expected checkpoint: {:?})", k, cp_key);
                                }
                                _ => {}
                            }
                        }
                        if CTRL == t_ctrl && SHIFT == t_shift && ALT == t_alt && k == t_key {
                            let mut c = hotkey_capturing.lock().unwrap();
                            *c = !*c;
                            let is_active = *c;
                            if is_active {
                                let mut grace = hotkey_grace.lock().unwrap();
                                *grace = None;
                            } else {
                                let mut grace = hotkey_grace.lock().unwrap();
                                *grace = Some(Instant::now() + Duration::from_millis(hotkey_grace_ms));
                            }
                            eprintln!(
                                "Audio capture {}",
                                if is_active { "started" } else { "stopped" }
                            );
                            drop(c);
                            // Trigger final correction when toggling OFF
                            #[cfg(feature = "llm-correct")]
                            if !is_active {
                                let _ = hotkey_final_tx.send(());
                            }
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

                        // Preview mode: checkpoint hotkey
                        #[cfg(feature = "preview-overlay")]
                        {
                            if hotkey_preview_mode
                                && CTRL == cp_ctrl
                                && SHIFT == cp_shift
                                && ALT == cp_alt
                                && k == cp_key
                            {
                                eprintln!("Checkpoint hotkey pressed");
                                let _ = hotkey_checkpoint_tx.send(());
                            }

                            // Preview mode: commit hotkey
                            if hotkey_preview_mode
                                && CTRL == cm_ctrl
                                && SHIFT == cm_shift
                                && ALT == cm_alt
                                && k == cm_key
                            {
                                eprintln!("Commit hotkey pressed");
                                let _ = hotkey_commit_tx.send(());
                            }
                        }
                        // Suppress unused warnings when preview-overlay is not enabled
                        #[cfg(not(feature = "preview-overlay"))]
                        {
                            let _ = &hotkey_checkpoint_tx;
                            let _ = &hotkey_commit_tx;
                        }
                    },
                    _ => {}
                }
            }) {
                eprintln!("Hotkey listener error: {:?}", e);
            }
        });
    }

    // Start in paused state - user must toggle to begin capturing
    #[cfg(feature = "hooks")]
    apply_state_change(
        &dictation_state,
        DictationState::Suspended,
        &notification_config,
        &hook_config,
    );
    #[cfg(not(feature = "hooks"))]
    apply_state_change(
        &dictation_state,
        DictationState::Suspended,
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
                let timing = KeyboardTiming {
                    char_delay: Duration::from_micros(args.type_delay_us),
                    key_delay: Duration::from_micros(args.key_delay_us),
                    backspace_delay: Duration::from_millis(args.backspace_delay_ms),
                    delete_word_delay: Duration::from_millis(args.delete_word_delay_ms),
                    chord_delay: Duration::from_millis(args.chord_delay_ms),
                };
                let mut keyboard = create_virtual_keyboard_with_timing(timing)
                    .context("Failed to initialize virtual keyboard. \
                              On Linux/Wayland, ensure you are in the 'input' group.")?;

                // Overlay handle - starts as None, spawned on first toggle
                #[cfg(feature = "preview-overlay")]
                let mut overlay_handle: Option<OverlayHandle> = None;
                #[cfg(not(feature = "preview-overlay"))]
                let overlay_handle: Option<()> = None;

                #[cfg(feature = "llm-correct")]
                let mut corrector = {
                    let ollama_url = args.ollama_url.clone()
                        .unwrap_or_else(|| "http://localhost:11434".to_string());
                    let fast_model = args
                        .ollama_model_fast
                        .clone()
                        .unwrap_or_else(|| args.ollama_model.clone());
                    let final_model = args
                        .ollama_model_final
                        .clone()
                        .unwrap_or_else(|| args.ollama_model.clone());
                    let config = LlmCorrectConfig {
                        endpoint: ollama_url.clone(),
                        model: fast_model,
                        final_model,
                        timeout_secs: 10,
                        num_predict_fast: args.ollama_num_predict_fast,
                        num_predict_final: args.ollama_num_predict_final,
                        temperature: 0.1,
                        profile: args.correction_profile.into(),
                    };
                    eprintln!(
                        "LLM correction enabled: live={} final={} ({})",
                        config.model,
                        config.final_model,
                        ollama_url
                    );
                    SentenceCorrector::new(config)?
                };
                #[cfg(feature = "llm-correct")]
                let mut correction_buffer = CorrectionBuffer::new(args.correction_profile.into());
                #[cfg(feature = "llm-correct")]
                let mut last_word_time = Instant::now();
                // Track how many words have been sent to the overlay since last checkpoint/commit
                #[cfg(all(feature = "llm-correct", feature = "preview-overlay"))]
                let mut overlay_word_count: usize = 0;

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
                let audio_grace = capture_grace_until.clone();
                thread::spawn(move || {
                    while let Ok(chunk) = audio_rx_clone.recv() {
                        let should_send = {
                            let is_capturing = *audio_capturing.lock().unwrap();
                            if is_capturing {
                                true
                            } else {
                                let mut grace = audio_grace.lock().unwrap();
                                if let Some(until) = *grace {
                                    if Instant::now() <= until {
                                        true
                                    } else {
                                        *grace = None;
                                        false
                                    }
                                } else {
                                    false
                                }
                            }
                        };

                        if should_send {
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

                // Track if we've already reported overlay closed (to avoid log spam)
                #[cfg(feature = "preview-overlay")]
                let mut overlay_closed_logged = false;

                loop {
                    // Drain all overlay responses (paste text, closed)
                    #[cfg(feature = "preview-overlay")]
                    if let Some(ref handle) = overlay_handle {
                        while let Some(response) = handle.try_recv() {
                            match response {
                                OverlayResponse::PasteText(text) => {
                                    eprintln!("[PREVIEW] Pasting text: {} chars", text.len());
                                    if let Err(e) = copy_and_paste(&text, keyboard.as_mut(), &paste_hotkey) {
                                        eprintln!("[PREVIEW PASTE ERROR] {}", e);
                                    }
                                }
                                OverlayResponse::Closed => {
                                    if !overlay_closed_logged {
                                        eprintln!("[PREVIEW] Overlay closed, will respawn on next toggle");
                                        overlay_closed_logged = true;
                                    }
                                }
                            }
                        }
                    }

                    // Set overlay_handle to None if overlay was closed (check channel disconnect)
                    #[cfg(feature = "preview-overlay")]
                    if overlay_closed_logged && overlay_handle.is_some() {
                        overlay_handle = None;
                    }

                    select! {
                        recv(stop_rx) -> _ => {
                            #[cfg(feature = "preview-overlay")]
                            if let Some(ref handle) = overlay_handle {
                                let _ = handle.close();
                            }
                            break;
                        }
                        // Preview mode: checkpoint (paste current buffer, continue)
                        recv(checkpoint_rx) -> _ => {
                            #[cfg(feature = "preview-overlay")]
                            if let Some(ref handle) = overlay_handle {
                                // Drain any duplicate checkpoint signals
                                while checkpoint_rx.try_recv().is_ok() {}
                                eprintln!("[CHECKPOINT] Processing...");

                                // Drain in-flight words from WebSocket before checkpointing
                                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                                loop {
                                    match tokio::time::timeout(
                                        std::time::Duration::from_millis(100),
                                        read.next()
                                    ).await {
                                        Ok(Some(Ok(Message::Text(text)))) => {
                                            if let Ok(json) = serde_json::from_str::<Value>(&text) {
                                                if let Some(word) = json.get("word").and_then(|v| v.as_str()) {
                                                    let has_alphanumeric = word.chars().any(|c| c.is_alphanumeric());
                                                    if !word.is_empty() && has_alphanumeric {
                                                        let _ = handle.send_word(word.to_string());
                                                        #[cfg(feature = "llm-correct")]
                                                        {
                                                            correction_buffer.add_word(word);
                                                            overlay_word_count += 1;
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        _ => break,
                                    }
                                }

                                if let Err(e) = handle.checkpoint() {
                                    eprintln!("[CHECKPOINT ERROR] {}", e);
                                }
                                // Reset correction state — checkpointed text is pasted, start fresh
                                // Must clear paragraph too, otherwise auto-commit re-pastes
                                // the checkpointed text from committed_sections
                                #[cfg(feature = "llm-correct")]
                                {
                                    correction_buffer.take_paragraph();
                                    overlay_word_count = 0;
                                }
                            }
                        }
                        // Preview mode: commit (paste all, close overlay, pause)
                        recv(commit_rx) -> _ => {
                            #[cfg(feature = "preview-overlay")]
                            if let Some(ref handle) = overlay_handle {
                                // Drain any duplicate commit signals
                                while commit_rx.try_recv().is_ok() {}
                                eprintln!("[COMMIT] Processing...");
                                if let Err(e) = handle.commit() {
                                    eprintln!("[COMMIT ERROR] {}", e);
                                }
                                #[cfg(feature = "llm-correct")]
                                { overlay_word_count = 0; }
                                // Also pause capturing
                                *capturing.lock().unwrap() = false;
                                eprintln!("[COMMIT] Paused capturing");
                            }
                        }
                        // Respawn overlay when toggle turns on and overlay is closed
                        recv(respawn_overlay_rx) -> _ => {
                            #[cfg(feature = "preview-overlay")]
                            if preview_mode && overlay_handle.is_none() {
                                eprintln!("[PREVIEW] Respawning overlay...");
                                match spawn_overlay(preview_window_width, preview_window_height) {
                                    Ok(handle) => {
                                        eprintln!("Preview overlay respawned");
                                        overlay_handle = Some(handle);
                                        overlay_closed_logged = false;
                                    }
                                    Err(e) => {
                                        eprintln!("Failed to respawn preview overlay: {}", e);
                                    }
                                }
                            }
                        }
                        // Discard overlay session (close without paste, reset)
                        recv(discard_rx) -> _ => {
                            #[cfg(feature = "preview-overlay")]
                            if let Some(ref handle) = overlay_handle {
                                while discard_rx.try_recv().is_ok() {}
                                eprintln!("[DISCARD] Closing overlay without paste");
                                let _ = handle.close();
                                #[cfg(feature = "llm-correct")]
                                {
                                    correction_buffer.reset_chunk();
                                    let _ = correction_buffer.take_paragraph(); // drain paragraph too
                                    overlay_word_count = 0;
                                }
                                *capturing.lock().unwrap() = false;
                                write_status("paused");
                            }
                        }
                        // Trigger final correction when capture is toggled off
                        recv(final_correct_rx) -> _ => {
                            #[cfg(feature = "llm-correct")]
                            {
                                // Small delay to allow any in-flight words to arrive
                                let grace_ms = args.capture_grace_ms.saturating_add(200);
                                let delay_ms = if grace_ms > 300 { grace_ms } else { 300 };
                                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;

                                // Preview mode: drain words to overlay, run final correction, then commit
                                #[cfg(feature = "preview-overlay")]
                                if preview_mode {
                                    // Drain any remaining words to overlay
                                    let drain_deadline = Instant::now()
                                        + std::time::Duration::from_millis(
                                            args.capture_grace_ms.saturating_add(600),
                                        );
                                    loop {
                                        match tokio::time::timeout(
                                            std::time::Duration::from_millis(120),
                                            read.next()
                                        ).await {
                                            Ok(Some(Ok(Message::Text(text)))) => {
                                                if let Ok(json) = serde_json::from_str::<Value>(&text) {
                                                    if let Some(word) = json.get("word").and_then(|v| v.as_str()) {
                                                        let has_alphanumeric = word.chars().any(|c| c.is_alphanumeric());
                                                        if !word.is_empty() && has_alphanumeric {
                                                            if let Some(ref handle) = overlay_handle {
                                                                let _ = handle.send_word(word.to_string());
                                                            }
                                                            correction_buffer.add_word(word);
                                                            overlay_word_count += 1;
                                                        }
                                                    }
                                                }
                                            }
                                            _ => {
                                                if Instant::now() >= drain_deadline {
                                                    break;
                                                }
                                            }
                                        }
                                    }
                                    // Run final correction through overlay
                                    if correction_buffer.paragraph_len() >= 2 {
                                        if let Some(ref handle) = overlay_handle {
                                            let _ = handle.set_status(OverlayStatus::Correcting);
                                        }
                                        write_status("processing");
                                        let (paragraph, _word_count, _char_count) = correction_buffer.take_paragraph();
                                        match corrector.correct_paragraph(&paragraph).await {
                                            Ok(corrected) if corrected != paragraph => {
                                                if !is_safe_correction(&paragraph, &corrected, args.correction_profile.into()) {
                                                    eprintln!("[TOGGLE-OFF FINAL SKIP] low similarity");
                                                } else {
                                                    eprintln!("[TOGGLE-OFF FINAL] '{}' -> '{}'", paragraph, corrected);
                                                    if let Some(ref handle) = overlay_handle {
                                                        let _ = handle.send_correction(corrected);
                                                    }
                                                }
                                            }
                                            Ok(_) => {}
                                            Err(e) => eprintln!("[TOGGLE-OFF FINAL ERROR] {}", e),
                                        }
                                    }
                                    // Commit overlay (paste buffer, close window)
                                    if let Some(ref handle) = overlay_handle {
                                        eprintln!("[TOGGLE-OFF] Committing overlay");
                                        let _ = handle.commit();
                                        // Wait for overlay to fully close so respawn works
                                        loop {
                                            if let Some(response) = handle.try_recv() {
                                                match response {
                                                    OverlayResponse::PasteText(text) => {
                                                        eprintln!("[TOGGLE-OFF] Pasting text: {} chars", text.len());
                                                        if let Err(e) = copy_and_paste(&text, keyboard.as_mut(), &paste_hotkey) {
                                                            eprintln!("[TOGGLE-OFF PASTE ERROR] {}", e);
                                                        }
                                                    }
                                                    OverlayResponse::Closed => {
                                                        eprintln!("[TOGGLE-OFF] Overlay closed");
                                                        break;
                                                    }
                                                }
                                            }
                                            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                                        }
                                    }
                                    overlay_handle = None;
                                    overlay_closed_logged = false;
                                    overlay_word_count = 0;
                                    write_status("paused");
                                }

                                // Normal mode: drain words, type directly
                                #[cfg(feature = "preview-overlay")]
                                if !preview_mode {
                                    // Drain any remaining words from websocket
                                    let drain_deadline = Instant::now()
                                        + std::time::Duration::from_millis(
                                            args.capture_grace_ms.saturating_add(600),
                                        );
                                    loop {
                                        match tokio::time::timeout(
                                            std::time::Duration::from_millis(120),
                                            read.next()
                                        ).await {
                                            Ok(Some(Ok(Message::Text(text)))) => {
                                                if let Ok(json) = serde_json::from_str::<Value>(&text) {
                                                    if let Some(word) = json.get("word").and_then(|v| v.as_str()) {
                                                        let has_alphanumeric = word.chars().any(|c| c.is_alphanumeric());
                                                        if !word.is_empty() && has_alphanumeric {
                                                            keyboard.type_text(word).ok();
                                                            keyboard.press_key(SpecialKey::Space).ok();
                                                            correction_buffer.add_word(word);
                                                        }
                                                    }
                                                }
                                            }
                                            _ => {
                                                if Instant::now() >= drain_deadline {
                                                    break;
                                                }
                                            }
                                        }
                                    }
                                    if correction_buffer.paragraph_len() >= 2 {
                                        write_status("processing");
                                        if let Err(e) = correct_final_paragraph(
                                            &mut keyboard,
                                            &mut correction_buffer,
                                            &mut corrector,
                                        ).await {
                                            eprintln!("[TOGGLE-OFF FINAL ERROR] {}", e);
                                        }
                                        write_status("paused");
                                    }
                                }

                                // Non-preview-overlay build
                                #[cfg(not(feature = "preview-overlay"))]
                                {
                                    let drain_deadline = Instant::now()
                                        + std::time::Duration::from_millis(800);
                                    loop {
                                        match tokio::time::timeout(
                                            std::time::Duration::from_millis(120),
                                            read.next()
                                        ).await {
                                            Ok(Some(Ok(Message::Text(text)))) => {
                                                if let Ok(json) = serde_json::from_str::<Value>(&text) {
                                                    if let Some(word) = json.get("word").and_then(|v| v.as_str()) {
                                                        let has_alphanumeric = word.chars().any(|c| c.is_alphanumeric());
                                                        if !word.is_empty() && has_alphanumeric {
                                                            keyboard.type_text(word).ok();
                                                            keyboard.press_key(SpecialKey::Space).ok();
                                                            correction_buffer.add_word(word);
                                                        }
                                                    }
                                                }
                                            }
                                            _ => {
                                                if Instant::now() >= drain_deadline {
                                                    break;
                                                }
                                            }
                                        }
                                    }
                                    if correction_buffer.paragraph_len() >= 2 {
                                        write_status("processing");
                                        if let Err(e) = correct_final_paragraph(
                                            &mut keyboard,
                                            &mut correction_buffer,
                                            &mut corrector,
                                        ).await {
                                            eprintln!("[TOGGLE-OFF FINAL ERROR] {}", e);
                                        }
                                        write_status("paused");
                                    }
                                }
                            }
                        }
                        default(std::time::Duration::from_millis(200)) => {
                            // Use timeout to avoid blocking forever on websocket read
                            let ws_result = tokio::time::timeout(
                                std::time::Duration::from_millis(200),
                                read.next()
                            ).await;

                            if let Ok(Some(message)) = ws_result {
                                match message {
                                    Ok(Message::Text(text)) => {
                                        // eprintln!("[WS RECEIVED] {}", text);
                                        if let Ok(json) = serde_json::from_str::<Value>(&text) {
                                            // Preview mode: send words to overlay instead of typing
                                            #[cfg(feature = "preview-overlay")]
                                            if preview_mode {
                                                let is_capturing = *capturing.lock().unwrap();
                                                if is_capturing {
                                                    if let Some(word) = json.get("word").and_then(|v| v.as_str()) {
                                                        let has_alphanumeric = word.chars().any(|c| c.is_alphanumeric());
                                                        if !word.is_empty() && has_alphanumeric {
                                                            #[cfg(feature = "llm-correct")]
                                                            { last_word_time = Instant::now(); }
                                                            if let Some(ref handle) = overlay_handle {
                                                                if let Err(e) = handle.send_word(word.to_string()) {
                                                                    eprintln!("[PREVIEW WORD ERROR] {}", e);
                                                                }
                                                                // Also add to correction buffer for LLM correction
                                                                #[cfg(feature = "llm-correct")]
                                                                {
                                                                    correction_buffer.add_word(word);
                                                                    overlay_word_count += 1;
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            } else {
                                                // Normal mode: type directly
                                                #[cfg(feature = "llm-correct")]
                                                {
                                                    let is_capturing = *capturing.lock().unwrap();
                                                    if is_capturing {
                                                        if let Some(word) = json.get("word").and_then(|v| v.as_str()) {
                                                            last_word_time = Instant::now();
                                                     if let Err(e) = handle_word_with_correction(
                                                         word,
                                                         &mut keyboard,
                                                         &mut correction_buffer,
                                                         &mut corrector,
                                                         args.correction_profile.into(),
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
                                            }

                                            // Non-preview-overlay build: use normal handling
                                            #[cfg(not(feature = "preview-overlay"))]
                                            {
                                                #[cfg(feature = "llm-correct")]
                                                {
                                                    let is_capturing = *capturing.lock().unwrap();
                                                    if is_capturing {
                                                        if let Some(word) = json.get("word").and_then(|v| v.as_str()) {
                                                            last_word_time = Instant::now();
                                                            if let Err(e) = handle_word_with_correction(
                                                                word,
                                                                &mut keyboard,
                                                                &mut correction_buffer,
                                                                &mut corrector,
                                                                args.correction_profile.into(),
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
                            } else if let Ok(None) = ws_result {
                                // Stream ended
                                break;
                            }
                            // Timeout case (Err) - continue to check pause detection

                            // Check for pauses to trigger corrections
                            #[cfg(feature = "llm-correct")]
                            {
                                let is_capturing = *capturing.lock().unwrap();
                                let elapsed = last_word_time.elapsed();

                                // Preview mode: send corrections to overlay instead of typing
                                #[cfg(feature = "preview-overlay")]
                                if preview_mode {
                                    if is_capturing && elapsed >= std::time::Duration::from_millis(1500)
                                        && correction_buffer.chunk_len() >= 2
                                    {
                                        // 1.5+ second pause: chunk correction for preview
                                        let (original, word_count, _char_count, original_words) = correction_buffer.take_chunk();
                                        let chunk_start = overlay_word_count.saturating_sub(word_count);
                                        if let Some(ref handle) = overlay_handle {
                                            let _ = handle.set_status(OverlayStatus::Correcting);
                                        }
                                        write_status("processing");

                                        // Run correction concurrently with word reading so
                                        // words arriving during the Ollama roundtrip still
                                        // flow to the overlay and correction buffer.
                                        let correction_fut = corrector.correct_sentence(&original);
                                        tokio::pin!(correction_fut);
                                        let mut correction_result = None;

                                        loop {
                                            tokio::select! {
                                                result = &mut correction_fut => {
                                                    correction_result = Some(result);
                                                    break;
                                                }
                                                msg = read.next() => {
                                                    if let Some(Ok(Message::Text(text))) = msg {
                                                        if let Ok(json) = serde_json::from_str::<Value>(&text) {
                                                            if let Some(word) = json.get("word").and_then(|v| v.as_str()) {
                                                                let has_alphanumeric = word.chars().any(|c| c.is_alphanumeric());
                                                                if !word.is_empty() && has_alphanumeric {
                                                                    last_word_time = Instant::now();
                                                                    if let Some(ref handle) = overlay_handle {
                                                                        let _ = handle.send_word(word.to_string());
                                                                    }
                                                                    correction_buffer.add_word(word);
                                                                    overlay_word_count += 1;
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }

                                        // Apply correction result
                                        if let Some(Ok(corrected)) = correction_result {
                                            if corrected != original {
                                                if !is_safe_correction(&original, &corrected, args.correction_profile.into()) {
                                                    eprintln!("[PREVIEW CHUNK SKIP] low similarity");
                                                } else {
                                                    eprintln!("[PREVIEW CHUNK] '{}' -> '{}'", original, corrected);
                                                    if let Some(ref handle) = overlay_handle {
                                                        if let Err(e) = handle.send_chunk_correction(&corrected, chunk_start, word_count) {
                                                            eprintln!("[PREVIEW CORRECTION ERROR] {}", e);
                                                        }
                                                    }
                                                    // Keep paragraph_words in sync with corrected text
                                                    correction_buffer.apply_chunk_correction(&original_words, &corrected);
                                                }
                                            }
                                        } else if let Some(Err(e)) = correction_result {
                                            eprintln!("[PREVIEW CHUNK ERROR] {}", e);
                                        }

                                        if let Some(ref handle) = overlay_handle {
                                            let _ = handle.set_status(OverlayStatus::Listening);
                                        }
                                        write_status("listening");
                                        last_word_time = Instant::now();
                                    }

                                    // Auto-commit after extended silence (10+ seconds)
                                    if is_capturing
                                        && overlay_handle.is_some()
                                        && elapsed.as_secs() >= AUTO_COMMIT_PAUSE_SECS
                                        && correction_buffer.paragraph_len() > 0
                                    {
                                        eprintln!(
                                            "[AUTO-COMMIT] {} seconds of silence, committing",
                                            elapsed.as_secs()
                                        );
                                        if let Some(ref handle) = overlay_handle {
                                            let _ = handle.commit();
                                        }
                                        overlay_word_count = 0;
                                        *capturing.lock().unwrap() = false;
                                    }
                                } else {
                                    // Normal mode: type directly with backspace+retype
                                    if is_capturing && elapsed >= std::time::Duration::from_secs(5)
                                        && correction_buffer.paragraph_len() >= 2
                                    {
                                        // 5+ second pause: final paragraph correction
                                        send_processing_notification(&notification_config);
                                        *capturing.lock().unwrap() = false;
                                        eprintln!("[AUTO FINAL] pausing capture for processing");
                                        let drain_deadline = Instant::now()
                                            + std::time::Duration::from_millis(800);
                                        loop {
                                            match tokio::time::timeout(
                                                std::time::Duration::from_millis(120),
                                                read.next(),
                                            )
                                            .await
                                            {
                                                Ok(Some(Ok(Message::Text(text)))) => {
                                                    if let Ok(json) = serde_json::from_str::<Value>(&text) {
                                                        if let Some(word) = json.get("word").and_then(|v| v.as_str()) {
                                                            let has_alphanumeric = word.chars().any(|c| c.is_alphanumeric());
                                                            if !word.is_empty() && has_alphanumeric {
                                                                keyboard.type_text(word).ok();
                                                                keyboard.press_key(SpecialKey::Space).ok();
                                                                correction_buffer.add_word(word);
                                                                last_word_time = Instant::now();
                                                            }
                                                        }
                                                    }
                                                }
                                                Ok(Some(Ok(Message::Close(_)))) => break,
                                                Ok(Some(Err(_))) => break,
                                                Ok(None) => break,
                                                _ => {
                                                    if Instant::now() >= drain_deadline {
                                                        break;
                                                    }
                                                }
                                            }
                                        }
                                        write_status("processing");
                                        if let Err(e) = correct_final_paragraph(
                                            &mut keyboard,
                                            &mut correction_buffer,
                                            &mut corrector,
                                        ).await {
                                            eprintln!("[FINAL ERROR] {}", e);
                                        }
                                        write_status("listening");
                                        *capturing.lock().unwrap() = true;
                                        last_word_time = Instant::now();
                                    } else if is_capturing && elapsed >= std::time::Duration::from_millis(1500)
                                        && correction_buffer.chunk_len() >= 2
                                    {
                                        // 1.5+ second pause: chunk correction
                                        let (original, _word_count, char_count, original_words) = correction_buffer.take_chunk();
                                        write_status("processing");
                                        match corrector.correct_sentence(&original).await {
                                            Ok(corrected) if corrected != original => {
                                                if !is_safe_correction(&original, &corrected, args.correction_profile.into()) {
                                                    eprintln!("[PAUSE CHUNK SKIP] low similarity");
                                                    write_status("listening");
                                                    last_word_time = Instant::now();
                                                    continue;
                                                }
                                                eprintln!("[PAUSE CHUNK] '{}' -> '{}'", original, corrected);
                                                let total_backspaces = char_count.saturating_add(1);
                                                for _ in 0..total_backspaces {
                                                    keyboard.press_key(SpecialKey::Backspace)?;
                                                }
                                                keyboard.type_text(&corrected)?;
                                                keyboard.press_key(SpecialKey::Space)?;
                                                correction_buffer.apply_chunk_correction(&original_words, &corrected);
                                            }
                                            Ok(_) => {}
                                            Err(e) => eprintln!("[PAUSE CHUNK ERROR] {}", e),
                                        }
                                        write_status("listening");
                                        last_word_time = Instant::now();
                                    }
                                }

                                // Non-preview-overlay build: use normal handling
                                #[cfg(not(feature = "preview-overlay"))]
                                {
                                    if is_capturing && elapsed >= std::time::Duration::from_secs(5)
                                        && correction_buffer.paragraph_len() >= 2
                                    {
                                        // 5+ second pause: final paragraph correction
                                        send_processing_notification(&notification_config);
                                        *capturing.lock().unwrap() = false;
                                        eprintln!("[AUTO FINAL] pausing capture for processing");
                                        let drain_deadline = Instant::now()
                                            + std::time::Duration::from_millis(800);
                                        loop {
                                            match tokio::time::timeout(
                                                std::time::Duration::from_millis(120),
                                                read.next(),
                                            )
                                            .await
                                            {
                                                Ok(Some(Ok(Message::Text(text)))) => {
                                                    if let Ok(json) = serde_json::from_str::<Value>(&text) {
                                                        if let Some(word) = json.get("word").and_then(|v| v.as_str()) {
                                                            let has_alphanumeric = word.chars().any(|c| c.is_alphanumeric());
                                                            if !word.is_empty() && has_alphanumeric {
                                                                keyboard.type_text(word).ok();
                                                                keyboard.press_key(SpecialKey::Space).ok();
                                                                correction_buffer.add_word(word);
                                                                last_word_time = Instant::now();
                                                            }
                                                        }
                                                    }
                                                }
                                                Ok(Some(Ok(Message::Close(_)))) => break,
                                                Ok(Some(Err(_))) => break,
                                                Ok(None) => break,
                                                _ => {
                                                    if Instant::now() >= drain_deadline {
                                                        break;
                                                    }
                                                }
                                            }
                                        }
                                        write_status("processing");
                                        if let Err(e) = correct_final_paragraph(
                                            &mut keyboard,
                                            &mut correction_buffer,
                                            &mut corrector,
                                        ).await {
                                            eprintln!("[FINAL ERROR] {}", e);
                                        }
                                        write_status("listening");
                                        *capturing.lock().unwrap() = true;
                                        last_word_time = Instant::now();
                                    } else if is_capturing && elapsed >= std::time::Duration::from_millis(1500)
                                        && correction_buffer.chunk_len() >= 2
                                    {
                                        // 1.5+ second pause: chunk correction
                                        let (original, _word_count, char_count, original_words) = correction_buffer.take_chunk();
                                        write_status("processing");
                                        match corrector.correct_sentence(&original).await {
                                            Ok(corrected) if corrected != original => {
                                                if !is_safe_correction(&original, &corrected, args.correction_profile.into()) {
                                                    eprintln!("[PAUSE CHUNK SKIP] low similarity");
                                                    write_status("listening");
                                                    last_word_time = Instant::now();
                                                    continue;
                                                }
                                                eprintln!("[PAUSE CHUNK] '{}' -> '{}'", original, corrected);
                                                let total_backspaces = char_count.saturating_add(1);
                                                for _ in 0..total_backspaces {
                                                    keyboard.press_key(SpecialKey::Backspace)?;
                                                }
                                                keyboard.type_text(&corrected)?;
                                                keyboard.press_key(SpecialKey::Space)?;
                                                correction_buffer.apply_chunk_correction(&original_words, &corrected);
                                            }
                                            Ok(_) => {}
                                            Err(e) => eprintln!("[PAUSE CHUNK ERROR] {}", e),
                                        }
                                        write_status("listening");
                                        last_word_time = Instant::now();
                                    }
                                }
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
    let _ = fs::remove_file(get_status_file());
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
    // Force exit — the GTK overlay thread may still be running app.run_with_args()
    // which keeps the process alive. Clean shutdown already happened above.
    std::process::exit(0);
}

/// Tracks typed words for chunked + final correction
#[cfg(feature = "llm-correct")]
struct CorrectionBuffer {
    /// Current chunk being accumulated
    chunk_words: Vec<String>,
    chunk_char_count: usize,
    /// Full paragraph for final correction
    paragraph_words: Vec<String>,
    paragraph_char_count: usize,
    /// Words per chunk before triggering correction
    chunk_size: usize,
    correct_on_commas: bool,
    enable_chunk_correction: bool,
}

#[cfg(feature = "llm-correct")]
impl CorrectionBuffer {
    fn new(profile: CorrectionProfile) -> Self {
        let (chunk_size, correct_on_commas, enable_chunk_correction) = match profile {
            CorrectionProfile::Journal => (6, true, false),
            CorrectionProfile::Technical => (10, false, true),
        };
        Self {
            chunk_words: Vec::new(),
            chunk_char_count: 0,
            paragraph_words: Vec::new(),
            paragraph_char_count: 0,
            chunk_size,
            correct_on_commas,
            enable_chunk_correction,
        }
    }

    fn add_word(&mut self, word: &str) {
        // Add to chunk (if enabled)
        if self.enable_chunk_correction {
            if !self.chunk_words.is_empty() {
                self.chunk_char_count += 1; // space
            }
            self.chunk_char_count += word.len();
            self.chunk_words.push(word.to_string());
        }

        // Add to paragraph
        if !self.paragraph_words.is_empty() {
            self.paragraph_char_count += 1; // space
        }
        self.paragraph_char_count += word.len();
        self.paragraph_words.push(word.to_string());
    }

    fn take_chunk(&mut self) -> (String, usize, usize, Vec<String>) {
        let text = self.chunk_words.join(" ");
        let word_count = self.chunk_words.len();
        let char_count = self.chunk_char_count;
        let words = std::mem::take(&mut self.chunk_words);
        self.chunk_char_count = 0;
        (text, word_count, char_count, words)
    }

    fn take_paragraph(&mut self) -> (String, usize, usize) {
        let text = self.paragraph_words.join(" ");
        let word_count = self.paragraph_words.len();
        let char_count = self.paragraph_char_count;
        self.paragraph_words.clear();
        self.paragraph_char_count = 0;
        self.chunk_words.clear();
        self.chunk_char_count = 0;
        (text, word_count, char_count)
    }

    fn should_correct_chunk(&self, word: &str) -> bool {
        if !self.enable_chunk_correction {
            return false;
        }
        let trimmed = word.trim();
        // Correct on: sentence end, comma, semicolon, or chunk size reached
        let ends_sentence = trimmed.ends_with('.')
            || trimmed.ends_with('?')
            || trimmed.ends_with('!');
        let ends_minor = trimmed.ends_with(',') || trimmed.ends_with(';');

        ends_sentence || (self.correct_on_commas && ends_minor) || self.chunk_words.len() >= self.chunk_size
    }

    fn chunk_len(&self) -> usize {
        if self.enable_chunk_correction {
            self.chunk_words.len()
        } else {
            0
        }
    }

    fn paragraph_len(&self) -> usize {
        self.paragraph_words.len()
    }

    fn reset_chunk(&mut self) {
        self.chunk_words.clear();
        self.chunk_char_count = 0;
    }

    /// Update paragraph after a chunk correction to reflect what's actually on screen
    fn apply_chunk_correction(&mut self, original_words: &[String], corrected: &str) {
        // Remove the original chunk words from paragraph
        let chunk_len = original_words.len();
        let old_para_len = self.paragraph_words.len();
        if self.paragraph_words.len() >= chunk_len {
            self.paragraph_words.truncate(self.paragraph_words.len() - chunk_len);
        }

        // Add the corrected words to paragraph
        let corrected_words: Vec<&str> = corrected.split_whitespace().collect();
        for word in &corrected_words {
            self.paragraph_words.push(word.to_string());
        }

        let old_char_count = self.paragraph_char_count;
        // Recalculate paragraph char count from actual words
        self.paragraph_char_count = if self.paragraph_words.is_empty() {
            0
        } else {
            self.paragraph_words.iter().map(|w| w.len()).sum::<usize>()
                + self.paragraph_words.len() - 1 // spaces between words
        };

        eprintln!("[CHUNK APPLY] para_words: {} -> {}, para_chars: {} -> {}, corrected_len={}",
                  old_para_len, self.paragraph_words.len(),
                  old_char_count, self.paragraph_char_count,
                  corrected.len());
    }
}

#[cfg(feature = "llm-correct")]
fn is_safe_correction(original: &str, corrected: &str, profile: CorrectionProfile) -> bool {
    let filler_words = ["uh", "um", "erm", "uhh", "umm"];
    let normalize = |word: &str| {
        word.trim_matches(|c: char| !c.is_alphanumeric())
            .to_lowercase()
    };

    let original_words: Vec<String> = original
        .split_whitespace()
        .map(normalize)
        .filter(|w| !w.is_empty() && !filler_words.contains(&w.as_str()))
        .collect();
    let corrected_words: Vec<String> = corrected
        .split_whitespace()
        .map(normalize)
        .filter(|w| !w.is_empty() && !filler_words.contains(&w.as_str()))
        .collect();

    if original_words.is_empty() || corrected_words.is_empty() {
        return false;
    }

    let original_len = original_words.len();
    let corrected_len = corrected_words.len();

    let (min_ratio, max_ratio, base_threshold) = match profile {
        CorrectionProfile::Journal => (0.95, 1.05, 0.95),
        CorrectionProfile::Technical => (0.9, 1.1, 0.9),
    };

    let len_ratio = corrected_len as f32 / original_len as f32;
    if len_ratio > max_ratio || len_ratio < min_ratio {
        return false;
    }

    let original_set: HashSet<String> = original_words.into_iter().collect();
    let corrected_set: HashSet<String> = corrected_words.into_iter().collect();
    let overlap = original_set.intersection(&corrected_set).count();
    let overlap_ratio = overlap as f32 / original_set.len() as f32;

    let threshold = if original_set.len() <= 3 { 0.7 } else { base_threshold };
    overlap_ratio >= threshold
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

/// Handle a word with chunked LLM correction
#[cfg(feature = "llm-correct")]
async fn handle_word_with_correction(
    word: &str,
    keyboard: &mut Box<dyn VirtualKeyboard>,
    buffer: &mut CorrectionBuffer,
    corrector: &mut SentenceCorrector,
    profile: CorrectionProfile,
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

    // Check if we should correct this chunk
    if buffer.should_correct_chunk(word) && buffer.chunk_len() >= 2 {
        let (original, _word_count, char_count, original_words) = buffer.take_chunk();

        // Get correction from LLM
        match corrector.correct_sentence(&original).await {
            Ok(corrected) if corrected != original => {
                if !is_safe_correction(&original, &corrected, profile) {
                    eprintln!("[CHUNK SKIP] low similarity");
                    return Ok(());
                }
                eprintln!("[CHUNK] '{}' -> '{}'", original, corrected);

                // Delete trailing space plus typed characters to avoid word-boundary issues
                let total_backspaces = char_count.saturating_add(1);
                for _ in 0..total_backspaces {
                    keyboard.press_key(SpecialKey::Backspace)?;
                }

                // Type corrected text
                keyboard.type_text(&corrected)?;
                keyboard.press_key(SpecialKey::Space)?;

                // Update paragraph to reflect what's actually on screen
                buffer.apply_chunk_correction(&original_words, &corrected);
            }
            Ok(_) => {
                // No correction needed
            }
            Err(e) => {
                eprintln!("[CHUNK ERROR] {}", e);
            }
        }
    }

    Ok(())
}

/// Final paragraph correction when dictation pauses
#[cfg(feature = "llm-correct")]
async fn correct_final_paragraph(
    keyboard: &mut Box<dyn VirtualKeyboard>,
    buffer: &mut CorrectionBuffer,
    corrector: &mut SentenceCorrector,
) -> Result<()> {
    if buffer.paragraph_len() < 2 {
        return Ok(());
    }

    let (original, word_count, char_count) = buffer.take_paragraph();

    eprintln!("[FINAL DEBUG] word_count={}, original_len={}", word_count, original.len());

    match corrector.correct_paragraph(&original).await {
        Ok(corrected) if corrected != original => {
            if !is_safe_correction(&original, &corrected, corrector.profile()) {
                eprintln!("[FINAL SKIP] low similarity");
                return Ok(());
            }
            eprintln!("[FINAL] '{}' -> '{}'", original, corrected);

            // Delete trailing space plus typed characters to avoid word-boundary issues
            let total_backspaces = char_count.saturating_add(1);
            for _ in 0..total_backspaces {
                keyboard.press_key(SpecialKey::Backspace)?;
            }

            // Type corrected text
            keyboard.type_text(&corrected)?;
            keyboard.press_key(SpecialKey::Space)?;
        }
        Ok(_) => {
            eprintln!("[FINAL] No changes needed");
        }
        Err(e) => {
            eprintln!("[FINAL ERROR] {}", e);
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

    // Write status for waybar integration
    let status_str = match new_state {
        DictationState::Listening => "listening",
        DictationState::Suspended => "paused",
        DictationState::Inactive => "inactive",
    };
    write_status(status_str);

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

    // Write status for waybar integration
    let status_str = match new_state {
        DictationState::Listening => "listening",
        DictationState::Suspended => "paused",
        DictationState::Inactive => "inactive",
    };
    write_status(status_str);

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

fn send_processing_notification(notifications: &DictationNotificationConfig) {
    if !notifications.enabled {
        return;
    }

    let message = notifications.processing_message.as_str();
    if message.trim().is_empty() {
        return;
    }

    if let Err(err) = notify("eaRS Dictation", message) {
        eprintln!("Failed to send processing notification: {}", err);
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
        let part = part.trim().to_lowercase();
        match part.as_str() {
            "ctrl" | "control" => ctrl = true,
            "shift" => shift = true,
            "alt" => alt = true,
            // Special keys
            "return" | "enter" => key = rdev::Key::Return,
            "kpadd" | "kp_add" | "numpadplus" => key = rdev::Key::KpPlus,
            "kpminus" | "kp_minus" | "numpadminus" => key = rdev::Key::KpMinus,
            "kpenter" | "kp_enter" | "numpadenter" => key = rdev::Key::KpReturn,
            "space" => key = rdev::Key::Space,
            "tab" => key = rdev::Key::Tab,
            "escape" | "esc" => key = rdev::Key::Escape,
            "backspace" => key = rdev::Key::Backspace,
            "delete" => key = rdev::Key::Delete,
            // Letter keys
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
