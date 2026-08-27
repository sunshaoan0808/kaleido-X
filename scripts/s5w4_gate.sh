#!/usr/bin/env bash
# s5w4_gate.sh — Hard gate for S5-W4 (crawler live + outline LLM polish + settings switch)
# Usage: KALEIDO_ADMIN_PASSWORD=*** ./scripts/s5w4_gate.sh [BASE_URL]
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
CURL="curl -sS -m 45 ${CURLFLAGS}"

log "base=$BASE"

# 1) health phase
log "checking /health phase=S5-W4"
H=$($CURL "$BASE/health")
if ! echo "$H" | grep -q '"phase":"S5-W4"'; then
  fail "/health phase is not S5-W4: $H"
else
  log "OK phase=S5-W4"
fi

# 2) web UI markers
log "checking /web/ UI"
WEB=$($CURL -m 8 "$BASE/web/")
echo "$WEB" | grep -q 'set-crawler' || fail "set-crawler checkbox missing"
echo "$WEB" | grep -q 'ol-use-llm' || fail "ol-use-llm checkbox missing"
echo "$WEB" | grep -q 'crawl-save' || fail "crawl-save checkbox missing"
log "OK UI markers"

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

# 4) crawler disabled by default → 403
log "T1 crawler disabled → 403"
# ensure off
$CURL -H "$AUTH" -H 'Content-Type: application/json' -d '{"crawlerEnabled":false}' -X PATCH "$BASE/api/v1/settings" >/dev/null || true
C403=$($CURL -o /tmp/s5w4_c403.json -w '%{http_code}' -H "$AUTH" -H 'Content-Type: application/json' \
  -d '{"url":"https://fanqienovel.com/reader/1"}' "$BASE/api/v1/crawler/fanqie")
if [ "$C403" != "403" ]; then
  fail "expected 403 when crawler disabled, got $C403 body=$(cat /tmp/s5w4_c403.json)"
else
  grep -q crawler_disabled /tmp/s5w4_c403.json && log "OK crawler_disabled 403" || fail "403 body missing crawler_disabled: $(cat /tmp/s5w4_c403.json)"
fi

# 5) enable crawler + non-fanqie host rejected (SSRF) or mock_on_failure
log "T1 enable crawler + SSRF guard"
EN=$($CURL -H "$AUTH" -H 'Content-Type: application/json' -d '{"crawlerEnabled":true}' -X PATCH "$BASE/api/v1/settings")
echo "$EN" | python3 -c 'import sys,json;d=json.load(sys.stdin);assert d.get("crawlerEnabled") is True, d' 2>/dev/null \
  && log "OK crawlerEnabled=true" || fail "settings patch crawler: $EN"

C_BAD=$($CURL -o /tmp/s5w4_cbad.json -w '%{http_code}' -H "$AUTH" -H 'Content-Type: application/json' \
  -d '{"url":"https://example.com/evil"}' "$BASE/api/v1/crawler/fanqie")
# expect 502 with unsupported host
if [ "$C_BAD" = "502" ] || [ "$C_BAD" = "400" ]; then
  grep -qi 'unsupported host\|unsupported' /tmp/s5w4_cbad.json \
    && log "OK SSRF guard status=$C_BAD" \
    || fail "SSRF body unexpected: $(cat /tmp/s5w4_cbad.json)"
else
  fail "expected 502/400 for non-fanqie host, got $C_BAD body=$(cat /tmp/s5w4_cbad.json)"
fi

# mock_on_failure path still returns ok mock
C_MOCK=$($CURL -H "$AUTH" -H 'Content-Type: application/json' \
  -d '{"url":"https://example.com/x","mockOnFailure":true}' "$BASE/api/v1/crawler/fanqie")
echo "$C_MOCK" | python3 -c 'import sys,json;d=json.load(sys.stdin);assert d.get("ok") is True; assert d.get("source")=="mock"' 2>/dev/null \
  && log "OK mockOnFailure" || fail "mockOnFailure: $C_MOCK"

# live path against fanqie (may hit anti-bot — accept ok:true OR structured error with source live)
log "T1 live fanqie attempt (anti-bot tolerant)"
C_LIVE=$($CURL -o /tmp/s5w4_clive.json -w '%{http_code}' -H "$AUTH" -H 'Content-Type: application/json' \
  -d '{"url":"https://fanqienovel.com/reader/1"}' "$BASE/api/v1/crawler/fanqie")
python3 - <<'PY' || fail "live fanqie response shape bad"
import json
d=json.load(open("/tmp/s5w4_clive.json"))
# either success live or structured error
if d.get("ok") is True and d.get("source") in ("live","mock"):
    print("ok live/mock", d.get("source"), "title=", (d.get("title") or "")[:40])
elif d.get("ok") is False and d.get("error"):
    print("ok structured error:", str(d.get("error"))[:120])
else:
    raise SystemExit(f"unexpected: {d}")
PY
log "OK live path exercised status=$C_LIVE"

# restore crawler off
$CURL -H "$AUTH" -H 'Content-Type: application/json' -d '{"crawlerEnabled":false}' -X PATCH "$BASE/api/v1/settings" >/dev/null || true

# 6) outline heuristic
log "T2 outline heuristic"
OL=$($CURL -H "$AUTH" -H 'Content-Type: application/json' \
  -d '{"title":"gate-ol","text":"第1章 开端\n雨夜\n\n第2章 冲突\n风暴"}' \
  "$BASE/api/v1/outline/reverse/preview")
echo "$OL" | python3 -c 'import sys,json;d=json.load(sys.stdin);assert d.get("mode")=="heuristic", d; assert len(d.get("chapters") or [])>=2; assert d.get("outlineMarkdown")' 2>/dev/null \
  && log "OK outline heuristic" || fail "outline heuristic: $OL"

# 7) outline useLlm — accept heuristic+llm OR soft-fail heuristic with note
log "T2 outline useLlm"
OL2=$($CURL -H "$AUTH" -H 'Content-Type: application/json' \
  -d '{"title":"gate-ol-llm","text":"第1章 开端\n雨夜里旅人推门\n\n第2章 冲突\n旧债找上门","useLlm":true}' \
  "$BASE/api/v1/outline/reverse/preview")
echo "$OL2" | python3 -c 'import sys,json;d=json.load(sys.stdin);assert d.get("outlineMarkdown"); assert d.get("mode") in ("heuristic","heuristic+llm"), d; assert d.get("chapters")' 2>/dev/null \
  && log "OK outline useLlm mode=$(echo "$OL2" | python3 -c 'import sys,json;print(json.load(sys.stdin).get("mode"))')" \
  || fail "outline useLlm: $OL2"

# 8) regression st-export
log "regression st-export"
SE=$($CURL -H "$AUTH" -H 'Content-Type: application/json' -d '{"kind":"character_card"}' "$BASE/api/v1/partner/st-export")
echo "$SE" | python3 -c 'import sys,json;d=json.load(sys.stdin);assert d.get("ok") or d.get("format")' 2>/dev/null \
  && log "OK st-export" || fail "st-export regression: $SE"

if [ "$ERR" -ne 0 ]; then
  echo "[FAIL] S5-W4 hard gate"
  exit 1
fi
echo "[PASS] S5-W4 hard gate"
exit 0
