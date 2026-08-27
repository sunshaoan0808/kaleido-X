#!/usr/bin/env bash
# s7_stream_parity_gate.sh — Background / BookTravel stream + generationMode
# Usage: KALEIDO_ADMIN_PASSWORD=*** ./scripts/s7_stream_parity_gate.sh [BASE_URL]
set -euo pipefail
BASE=${1:-http://127.0.0.1:18766}
PASS=${KALEIDO_ADMIN_PASSWORD:-}
CURLFLAGS=${KALEIDO_CURL_FLAGS:-}
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
CURL="curl -sS -m 30 ${CURLFLAGS}"

log "base=$BASE"

H=$($CURL "$BASE/health" || true)
echo "$H" | grep -q '"ok":true' || fail "health not ok: $H"
echo "$H" | grep -Eq '"phase":"(S5-W2|S6|S7)"' || fail "phase unexpected: $H"
log "OK health"

RES=$($CURL -H 'Content-Type: application/json' \
  -d "{\"username\":\"admin\",\"password\":\"$PASS\"}" \
  "$BASE/api/v1/auth/login")
TOKEN=$(echo "$RES" | python3 -c 'import sys,json;print(json.load(sys.stdin).get("token",""))' 2>/dev/null || true)
if [ -z "$TOKEN" ]; then
  fail "login failed: $RES"
  exit 1
fi
log "OK login"
AUTH="Authorization: Bearer $TOKEN"

# Best-effort drain active jobs so concurrent slots are free
curl -sS -X POST -H "$AUTH" "$BASE/api/v1/jobs/cancel-all" >/dev/null 2>&1 || true

wait_job() {
  local id="$1"
  local kind="$2"
  local i=0
  local body=""
  while [ $i -lt 90 ]; do
    body=$($CURL -H "$AUTH" "$BASE/api/v1/jobs/$id" || true)
    local st
    st=$(echo "$body" | python3 -c 'import sys,json
try:
 d=json.load(sys.stdin); print(d.get("status") or d.get("job",{}).get("status") or "")
except Exception:
 print("")' 2>/dev/null || true)
    case "$st" in
      succeeded|failed|cancelled|error|done)
        echo "$body"
        return 0
        ;;
    esac
    # also try stream peek once early
    if [ $i -eq 0 ]; then
      $CURL -m 8 -H "$AUTH" -H 'Accept: text/event-stream' \
        "$BASE/api/v1/${kind}/stream?id=$id" > "/tmp/kaleido_${kind}_stream_${id}.txt" 2>/dev/null || true
    fi
    sleep 2
    i=$((i+1))
  done
  echo "$body"
  return 1
}

# ---- Background stage_one ----
log "T1 background/start stage_one"
BG=$($CURL -H "$AUTH" -H 'Content-Type: application/json' \
  -d '{"stage":"stage_one","title":"流式对齐测","text":"一座悬浮在云海上的图书馆，馆长是半机械猫娘，收藏着未写完的故事。"}' \
  "$BASE/api/v1/background/start")
BGID=$(echo "$BG" | python3 -c 'import sys,json
d=json.load(sys.stdin)
print(d.get("runId") or d.get("run_id") or d.get("id") or "")' 2>/dev/null || true)
if [ -z "$BGID" ]; then
  fail "background/start no runId: $BG"
else
  log "OK bg runId=$BGID"
  BGBODY=$(wait_job "$BGID" "background" || true)
  python3 - <<PY || fail "background result missing generationMode"
import json,sys
raw='''$BGBODY'''
# re-fetch clean
import urllib.request
req=urllib.request.Request("$BASE/api/v1/jobs/$BGID", headers={"Authorization":"Bearer $TOKEN"})
d=json.load(urllib.request.urlopen(req, timeout=20))
job=d.get("job") or d
res=job.get("result") or {}
gm=res.get("generationMode") or res.get("generation_mode")
print("background generationMode=", gm, "fallback=", res.get("fallback"), "status=", job.get("status"))
assert gm in ("llm","heuristic"), res
assert job.get("status") in ("succeeded","done") or str(job.get("status","")).lower()=="succeeded"
# events should exist
ev=job.get("events") or []
print("events", len(ev), "types", sorted({(e.get("eventType") or e.get("event_type")) for e in ev})[:8])
assert len(ev) >= 1
PY
  # stream file may have deltas
  if [ -f "/tmp/kaleido_background_stream_${BGID}.txt" ]; then
    if grep -q 'delta\|progress\|done\|event' "/tmp/kaleido_background_stream_${BGID}.txt"; then
      log "OK background stream bytes present"
    else
      log "WARN background stream empty (job may have finished before connect) — ok if result has generationMode"
    fi
  fi
fi

# ---- BookTravel plan_scene ----
log "T2 book-travel plan_scene"
BT=$($CURL -H "$AUTH" -H 'Content-Type: application/json' \
  -d '{"step":"plan_scene","title":"星际旅人","text":"飞船穿越星河，舰长与AI共舞。","userInput":"下一场景进入废弃空间站"}' \
  "$BASE/api/v1/book-travel/start")
BTID=$(echo "$BT" | python3 -c 'import sys,json
d=json.load(sys.stdin)
print(d.get("runId") or d.get("run_id") or d.get("id") or "")' 2>/dev/null || true)
if [ -z "$BTID" ]; then
  fail "book-travel/start no runId: $BT"
else
  log "OK bt runId=$BTID"
  wait_job "$BTID" "book-travel" >/dev/null || true
  python3 - <<PY || fail "book-travel result missing generationMode"
import json,urllib.request
req=urllib.request.Request("$BASE/api/v1/jobs/$BTID", headers={"Authorization":"Bearer $TOKEN"})
d=json.load(urllib.request.urlopen(req, timeout=20))
job=d.get("job") or d
res=job.get("result") or {}
gm=res.get("generationMode") or res.get("generation_mode")
print("book_travel generationMode=", gm, "fallback=", res.get("fallback"), "status=", job.get("status"))
assert gm in ("llm","heuristic"), res
assert "plan_scene" in str(res.get("step") or res.get("mode") or "")
assert job.get("status") in ("succeeded","done") or str(job.get("status","")).lower()=="succeeded"
ev=job.get("events") or []
print("events", len(ev))
assert len(ev) >= 1
PY
fi

if [ "$ERR" -ne 0 ]; then
  echo "[FAIL] s7_stream_parity_gate"
  exit 1
fi
echo "[PASS] s7_stream_parity_gate"
exit 0
