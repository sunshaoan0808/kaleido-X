#!/usr/bin/env bash
# s7_gate.sh — hard gate for S7 (phase + tools + app-state + regression)
set -euo pipefail
BASE=${1:-http://127.0.0.1:18766}
ERR=0
log() { echo "[$(date -u +%H:%M:%S)] $*"; }
fail() { echo "[FAIL] $*"; ERR=$((ERR+1)); }

CURL=(curl -sS -m 30)
PASS=""
USER_NAME="${KALEIDO_ADMIN_USER:-admin}"
if [ -f ${REPO:-.}/.env ]; then
  # shellcheck disable=SC1091
  set -a; source ${REPO:-.}/.env; set +a
  PASS="${KALEIDO_ADMIN_PASSWORD:-}"
  USER_NAME="${KALEIDO_ADMIN_USER:-admin}"
fi

log "base=$BASE"
H=$("${CURL[@]}" "$BASE/health" || true)
echo "$H" | python3 -c 'import sys,json;d=json.load(sys.stdin);assert d.get("ok") is True; assert d.get("phase")=="S7", d' \
  && log "OK health phase=S7" || fail "health phase not S7: $H"

WEB=$("${CURL[@]}" "$BASE/web/" || true)
echo "$WEB" | grep -q 'works-preview\|data-tab="works"\|works-editor' && log "OK web works surface" || fail "web works markers"

if [ -z "$PASS" ]; then
  fail "no admin password"
else
  LOGIN=$("${CURL[@]}" -H 'Content-Type: application/json' \
    -d "{\"username\":\"$USER_NAME\",\"password\":\"$PASS\"}" "$BASE/api/v1/auth/login" || true)
  TOKEN=$(echo "$LOGIN" | python3 -c 'import sys,json;print(json.load(sys.stdin).get("token",""))' 2>/dev/null || true)
  if [ -z "$TOKEN" ]; then
    fail "login failed: $LOGIN"
  else
    log "OK login"
    AUTH="Authorization: Bearer $TOKEN"
    # app-state
    AS=$("${CURL[@]}" -H "$AUTH" "$BASE/api/v1/app-state" || true)
    echo "$AS" | python3 -c 'import sys,json;d=json.load(sys.stdin);assert d.get("ok") is True' \
      && log "OK app-state GET" || fail "app-state GET: $AS"
    # style-presets
    SP=$("${CURL[@]}" -H "$AUTH" "$BASE/api/v1/style-presets" || true)
    echo "$SP" | python3 -c 'import sys,json;json.load(sys.stdin)' && log "OK style-presets" || fail "style-presets: $SP"
    # agent tools edit dry path — expect 4xx without body fields but not 404
    for t in edit grep glob; do
      CODE=$("${CURL[@]}" -o /tmp/s7_tool.json -w '%{http_code}' -H "$AUTH" -H 'Content-Type: application/json' \
        -d '{}' "$BASE/api/v1/agent/tools/$t" || true)
      if [ "$CODE" = "404" ]; then fail "tools/$t missing 404"; else log "OK tools/$t routed code=$CODE"; fi
    done
    # outline analyze/finalize
    for p in analyze finalize; do
      CODE=$("${CURL[@]}" -o /tmp/s7_out.json -w '%{http_code}' -H "$AUTH" -H 'Content-Type: application/json' \
        -d '{"text":"第一章 测试\\n内容"}' "$BASE/api/v1/outline/reverse/$p" || true)
      if [ "$CODE" = "404" ]; then fail "outline/$p 404"; else log "OK outline/$p code=$CODE"; fi
    done
  fi
fi

if [ "$ERR" -ne 0 ]; then
  echo "[FAIL] S7 hard gate errors=$ERR"
  exit 1
fi
echo "[PASS] S7 hard gate"
exit 0
