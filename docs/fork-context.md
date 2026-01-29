# eaRS Fork Context

Reference document for future Claude sessions working on this fork.

## Environment

| Machine | Hostname | Compositor | GPU | Role |
|---------|----------|-----------|-----|------|
| Desktop | athena | Hyprland | 7900xtx | Runs `ears-server` (Parakeet STT), Ollama (qwen2.5:7b) |
| Laptop | fw | niri | integrated | Runs `ears-dictation` client, connects to athena over Tailscale |

- Tailscale connects fw <-> athena. Use Tailscale hostnames (`athena`, `fw`) not IPs.
- Ollama runs on athena at `http://athena:11434`.
- ears-server runs on athena at `ws://athena:8765` (binds `0.0.0.0:8765`).
- fw runs the dictation client as a systemd user service (`ears-dictation-remote.service`).

## What This Fork Adds (upstream: tommyfalkowski/eaRS)

### LLM Correction (`llm-correct` feature)

Automatic grammar/punctuation cleanup of transcribed text via Ollama.

**Chunked correction** (1.5s pause or 6+ words or punctuation):
- Sends accumulated chunk to Ollama for grammar fix
- Backspaces typed words and retypes corrected text

**Paragraph correction** (5s pause in normal mode):
- Sends full accumulated paragraph for thorough cleanup
- Fixes cross-chunk artifacts, capitalization, coherence

Key files:
- `src/llm_correct.rs` - Ollama API client and correction logic
- `src/bin/ears-dictation.rs` - CorrectionBuffer, pause detection, chunk/paragraph triggers

### Preview Overlay (`preview-overlay` feature)

Buffer-first dictation mode. Instead of typing words directly into the focused app, words accumulate in a floating overlay window. User reviews, then commits (pastes) to the focused app.

**GTK4 layer-shell implementation** (replaced original egui):
- Non-focusable Wayland overlay via `gtk4-layer-shell`
- Anchored bottom-right with 32px margins
- Dark semi-transparent background
- Committed text (dimmed) vs active text (bright)
- Status bar: listening/correcting/paused + word count
- Works on any layer-shell compositor (niri, sway, Hyprland)

**Commands** (sent from dictation client to overlay via async_channel):
- `Word(String)` - new STT word
- `Correction(String)` - LLM correction result
- `Checkpoint` - paste current buffer, continue dictating
- `Commit` - paste all, close overlay
- `Close` - close without pasting
- `Status(OverlayStatus)` - update status indicator

**Responses** (sent from overlay back to dictation client via mpsc):
- `PasteText(String)` - text to paste to focused app
- `Closed` - overlay was closed

**Auto-commit**: 10 seconds of silence with words in buffer triggers automatic commit.

Key files:
- `src/gtk_overlay.rs` - GTK4 layer-shell overlay (OverlayHandle, spawn_overlay, OverlayState)
- `src/preview_buffer.rs` - PreviewBuffer (committed/active sections, checkpoint/commit)
- `src/clipboard.rs` - Clipboard paste via wl-clipboard-rs/arboard

### Standalone Overlay Binary (`overlay` feature, WIP)

`ears-overlay` - runs entirely on fw with local Parakeet STT + remote Ollama. No ears-server needed. Currently a skeleton (`src/bin/ears-overlay.rs`), tasks 13-14 in the implementation plan.

### Pause Detection and Waybar Integration

Status file at `~/.local/state/ears/status.json` with states: listening, paused, processing, inactive. Waybar reads this for indicator display.

### Other Changes

- Streaming dedup and typing reliability fixes
- Verbose debug logging silenced for daemon use
- sentencepiece-sys vendored for static builds
- Parakeet model download progress bar

## Architecture

```
fw (laptop, niri)                          athena (desktop, Hyprland)
┌──────────────────────┐                   ┌──────────────────────┐
│  ears-dictation      │───WebSocket──────>│  ears-server         │
│  (systemd service)   │   ws://athena:8765│  (Parakeet STT)      │
│                      │                   │  ROCm / 7900xtx      │
│  ┌────────────────┐  │                   └──────────────────────┘
│  │ gtk4 overlay   │  │
│  │ (layer-shell)  │  │───HTTP───────────>┌──────────────────────┐
│  └────────────────┘  │  http://athena:   │  Ollama              │
│                      │  11434            │  qwen2.5:7b           │
│  ┌────────────────┐  │                   └──────────────────────┘
│  │ uinput vkbd    │  │
│  │ (types text)   │  │
│  └────────────────┘  │
└──────────────────────┘
        ↑ Tailscale mesh
```

## Systemd Services

| Service | Machine | Purpose |
|---------|---------|---------|
| `ears-server-remote.service` | athena | Parakeet STT, binds 0.0.0.0:8765 |
| `ears-dictation-remote.service` | fw | Dictation client, connects to athena, `--preview` mode |
| `ears-server.service` | either | Local-only STT (localhost) |
| `ears-dictation.service` | either | Local-only dictation |

