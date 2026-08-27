#!/usr/bin/env bash
# s8_gate.sh — hard gate for S8 (phase + embed ready + stress + author/embedlab UI markers)
set -euo pipefail
BASE=${1:-http://127.0.0.1:18766}
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ERR=0
log() { echo "[$(date -u +%H:%M:%S)] $*"; }
fail() { echo "[FAIL] $*"; ERR=$((ERR+1)); }

export no_proxy="localhost,127.0.0.1,.local,kaleido.example.com"
export NO_PROXY="$no_proxy"
unset http_proxy https_proxy HTTP_PROXY HTTPS_PROXY ALL_PROXY all_proxy 2>/dev/null || true

CURL=(curl -sS -m 30)
if [ -f "$ROOT/.env" ]; then
  # shellcheck disable=SC1091
  set -a; source "$ROOT/.env"; set +a
fi

log "base=$BASE"
H=$("${CURL[@]}" "$BASE/health" || true)
echo "$H" | python3 -c 'import sys,json;d=json.load(sys.stdin);assert d.get("ok") is True; assert d.get("phase")=="S8", d' \
  && log "OK health phase=S8" || fail "health phase not S8: $H"

# Inline / remote embed readiness (ST-21/22 + embed_local)
echo "$H" | python3 -c '
import sys,json
d=json.load(sys.stdin)
e=d.get("embedding") or {}
assert e.get("enabled") is True, e
assert e.get("ready") is True, e
assert e.get("backend") in ("fastembed","remote"), e
dim=e.get("dim")
assert dim in (512, None) or dim==512, e
print("embedding", {k:e.get(k) for k in ("ready","backend","dim","model")})
' && log "OK embedding ready" || fail "embedding not ready: $H"

TMP_WEB=$(mktemp)
trap 'rm -f "$TMP_WEB"' EXIT
"${CURL[@]}" "$BASE/web/" >"$TMP_WEB" || true
grep -q 'az-publish-btn' "$TMP_WEB" && log "OK web az-publish" || fail "web az-publish-btn"
grep -q 'az-inject-btn' "$TMP_WEB" && log "OK web az-inject" || fail "web az-inject-btn"
grep -q 'az-live-enabled' "$TMP_WEB" && log "OK web az-live" || fail "web az-live-enabled"
grep -q 'data-tab="embedlab"' "$TMP_WEB" && log "OK web embedlab tab" || fail "web embedlab tab"
grep -q 'id="tab-embedlab"' "$TMP_WEB" && log "OK web tab-embedlab" || fail "web tab-embedlab"
grep -q 'id="st-recall-bar"' "$TMP_WEB" && log "OK web st-recall-bar" || fail "web st-recall-bar"
grep -qE 'data-tab="author"|id="tab-author"|作者' "$TMP_WEB" && log "OK author surface" || log "WARN author tab marker soft"

# node syntax
if command -v node >/dev/null 2>&1; then
  node --check "$ROOT/web/app.js" && log "OK node --check app.js" || fail "node --check app.js"
fi

# login BEFORE stress (rate window 10/300s — stress must not burn the budget first)
TOKEN=""
if [ -n "${KALEIDO_ADMIN_PASSWORD:-}" ]; then
  LOGIN=$("${CURL[@]}" -H 'Content-Type: application/json' \
    -d "{\"username\":\"${KALEIDO_ADMIN_USER:-admin}\",\"password\":\"$KALEIDO_ADMIN_PASSWORD\"}" \
    "$BASE/api/v1/auth/login" || true)
  TOKEN=$(echo "$LOGIN" | python3 -c 'import sys,json;print(json.load(sys.stdin).get("token",""))' 2>/dev/null || true)
  if [ -n "$TOKEN" ]; then
    log "OK login"
    ST=$("${CURL[@]}" -H "Authorization: Bearer $TOKEN" "$BASE/api/v1/embed/status" || true)
    echo "$ST" | python3 -c '
import sys,json
d=json.load(sys.stdin)
e=(d.get("embedding") or d)
assert d.get("ok") is True or e.get("ready") is True, d
assert e.get("ready") is True, e
' && log "OK embed/status" || fail "embed/status: $ST"
    # light embeddings smoke (1 string)
    EM=$("${CURL[@]}" -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
      -d '{"input":"gate ping","model":"BAAI/bge-small-zh-v1.5"}' \
      "$BASE/api/v1/embeddings" || true)
    echo "$EM" | python3 -c '
import sys,json
d=json.load(sys.stdin)
assert d.get("object")=="list" or d.get("data"), d
vec=(d.get("data") or [{}])[0].get("embedding") or []
assert len(vec)==512, ("dim", len(vec))
print("backend", d.get("backend"), "dim", len(vec))
' && log "OK embeddings dim=512" || fail "embeddings: $EM"
  else
    fail "login for embed/regression"
  fi
else
  log "WARN KALEIDO_ADMIN_PASSWORD unset — skip auth embed smoke"
fi

# stress (lighter defaults for gate; override via env)
# Reuse gate login token so stress does not burn KALEIDO_LOGIN_MAX_ATTEMPTS.
export S8_WORKERS="${S8_WORKERS:-12}"
export S8_ROUNDS="${S8_ROUNDS:-2}"
if [ -n "$TOKEN" ]; then
  export S8_TOKEN="$TOKEN"
fi
if python3 "$ROOT/scripts/s8_stress.py" "$BASE"; then
  log "OK s8_stress"
else
  fail "s8_stress"
fi

# regression s7 surface still alive (app-state / author)
if [ -n "$TOKEN" ]; then
  AS=$("${CURL[@]}" -H "Authorization: Bearer $TOKEN" "$BASE/api/v1/app-state" || true)
  echo "$AS" | python3 -c 'import sys,json;d=json.load(sys.stdin);assert d.get("ok") is True' \
    && log "OK app-state" || fail "app-state: $AS"
  AP=$("${CURL[@]}" -H "Authorization: Bearer $TOKEN" "$BASE/api/v1/author/projects" || true)
  echo "$AP" | python3 -c 'import sys,json;d=json.load(sys.stdin);assert "projects" in d or isinstance(d,list)' \
    && log "OK author projects" || fail "author projects: $AP"
fi

if [ "$ERR" -ne 0 ]; then
  echo "[FAIL] S8 hard gate errors=$ERR"
  exit 1
fi
echo "[PASS] S8 hard gate"
exit 0
