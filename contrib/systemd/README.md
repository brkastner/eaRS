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

This setup runs the STT server and Ollama on a powerful desktop, with the
dictation client on a laptop connecting over Tailscale.

**On desktop (server):**
```bash
# Install and start Ollama
curl -fsSL https://ollama.com/install.sh | sh
sudo systemctl enable --now ollama
ollama pull qwen2.5:7b

# Start ears-server (binds to all interfaces)
systemctl --user enable --now ears-server-remote
```

**On laptop (client):**
```bash
# Build with LLM correction support
cargo install --features "parakeet,amd,llm-correct" --path .

# Create override with your desktop's Tailscale hostname
mkdir -p ~/.config/systemd/user/ears-dictation-remote.service.d
cat > ~/.config/systemd/user/ears-dictation-remote.service.d/override.conf << 'EOF'
[Service]
Environment=EARS_SERVER_URL=ws://YOUR-DESKTOP.tailnet:8765
Environment=EARS_OLLAMA_URL=http://YOUR-DESKTOP.tailnet:11434
EOF

systemctl --user daemon-reload
systemctl --user enable --now ears-dictation-remote
```

## LLM Correction

When built with `--features llm-correct`, the dictation client sends completed
sentences to Ollama for grammar/punctuation correction. This happens in
real-time: words are typed immediately, and corrections are applied at sentence
boundaries by backspacing and retyping.

**Environment variables:**
- `EARS_SERVER_URL` - WebSocket URL of ears-server (default: ws://127.0.0.1:8765)
- `EARS_OLLAMA_URL` - Ollama API endpoint (default: http://localhost:11434)
- `EARS_OLLAMA_MODEL` - Model for correction (default: qwen2.5:7b)

**CLI options:**
```bash
ears-dictation --server ws://desktop:8765 --ollama-url http://desktop:11434 --ollama-model qwen2.5:7b
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
