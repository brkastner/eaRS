#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
STATE_DIR="${XDG_STATE_HOME:-$HOME/.local/state}/ears"
PROFILE_FILE="$STATE_DIR/correction_profile"
PID_FILE="$STATE_DIR/dictation.pid"

usage() {
  cat <<'EOF'
Usage: toggle-dictation-profile.sh [options] [-- <dictation args>]

Options:
  --profile <name>           Start dictation with explicit profile.
  --journal                  Shortcut for --profile journal.
  --technical                Shortcut for --profile technical.
  --status                   Print current profile and exit.
  --no-ollama-autostart        Disable automatic ollama start.
  --no-preload                Disable model preload.
  --ollama-keep-alive <val>   keep_alive value (default: -1).
  --ollama-model <name>       Use one model for fast+final (single-model).
  --ollama-journal-model <n>  Single model for journal profile.
  --ollama-technical-model<n> Single model for technical profile.
  --only                      Use one model per profile (journal/technical).
  --no-gateway-autostart       Disable gateway auto-start.
  --no-ears-server-autostart   Disable ears-server auto-start.
  --shutdown                   Stop dictation, gateway, ears-server, and unload ollama.
  -h, --help                  Show this help.

If no profile is provided, the script toggles between journal and technical.
EOF
}

profile_arg=""
action="toggle"
shutdown=false
pass_args=()
ollama_autostart=true
ollama_preload=true
ollama_keep_alive="-1"
ollama_model=""
ollama_journal_model=""
ollama_technical_model=""
only=false
gateway_autostart=true
ears_server_autostart=true

while [[ $# -gt 0 ]]; do
  case "$1" in
    --profile)
      profile_arg="${2:-}"
      shift 2
      ;;
    --journal)
      profile_arg="journal"
      shift
      ;;
    --technical)
      profile_arg="technical"
      shift
      ;;
    --status)
      action="status"
      shift
      ;;
    --shutdown)
      shutdown=true
      shift
      ;;
    --no-ollama-autostart)
      ollama_autostart=false
      shift
      ;;
    --no-preload)
      ollama_preload=false
      shift
      ;;
    --ollama-keep-alive)
      ollama_keep_alive="${2:-}"
      shift 2
      ;;
    --ollama-model)
      ollama_model="${2:-}"
      shift 2
      ;;
    --ollama-journal-model)
      ollama_journal_model="${2:-}"
      shift 2
      ;;
    --ollama-technical-model)
      ollama_technical_model="${2:-}"
      shift 2
      ;;
    --only)
      only=true
      shift
      ;;
    --no-gateway-autostart)
      gateway_autostart=false
      shift
      ;;
    --no-ears-server-autostart)
      ears_server_autostart=false
      shift
      ;;
    --)
      shift
      pass_args=("$@")
      break
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage
      exit 1
      ;;
  esac
done

mkdir -p "$STATE_DIR"

current_profile="journal"
if [[ -f "$PROFILE_FILE" ]]; then
  current_profile="$(cat "$PROFILE_FILE" 2>/dev/null || echo "journal")"
fi

if [[ "$action" == "status" ]]; then
  echo "$current_profile"
  exit 0
fi

if $shutdown; then
  if [[ -f "$PID_FILE" ]]; then
    pid="$(cat "$PID_FILE" 2>/dev/null || true)"
    if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
      kill "$pid" 2>/dev/null || true
      sleep 0.2
    fi
  fi

  gateway_dir="${ASR_GATEWAY_DIR:-$HOME/dev/asr-gateway}"
  if [[ -d "$gateway_dir" ]] && command -v just >/dev/null 2>&1; then
    (cd "$gateway_dir" && just stop-all) || true
  fi

  if [[ -x "${EARS_SERVER_BIN:-$ROOT/target/release/ears}" ]]; then
    "${EARS_SERVER_BIN:-$ROOT/target/release/ears}" server stop >/dev/null 2>&1 || true
  fi

  if command -v ollama >/dev/null 2>&1; then
    ollama stop "${EARS_OLLAMA_MODEL_FAST:-}" >/dev/null 2>&1 || true
    ollama stop "${EARS_OLLAMA_MODEL_FINAL:-}" >/dev/null 2>&1 || true
    ollama stop "${EARS_OLLAMA_MODEL_JOURNAL:-}" >/dev/null 2>&1 || true
    ollama stop "${EARS_OLLAMA_MODEL_TECHNICAL:-}" >/dev/null 2>&1 || true
    ollama stop "qwen2.5:7b" >/dev/null 2>&1 || true
    ollama stop "qwen2.5:32b-16k" >/dev/null 2>&1 || true
  fi

  echo "shutdown complete"
  exit 0
fi

if [[ -n "$profile_arg" ]]; then
  profile="$profile_arg"
