#!/usr/bin/env bash
# Dev launcher: start whisperd + whisper-tray together from the repo, the way a
# packaged Whisper.app would (D-0014), but without building a bundle.
#
# Builds release binaries if they are missing (or with --build to force), starts
# whisperd in the background (logging to the same file the tray uses), waits for
# /v1/health, then runs whisper-tray in the foreground. On exit/Ctrl-C it SIGINTs
# ONLY a daemon it started — an already-running daemon is left alone.
#
# Usage: scripts/run.sh [--build]
set -euo pipefail

export PATH="/opt/homebrew/opt/rustup/bin:$PATH"

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WHISPERD="$REPO/target/release/whisperd"
TRAY="$REPO/target/release/whisper-tray"
URL="http://127.0.0.1:8737"
LOG_DIR="$HOME/Library/Logs/whisper-engine"
LOG="$LOG_DIR/whisperd.log"

FORCE_BUILD=0
for arg in "$@"; do
  case "$arg" in
    --build) FORCE_BUILD=1 ;;
    -h|--help) echo "Usage: scripts/run.sh [--build]"; exit 0 ;;
    *) echo "unknown argument: $arg" >&2; exit 2 ;;
  esac
done

if [ "$FORCE_BUILD" -eq 1 ] || [ ! -x "$WHISPERD" ] || [ ! -x "$TRAY" ]; then
  echo "==> Building release binaries"
  cargo build --release -p whisper-iface --features platform --bin whisperd
  cargo build --release -p whisper-ux --bin whisper-tray
fi

mkdir -p "$LOG_DIR"

STARTED_DAEMON=0
DAEMON_PID=""

cleanup() {
  # Only stop a daemon we started (never touch an adopted one).
  if [ "$STARTED_DAEMON" -eq 1 ] && [ -n "$DAEMON_PID" ]; then
    echo "==> Stopping whisperd ($DAEMON_PID)"
    kill -INT "$DAEMON_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT INT TERM

if curl -sf "$URL/v1/health" >/dev/null 2>&1; then
  echo "==> Daemon already running at $URL — adopting it (not managed by this script)"
else
  echo "==> Starting whisperd (log: $LOG)"
  "$WHISPERD" >>"$LOG" 2>&1 &
  DAEMON_PID=$!
  STARTED_DAEMON=1
  # Wait for readiness (models load lazily; /v1/health is up as soon as bound).
  for _ in $(seq 1 50); do
    if curl -sf "$URL/v1/health" >/dev/null 2>&1; then
      break
    fi
    sleep 0.2
  done
  if ! curl -sf "$URL/v1/health" >/dev/null 2>&1; then
    echo "whisperd did not become reachable — see $LOG" >&2
    exit 1
  fi
fi

echo "==> Starting whisper-tray (foreground; Ctrl-C to quit)"
"$TRAY"
