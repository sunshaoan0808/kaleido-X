#!/usr/bin/env bash
# Kaleido S4 gate: works FS CRUD + jail + auth + health phase
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

if [[ -f .env ]]; then
  set -a
  # shellcheck disable=SC1091
  source .env
  set +a
fi

# Client target must be loopback; env may set KALEIDO_HOST=0.0.0.0 for bind.
HOST="${KALEIDO_GATE_HOST:-127.0.0.1}"
PORT="${KALEIDO_PORT:-18766}"
BASE="http://${HOST}:${PORT}"
USER="${KALEIDO_ADMIN_USER:-admin}"
PASS="${KALEIDO_ADMIN_PASSWORD:-}"

if [[ -z "$PASS" ]]; then
  echo "FAIL: KALEIDO_ADMIN_PASSWORD unset" >&2
  exit 2
fi

pass() { echo "PASS: $*"; }
fail() { echo "FAIL: $*" >&2; exit 1; }

json_field() {
  python3 -c 'import json,sys; d=json.loads(sys.argv[1]); k=sys.argv[2];
v=d.get(k,"");
print(v if not isinstance(v,(dict,list)) else json.dumps(v))' "$1" "$2"
}

urlenc() {
  python3 -c 'import urllib.parse,sys; print(urllib.parse.quote(sys.argv[1]))' "$1"
}

echo "== S4 gate against $BASE =="

# 1) health phase
H=$(curl -fsS "$BASE/health")
PHASE=$(json_field "$H" phase)
# phase must be S4 or later (S5..S9+); fresh bootstraps report the latest phase
[[ "$PHASE" =~ ^S[0-9]+$ && "$PHASE" > "S3" ]] || fail "health phase=$PHASE expected >=S4"
pass "health phase=$PHASE"

INFO=$(curl -fsS "$BASE/api/v1/public/info")
python3 -c 'import json,sys
info=json.loads(sys.argv[1])
feats=info.get("features") if isinstance(info.get("features"), dict) else {}
ok = (isinstance(feats, dict) and feats.get("works_fs") is True) or info.get("works_fs") is True
sys.exit(0 if ok else 1)' "$INFO" || fail "features.works_fs not true (body=$INFO)"
pass "public info works_fs=true"

# 2) unauth -> 401
CODE=$(curl -s -o /tmp/s4_unauth.json -w '%{http_code}' "$BASE/api/v1/works")
[[ "$CODE" == "401" ]] || fail "unauth works got HTTP $CODE"
pass "unauth works -> 401"

# 3) login
LOGIN=$(curl -fsS -X POST "$BASE/api/v1/auth/login" \
  -H 'Content-Type: application/json' \
  -d "{\"username\":\"$USER\",\"password\":\"$PASS\"}")
TOKEN=$(json_field "$LOGIN" token)
[[ -n "$TOKEN" && "$TOKEN" != "None" ]] || fail "login missing token"
AUTH="Authorization: Bearer $TOKEN"
pass "login"

STAMP=$(date +%s)
DIR="s4-gate-${STAMP}"
FILE="${DIR}/hello.md"
FILE2="${DIR}/renamed.md"

# 4) mkdir
curl -fsS -X POST "$BASE/api/v1/works/dir" -H "$AUTH" -H 'Content-Type: application/json' \
  -d "{\"path\":\"$DIR\"}" >/tmp/s4_mkdir.json
pass "mkdir $DIR"

# 5) write
BODY=$(python3 -c 'import json,sys; print(json.dumps({"path":sys.argv[1],"content":sys.argv[2]}))' \
  "$FILE" "# S4 gate ${STAMP}
hello works
")
curl -fsS -X PUT "$BASE/api/v1/works/file" -H "$AUTH" -H 'Content-Type: application/json' \
  -d "$BODY" >/tmp/s4_write.json
pass "write $FILE"

# 6) read
READ=$(curl -fsS "$BASE/api/v1/works/file?path=$(urlenc "$FILE")" -H "$AUTH")
echo "$READ" | grep -q "S4 gate ${STAMP}" || fail "read content mismatch: $READ"
pass "read $FILE"

# 7) list
LIST=$(curl -fsS "$BASE/api/v1/works?path=$(urlenc "$DIR")&depth=1" -H "$AUTH")
echo "$LIST" | grep -q 'hello.md' || fail "list missing hello.md: $LIST"
pass "list $DIR"

# 8) stat
STAT=$(curl -fsS "$BASE/api/v1/works/stat?path=$(urlenc "$FILE")" -H "$AUTH")
echo "$STAT" | grep -Eq '"kind"[[:space:]]*:[[:space:]]*"file"' || fail "stat not file: $STAT"
pass "stat $FILE"

# 9) rename
curl -fsS -X POST "$BASE/api/v1/works/rename" -H "$AUTH" -H 'Content-Type: application/json' \
  -d "{\"from\":\"$FILE\",\"to\":\"$FILE2\"}" >/tmp/s4_rename.json
curl -fsS "$BASE/api/v1/works/file?path=$(urlenc "$FILE2")" -H "$AUTH" >/tmp/s4_read2.json
pass "rename -> $FILE2"

# 10) traversal rejected
TCODE=$(curl -s -o /tmp/s4_trav.json -w '%{http_code}' \
  "$BASE/api/v1/works/file?path=..%2F..%2Fetc%2Fpasswd" -H "$AUTH")
[[ "$TCODE" == "400" || "$TCODE" == "403" ]] || fail "traversal got HTTP $TCODE body=$(cat /tmp/s4_trav.json)"
pass "traversal ../../etc/passwd -> $TCODE"

TCODE2=$(curl -s -o /tmp/s4_trav2.json -w '%{http_code}' \
  "$BASE/api/v1/works/file?path=%2Fetc%2Fpasswd" -H "$AUTH")
[[ "$TCODE2" == "400" || "$TCODE2" == "403" ]] || fail "absolute got HTTP $TCODE2"
pass "absolute /etc/passwd -> $TCODE2"

# 11) delete recursive
curl -fsS -X DELETE "$BASE/api/v1/works?path=$(urlenc "$DIR")&recursive=true" -H "$AUTH" >/tmp/s4_del.json
DCODE=$(curl -s -o /tmp/s4_gone.json -w '%{http_code}' \
  "$BASE/api/v1/works/stat?path=$(urlenc "$DIR")" -H "$AUTH")
[[ "$DCODE" == "404" ]] || fail "delete incomplete stat HTTP $DCODE"
pass "delete recursive $DIR"

# 12) web shell mentions works tab
# NOTE: do NOT pipe WEB into grep -q under pipefail — the page is >100KB now,
# grep -q exits early and the writer gets SIGPIPE → pipeline rc=141 → false fail.
WEB=$(curl -fsS "$BASE/web/")
[[ "$WEB" == *'作品'* ]] || fail "web shell missing works tab"
[[ "$WEB" =~ S[0-9] ]] || fail "web shell missing phase badge"
pass "web /web/ works tab + phase badge"

echo "ALL S4 GATES PASSED"