Remote service env vars:
```
EARS_SERVER_URL=ws://athena:8765
EARS_OLLAMA_URL=http://athena:11434
EARS_OLLAMA_MODEL=qwen2.5:7b
```

## Build Commands

```bash
# fw (laptop) - dictation client with overlay and LLM correction
cargo build --release --features preview-overlay,parakeet,llm-correct,hooks

# athena (desktop) - server with ROCm acceleration
cargo build --release --features parakeet,amd

# Standalone overlay (WIP) - local STT + remote Ollama
cargo build --release --features overlay,amd
```

## Feature Flags

| Feature | Purpose |
|---------|---------|
| `parakeet` | Parakeet TDT 0.6B engine (requires prkt, webrtc-vad) |
| `llm-correct` | Ollama text correction (requires reqwest) |
| `preview-overlay` | GTK4 layer-shell overlay mode (gtk4, gtk4-layer-shell, async-channel, clipboard) |
| `overlay` | Standalone binary with local STT + overlay (superset of preview-overlay + parakeet) |
| `hooks` | Shell command hooks for state changes |
| `amd` | ROCm acceleration for Parakeet |
| `nvidia` | CUDA acceleration |

## Comparison: eaRS vs hyprwhspr

### For dictating to Claude sessions (the primary use case)

**eaRS wins** because:
1. **Buffer-first preview** - see full text before sending to Claude. Catch transcription errors before they become bad prompts.
2. **LLM correction** - Ollama cleans spoken English into written English. "so basically the overlay should use layer shell" becomes "The overlay should use layer-shell." Claude gets cleaner input, produces cleaner output.
3. **No clipboard clobbering** - uinput virtual keyboard doesn't touch clipboard. Critical when copying code/errors between Claude sessions.
4. **Real-time streaming** - words appear as you speak. Immediately catch when Parakeet mishears technical terms.

**hyprwhspr** is better for short voice commands (push-to-talk mode) and has more backend variety (OpenAI, Groq, custom REST). But its batch model (record -> stop -> paste) and clipboard-based injection make it worse for iterative Claude workflows.

### Feature comparison

| | eaRS | hyprwhspr |
|---|---|---|
| Transcription | Real-time streaming | Batch (record then transcribe) |
| Text preview | Full overlay with committed/active sections | Mic OSD only |
| LLM cleanup | Built-in Ollama correction | None (word overrides only) |
| Text injection | uinput virtual keyboard | Clipboard paste |
| AMD GPU | ROCm | Vulkan |
| Wayland | Any layer-shell compositor | Hyprland-first |
| Recording modes | Toggle only | Toggle, PTT, auto, long-form |
| Install | Build from source | AUR, pip, distro scripts |

### hyprwhspr best configs (for reference)

**Local Parakeet (comparable to eaRS):**
```json
{
  "primary_shortcut": "SUPER+ALT+D",
  "recording_mode": "auto",
  "transcription_backend": "parakeet",
  "model": "nvidia/parakeet-tdt-0.6b-v2",
  "language": "en",
  "paste_mode": "ctrl_shift",
  "symbol_replacements": true
}
```

**Local Whisper large-v3 (best single-pass accuracy):**
```json
{
  "primary_shortcut": "SUPER+ALT+D",
  "recording_mode": "toggle",
  "transcription_backend": "pywhispercpp",
  "model": "large-v3",
  "language": "en",
  "symbol_replacements": true
}
```

**Groq cloud (lowest latency, not local):**
```json
{
  "primary_shortcut": "SUPER+ALT+D",
  "recording_mode": "push_to_talk",
  "transcription_backend": "rest-api",
  "rest_endpoint_url": "https://api.groq.com/openai/v1/audio/transcriptions",
  "rest_model": "whisper-large-v3-turbo"
}
```

## Future: Editable Overlay

The overlay currently displays read-only Labels. Adding cursor/selection editing would make it significantly more useful for Claude session dictation (fix a word before committing).

**Recommended approach: pause-to-edit**
- Click overlay -> auto-pauses STT, grabs keyboard (`KeyboardMode::OnDemand`)
- Edit text freely (GtkTextView)
- Click away or keybind -> resumes STT

**Implementation:**
1. Swap Labels for GtkTextView with TextBuffer
2. Toggle layer-shell KeyboardMode on focus
3. Wire pause-on-focus signal to STT capture toggle

This is moderate complexity (PreviewBuffer already tracks committed/active sections). The GTK4 widget swap is straightforward; the UX for concurrent STT + editing is the design challenge.

## Open Work

Remaining tasks from the gtk4-layer-shell plan:
- **Task 11**: Integration testing (manual - test overlay spawning, dictation flow, toggle cycle)
- **Task 12**: Update CLAUDE.md with overlay architecture
- **Task 13**: Create `ears-overlay.service` systemd unit
- **Task 14**: Wire up complete `ears-overlay` binary (local Parakeet + remote Ollama + GTK4 overlay)
