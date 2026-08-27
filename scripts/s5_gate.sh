#!/usr/bin/env bash
# Kaleido S5 gate: sectional S5 / S5-W0 surface checks
# Soft dry-run vs live S4: KALEIDO_GATE_SOFT=1 bash scripts/s5_gate.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
if [[ -f "$SCRIPT_DIR/../.env" || -f "$SCRIPT_DIR/../Cargo.toml" ]]; then
  ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
elif [[ -f "$SCRIPT_DIR/../source/kaleido-server/.env" || -f "$SCRIPT_DIR/../source/kaleido-server/Cargo.toml" ]]; then
  ROOT="$(cd "$SCRIPT_DIR/../source/kaleido-server" && pwd)"
elif [[ -n "${KALEIDO_SERVER_ROOT:-}" ]]; then
  ROOT="$KALEIDO_SERVER_ROOT"
else
  ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
fi
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
SOFT="${KALEIDO_GATE_SOFT:-0}"
ALLOW_SKIP="${KALEIDO_GATE_ALLOW_SKIP:-0}"

TMPDIR_GATE="${TMPDIR:-/tmp}/kaleido-s5-gate-$$"
mkdir -p "$TMPDIR_GATE"
cleanup() { rm -rf "$TMPDIR_GATE"; }
trap cleanup EXIT

declare -a SECTION_NAMES=()
declare -a SECTION_STATUSES=()
SECTION_FAIL=0
SECTION_SKIP=0
SECTION_PASS=0
AUTH=""
TOKEN=""
INFO_JSON=""
PHASE_SEEN=""
FEATURES_JOBS_V2="0"

record() {
  local status="$1"
  local name="$2"
  SECTION_NAMES+=("$name")
  SECTION_STATUSES+=("$status")
  case "$status" in
    PASS) SECTION_PASS=$((SECTION_PASS + 1)) ;;
    FAIL) SECTION_FAIL=$((SECTION_FAIL + 1)) ;;
    SKIP) SECTION_SKIP=$((SECTION_SKIP + 1)) ;;
  esac
}

pass() {
  echo "PASS: $*"
  record PASS "$1"
}

fail_section() {
  # $1 = short name, rest = detail
  local name="$1"
  shift || true
  echo "FAIL: $name${*:+ — $*}" >&2
  record FAIL "$name"
}

skip_section() {
  local name="$1"
  shift || true
  echo "SKIP: $name${*:+ — $*}"
  record SKIP "$name"
}

json_field() {
  python3 -c 'import json,sys
d=json.loads(sys.argv[1]); k=sys.argv[2]
v=d.get(k,"")
print(v if not isinstance(v,(dict,list)) else json.dumps(v))' "$1" "$2"
}

urlenc() {
  python3 -c 'import urllib.parse,sys; print(urllib.parse.quote(sys.argv[1]))' "$1"
}

phase_is_s5() {
  local p="$1"
  [[ "$p" == "S5" || "$p" == "S5-W0" || "$p" == S5* ]]
}

http_code() {
  # usage: http_code METHOD URL [curl args...]
  # writes body to $TMPDIR_GATE/body, prints code
  local method="$1"
  local url="$2"
  shift 2
  curl -sS -o "$TMPDIR_GATE/body" -w '%{http_code}' -X "$method" "$url" "$@" || echo "000"
}

route_exists_status() {
  # Accept 200/201/400/401/422/405 as route exists; 404 = missing
  # 201: create-style starts (e.g. background/start returns Created + job body)
  local code="$1"
  case "$code" in
    200|201|400|401|422|405) return 0 ;;
    *) return 1 ;;
  esac
}

echo "== S5 gate against $BASE (soft=$SOFT allow_skip=$ALLOW_SKIP) =="

if [[ -z "$PASS" ]]; then
  echo "FAIL: KALEIDO_ADMIN_PASSWORD unset" >&2
  exit 2
fi

# ---------- 1) health ----------
HCODE=$(curl -sS -o "$TMPDIR_GATE/health.json" -w '%{http_code}' "$BASE/health" || echo "000")
if [[ "$HCODE" != "200" ]]; then
  # try /api/v1/health fallback
  HCODE=$(curl -sS -o "$TMPDIR_GATE/health.json" -w '%{http_code}' "$BASE/api/v1/health" || echo "000")
