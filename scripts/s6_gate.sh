#!/usr/bin/env bash
# s6_gate.sh — Hard gate for S6 (Capacitor Android shell + apiBase + GH APK)
# Usage: KALEIDO_ADMIN_PASSWORD=*** ./scripts/s6_gate.sh [BASE_URL]
# Optional: KALEIDO_APK_PATH=...  (default: ../kaleido-android/dist/kaleido-s6-debug.apk)
set -euo pipefail
BASE=${1:-http://127.0.0.1:18766}
PASS=${KALEIDO_ADMIN_PASSWORD:-}
CURLFLAGS=${KALEIDO_CURL_FLAGS:-}
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APK=${KALEIDO_APK_PATH:-$ROOT/../kaleido-android/dist/kaleido-s6-debug.apk}
ANDROID_ROOT=${KALEIDO_ANDROID_ROOT:-$ROOT/../kaleido-android}
TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT

# Avoid proxy hijack (local bind + public TLS)
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
if [ -z "$PASS" ]; then
  echo "[FAIL] KALEIDO_ADMIN_PASSWORD not set and not in .env" >&2
  exit 1
fi

ERR=0
log() { echo "[$(date -u +%H:%M:%S)] $*"; }
fail() { echo "[FAIL] $*" >&2; ERR=1; }

curl_get() {
  # shellcheck disable=SC2086
  curl -sS -m 30 $CURLFLAGS "$@"
}

log "base=$BASE"

# 1) health phase=S6
log "checking /health phase=S6"
H=$(curl_get "$BASE/health" || true)
if ! echo "$H" | grep -q '"phase":"S6"'; then
  fail "/health phase is not S6: $H"
else
  log "OK phase=S6"
fi

# 2) public info android_shell=capacitor
log "checking /api/v1/public/info android_shell"
INFO=$(curl_get "$BASE/api/v1/public/info" || true)
printf '%s' "$INFO" >"$TMPDIR/info.json"
if python3 -c '
import json
d=json.load(open("'"$TMPDIR"'/info.json"))
assert d.get("phase")=="S6", d
feats=d.get("features") or {}
assert feats.get("android_shell")=="capacitor", feats
' 2>/dev/null; then
  log "OK features.android_shell=capacitor"
else
  fail "public/info: $INFO"
fi

# 3) web UI markers — write to file (avoid pipefail+SIGPIPE on huge bodies)
log "checking /web/ S6 + api-base"
curl_get -m 10 "$BASE/web/" >"$TMPDIR/web.html" || true
grep -q 'id="api-base"' "$TMPDIR/web.html" || fail 'api-base input missing'
grep -qi 'S6' "$TMPDIR/web.html" || fail 'S6 title/tagline missing'
grep -q 'kaleido.example.com' "$TMPDIR/web.html" || fail 'default public URL placeholder missing'
log "OK UI markers"

# 4) web/app.js apiBase helpers
log "checking /web/app.js apiBase"
curl_get -m 10 "$BASE/web/app.js" >"$TMPDIR/app.js" || true
grep -q 'kaleido_api_base' "$TMPDIR/app.js" || fail 'API_BASE_KEY missing in app.js'
grep -q 'function apiBase' "$TMPDIR/app.js" || fail 'apiBase fn missing'
grep -q 'isCapacitor' "$TMPDIR/app.js" || fail 'isCapacitor missing'
grep -q 'DEFAULT_REMOTE' "$TMPDIR/app.js" || fail 'DEFAULT_REMOTE missing'
log "OK app.js apiBase"

# 5) login
log "login"
RES=$(curl_get -H 'Content-Type: application/json' \
  -d "{\"username\":\"admin\",\"password\":\"$PASS\"}" \
  "$BASE/api/v1/auth/login" || true)
TOKEN=$(printf '%s' "$RES" | python3 -c 'import sys,json;print(json.load(sys.stdin).get("token",""))' 2>/dev/null || true)
if [ -z "$TOKEN" ]; then
  fail "login failed: $RES"
  echo "[FAIL] gate aborted (no token)"
  exit 1
fi
log "OK login token_len=${#TOKEN}"
AUTH="Authorization: Bearer $TOKEN"

# 6) settings GET
log "GET /api/v1/settings"
SET=$(curl_get -H "$AUTH" "$BASE/api/v1/settings" || true)
printf '%s' "$SET" >"$TMPDIR/settings.json"
if python3 -c 'import json; d=json.load(open("'"$TMPDIR"'/settings.json")); assert isinstance(d, dict)' 2>/dev/null; then
  log "OK settings"
else
  fail "settings: $SET"
fi

# 7) Capacitor project layout
log "checking kaleido-android project"
if [ ! -d "$ANDROID_ROOT" ]; then
  fail "android root missing: $ANDROID_ROOT"
else
  test -f "$ANDROID_ROOT/capacitor.config.json" || fail "capacitor.config.json missing"
  test -f "$ANDROID_ROOT/.github/workflows/android-apk.yml" || fail "GH workflow missing"
  test -f "$ANDROID_ROOT/android/app/build.gradle" || fail "android/app/build.gradle missing"
  if grep -q 'io.github.kaleido' "$ANDROID_ROOT/android/app/build.gradle"; then
    log "OK applicationId io.github.kaleido"
  else
    fail "applicationId not io.github.kaleido"
  fi
  if grep -q 'io.github.kaleido' "$ANDROID_ROOT/capacitor.config.json"; then
    log "OK capacitor appId"
  else
    fail "capacitor appId mismatch"
  fi
  test -f "$ANDROID_ROOT/www/index.html" || fail "www/index.html missing"
  test -f "$ANDROID_ROOT/www/app.js" || fail "www/app.js missing"
fi

# 8) Debug APK artifact
log "checking debug APK: $APK"
if [ ! -f "$APK" ]; then
  fail "APK missing: $APK (download GH artifact kaleido-s6-debug-apk)"
else
  SIZE=$(stat -c%s "$APK")
  if [ "$SIZE" -lt 500000 ]; then
    fail "APK too small: $SIZE bytes"
  else
    if file "$APK" | grep -qi 'Android\|Zip\|APK'; then
      log "OK APK size=${SIZE} type=$(file -b "$APK" | cut -c1-60)"
    else
      fail "APK file type unexpected: $(file "$APK")"
    fi
  fi
fi

if [ "$ERR" -ne 0 ]; then
  echo "[FAIL] s6_gate base=$BASE"
  exit 1
fi
echo "[PASS] s6_gate base=$BASE"
exit 0
