#!/usr/bin/env bash
# Story Tavern MVP smoke gate (ST-3/ST-4)
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
set -a; [ -f .env ] && . ./.env; set +a
BASE="${KALEIDO_BASE:-http://127.0.0.1:18766}"
fail() { echo "FAIL: $*"; exit 1; }
pass() { echo "PASS: $*"; }

curl -fsS -m 5 "$BASE/health" | grep -q '"ok":true' || fail health
pass health

LOGIN=$(curl -fsS -m 5 -H 'Content-Type: application/json' \
  -d "{\"username\":\"${KALEIDO_ADMIN_USER}\",\"password\":\"${KALEIDO_ADMIN_PASSWORD}\"}" \
  "$BASE/api/v1/auth/login") || fail login
TOKEN=$(python3 -c 'import json,sys;print(json.load(sys.stdin)["token"])' <<<"$LOGIN")
[ -n "$TOKEN" ] || fail token
AUTH=( -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' )
pass login

PACKS=$(curl -fsS -m 5 "${AUTH[@]}" "$BASE/api/v1/story-tavern/packs")
python3 - <<PY || fail packs
import json,sys
d=json.loads('''$PACKS''')
ps=d.get('packs') or d
assert any(p.get('id')=='demo-rain-alley' for p in ps), ps
print('packs', len(ps))
PY
pass packs-list

# ensure demo
curl -fsS -m 10 -X POST "${AUTH[@]}" "$BASE/api/v1/story-tavern/packs/demo" >/dev/null
pass packs-demo

# create P1 session
SESS=$(curl -fsS -m 10 -X POST "${AUTH[@]}" \
  -d '{"packId":"demo-rain-alley","playable":"P1","playMode":"free","userTier":"standard","adultConfirmed":true}' \
  "$BASE/api/v1/story-tavern/sessions")
SID=$(python3 -c 'import json,sys;print(json.load(sys.stdin)["sessionId"])' <<<"$SESS")
[ -n "$SID" ] || fail create-session
pass create-session "$SID"

# create P3 session
SESS3=$(curl -fsS -m 10 -X POST "${AUTH[@]}" \
  -d '{"packId":"demo-rain-alley","playable":"P3","playMode":"mainline","userTier":"standard","adultConfirmed":true,"entry":{"entryRole":"protagonist","metaKnowledge":"reader","rewriteIntensity":"rewrite","vesselCharacterId":"cc-shentang"}}' \
  "$BASE/api/v1/story-tavern/sessions")
SID3=$(python3 -c 'import json,sys;print(json.load(sys.stdin)["sessionId"])' <<<"$SESS3")
[ -n "$SID3" ] || fail create-p3
pass create-p3 "$SID3"

# chapter read
CH=$(curl -fsS -m 5 "${AUTH[@]}" "$BASE/api/v1/story-tavern/packs/demo-rain-alley/chapters/chapters%2Fch01.md")
python3 -c 'import json,sys;d=json.load(sys.stdin);assert d.get("content"),d' <<<"$CH" || fail chapter-read
pass chapter-read

# web markers
grep -q 'id="tab-tavern"' web/index.html || fail web-tab
grep -q 'id="st-play-p1"' web/index.html || fail web-p1
grep -q 'id="st-play-p3"' web/index.html || fail web-p3
grep -q 'id="st-lore-panel"' web/index.html || fail web-lore
grep -q 'stImportNovel' web/app.js || fail web-import
grep -q 'stRenderLore' web/app.js || fail web-lore-js
node --check web/app.js
pass web-static



# P3 advance + canon + lore (ST-5)
ADV=$(curl -fsS -m 10 -X POST "${AUTH[@]}"   -d '{"packId":"demo-rain-alley","playable":"P3","playMode":"mainline","userTier":"standard","adultConfirmed":true,"entry":{"entryRole":"protagonist","metaKnowledge":"reader","rewriteIntensity":"rewrite","vesselCharacterId":"cc-shentang"}}'   "$BASE/api/v1/story-tavern/sessions")
SID_ADV=$(python3 -c 'import json,sys;print(json.load(sys.stdin)["sessionId"])' <<<"$ADV")
NODE0=$(python3 -c 'import json,sys;print(json.load(sys.stdin).get("nodeId",""))' <<<"$ADV")
TURN=$(curl -fsS -m 15 -X POST "${AUTH[@]}" -d '{"message":"继续推进"}'   "$BASE/api/v1/story-tavern/sessions/$SID_ADV/turn")
RUN=$(python3 -c 'import json,sys;print(json.load(sys.stdin)["runId"])' <<<"$TURN")
(curl -fsS -m 180 -H "Authorization: Bearer $TOKEN" -H 'Accept: text/event-stream'   "$BASE/api/v1/story-tavern/sessions/$SID_ADV/stream?runId=$RUN" >/tmp/st_smoke_stream.txt || true) &
NODE1="$NODE0"
for i in $(seq 1 50); do
  AFTER=$(curl -fsS -m 5 "${AUTH[@]}" "$BASE/api/v1/story-tavern/sessions/$SID_ADV")
  NODE1=$(python3 -c 'import json,sys;print(json.load(sys.stdin).get("nodeId",""))' <<<"$AFTER")
  TURN_N=$(python3 -c 'import json,sys;print(json.load(sys.stdin).get("turn",0))' <<<"$AFTER")
  if [ "$NODE0" = "n1" ] && [ "$NODE1" = "n2" ]; then break; fi
  sleep 3
done
wait || true
[ "$NODE0" = "n1" ] && [ "$NODE1" = "n2" ] || fail "advance-node $NODE0->$NODE1"
pass advance-node "$NODE0->$NODE1"

CAN=$(curl -fsS -m 10 -X POST "${AUTH[@]}"   -d '{"packId":"demo-rain-alley","playable":"P3","playMode":"mainline","userTier":"standard","adultConfirmed":true,"entry":{"entryRole":"protagonist","metaKnowledge":"reader","rewriteIntensity":"canon","vesselCharacterId":"cc-shentang"}}'   "$BASE/api/v1/story-tavern/sessions")
SID_C=$(python3 -c 'import json,sys;print(json.load(sys.stdin)["sessionId"])' <<<"$CAN")
TURN_C=$(curl -fsS -m 15 -X POST "${AUTH[@]}" -d '{"message":"[剧情推进] 回到主线"}'   "$BASE/api/v1/story-tavern/sessions/$SID_C/turn")
RUN_C=$(python3 -c 'import json,sys;print(json.load(sys.stdin)["runId"])' <<<"$TURN_C")
# drain stream in background; poll session until turn advances or 120s
(curl -fsS -m 120 -H "Authorization: Bearer $TOKEN" -H 'Accept: text/event-stream'   "$BASE/api/v1/story-tavern/sessions/$SID_C/stream?runId=$RUN_C" >/tmp/st_smoke_canon.txt || true) &
CANON_OK=0
for i in $(seq 1 40); do
  AFTER_C=$(curl -fsS -m 5 "${AUTH[@]}" "$BASE/api/v1/story-tavern/sessions/$SID_C")
  if python3 -c 'import json,sys;d=json.load(sys.stdin);raise SystemExit(0 if any("回归原著" in (m.get("content") or "") for m in d.get("messages") or []) else 1)' <<<"$AFTER_C"; then
    CANON_OK=1; break
  fi
  # also accept node advanced + turn>=1 even if note delayed
  TURN_N=$(python3 -c 'import json,sys;print(json.load(sys.stdin).get("turn",0))' <<<"$AFTER_C")
  NODE_N=$(python3 -c 'import json,sys;print(json.load(sys.stdin).get("nodeId",""))' <<<"$AFTER_C")
  if [ "$TURN_N" -ge 1 ] && [ "$NODE_N" = "n2" ]; then
    # wait a bit more for note
    sleep 1
    AFTER_C=$(curl -fsS -m 5 "${AUTH[@]}" "$BASE/api/v1/story-tavern/sessions/$SID_C")
    if python3 -c 'import json,sys;d=json.load(sys.stdin);raise SystemExit(0 if any("回归原著" in (m.get("content") or "") for m in d.get("messages") or []) else 1)' <<<"$AFTER_C"; then
      CANON_OK=1; break
    fi
  fi
  sleep 3
done
wait || true
[ "$CANON_OK" = 1 ] || fail canon-note
pass canon-note

PACK=$(curl -fsS -m 5 "${AUTH[@]}" "$BASE/api/v1/story-tavern/packs/demo-rain-alley")
python3 -c 'import json,sys;d=json.load(sys.stdin);assert isinstance(d.get("loreEntries"), list) and len(d["loreEntries"])>=1' <<<"$PACK" || fail lore-present
pass lore-present
grep -q 'filter_lore_entries' crates/kaleido-server/src/story_tavern.rs || fail lore-filter-src
grep -q '世界书 / Lore' crates/kaleido-server/src/story_tavern.rs || fail lore-prompt-src
pass lore-filter-src


# ST-6 mode switch (mainline ↔ free)
MOD=$(curl -fsS -m 10 -X POST "${AUTH[@]}"   -d '{"packId":"demo-rain-alley","playable":"P3","playMode":"mainline","userTier":"standard","adultConfirmed":true,"entry":{"entryRole":"protagonist","metaKnowledge":"reader","rewriteIntensity":"rewrite","vesselCharacterId":"cc-shentang"}}'   "$BASE/api/v1/story-tavern/sessions")
SID_M=$(python3 -c 'import json,sys;print(json.load(sys.stdin)["sessionId"])' <<<"$MOD")
FREE=$(curl -fsS -m 10 -X POST "${AUTH[@]}" -d '{"playMode":"free"}'   "$BASE/api/v1/story-tavern/sessions/$SID_M/mode")
python3 -c 'import json,sys;d=json.load(sys.stdin);assert d.get("playMode")=="free",d;assert any("模式切换" in (m.get("content") or "") for m in d.get("messages") or [])' <<<"$FREE" || fail mode-to-free
pass mode-to-free
MAIN=$(curl -fsS -m 10 -X POST "${AUTH[@]}" -d '{"playMode":"mainline"}'   "$BASE/api/v1/story-tavern/sessions/$SID_M/mode")
python3 -c 'import json,sys;d=json.load(sys.stdin);assert d.get("playMode")=="mainline",d' <<<"$MAIN" || fail mode-to-mainline
pass mode-to-mainline
grep -q 'st-mode-toggle' web/index.html || fail mode-ui
grep -q 'stSetPlayMode' web/app.js || fail mode-js
pass mode-ui

# ST-7 saves
SAVE_SESS=$(curl -fsS -m 10 -X POST "${AUTH[@]}"   -d '{"packId":"demo-rain-alley","playable":"P3","playMode":"mainline","userTier":"standard","adultConfirmed":true,"entry":{"entryRole":"protagonist","metaKnowledge":"reader","rewriteIntensity":"rewrite","vesselCharacterId":"cc-shentang"}}'   "$BASE/api/v1/story-tavern/sessions")
SID_S=$(python3 -c 'import json,sys;print(json.load(sys.stdin)["sessionId"])' <<<"$SAVE_SESS")
SAVE1=$(curl -fsS -m 10 -X POST "${AUTH[@]}" -d '{"label":"smoke-起点"}'   "$BASE/api/v1/story-tavern/sessions/$SID_S/saves")
SAVE_ID=$(python3 -c 'import json,sys;print(json.load(sys.stdin)["saveId"])' <<<"$SAVE1")
curl -fsS -m 10 -X POST "${AUTH[@]}" -d '{"playMode":"free"}'   "$BASE/api/v1/story-tavern/sessions/$SID_S/mode" >/dev/null
REST=$(curl -fsS -m 10 -X POST "${AUTH[@]}" -d '{}'   "$BASE/api/v1/story-tavern/sessions/$SID_S/saves/$SAVE_ID/restore")
python3 -c 'import json,sys;d=json.load(sys.stdin);assert d.get("playMode")=="mainline",d;assert any("回档" in (m.get("content") or "") for m in d.get("messages") or [])' <<<"$REST" || fail save-restore
pass save-restore
grep -q 'st-save-list' web/index.html || fail save-ui
grep -q 'stCreateSave' web/app.js || fail save-js
pass save-ui

# ST-8 pack zip export/import
curl -fsS -m 15 -H "Authorization: Bearer $TOKEN"   -o /tmp/st_smoke_pack.zip   "$BASE/api/v1/story-tavern/packs/demo-rain-alley/export.zip" || fail pack-export
python3 - <<'PY2' || fail pack-export-zip
import zipfile
z=zipfile.ZipFile('/tmp/st_smoke_pack.zip')
names=set(z.namelist())
assert 'pack.json' in names, names
assert any(n.startswith('chapters/') for n in names), names
PY2
pass pack-export
python3 -c 'import base64,json; json.dump({"zipBase64": base64.b64encode(open("/tmp/st_smoke_pack.zip","rb").read()).decode(), "id":"pack-smoke-zip"}, open("/tmp/st_smoke_import.json","w"))'
IMP=$(curl -fsS -m 15 -X POST "${AUTH[@]}"   --data-binary @/tmp/st_smoke_import.json   "$BASE/api/v1/story-tavern/packs/import")
python3 -c 'import json,sys;d=json.load(sys.stdin);assert d.get("id","").startswith("pack-smoke-zip"),d;assert len(d.get("chapters") or [])>=2' <<<"$IMP" || fail pack-import
pass pack-import
grep -q 'st-pack-export' web/index.html || fail pack-zip-ui
grep -q 'stExportPackZip' web/app.js || fail pack-zip-js
pass pack-zip-ui

# ST-9 side + resumeNodeId
SIDE=$(curl -fsS -m 10 -X POST "${AUTH[@]}"   -d '{"packId":"demo-rain-alley","playable":"P3","playMode":"mainline","userTier":"standard","adultConfirmed":true,"entry":{"entryRole":"protagonist","metaKnowledge":"reader","rewriteIntensity":"rewrite","vesselCharacterId":"cc-shentang"}}'   "$BASE/api/v1/story-tavern/sessions")
SID_SIDE=$(python3 -c 'import json,sys;print(json.load(sys.stdin)["sessionId"])' <<<"$SIDE")
TO_SIDE=$(curl -fsS -m 10 -X POST "${AUTH[@]}" -d '{"playMode":"side"}'   "$BASE/api/v1/story-tavern/sessions/$SID_SIDE/mode")
python3 -c 'import json,sys;d=json.load(sys.stdin);assert d.get("playMode")=="side",d;assert d.get("resumeNodeId")=="n1",d' <<<"$TO_SIDE" || fail mode-to-side
pass mode-to-side
curl -fsS -m 10 "${AUTH[@]}" "$BASE/api/v1/story-tavern/sessions/$SID_SIDE" -o /tmp/st_side_sess.json
python3 -c 'import json;d=json.load(open("/tmp/st_side_sess.json"));d["nodeId"]="n2";json.dump(d,open("/tmp/st_side_mut.json","w"))'
curl -fsS -m 10 -X PUT "${AUTH[@]}" --data-binary @/tmp/st_side_mut.json   "$BASE/api/v1/story-tavern/sessions/$SID_SIDE" >/dev/null
BACK=$(curl -fsS -m 10 -X POST "${AUTH[@]}" -d '{"playMode":"mainline"}'   "$BASE/api/v1/story-tavern/sessions/$SID_SIDE/mode")
python3 -c 'import json,sys;d=json.load(sys.stdin);assert d.get("playMode")=="mainline",d;assert d.get("nodeId")=="n1",d;assert not d.get("resumeNodeId")' <<<"$BACK" || fail side-resume
pass side-resume
grep -q 'st-mode-side' web/index.html || fail side-ui
pass side-ui


# ST-10 multi-speaker focus
FOC=$(curl -fsS -m 10 -X POST "${AUTH[@]}"   -d '{"packId":"demo-rain-alley","playable":"P2","playMode":"mainline","userTier":"standard","adultConfirmed":true}'   "$BASE/api/v1/story-tavern/sessions")
SID_F=$(python3 -c 'import json,sys;print(json.load(sys.stdin)["sessionId"])' <<<"$FOC")
curl -fsS -m 10 "${AUTH[@]}" "$BASE/api/v1/story-tavern/sessions/$SID_F" -o /tmp/st_focus_sess.json
python3 -c 'import json;d=json.load(open("/tmp/st_focus_sess.json"));d["presentCharacterIds"]=["cc-shentang","cc-linwan"];json.dump(d,open("/tmp/st_focus_mut.json","w"))'
curl -fsS -m 10 -X PUT "${AUTH[@]}" --data-binary @/tmp/st_focus_mut.json "$BASE/api/v1/story-tavern/sessions/$SID_F" >/dev/null
SETF=$(curl -fsS -m 10 -X POST "${AUTH[@]}" -d '{"characterId":"cc-shentang","speakerRotation":true}'   "$BASE/api/v1/story-tavern/sessions/$SID_F/focus")
python3 -c 'import json,sys;d=json.load(sys.stdin);assert d.get("focusCharacterId")=="cc-shentang",d;assert d.get("speakerRotation") is True' <<<"$SETF" || fail focus-set
pass focus-set
# unit: rotate after turn is covered by cargo test; smoke checks UI + API set
grep -q 'st-focus-bar' web/index.html || fail focus-ui
grep -q 'stRenderFocusBar' web/app.js || fail focus-js
grep -q 'rotate_focus_character' crates/kaleido-core/src/story_tavern.rs || fail focus-core
pass focus-ui


# ST-11 vessel rebind
VB=$(curl -fsS -m 10 -X POST "${AUTH[@]}"   -d '{"packId":"demo-rain-alley","playable":"P3","playMode":"mainline","userTier":"standard","adultConfirmed":true,"entry":{"entryRole":"supporting","metaKnowledge":"reader","rewriteIntensity":"rewrite","vesselCharacterId":"cc-shentang"}}'   "$BASE/api/v1/story-tavern/sessions")
SID_V=$(python3 -c 'import json,sys;print(json.load(sys.stdin)["sessionId"])' <<<"$VB")
RB=$(curl -fsS -m 10 -X POST "${AUTH[@]}" -d '{"vesselCharacterId":"cc-linwan","entryRole":"supporting"}'   "$BASE/api/v1/story-tavern/sessions/$SID_V/rebind-vessel")
python3 -c 'import json,sys;d=json.load(sys.stdin);assert (d.get("entry") or {}).get("vesselCharacterId")=="cc-linwan",d;assert any("vessel_change" in (m.get("content") or "") for m in d.get("messages") or [])' <<<"$RB" || fail vessel-rebind
pass vessel-rebind
grep -q 'st-vessel-select' web/index.html || fail vessel-ui
grep -q 'stRebindVessel' web/app.js || fail vessel-js
pass vessel-ui


# ST-12 L2/L3 schema present on new session + core symbols
NS=$(curl -fsS -m 10 -X POST "${AUTH[@]}"   -d '{"packId":"demo-rain-alley","playable":"P1","playMode":"free","userTier":"standard","adultConfirmed":true}'   "$BASE/api/v1/story-tavern/sessions")
python3 -c 'import json,sys;d=json.load(sys.stdin);assert "memoryL2" in d and "events" in (d.get("memoryL2") or {}),d;assert "memoryL3" in d and "facts" in (d.get("memoryL3") or {}),d' <<<"$NS" || fail l2-l3-schema
pass l2-l3-schema
grep -q 'heuristic_l2_l3_from_turn' crates/kaleido-core/src/tavern_engine.rs || fail l2-l3-core
grep -q '近期事件 (L2)' crates/kaleido-server/src/story_tavern.rs || fail l2-l3-prompt
pass l2-l3-core


# ST-13 lightweight node graph edit via pack upsert
curl -fsS -m 10 "${AUTH[@]}" "$BASE/api/v1/story-tavern/packs/demo-rain-alley" -o /tmp/st_demo_pack.json
python3 -c 'import json,uuid; d=json.load(open("/tmp/st_demo_pack.json")); d["id"]="pack-smoke-nodes-"+str(uuid.uuid4())[:8]; d["title"]="smoke-nodes"; d.setdefault("nodes",[]); d["nodes"].append({"id":"n9","chapterId":"ch02","title":"smoke","entry":"","exit":[],"lockedBeats":[],"allowedDivergence":"branch","presentCharacters":[],"summary":"s"});
chs=d.get("chapters") or []
for ch in chs:
  if ch.get("id")=="ch02":
    ch.setdefault("nodeIds",[])
    if "n9" not in ch["nodeIds"]: ch["nodeIds"].append("n9")
json.dump(d, open("/tmp/st_node_edit_pack.json","w"))'
SAVED=$(curl -fsS -m 15 -X POST "${AUTH[@]}" --data-binary @/tmp/st_node_edit_pack.json "$BASE/api/v1/story-tavern/packs")
python3 -c 'import json,sys;d=json.load(sys.stdin);assert any(n.get("id")=="n9" for n in d.get("nodes") or []), [n.get("id") for n in d.get("nodes") or []]' <<<"$SAVED" || fail node-edit
pass node-edit
grep -q 'st-node-panel' web/index.html || fail node-ui
grep -q 'stRenderNodes' web/app.js || fail node-js
grep -q 'stParseExitsText' web/app.js || fail node-exits
pass node-ui

# ST-14 LLM extraction (every 3 turns)
SID_X=$(curl -fsS -m 10 -X POST "${AUTH[@]}" \
  -d '{"packId":"demo-rain-alley","playable":"P1","playMode":"free","userTier":"standard","adultConfirmed":true}' \
  "$BASE/api/v1/story-tavern/sessions" | python3 -c 'import json,sys;print(json.load(sys.stdin)["sessionId"])')
for i in 1 2 3; do
  curl -fsS -m 120 -X POST "${AUTH[@]}" \
    -d "{\"sessionId\":\"$SID_X\",\"message\":\"第${i}句\"}" \
    "$BASE/api/v1/story-tavern/sessions/$SID_X/turn" >/dev/null 2>&1
  sleep 10
 done
 # Poll session until LLM extraction appears or timeout
 _end=$((SECONDS+180))
 while [ $SECONDS -lt $_end ]; do
   sleep 10
   _json=$(curl -fsS -m 5 "${AUTH[@]}" "$BASE/api/v1/story-tavern/sessions/$SID_X")
   _evs=$(echo "$_json" | python3 -c 'import json,sys;d=json.load(sys.stdin);evs=d.get("memoryL2",{}).get("events",[]);llm=[e for e in evs if not e.get("id","").startswith("e-h-")];print(len(llm))')
   [ "$_evs" -gt 0 ] && break
 done
 echo "$_json" \
   | python3 -c 'import json,sys;d=json.load(sys.stdin);evs=d.get("memoryL2",{}).get("events",[]);llm=[e for e in evs if not e.get("id","").startswith("e-h-")];print(f"events={len(evs)} llm={len(llm)}");sys.exit(0 if llm else 1)'\
   || fail llm-extraction
pass llm-extraction

echo "ALL PASS"