else
  if [[ "$current_profile" == "journal" ]]; then
    profile="technical"
  else
    profile="journal"
  fi
fi

if [[ "$profile" != "journal" && "$profile" != "technical" ]]; then
  echo "Invalid profile: $profile" >&2
  exit 1
fi

echo "$profile" > "$PROFILE_FILE"

if [[ -z "${EARS_SERVER_URL:-}" ]]; then
  if [[ "$profile" == "technical" ]]; then
    export EARS_SERVER_URL="ws://127.0.0.1:8771/ws"
  else
    export EARS_SERVER_URL="ws://127.0.0.1:8770/ws"
  fi
fi

server_url="$EARS_SERVER_URL"
server_hostport="${server_url#*://}"
server_hostport="${server_hostport%%/*}"
server_host="${server_hostport%%:*}"
server_port="${server_hostport##*:}"

check_tcp() {
  local host="$1"
  local port="$2"
  if command -v nc >/dev/null 2>&1; then
    nc -z -w 1 "$host" "$port" >/dev/null 2>&1
    return $?
  fi
  (echo > "/dev/tcp/$host/$port") >/dev/null 2>&1
}

get_env_value() {
  local key="$1"
  local file="$2"
  local line
  line="$(grep -E "^[[:space:]]*${key}=" "$file" 2>/dev/null | tail -n1 || true)"
  if [[ -z "$line" ]]; then
    return 1
  fi
  local value="${line#*=}"
  value="${value%\"}"
  value="${value#\"}"
  value="${value%\'}"
  value="${value#\'}"
  echo "$value"
  return 0
}

start_ears_server_if_needed() {
  local upstream_url="$1"
  local upstream_hostport="${upstream_url#*://}"
  upstream_hostport="${upstream_hostport%%/*}"
  local upstream_host="${upstream_hostport%%:*}"
  local upstream_port="${upstream_hostport##*:}"

  if [[ "$upstream_host" != "127.0.0.1" && "$upstream_host" != "localhost" && "$upstream_host" != "::1" ]]; then
    return 0
  fi

  if check_tcp "$upstream_host" "$upstream_port"; then
    return 0
  fi

  local ears_bin="${EARS_SERVER_BIN:-$ROOT/target/release/ears}"
  local engine="${EARS_SERVER_ENGINE:-parakeet}"
  local device="${EARS_SERVER_DEVICE:-amd}"
  local bind="${EARS_SERVER_BIND:-0.0.0.0:$upstream_port}"

  if [[ ! -x "$ears_bin" ]]; then
    echo "ears binary not found at $ears_bin" >&2
    return 1
  fi

  args=(server start --engine "$engine" --bind "$bind")
  if [[ "$engine" == "parakeet" ]]; then
    args+=(--parakeet-device "$device")
  fi

  "$ears_bin" "${args[@]}" >/dev/null 2>&1 || true
}

upstream_url=""
if [[ "$server_port" == "8770" ]]; then
  if [[ -n "${EARS_SERVER_URL_JOURNAL:-}" ]]; then
    upstream_url="$EARS_SERVER_URL_JOURNAL"
  else
    env_file="${ASR_GATEWAY_DIR:-$HOME/dev/asr-gateway}/.env"
    if [[ -f "$env_file" ]]; then
      upstream_url="$(get_env_value EARS_SERVER_URL_JOURNAL "$env_file" || true)"
    fi
  fi
elif [[ "$server_port" == "8771" ]]; then
  if [[ -n "${EARS_SERVER_URL_TECHNICAL:-}" ]]; then
    upstream_url="$EARS_SERVER_URL_TECHNICAL"
  else
    env_file="${ASR_GATEWAY_DIR:-$HOME/dev/asr-gateway}/.env"
    if [[ -f "$env_file" ]]; then
      upstream_url="$(get_env_value EARS_SERVER_URL_TECHNICAL "$env_file" || true)"
    fi
  fi
fi

if [[ -z "$upstream_url" ]]; then
  upstream_url="$server_url"
fi

if $ears_server_autostart; then
  start_ears_server_if_needed "$upstream_url" || true
fi

