#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export KALEIDO_DESKTOP_URL="${KALEIDO_DESKTOP_URL:-http://127.0.0.1:18766/web/}"
export KALEIDO_API_BASE="${KALEIDO_API_BASE:-http://127.0.0.1:18766}"
cd "$ROOT/src-tauri"
exec cargo run "$@"
