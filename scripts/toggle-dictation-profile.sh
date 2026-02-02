#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
STATE_DIR="${XDG_STATE_HOME:-$HOME/.local/state}/ears"
PROFILE_FILE="$STATE_DIR/correction_profile"
PID_FILE="$STATE_DIR/dictation.pid"

usage() {
  cat <<'EOF'
Usage: toggle-dictation-profile.sh [--profile journal|technical] [--status] [-- <dictation args>]

Options:
  --profile <name>  Start dictation with explicit profile.
  --status          Print current profile and exit.
  --journal         Shortcut for --profile journal.
  --technical       Shortcut for --profile technical.
  -h, --help        Show this help.

If no profile is provided, the script toggles between journal and technical.
EOF
}

profile_arg=""
action="toggle"
pass_args=()

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

if [[ -f "$PID_FILE" ]]; then
  pid="$(cat "$PID_FILE" 2>/dev/null || true)"
  if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
    kill "$pid" 2>/dev/null || true
    sleep 0.2
  fi
fi

export EARS_CORRECTION_PROFILE="$profile"
exec "$ROOT/scripts/run-dictation-llm.sh" "${pass_args[@]}"