if $gateway_autostart; then
  if [[ "$server_host" == "127.0.0.1" || "$server_host" == "localhost" || "$server_host" == "::1" ]]; then
    if [[ "$server_port" == "8770" || "$server_port" == "8771" || "$server_port" == "8772" ]]; then
      if ! check_tcp "$server_host" "$server_port"; then
        gateway_dir="${ASR_GATEWAY_DIR:-$HOME/dev/asr-gateway}"
        if [[ -d "$gateway_dir" ]]; then
          if command -v just >/dev/null 2>&1; then
            if [[ ! -f "$gateway_dir/.env" ]]; then
              echo "asr-gateway .env not found at $gateway_dir/.env" >&2
            fi
            case "$server_port" in
              8770) gateway_target="journal-start" ;;
              8771) gateway_target="technical-start" ;;
              8772) gateway_target="accuracy-start" ;;
              *)
                if [[ "$profile" == "technical" ]]; then
                  gateway_target="technical-start"
                else
                  gateway_target="journal-start"
                fi
                ;;
            esac
            (cd "$gateway_dir" && just "$gateway_target")
          else
            echo "just not found; cannot start gateway" >&2
          fi
        else
          echo "asr-gateway not found at $gateway_dir" >&2
        fi
      fi
    fi
  fi
fi

ollama_url="${EARS_OLLAMA_URL:-http://127.0.0.1:11434}"
ollama_host="${ollama_url#*://}"
ollama_host="${ollama_host%%/*}"
ollama_host="${ollama_host%%:*}"

if $ollama_autostart; then
  if [[ "$ollama_host" == "127.0.0.1" || "$ollama_host" == "localhost" || "$ollama_host" == "::1" ]]; then
    if ! curl -s "$ollama_url/api/tags" >/dev/null 2>&1; then
      if command -v ollama >/dev/null 2>&1; then
        nohup ollama serve >/tmp/ollama-serve.log 2>&1 &
        for _ in {1..20}; do
          if curl -s "$ollama_url/api/tags" >/dev/null 2>&1; then
            break
          fi
          sleep 0.2
        done
      else
        echo "ollama not found in PATH" >&2
      fi
    fi
  else
    echo "ollama-autostart skipped (non-local EARS_OLLAMA_URL: $ollama_url)" >&2
  fi
fi

selected_model=""
if [[ -n "$ollama_model" ]]; then
  selected_model="$ollama_model"
elif $only; then
  if [[ "$profile" == "journal" && -n "$ollama_journal_model" ]]; then
    selected_model="$ollama_journal_model"
  elif [[ "$profile" == "technical" && -n "$ollama_technical_model" ]]; then
    selected_model="$ollama_technical_model"
  elif [[ "$profile" == "journal" && -n "${EARS_OLLAMA_MODEL_JOURNAL:-}" ]]; then
    selected_model="$EARS_OLLAMA_MODEL_JOURNAL"
  elif [[ "$profile" == "technical" && -n "${EARS_OLLAMA_MODEL_TECHNICAL:-}" ]]; then
    selected_model="$EARS_OLLAMA_MODEL_TECHNICAL"
  elif [[ "$profile" == "journal" ]]; then
    selected_model="qwen2.5:7b"
  else
    selected_model="qwen2.5:32b-16k"
  fi
fi

if [[ -n "$selected_model" ]]; then
  export EARS_OLLAMA_MODEL_FAST="$selected_model"
  export EARS_OLLAMA_MODEL_FINAL="$selected_model"
fi

if $only && command -v ollama >/dev/null 2>&1; then
  if [[ "$ollama_host" == "127.0.0.1" || "$ollama_host" == "localhost" || "$ollama_host" == "::1" ]]; then
    stop_models=(
      "$ollama_model"
      "$ollama_journal_model"
      "$ollama_technical_model"
      "${EARS_OLLAMA_MODEL_FAST:-}"
      "${EARS_OLLAMA_MODEL_FINAL:-}"
      "${EARS_OLLAMA_MODEL_JOURNAL:-}"
      "${EARS_OLLAMA_MODEL_TECHNICAL:-}"
      "qwen2.5:7b"
      "qwen2.5:32b-16k"
    )
    for model in "${stop_models[@]}"; do
      [[ -z "$model" ]] && continue
      [[ "$model" == "$selected_model" ]] && continue
      ollama stop "$model" >/dev/null 2>&1 || true
    done
  fi
fi

if $ollama_preload; then
  preload_model="$selected_model"
  if [[ -z "$preload_model" ]]; then
    preload_model="${EARS_OLLAMA_MODEL_FINAL:-${EARS_OLLAMA_MODEL_FAST:-}}"
  fi
  if [[ -n "$preload_model" ]]; then
    curl -s "$ollama_url/api/generate" \
      -H "Content-Type: application/json" \
      -d "{\"model\":\"$preload_model\",\"keep_alive\":$ollama_keep_alive}" \
      >/dev/null 2>&1 || true
  fi
fi

if [[ -f "$PID_FILE" ]]; then
  pid="$(cat "$PID_FILE" 2>/dev/null || true)"
  if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
    kill "$pid" 2>/dev/null || true
    sleep 0.2
  fi
fi

export EARS_CORRECTION_PROFILE="$profile"
exec "$ROOT/scripts/run-dictation-llm.sh" "${pass_args[@]}"
