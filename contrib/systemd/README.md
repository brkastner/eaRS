# eaRS Systemd User Services

User-level systemd services for running eaRS as daemons.

## Installation

```bash
# Copy services to user systemd directory
cp contrib/systemd/*.service ~/.config/systemd/user/

# Reload systemd
systemctl --user daemon-reload
```

## Building

```bash
# Local use (no LLM correction)
cargo install --features "parakeet,amd" --path .

# With LLM correction (requires Ollama)
cargo install --features "parakeet,amd,llm-correct" --path .
```

## Service Variants

### Local Setup (single machine)
- `ears-server.service` - STT server (localhost only)
- `ears-dictation.service` - Dictation client (requires ears-server)

```bash
systemctl --user enable --now ears-server ears-dictation
```

### Remote Setup (desktop server + laptop client via Tailscale)

Remote setups still work using `ears-dictation-remote.service`, which defaults
to Tailscale hostname `athena`.

## LLM Correction

When built with `--features llm-correct`, the dictation client sends completed
sentences to Ollama for grammar/punctuation correction. This happens in
real-time: words are typed immediately, and corrections are applied at sentence
boundaries by backspacing and retyping.

**Environment variables:**
- `EARS_SERVER_URL` - WebSocket URL of ears-server (default: ws://127.0.0.1:8765)
- `EARS_OLLAMA_URL` - Ollama API endpoint (default: http://localhost:11434)
- `EARS_OLLAMA_MODEL` - Model for correction (default: qwen2.5:14b)
- `EARS_OLLAMA_MODEL_FAST` / `EARS_OLLAMA_MODEL_FINAL` - Split fast/final models
- `EARS_CORRECTION_PROFILE` - `journal` or `technical`

**CLI options:**
```bash
ears-dictation --server ws://desktop:8765 --ollama-url http://desktop:11434 --ollama-model qwen2.5:14b
```

## Logs

Logs are written to `~/.local/state/ears/`:
- `server.log` - Server output
- `dictation.log` - Dictation client output

```bash
# Follow logs
tail -f ~/.local/state/ears/server.log
tail -f ~/.local/state/ears/dictation.log
```

## Toggle Dictation

Send SIGUSR1 to toggle audio capture:
```bash
pkill -USR1 ears-dictation
```

Bind this to a key in your window manager (niri, Hyprland, etc.).
