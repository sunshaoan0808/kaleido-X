#!/usr/bin/env bash
# s5w1_gate.sh — Hard gate for S5-W1
# Usage: KALEIDO_ADMIN_PASSWORD=*** ./scripts/s5w1_gate.sh [BASE_URL]
# Optional: KALEIDO_CURL_FLAGS="--resolve host:443:IP" for public DNS issues
set -euo pipefail
BASE=${1:-http://127.0.0.1:18766}
PASS=${KALEIDO_ADMIN_PASSWORD:-}
CURLFLAGS=${KALEIDO_CURL_FLAGS:-}
# If using public HTTPS and no explicit flags, hard-resolve A record to avoid local DNS/proxy quirks
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

# base curl command; for HTTPS public with static IP, prefer --resolve to avoid proxy/DNS
CURL="curl -sS -m 8 ${CURLFLAGS}"

log "base=$BASE"

# 1) health + phase
log "checking /health phase=S5-W1"
H=$($CURL "$BASE/health")
if ! echo "$H" | grep -q '"phase":"S5-W1"'; then
  fail "/health phase is not S5-W1: $H"
else
  log "OK phase=S5-W1"
fi
if ! echo "$H" | grep -q '"llm_configured":true'; then
  fail "LLM not configured"
else
  log "OK llm_configured"
fi

# 2) web shell /web
log "checking /web/"
$CURL -m 8 -o /dev/null -w "%{http_code}" "$BASE/web/" | grep -q '^200$' || fail "/web/ not 200"
log "OK /web/"

# 3) login
log "login"
RES=$($CURL -H 'Content-Type: application/json' -d "{\"username\":\"admin\",\"password\":\"$PASS\"}" "$BASE/api/v1/auth/login")
TOKEN=$(echo "$RES" | python3 -c 'import sys,json;print(json.load(sys.stdin).get("token",""))' 2>/dev/null || true)
if [ -z "$TOKEN" ]; then
  fail "login failed: $RES"
else
  log "OK login token_len=${#TOKEN}"
fi

AUTH="Authorization: Bearer $TOKEN"

# 4) settings read + write (LLM editable from web)
log "settings GET"
S=$($CURL -H "$AUTH" "$BASE/api/v1/settings")
echo "$S" | python3 -m json.tool > /dev/null && log "OK settings JSON" || fail "settings GET invalid JSON: $S"
log "settings PATCH baseUrl"
SP=$($CURL -H "$AUTH" -H 'Content-Type: application/json' -X PATCH -d '{"llmBaseUrl":"http://127.0.0.1:8090/v1","llmModel":"deepseek-v4-flash","modelInterface":"OpenAI"}' "$BASE/api/v1/settings")
echo "$SP" | grep -q '"llmBaseUrlConfigured":true' || fail "settings PATCH did not set baseUrl: $SP"
log "OK settings PATCH"

# 5) agent tools read — enabled default
log "agent/tools/read with relative jail file path"
ACODE=$($CURL -H "$AUTH" -H 'Content-Type: application/json' -o /dev/null -w "%{http_code}" -d '{"path":"state/sessions.json"}' "$BASE/api/v1/agent/tools/read")
if [ "$ACODE" = "200" ]; then
  log "OK agent/read=$ACODE"
else
  fail "agent/read unexpected $ACODE"
fi

# 6) bash sandbox default disabled => 403
log "bash sandbox disabled by default => expect 403"
BCODE=$($CURL -H "$AUTH" -H 'Content-Type: application/json' -o /dev/null -w "%{http_code}" -d '{"command":"echo test"}' "$BASE/api/v1/agent/tools/bash")
if [ "$BCODE" = "403" ]; then
  log "OK bash default disabled=403"
else
  fail "bash default expected 403 got $BCODE"
fi

# 7) crawler default disabled => 403
log "crawler disabled by default => expect 403"
CCODE=$($CURL -H "$AUTH" -H 'Content-Type: application/json' -o /dev/null -w "%{http_code}" -d '{"url":"https://example.com"}' "$BASE/api/v1/crawler/fanqie")
if [ "$CCODE" = "403" ]; then
  log "OK crawler default disabled=403"
else
  fail "crawler default expected 403 got $CCODE"
fi

# 8) web tabs contain Agent + Crawler + Bottom nav tools
log "web shell tabs"
HTML=$($CURL "$BASE/web/")
echo "$HTML" | grep -q 'data-tab="agent"' || fail "missing agent tab"
echo "$HTML" | grep -q 'data-tab="crawler"' || fail "missing crawler tab"
echo "$HTML" | grep -q 'data-mnav="tools"' || fail "missing mobile tools"
echo "$HTML" | grep -q 'id="bottom-nav"' || fail "missing bottom-nav"
log "OK web UI"

if [ "$ERR" -ne 0 ]; then
  echo "[FAIL] gate completed with errors"
  exit 1
fi

echo "[PASS] S5-W1 hard gate"
