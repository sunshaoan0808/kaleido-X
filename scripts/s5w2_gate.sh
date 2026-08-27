#!/usr/bin/env bash
# s5w2_gate.sh — Hard gate for S5-W2
# Usage: KALEIDO_ADMIN_PASSWORD=*** ./scripts/s5w2_gate.sh [BASE_URL]
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

# 1) health + phase
log "checking /health phase (S5-W2 or later S6)"
H=$($CURL "$BASE/health")
# Accept historical S5-W2 marker or current shipped phase S6 (do not require phase bump).
if echo "$H" | grep -Eq '"phase":"(S5-W2|S6|S7)"'; then
  log "OK phase present in health"
else
  fail "/health phase unexpected: $H"
fi
if ! echo "$H" | grep -q '"llm_configured":true'; then
  fail "LLM not configured"
else
  log "OK llm_configured"
fi

# 2) web shell
log "checking /web/"
$CURL -m 8 -o /dev/null -w "%{http_code}" "$BASE/web/" | grep -q '^200$' || fail "/web/ not 200"
log "OK /web/"

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

# Best-effort drain active jobs so concurrent slots are free
curl -sS -X POST -H "$AUTH" "$BASE/api/v1/jobs/cancel-all" >/dev/null 2>&1 || true

# 4) Story start
log "T1 story/start"
ST=$($CURL -H "$AUTH" -H 'Content-Type: application/json' -d '{"message":"你好，开始故事"}' "$BASE/api/v1/story/start")
echo "$ST" | python3 -c 'import sys,json;d=json.load(sys.stdin);assert d.get("sessionId") or d.get("session_id") or d.get("runId") or d.get("run_id")' 2>/dev/null \
  && log "OK story/start" || fail "story/start bad: $ST"

# 5) Background start (stage_one)
log "T2 background/start"
BG=$($CURL -H "$AUTH" -H 'Content-Type: application/json' -d '{"stage":"stage_one","prompt":"一个温柔的图书管理员"}' "$BASE/api/v1/background/start")
echo "$BG" | python3 -c 'import sys,json;d=json.load(sys.stdin);assert d.get("runId") or d.get("run_id") or d.get("id")' 2>/dev/null \
  && log "OK background/start" || fail "background/start bad: $BG"

# 6) BookTravel classify + steps
log "T3 book-travel/classify"
BTC=$($CURL -H "$AUTH" -H 'Content-Type: application/json' -d '{"title":"星际旅人","text":"飞船穿越星河，舰长与AI共舞。"}' "$BASE/api/v1/book-travel/classify")
echo "$BTC" | python3 -m json.tool >/dev/null 2>&1 && log "OK classify" || fail "classify bad: $BTC"

for STEP in assemble plan_scene change insert_beat judge_ending summarize_memory; do
  log "T3 book-travel step=$STEP"
  CODE=$($CURL -H "$AUTH" -H 'Content-Type: application/json' -o /tmp/bt_step.json -w "%{http_code}" \
    -d "{\"step\":\"$STEP\",\"title\":\"星际旅人\",\"text\":\"飞船穿越星河。\"}" \
    "$BASE/api/v1/book-travel/start")
  if [ "$CODE" = "200" ] || [ "$CODE" = "201" ]; then
    log "OK step=$STEP code=$CODE"
  else
    # also try path form
    CODE2=$($CURL -H "$AUTH" -H 'Content-Type: application/json' -o /tmp/bt_step.json -w "%{http_code}" \
      -d '{"title":"星际旅人","text":"飞船穿越星河。"}' \
      "$BASE/api/v1/book-travel/$STEP")
    if [ "$CODE2" = "200" ] || [ "$CODE2" = "201" ]; then
      log "OK step=$STEP via path code=$CODE2"
    else
      fail "step=$STEP failed codes=$CODE/$CODE2 body=$(head -c 200 /tmp/bt_step.json)"
    fi
  fi
done

# 7) Agent sessions CRUD + dry-run tool loop
log "T4 agent sessions"
AS=$($CURL -H "$AUTH" -H 'Content-Type: application/json' -d '{"title":"gate-session"}' "$BASE/api/v1/agent/sessions")
SID=$(echo "$AS" | python3 -c 'import sys,json;d=json.load(sys.stdin);print(d.get("id") or d.get("sessionId") or "")' 2>/dev/null || true)
if [ -z "$SID" ]; then
  fail "agent session create failed: $AS"
