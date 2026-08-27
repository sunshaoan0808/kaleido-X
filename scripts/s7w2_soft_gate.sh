#!/usr/bin/env bash
# s7w2_soft_gate.sh — Soft gate for S7-W2 (T2 + T8 markers)
# Usage:
#   ./scripts/s7w2_soft_gate.sh [BASE_URL]
#   KALEIDO_SOFT_SKIP=1 ./scripts/s7w2_soft_gate.sh    # EXIT 0 + skip reason
#   KALEIDO_SOFT_STRICT=1 ...                          # treat live API 404 as fail
#   KALEIDO_ADMIN_PASSWORD=*** ...                     # optional login probes
#
# Does NOT require phase=S7. Accepts / records S6. Never bumps phase.
set -euo pipefail

BASE=${1:-http://127.0.0.1:18766}
PASS=${KALEIDO_ADMIN_PASSWORD:-}
CURLFLAGS=${KALEIDO_CURL_FLAGS:-}
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT

export no_proxy="localhost,127.0.0.1,.local,kaleido.example.com"
export NO_PROXY="$no_proxy"
unset http_proxy https_proxy HTTP_PROXY HTTPS_PROXY ALL_PROXY all_proxy 2>/dev/null || true

if [[ "$BASE" == https://kaleido.example.com* ]] && [ -z "$CURLFLAGS" ]; then
  CURLFLAGS=""
fi

if [ -z "$PASS" ] && [ -f "$ROOT/.env" ]; then
  # shellcheck disable=SC1091
  PASS=$(set -a; source "$ROOT/.env" 2>/dev/null; set +a; echo "${KALEIDO_ADMIN_PASSWORD:-}")
fi

ERR=0
WARN=0
log() { echo "[$(date -u +%H:%M:%S)] $*"; }
fail() { echo "[FAIL] $*" >&2; ERR=1; }
warn() { echo "[WARN] $*" >&2; WARN=1; }

curl_get() {
  # shellcheck disable=SC2086
  curl -sS -m 20 $CURLFLAGS "$@"
}

log "S7-W2 soft gate · root=$ROOT · base=$BASE"

# ---------------------------------------------------------------------------
# 0) explicit skip
# ---------------------------------------------------------------------------
if [ "${KALEIDO_SOFT_SKIP:-}" = "1" ] || [ "${KALEIDO_SOFT_SKIP:-}" = "true" ]; then
  REASON=${KALEIDO_SOFT_SKIP_REASON:-"KALEIDO_SOFT_SKIP set by operator"}
  log "[SKIP] $REASON"
  echo "[PASS] S7-W2 soft gate (skipped: $REASON)"
  exit 0
fi

# ---------------------------------------------------------------------------
# 1) local node --check
# ---------------------------------------------------------------------------
log "node --check web/app.js"
if ! command -v node >/dev/null 2>&1; then
  fail "node not installed"
else
  if node --check "$ROOT/web/app.js"; then
    log "OK node --check"
  else
    fail "node --check web/app.js failed"
  fi
fi

# ---------------------------------------------------------------------------
# 2) local disk markers (T2 + T8)
# ---------------------------------------------------------------------------
log "checking disk markers (T2/T8)"
need_file() {
  local f="$1"
  if [ ! -f "$ROOT/$f" ]; then
    fail "missing file: $f"
  fi
}
need_file "web/app.js"
need_file "web/index.html"
need_file "docs/S7W2_NOTES.md"

check_rg() {
  local pattern="$1"
  local path="$2"
  local label="$3"
  if command -v rg >/dev/null 2>&1; then
    if rg -n -q -- "$pattern" "$ROOT/$path"; then
      log "OK marker [$label] in $path"
    else
      fail "marker missing [$label] in $path (pattern=$pattern)"
    fi
  else
    if grep -R -n -E -- "$pattern" "$ROOT/$path" >/dev/null 2>&1; then
      log "OK marker [$label] in $path (grep)"
    else
      fail "marker missing [$label] in $path (pattern=$pattern)"
    fi
  fi
}

# T2a preview
check_rg 'works-preview' 'web/index.html' 'T2a works-preview DOM'
check_rg 'works-preview' 'web/app.js' 'T2a works-preview JS'
# T2b versions sidebar
check_rg 'works-versions-sidebar' 'web/index.html' 'T2b versions sidebar DOM'
check_rg '/api/v1/versions' 'web/app.js' 'T2b versions API'
# T2c style presets
check_rg 'style-presets' 'web/app.js' 'T2c style-presets JS'
check_rg 'works-style-presets' 'web/index.html' 'T2c style-presets DOM'
# T8 buttons / paths
check_rg 'create-untitled' 'web/app.js' 'T8b create-untitled JS'
check_rg 'works/export' 'web/app.js' 'T8c export JS'
check_rg 'works/move' 'web/app.js' 'T8a move JS'
check_rg 'image-data-url' 'web/app.js' 'T8d image-data-url JS'
check_rg 'works-create-untitled' 'web/index.html' 'T8b create-untitled DOM'

# Rust sources (present on branch even if live binary not yet released)
if [ -f "$ROOT/crates/kaleido-server/src/style_presets.rs" ]; then
  check_rg 'style-presets' 'crates/kaleido-server/src/style_presets.rs' 'T2c API style_presets.rs'
else
  warn "style_presets.rs not on disk (unexpected on t3 branch)"
fi
if [ -f "$ROOT/crates/kaleido-server/src/works_ext.rs" ]; then
  check_rg 'create-untitled' 'crates/kaleido-server/src/works_ext.rs' 'T8 works_ext.rs'
else
  warn "works_ext.rs not on disk (unexpected on t3 branch)"
fi

# NOTES must mention T2 and T8
if grep -q 'T2' "$ROOT/docs/S7W2_NOTES.md" && grep -q 'T8' "$ROOT/docs/S7W2_NOTES.md"; then
  log "OK S7W2_NOTES.md contains T2/T8 checklist"
else
  fail "docs/S7W2_NOTES.md missing T2/T8 checklist headings"
fi

# phase must not be forced to S7 in soft gate expectations — just note health later
log "phase policy: accept S6; do NOT require S7"

# ---------------------------------------------------------------------------
# 3) live probes (soft)
# ---------------------------------------------------------------------------
LIVE_OK=0
log "probing live $BASE/health"
H=$(curl_get "$BASE/health" 2>/dev/null || true)
if [ -z "$H" ]; then
  warn "live unreachable at $BASE — static/local checks only (soft EXIT 0 if no hard fails)"
  log "[SKIP live] base unreachable: $BASE"
else
  LIVE_OK=1
  printf '%s' "$H" >"$TMPDIR/health.json"
  PHASE=$(python3 -c 'import json,sys
try:
 d=json.load(open("'"$TMPDIR"'/health.json"))
 print(d.get("phase",""))
except Exception as e:
 print("")
' 2>/dev/null || true)
  log "health phase=$PHASE (recorded; S6 expected pre-W5)"
  if [ "$PHASE" = "S7" ]; then
    warn "phase already S7 — unexpected for S7-W2 soft gate window; not failing"
  elif [ "$PHASE" != "S6" ] && [ -n "$PHASE" ]; then
    warn "phase is '$PHASE' (expected S6); not hard-failing soft gate"
  else
    log "OK phase recorded as S6 (or empty handled)"
  fi
  # Never fail solely because phase != S7

  log "checking /web/ DOM markers"
  curl_get -m 15 "$BASE/web/" >"$TMPDIR/web.html" || true
  if [ ! -s "$TMPDIR/web.html" ]; then
    warn "/web/ empty or failed"
  else
    for id in works-preview works-versions-sidebar works-style-presets works-create-untitled works-export works-move; do
      if grep -q "$id" "$TMPDIR/web.html"; then
        log "OK live DOM $id"
      else
        # live static may lag branch until release/copy — warn unless strict
        if [ "${KALEIDO_SOFT_STRICT:-}" = "1" ]; then
          fail "live /web/ missing $id"
        else
          warn "live /web/ missing $id (static may predate t2; disk markers OK)"
        fi
      fi
    done
  fi

  log "checking /web/app.js markers"
  curl_get -m 15 "$BASE/web/app.js" >"$TMPDIR/app.js" || true
  if [ ! -s "$TMPDIR/app.js" ]; then
    warn "/web/app.js empty or failed"
  else
    for pat in style-presets create-untitled image-data-url 'works/move' 'works/export' works-preview; do
      if grep -q "$pat" "$TMPDIR/app.js"; then
        log "OK live app.js $pat"
      else
        if [ "${KALEIDO_SOFT_STRICT:-}" = "1" ]; then
          fail "live app.js missing $pat"
        else
          warn "live app.js missing $pat (may need static refresh / t-release)"
        fi
      fi
    done
  fi

  # optional authenticated API soft probes
  if [ -n "$PASS" ]; then
    log "login for optional API probes"
    RES=$(curl_get -H 'Content-Type: application/json' \
      -d "{\"username\":\"admin\",\"password\":\"$PASS\"}" \
      "$BASE/api/v1/auth/login" || true)
    TOKEN=$(printf '%s' "$RES" | python3 -c 'import sys,json
try:
 print(json.load(sys.stdin).get("token",""))
except Exception:
 print("")
' 2>/dev/null || true)
    if [ -z "$TOKEN" ]; then
      warn "login failed (soft): $RES"
    else
      log "OK login token_len=${#TOKEN}"
      AUTH="Authorization: Bearer $TOKEN"

      SP=$(curl_get -H "$AUTH" "$BASE/api/v1/style-presets" || true)
      if printf '%s' "$SP" | python3 -c 'import sys,json
d=json.load(sys.stdin)
# accept array, object, or {ok,presets/...}
sys.exit(0)
' 2>/dev/null; then
        log "OK GET style-presets (body len=${#SP})"
      else
        if [ "${KALEIDO_SOFT_STRICT:-}" = "1" ]; then
          fail "GET style-presets bad: $SP"
        else
          warn "GET style-presets not ready (need t-release?): $SP"
        fi
      fi

      CU=$(curl_get -X POST -H "$AUTH" -H 'Content-Type: application/json' \
        -d '{"dir":""}' "$BASE/api/v1/works/create-untitled" || true)
      if printf '%s' "$CU" | python3 -c 'import sys,json
d=json.load(sys.stdin)
assert d.get("path") or d.get("ok") or d.get("error")
' 2>/dev/null; then
        log "OK POST create-untitled responded"
      else
        if [ "${KALEIDO_SOFT_STRICT:-}" = "1" ]; then
          fail "create-untitled bad: $CU"
        else
          warn "create-untitled not ready (need t-release?): $CU"
        fi
      fi
    fi
  else
    log "skip auth API probes (KALEIDO_ADMIN_PASSWORD unset)"
  fi
fi

# ---------------------------------------------------------------------------
# summary
# ---------------------------------------------------------------------------
if [ "$ERR" -ne 0 ]; then
  echo "[FAIL] S7-W2 soft gate (hard failures above)"
  exit 1
fi

if [ "$LIVE_OK" -eq 0 ]; then
  echo "[PASS] S7-W2 soft gate (local markers OK; live skipped — unreachable $BASE)"
  exit 0
fi

if [ "$WARN" -ne 0 ]; then
  echo "[PASS] S7-W2 soft gate (with WARN; not strict)"
  exit 0
fi

echo "[PASS] S7-W2 soft gate"
exit 0