fi
if [[ "$HCODE" != "200" ]]; then
  fail_section "health" "HTTP $HCODE (need /health or /api/v1/health)"
  PHASE_SEEN="(unreachable)"
else
  H=$(cat "$TMPDIR_GATE/health.json")
  PHASE_SEEN=$(json_field "$H" phase)
  if phase_is_s5 "$PHASE_SEEN"; then
    pass "health" 
    echo "  detail: phase=$PHASE_SEEN"
  else
    fail_section "health" "phase=$PHASE_SEEN expected S5 / S5-W0 / S5*"
  fi
fi

# ---------- 2) public info ----------
ICODE=$(curl -sS -o "$TMPDIR_GATE/info.json" -w '%{http_code}' "$BASE/api/v1/public/info" || echo "000")
if [[ "$ICODE" != "200" ]]; then
  fail_section "public_info" "HTTP $ICODE"
else
  INFO_JSON=$(cat "$TMPDIR_GATE/info.json")
  if python3 -c 'import json,sys
info=json.loads(sys.argv[1])
feats=info.get("features") if isinstance(info.get("features"), dict) else {}
ok = (isinstance(feats, dict) and feats.get("works_fs") is True) or info.get("works_fs") is True
sys.exit(0 if ok else 1)' "$INFO_JSON"; then
    pass "public_info"
    echo "  detail: works_fs=true"
  else
    fail_section "public_info" "features.works_fs not true"
  fi
  # detect jobs_v2 for later section
  if python3 -c 'import json,sys
info=json.loads(sys.argv[1])
feats=info.get("features") if isinstance(info.get("features"), dict) else {}
ok = (isinstance(feats, dict) and (feats.get("jobs_v2") is True or feats.get("jobs") is True and "jobs_v2" in feats and feats.get("jobs_v2")))
# also accept top-level or endpoints.jobs present as weak signal — prefer explicit jobs_v2
sys.exit(0 if (isinstance(feats, dict) and feats.get("jobs_v2") is True) else 1)' "$INFO_JSON" 2>/dev/null; then
    FEATURES_JOBS_V2="1"
  fi
fi

# ---------- 3) auth ----------
AUTH_OK=0
UCODE=$(curl -sS -o "$TMPDIR_GATE/unauth.json" -w '%{http_code}' "$BASE/api/v1/works" || echo "000")
if [[ "$UCODE" != "401" ]]; then
  fail_section "auth" "unauth works got HTTP $UCODE expected 401"
else
  LCODE=$(curl -sS -o "$TMPDIR_GATE/login.json" -w '%{http_code}' -X POST "$BASE/api/v1/auth/login" \
    -H 'Content-Type: application/json' \
    -d "{\"username\":\"$USER\",\"password\":\"$PASS\"}" || echo "000")
  if [[ "$LCODE" != "200" ]]; then
    fail_section "auth" "login HTTP $LCODE"
  else
    TOKEN=$(json_field "$(cat "$TMPDIR_GATE/login.json")" token)
    if [[ -z "$TOKEN" || "$TOKEN" == "None" || "$TOKEN" == "null" ]]; then
      fail_section "auth" "login missing token"
    else
      AUTH="Authorization: Bearer $TOKEN"
      AUTH_OK=1
      pass "auth"
      echo "  detail: unauth=401 login=bearer"
    fi
  fi
fi

# ---------- 4) jobs_v2 ----------
if [[ "$FEATURES_JOBS_V2" != "1" ]]; then
  # S4 live binary has jobs but not jobs_v2 flag — SKIP or FAIL per policy
  msg="features.jobs_v2 absent (likely pre-S5 / S4 binary phase=${PHASE_SEEN})"
  if [[ "$SOFT" == "1" || "$ALLOW_SKIP" == "1" ]]; then
    skip_section "jobs_v2" "$msg"
  else
    fail_section "jobs_v2" "$msg"
  fi
elif [[ "$AUTH_OK" != "1" ]]; then
  fail_section "jobs_v2" "no bearer token (auth failed)"
