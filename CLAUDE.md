# eaRS - Local Speech-to-Text Dictation System

## Overview

eaRS is a local-first speech-to-text dictation system for Linux/Wayland. It uses local ML models (Parakeet or Kyutai) for real-time transcription with optional LLM-based text correction via Ollama.

## Architecture

```
┌─────────────────┐     ┌─────────────────┐     ┌─────────────────┐
│  Microphone     │────▶│  ears-server    │────▶│ ears-dictation  │
│  (audio input)  │     │  (STT engine)   │     │ (types to apps) │
└─────────────────┘     └─────────────────┘     └─────────────────┘
                               │                        │
                               ▼                        ▼
                        ┌─────────────┐          ┌─────────────┐
                        │ Parakeet or │          │   Ollama    │
                        │   Kyutai    │          │ (LLM cleanup│
                        │  (local ML) │          │  optional)  │
                        └─────────────┘          └─────────────┘
```

## Key Components

| Binary | Purpose |
|--------|---------|
| `ears-server` | WebSocket server running STT inference (Parakeet/Kyutai) |
| `ears-dictation` | Client that types transcribed text via virtual keyboard |
| `ears` | CLI for control operations |

## Dictation Flow

### Real-time Transcription
- Audio captured at device rate, resampled to 24kHz
- Streamed to ears-server via WebSocket
- Parakeet (16kHz) or Kyutai (24kHz) produces word-level transcriptions
- Words typed immediately as they arrive

### LLM Correction (when `llm-correct` feature enabled)

**Chunked correction (2-second pause or natural break):**
- Every 6 words OR at punctuation (`.`, `?`, `!`, `,`, `;`)
- Sends chunk to Ollama for grammar/punctuation cleanup
- Backspaces and retypes corrected text

**Final paragraph correction (5-second pause):**
- Detects extended silence (5+ seconds)
- Sends entire accumulated paragraph for thorough cleanup
- Fixes cross-chunk artifacts, capitalization, coherence

**Toggle-off correction (TODO):**
- When capture is toggled off (keybind/SIGUSR1)
- Should trigger final paragraph cleanup before stopping
- Not yet implemented - see PLAN-dictation-enhancements.md

## User Interaction

**Niri keybind:** Toggle dictation capture on/off
**Waybar indicator:** Shows current state
- 🎤 listening (actively transcribing)
- ⏸ paused (capture off)
- 🔄 processing (LLM correction in progress)
- (empty) inactive (daemon not running)

**Status file:** `~/.local/state/ears/status.json`

## Configuration

**Config file:** `~/.config/ears/config.toml`

Key settings:
```toml
[server]
websocket_port = 8765

[hotkeys]
toggle = "ctrl+shift+v"

[dictation.notifications]
enabled = true
```

## Systemd Services

```bash
# Local operation (default)
systemctl --user start ears-server
systemctl --user start ears-dictation

# Check status
systemctl --user status ears-server ears-dictation
```

## Build

```bash
# Full local setup with LLM correction
cargo build --release --features parakeet,llm-correct,hooks

# GPU acceleration (pick one)
--features nvidia   # CUDA
--features amd      # ROCm
--features apple    # Metal
```

## Development Notes

### Feature Flags
- `parakeet` - Enable Parakeet engine (NVIDIA TDT 0.6B)
- `llm-correct` - LLM text correction via Ollama
- `hooks` - Shell command hooks for state changes

### Key Files
- `src/bin/ears-dictation.rs` - Main dictation client
- `src/llm_correct.rs` - LLM correction logic
- `src/server/parakeet.rs` - Parakeet engine wrapper
- `contrib/systemd/` - Service files

### State Files
- `~/.local/state/ears/dictation.pid` - Dictation client PID
- `~/.local/state/ears/status.json` - Waybar status (listening/paused/processing/inactive)

### Correction Timing
- Chunk correction: 6 words OR punctuation OR 2s pause
- Final correction: 5s pause OR toggle off
- Both use Ollama with `qwen2.5:7b` model by default
