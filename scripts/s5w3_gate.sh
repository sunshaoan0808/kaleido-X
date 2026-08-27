#!/usr/bin/env bash
# s5w3_gate.sh — Hard gate for S5-W3 (versions + llm/test + residual UI)
# Usage: KALEIDO_ADMIN_PASSWORD=*** ./scripts/s5w3_gate.sh [BASE_URL]
set -euo pipefail
BASE=${1:-http://127.0.0.1:18766}
PASS=${KALEIDO_ADMIN_PASSWORD:-}
CURLFLAGS=${KALEIDO_CURL_FLAGS:-}
if [[ "$BASE" == https://kaleido.example.com* ]] && [ -z "$CURLFLAGS" ]; then
  CURLFLAGS=""
fi
if [ -z "$PASS" ] && [ -f ${REPO:-.}/.env ]; then
  PASS=$(set -a; source ${REPO:-.}/.env 2>/dev/null; set +a; echo "${KALEIDO_ADMIN_PASSWORD:-}")
fi
if [ -z "$PASS" ]; then
  echo "[FAIL] KALEIDO_ADMIN_PASSWORD not set and not in .env" >&2
  exit 1
fi

ERR=0
log() { echo "[$(date -u +%H:%M:%S)] $*"; }
fail() { echo "[FAIL] $*" >&2; ERR=1; }
CURL="curl -sS -m 25 ${CURLFLAGS}"

log "base=$BASE"

# 1) health phase
log "checking /health phase (S5-W3 or later S6)"
H=$($CURL "$BASE/health")
# Accept historical S5-W3 marker or current shipped phase S6 (do not require phase bump).
if echo "$H" | grep -Eq '"phase":"(S5-W3|S6|S7)"'; then
  log "OK phase present in health"
else
  fail "/health phase unexpected: $H"
fi
if ! echo "$H" | grep -q '"llm_configured":true'; then
  fail "LLM not configured"
else
  log "OK llm_configured"
fi

# 2) web + no PNG TODO residual
log "checking /web/"
WEB=$($CURL -m 8 "$BASE/web/")
echo "$WEB" | grep -q 'tab-st\|ST Import\|st-import\|data-tab="st"' || fail "ST tab missing on /web/"
if echo "$WEB" | grep -q 'PNG tEXt TODO'; then
  fail "ST tab still says PNG tEXt TODO"
else
  log "OK ST UI no PNG TODO"
fi
echo "$WEB" | grep -q 'works-version' || fail "works-version button missing"
log "OK works version buttons"

# 3) login
log "login"
RES=$($CURL -H 'Content-Type: application/json' -d "{\"username\":\"admin\",\"password\":\"$PASS\"}" "$BASE/api/v1/auth/login")
TOKEN=$(echo "$RES" | python3 -c 'import sys,json;print(json.load(sys.stdin).get("token",""))' 2>/dev/null || true)
if [ -z "$TOKEN" ]; then
  fail "login failed: $RES"
  echo "[FAIL] gate aborted (no token)"
  exit 1
fi
log "OK login token_len=${#TOKEN}"
AUTH="Authorization: Bearer $TOKEN"

# 4) llm/test real probe
log "T3 llm/test"
LT=$($CURL -H "$AUTH" -H 'Content-Type: application/json' -d '{}' "$BASE/api/v1/llm/test")
echo "$LT" | python3 -c 'import sys,json;d=json.load(sys.stdin);assert d.get("ok") is True, d; assert d.get("latencyMs") is not None' 2>/dev/null \
  && log "OK llm/test" || fail "llm/test bad: $LT"

# 5) versions CRUD on a works file
log "T2 versions"
WS_PATH="s5w3_gate_ver.md"
WF=$($CURL -X PUT -H "$AUTH" -H 'Content-Type: application/json' \
  -d "{\"path\":\"$WS_PATH\",\"content\":\"gate version v1 $(date -u +%s)\\n\"}" \
  "$BASE/api/v1/works/file")
echo "$WF" | python3 -c 'import sys,json;d=json.load(sys.stdin);assert d.get("path"), d' 2>/dev/null \
  && log "OK works write" || fail "works write failed: $WF"
CV=$($CURL -H "$AUTH" -H 'Content-Type: application/json' \
  -d "{\"path\":\"$WS_PATH\"}" "$BASE/api/v1/versions")
VID=$(echo "$CV" | python3 -c 'import sys,json;d=json.load(sys.stdin);print((d.get("version") or {}).get("id",""))' 2>/dev/null || true)
if [ -z "$VID" ]; then
  fail "create version failed: $CV"
else
  log "OK create version id=${VID:0:8}"
fi
LV=$($CURL -H "$AUTH" "$BASE/api/v1/versions?path=$WS_PATH")
echo "$LV" | python3 -c 'import sys,json;d=json.load(sys.stdin);assert d.get("ok") is True; assert len(d.get("versions") or [])>=1' 2>/dev/null \
  && log "OK list versions" || fail "list versions bad: $LV"
RC=$($CURL -H "$AUTH" "$BASE/api/v1/versions/content?path=$WS_PATH&versionId=$VID")
echo "$RC" | python3 -c 'import sys,json;d=json.load(sys.stdin);assert d.get("ok") is True; assert "gate version" in (d.get("content") or "")' 2>/dev/null \
  && log "OK read version content" || fail "read version bad: $RC"
AI=$($CURL -H "$AUTH" -H 'Content-Type: application/json' \
  -d "{\"path\":\"$WS_PATH\",\"versionId\":\"$VID\",\"score\":88,\"suggestion\":\"gate\"}" \
  "$BASE/api/v1/versions/ai")
echo "$AI" | python3 -c 'import sys,json;d=json.load(sys.stdin);assert d.get("ok") is True' 2>/dev/null \
  && log "OK versions/ai" || fail "versions/ai bad: $AI"
DV=$($CURL -H "$AUTH" -X DELETE "$BASE/api/v1/versions?path=$WS_PATH&versionId=$VID")
echo "$DV" | python3 -c 'import sys,json;d=json.load(sys.stdin);assert d.get("ok") is True' 2>/dev/null \
  && log "OK delete version" || fail "delete version bad: $DV"

# 6) regression: st-export still ok
log "regression st-export"
SE=$($CURL -H "$AUTH" -H 'Content-Type: application/json' -d '{"kind":"character_card"}' "$BASE/api/v1/partner/st-export")
echo "$SE" | python3 -c 'import sys,json;d=json.load(sys.stdin);assert d.get("ok") or d.get("format")' 2>/dev/null \
  && log "OK st-export" || fail "st-export regression: $SE"

if [ "$ERR" -ne 0 ]; then
  echo "[FAIL] S5-W3 hard gate"
  exit 1
fi
echo "[PASS] S5-W3 hard gate"
exit 0