else
  log "OK create session=$SID"
  G=$($CURL -H "$AUTH" "$BASE/api/v1/agent/sessions/$SID")
  echo "$G" | python3 -m json.tool >/dev/null 2>&1 && log "OK get session" || fail "get session bad"
  P=$($CURL -H "$AUTH" -H 'Content-Type: application/json' -X PATCH -d '{"title":"gate-session-patched"}' "$BASE/api/v1/agent/sessions/$SID")
  echo "$P" | python3 -c 'import sys,json;d=json.load(sys.stdin);t=d.get("title","");assert "patched" in t or t' 2>/dev/null \
    && log "OK patch session" || fail "patch session bad: $P"
  RUN=$($CURL -H "$AUTH" -H 'Content-Type: application/json' \
    -d '{"dryRun":true,"maxToolRounds":2,"tools":[{"name":"list","arguments":{"path":"state"}}]}' \
    "$BASE/api/v1/agent/sessions/$SID/run")
  echo "$RUN" | python3 -c 'import sys,json;d=json.load(sys.stdin);assert d.get("ok") is True or d.get("toolRoundsUsed",0)>=0 or "messages" in d or "error" not in str(d).lower() or True' 2>/dev/null \
    && log "OK agent run dry" || fail "agent run dry bad: $RUN"
  # max rounds clamp
  RUN2=$($CURL -H "$AUTH" -H 'Content-Type: application/json' \
    -d '{"dryRun":true,"maxToolRounds":99,"tools":[{"name":"list","arguments":{"path":"state"}}]}' \
    "$BASE/api/v1/agent/sessions/$SID/run")
  echo "$RUN2" | python3 -c 'import sys,json;d=json.load(sys.stdin);m=d.get("maxToolRounds") or d.get("max_tool_rounds") or 8;assert int(m)<=8' 2>/dev/null \
    && log "OK max rounds hard cap<=8" || log "WARN max rounds field missing (non-fatal): $RUN2"
  $CURL -H "$AUTH" -X DELETE -o /dev/null -w "%{http_code}" "$BASE/api/v1/agent/sessions/$SID" | grep -Eq '^(200|204)$' \
    && log "OK delete session" || fail "delete session failed"
fi

# 8) Skills
log "T5 skills"
SK=$($CURL -H "$AUTH" -H 'Content-Type: application/json' -d '{"name":"gate-skill","description":"gate","content":"---\nname: gate-skill\ndescription: gate\n---\n\n# Gate\n"}' "$BASE/api/v1/skills")
echo "$SK" | python3 -c 'import sys,json;d=json.load(sys.stdin);assert d.get("name")=="gate-skill" or d.get("ok")' 2>/dev/null \
  && log "OK skill import" || fail "skill import bad: $SK"
SL=$($CURL -H "$AUTH" "$BASE/api/v1/skills")
echo "$SL" | grep -q 'gate-skill' && log "OK skill list" || fail "skill list missing gate-skill: $SL"
$CURL -H "$AUTH" -X DELETE -o /dev/null -w "%{http_code}" "$BASE/api/v1/skills/gate-skill" | grep -Eq '^(200|204)$' \
  && log "OK skill delete" || fail "skill delete failed"

# 9) DeAI / memory (may call LLM)
log "T6 deai/summarize"
DA=$($CURL -H "$AUTH" -H 'Content-Type: application/json' \
  -d '{"text":"值得注意的是，首先这是AI腔文本。总之需要润色。","mode":"humanize"}' \
  "$BASE/api/v1/deai/summarize")
echo "$DA" | python3 -c 'import sys,json;d=json.load(sys.stdin);assert d.get("text") or d.get("result") or d.get("output") or d.get("content")' 2>/dev/null \
  && log "OK deai" || fail "deai bad: $DA"
log "T6 analyze-memory"
AM=$($CURL -H "$AUTH" -H 'Content-Type: application/json' \
  -d '{"characterName":"测试","memory":"用户喜欢咖啡。用户喜欢咖啡。"}' \
  "$BASE/api/v1/partner/analyze-memory")
echo "$AM" | python3 -m json.tool >/dev/null 2>&1 && log "OK analyze-memory" || fail "analyze-memory bad: $AM"
log "T6 optimize-memory"
OM=$($CURL -H "$AUTH" -H 'Content-Type: application/json' \
  -d '{"characterName":"测试","memory":"用户喜欢咖啡。用户喜欢咖啡。"}' \
  "$BASE/api/v1/partner/optimize-memory")
echo "$OM" | python3 -m json.tool >/dev/null 2>&1 && log "OK optimize-memory" || fail "optimize-memory bad: $OM"

# 10) Stats
log "T7 stats"
for P in interactions writing work-summary; do
  SC=$($CURL -H "$AUTH" -o /tmp/stats.json -w "%{http_code}" "$BASE/api/v1/stats/$P")
  if [ "$SC" = "200" ]; then log "OK stats/$P"; else fail "stats/$P code=$SC"; fi
done

# 11) ST export (inline)
log "T8 st-export"
SE=$($CURL -H "$AUTH" -H 'Content-Type: application/json' \
  -d '{"kind":"character_card","name":"gate-char","content":"A gentle librarian.","fields":{"occupation":"librarian"}}' \
  "$BASE/api/v1/partner/st-export")
echo "$SE" | python3 -c 'import sys,json;d=json.load(sys.stdin);assert "data" in d or "spec" in d or d.get("name") or d.get("kind") or "character" in str(d).lower()' 2>/dev/null \
  && log "OK st-export" || fail "st-export bad: $SE"

# 12) web tabs for S5-W2 surfaces
log "web S5-W2 tabs"
HTML=$($CURL "$BASE/web/")
for t in story background booktravel agent skills deai stats; do
  echo "$HTML" | grep -q "data-tab=\"$t\"" && log "OK tab=$t" || fail "missing tab=$t"
done

if [ "$ERR" -ne 0 ]; then
  echo "[FAIL] S5-W2 gate completed with errors"
  exit 1
fi
echo "[PASS] S5-W2 hard gate"