else
  JCODE=$(curl -sS -o "$TMPDIR_GATE/jobs.json" -w '%{http_code}' \
    "$BASE/api/v1/jobs" -H "$AUTH" || echo "000")
  if [[ "$JCODE" != "200" ]]; then
    fail_section "jobs_v2" "GET /api/v1/jobs HTTP $JCODE"
  else
    if python3 -c 'import json,sys
d=json.loads(open(sys.argv[1]).read())
# accept list wrapper or array
ok = isinstance(d, dict) and ("jobs" in d or "items" in d or "count" in d)
sys.exit(0 if ok else 1)' "$TMPDIR_GATE/jobs.json"; then
      pass "jobs_v2"
      echo "  detail: GET /api/v1/jobs 200 JSON list ok"
    else
      fail_section "jobs_v2" "response not list-shaped JSON"
    fi
  fi
fi

# ---------- 5) background route smoke ----------
BCODE=$(http_code POST "$BASE/api/v1/background/start" \
  -H 'Content-Type: application/json' \
  ${AUTH:+-H "$AUTH"} \
  -d '{}')
if route_exists_status "$BCODE"; then
  pass "background"
  echo "  detail: POST /api/v1/background/start -> $BCODE (route exists)"
elif [[ "$BCODE" == "404" ]]; then
  if [[ "$SOFT" == "1" ]] && ! phase_is_s5 "$PHASE_SEEN"; then
    skip_section "background" "HTTP 404 on S4 binary (soft)"
  else
    fail_section "background" "POST /api/v1/background/start -> 404"
  fi
else
  if [[ "$SOFT" == "1" ]] && ! phase_is_s5 "$PHASE_SEEN"; then
    skip_section "background" "HTTP $BCODE on non-S5 (soft)"
  else
    fail_section "background" "POST /api/v1/background/start -> HTTP $BCODE"
  fi
fi

# ---------- 6) book-travel classify ----------
CCODE=$(http_code POST "$BASE/api/v1/book-travel/classify" \
  -H 'Content-Type: application/json' \
  ${AUTH:+-H "$AUTH"} \
  -d '{}')
if route_exists_status "$CCODE"; then
  pass "book-travel"
  echo "  detail: POST /api/v1/book-travel/classify -> $CCODE (route exists)"
elif [[ "$CCODE" == "404" ]]; then
  if [[ "$SOFT" == "1" ]] && ! phase_is_s5 "$PHASE_SEEN"; then
    skip_section "book-travel" "HTTP 404 on S4 binary (soft)"
  else
    fail_section "book-travel" "POST /api/v1/book-travel/classify -> 404"
  fi
else
  if [[ "$SOFT" == "1" ]] && ! phase_is_s5 "$PHASE_SEEN"; then
    skip_section "book-travel" "HTTP $CCODE on non-S5 (soft)"
  else
    fail_section "book-travel" "POST /api/v1/book-travel/classify -> HTTP $CCODE"
  fi
fi

# ---------- 7) outline reverse preview ----------
OCODE=$(http_code POST "$BASE/api/v1/outline/reverse/preview" \
  -H 'Content-Type: application/json' \
  ${AUTH:+-H "$AUTH"} \
  -d '{}')
if route_exists_status "$OCODE"; then
  pass "outline"
  echo "  detail: POST /api/v1/outline/reverse/preview -> $OCODE (route exists)"
elif [[ "$OCODE" == "404" ]]; then
  if [[ "$SOFT" == "1" ]] && ! phase_is_s5 "$PHASE_SEEN"; then
    skip_section "outline" "HTTP 404 on S4 binary (soft)"
  else
    fail_section "outline" "POST /api/v1/outline/reverse/preview -> 404"
  fi
else
  if [[ "$SOFT" == "1" ]] && ! phase_is_s5 "$PHASE_SEEN"; then
    skip_section "outline" "HTTP $OCODE on non-S5 (soft)"
  else
    fail_section "outline" "POST /api/v1/outline/reverse/preview -> HTTP $OCODE"
  fi
fi

# ---------- 8) st-import ----------
SCODE=$(http_code POST "$BASE/api/v1/partner/st-import" \
  -H 'Content-Type: application/json' \
  ${AUTH:+-H "$AUTH"} \
  -d '{}')
if route_exists_status "$SCODE"; then
  pass "st-import"
  echo "  detail: POST /api/v1/partner/st-import -> $SCODE (route exists)"
elif [[ "$SCODE" == "404" ]]; then
  if [[ "$SOFT" == "1" ]] && ! phase_is_s5 "$PHASE_SEEN"; then
    skip_section "st-import" "HTTP 404 on S4 binary (soft)"
  else
    fail_section "st-import" "POST /api/v1/partner/st-import -> 404"
  fi
else
  if [[ "$SOFT" == "1" ]] && ! phase_is_s5 "$PHASE_SEEN"; then
    skip_section "st-import" "HTTP $SCODE on non-S5 (soft)"
  else
    fail_section "st-import" "POST /api/v1/partner/st-import -> HTTP $SCODE"
  fi
fi

# ---------- 9) works FS minimal CRUD ----------
if [[ "$AUTH_OK" != "1" ]]; then
  if [[ "$SOFT" == "1" ]]; then
    skip_section "works_fs" "no auth (soft)"
  else
    fail_section "works_fs" "no bearer token"
  fi
else
  STAMP=$(date +%s)
  DIR="s5-gate-${STAMP}"
  FILE="${DIR}/hello.md"
  WORKS_OK=1
  if ! curl -fsS -X POST "$BASE/api/v1/works/dir" -H "$AUTH" -H 'Content-Type: application/json' \
      -d "{\"path\":\"$DIR\"}" >"$TMPDIR_GATE/mkdir.json" 2>"$TMPDIR_GATE/mkdir.err"; then
    WORKS_OK=0
    fail_section "works_fs" "mkdir failed"
  else
    BODY=$(python3 -c 'import json,sys; print(json.dumps({"path":sys.argv[1],"content":sys.argv[2]}))' \
      "$FILE" "# S5 gate ${STAMP}
hello works
")
    if ! curl -fsS -X PUT "$BASE/api/v1/works/file" -H "$AUTH" -H 'Content-Type: application/json' \
        -d "$BODY" >"$TMPDIR_GATE/write.json" 2>"$TMPDIR_GATE/write.err"; then
      WORKS_OK=0
      fail_section "works_fs" "write failed"
    else
      READ=$(curl -fsS "$BASE/api/v1/works/file?path=$(urlenc "$FILE")" -H "$AUTH" || true)
      if ! echo "$READ" | grep -q "S5 gate ${STAMP}"; then
        WORKS_OK=0
        fail_section "works_fs" "read content mismatch"
      else
        # cleanup best-effort
        curl -fsS -X DELETE "$BASE/api/v1/works?path=$(urlenc "$DIR")&recursive=true" -H "$AUTH" \
          >"$TMPDIR_GATE/del.json" 2>/dev/null || true
        pass "works_fs"
        echo "  detail: mkdir/write/read/delete ok"
      fi
    fi
  fi
  # if mkdir/write failed we already recorded FAIL; ensure dir cleanup attempt
  if [[ "$WORKS_OK" != "1" ]]; then
    curl -sS -X DELETE "$BASE/api/v1/works?path=$(urlenc "$DIR")&recursive=true" -H "$AUTH" >/dev/null 2>&1 || true
  fi
fi

# ---------- summary ----------
echo
echo "== S5 gate summary =="
printf '%-14s %s\n' "SECTION" "STATUS"
printf '%-14s %s\n' "-------" "------"
for i in "${!SECTION_NAMES[@]}"; do
  printf '%-14s %s\n' "${SECTION_NAMES[$i]}" "${SECTION_STATUSES[$i]}"
done
echo
echo "totals: PASS=$SECTION_PASS FAIL=$SECTION_FAIL SKIP=$SECTION_SKIP phase_seen=$PHASE_SEEN soft=$SOFT"

if [[ "$SECTION_FAIL" -gt 0 ]]; then
  if [[ "$SOFT" == "1" ]]; then
    echo "SOFT MODE: sectional failures present; exiting 0"
    exit 0
  fi
  echo "HARD MODE: $SECTION_FAIL section(s) failed; exiting 1"
  exit 1
fi

echo "ALL REQUIRED S5 SECTIONS PASSED (or skipped by policy)"
exit 0
