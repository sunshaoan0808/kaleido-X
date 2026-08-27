#!/usr/bin/env python3
"""W5 vector activation smoke.

Gates:
  W5_EMBED_READY_OK
  W5_INDEX_REBUILD_OK
  W5_INDEX_STATUS_OK
  W5_VECTOR_QUERY_HIT_OK
  W5_KEYWORD_SKIPS_VECTORIZED_OK
  W5_WI_PREVIEW_VECTOR_ACTIVATE_OK
  W5_SMOKE_ALL_OK
"""
from __future__ import annotations

import json
import os
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any

BASE = os.environ.get("KALEIDO_BASE", "http://127.0.0.1:18766").rstrip("/")
ROOT = Path(__file__).resolve().parents[1]


def load_env() -> None:
    for p in [
        ROOT / ".env",
        ROOT / "data" / "Kaleido" / ".env",
        Path("${HOME}/.env"),
    ]:
        if not p.exists():
            continue
        for line in p.read_text(errors="ignore").splitlines():
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            k, v = line.split("=", 1)
            os.environ.setdefault(k.strip(), v.strip().strip('"').strip("'"))


def req(method: str, path: str, body: Any = None, token: str | None = None, timeout: int = 120):
    data = None
    headers = {"Accept": "application/json"}
    if body is not None:
        data = json.dumps(body, ensure_ascii=False).encode()
        headers["Content-Type"] = "application/json"
    if token:
        headers["Authorization"] = f"Bearer {token}"
    r = urllib.request.Request(BASE + path, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(r, timeout=timeout) as resp:
            raw = resp.read().decode()
            return resp.status, (json.loads(raw) if raw else {})
    except urllib.error.HTTPError as e:
        raw = e.read().decode()
        try:
            j = json.loads(raw) if raw else {}
        except Exception:
            j = {"raw": raw}
        return e.code, j


def login() -> str:
    load_env()
    candidates = []
    u = os.environ.get("KALEIDO_ADMIN_USER") or os.environ.get("KALEIDO_USER")
    pw = (
        os.environ.get("KALEIDO_ADMIN_PASS")
        or os.environ.get("KALEIDO_ADMIN_PASSWORD")
        or os.environ.get("KALEIDO_PASSWORD")
    )
    if u and pw:
        candidates.append((u, pw))
    candidates += [("admin", "admin"), ("aiclaw", "Aa123151")]
    last = (0, {})
    for u, p in candidates:
        code, j = req("POST", "/api/v1/auth/login", {"username": u, "password": p})
        last = (code, j)
        tok = None
        if isinstance(j, dict):
            tok = j.get("token")
            if not tok and isinstance(j.get("session"), dict):
                tok = j["session"].get("token")
        if tok:
            print(f"login ok user={u}")
            return tok
    if os.environ.get("KALEIDO_TOKEN"):
        return os.environ["KALEIDO_TOKEN"]
    raise SystemExit(f"login failed last={last}")


def gate(name: str, cond: bool, detail: str = "") -> bool:
    status = "OK" if cond else "FAIL"
    print(f"{name} {status}" + (f" {detail}" if detail else ""))
    return cond


def main() -> int:
    tok = login()
    ok = True
    stamp = int(time.time())
    wb_id = f"wb-w5-vec-{stamp}"

    # 0) embed ready
    code, emb = req("GET", "/api/v1/embed/status", token=tok)
    ready = isinstance(emb, dict) and (
        emb.get("ready") is True
        or emb.get("enabled") is True
        or (emb.get("embedding") or {}).get("ready") is True
    )
    # status may nest under embedding
    if not ready and isinstance(emb, dict):
        nested = emb.get("embedding") if isinstance(emb.get("embedding"), dict) else emb
        ready = bool(nested.get("ready") or nested.get("dim"))
    ok &= gate("W5_EMBED_READY_OK", code == 200 and ready, f"{code} {emb}")

    # 1) create world book with one keyword entry + one vectorized entry
    # Vectorized entry talks about 青衣门刺客 without the exact chat keyword "星落湖"
    # Keyword entry uses exact key "W5KEY..."
    key_tag = f"W5KEY{stamp}"
    vec_uid_hint = f"vec-{stamp}"
    code, wb = req(
        "POST",
        "/api/v1/partner/world-books",
        {
            "id": wb_id,
            "name": f"W5向量冒烟-{stamp}",
            "type": "world_book",
            "content": f"# W5 vector smoke {stamp}",
            "fields": {},
        },
        token=tok,
    )
    if code >= 300:
        print("create wb fail", code, wb, file=sys.stderr)
        return 2
    print("WB_CREATE", wb_id)

    # keyword entry
    code, k_created = req(
        "POST",
        f"/api/v1/partner/world-books/{wb_id}/entries",
        {
            "keys": [key_tag],
            "content": f"关键词条目命中{key_tag}",
            "comment": "w5-keyword",
            "constant": False,
            "order": 20,
            "position": 0,
            "vectorized": False,
        },
        token=tok,
    )
    if code >= 300 or not k_created.get("ok"):
        print("keyword entry fail", code, k_created, file=sys.stderr)
        return 2

    # vectorized entry — NO keys that match chat; relies on semantic similarity
    code, v_created = req(
        "POST",
        f"/api/v1/partner/world-books/{wb_id}/entries",
        {
            "uid": vec_uid_hint,
            "keys": ["青衣门刺客传说"],  # keys present for embed text, but chat won't contain exact
            "content": (
                "星落湖畔的隐秘传说：每逢月蚀之夜，青衣门的影刺会在湖面留下银色涟漪，"
                "只有懂得古咒的人才能看见刺客的行踪。"
            ),
            "comment": "w5-vectorized-lore",
            "constant": False,
            "order": 50,
            "position": 0,
            "vectorized": True,
            "extensions": {"vectorized": True},
        },
        token=tok,
    )
    if code >= 300 or not v_created.get("ok"):
        print("vector entry fail", code, v_created, file=sys.stderr)
        return 2
    v_entry = v_created.get("entry") or {}
    v_uid = str(v_entry.get("uid") or v_entry.get("id") or vec_uid_hint)
    print("VEC_ENTRY", v_uid)

    # confirm vectorized flag round-trips
    code, listed = req("GET", f"/api/v1/partner/world-books/{wb_id}/entries", token=tok)
    entries = listed.get("entries") or []
    vec_flags = []
    for e in entries:
        uid = str(e.get("uid") or e.get("id") or "")
        flag = e.get("vectorized")
        if flag is None and isinstance(e.get("extensions"), dict):
            flag = e["extensions"].get("vectorized")
        vec_flags.append((uid, flag))
    print("entries flags", vec_flags)

    # 2) rebuild index (onlyVectorized)
    code, reb = req(
        "POST",
        f"/api/v1/partner/world-books/{wb_id}/vector-index/rebuild",
        {"force": True, "onlyVectorized": True},
        token=tok,
        timeout=180,
    )
    indexed = int(reb.get("indexed") or 0) if isinstance(reb, dict) else 0
    ok &= gate(
        "W5_INDEX_REBUILD_OK",
        code == 200 and reb.get("ok") and indexed >= 1,
        f"{code} indexed={indexed} {reb}",
    )

    # 3) status
    code, st = req("GET", f"/api/v1/partner/world-books/{wb_id}/vector-index", token=tok)
    ok &= gate(
        "W5_INDEX_STATUS_OK",
        code == 200 and int(st.get("entryCount") or 0) >= 1 and st.get("exists") is True,
        f"{code} {st}",
    )

    # 4) direct vector query — semantic chat about 月蚀湖刺客 (no exact key)
    query = "月蚀之夜湖边有人看到银色涟漪，像是刺客留下的痕迹"
    code, qj = req(
        "POST",
        "/api/v1/partner/vector-query",
        {
            "worldBookId": wb_id,
            "query": query,
            "scoreThreshold": 0.25,
            "topK": 5,
        },
        token=tok,
        timeout=120,
    )
    hits = qj.get("hits") or [] if isinstance(qj, dict) else []
    hit_uids = [str(h.get("uid")) for h in hits]
    ok &= gate(
        "W5_VECTOR_QUERY_HIT_OK",
        code == 200 and qj.get("ok") and len(hits) >= 1 and v_uid in hit_uids,
        f"{code} hits={hits}",
    )

    # 5) wi-preview with unrelated chat that does NOT match keyword key_tag
    #    and does NOT contain exact vector keys — should activate via vector reason
    code, prev = req(
        "POST",
        "/api/v1/partner/wi-preview",
        {
            "worldBookId": wb_id,
            "messages": [
                {"role": "user", "content": query},
            ],
            "basePrompt": "W5 vector smoke harness",
            "dryRun": True,
            "vectorSettings": {
                "enabled": True,
                "scoreThreshold": 0.25,
                "topK": 5,
            },
            "worldInfoSettings": {"depth": 4, "budgetPct": 50},
        },
        token=tok,
        timeout=120,
    )
    activated = prev.get("activated") or [] if isinstance(prev, dict) else []
    reasons = [(a.get("uid"), a.get("reason")) for a in activated if isinstance(a, dict)]
    vec_act = int(prev.get("vectorActivated") or 0) if isinstance(prev, dict) else 0
    has_vector_reason = any(
        isinstance(a, dict)
        and str(a.get("uid")) == v_uid
        and str(a.get("reason") or "").startswith("vector:")
        for a in activated
    )
    # Also accept if vectorActivated>=1 and content present
    content_hit = any(
        isinstance(a, dict) and "星落湖" in str(a.get("content") or "") for a in activated
    )
    ok &= gate(
        "W5_WI_PREVIEW_VECTOR_ACTIVATE_OK",
        code == 200
        and prev.get("ok")
        and (has_vector_reason or (vec_act >= 1 and content_hit)),
        f"{code} vecAct={vec_act} reasons={reasons} skippedV={prev.get('skippedVectorized')}",
    )

    # 6) without vector settings / empty index path: keyword still works; vectorized still skipped when no hit
    #    Chat with only key_tag should activate keyword entry; vector entry may or may not.
    code, prev2 = req(
        "POST",
        "/api/v1/partner/wi-preview",
        {
            "worldBookId": wb_id,
            "messages": [{"role": "user", "content": f"请提及 {key_tag} 即可"}],
            "basePrompt": "keyword path",
            "dryRun": True,
            "vectorSettings": {"enabled": False},
            "worldInfoSettings": {"depth": 2},
        },
        token=tok,
    )
    act2 = prev2.get("activated") or [] if isinstance(prev2, dict) else []
    key_hit = any(
        isinstance(a, dict) and key_tag in str(a.get("content") or "") + str(a.get("reason") or "")
        for a in act2
    )
    # with vector disabled, vectorized entries should be skipped (not keyword-activated)
    skipped = int(prev2.get("skippedVectorized") or 0) if isinstance(prev2, dict) else 0
    ok &= gate(
        "W5_KEYWORD_SKIPS_VECTORIZED_OK",
        code == 200 and key_hit and skipped >= 1,
        f"{code} key_hit={key_hit} skipped={skipped} act={[(a.get('uid'), a.get('reason')) for a in act2 if isinstance(a, dict)]}",
    )

    ok &= gate("W5_SMOKE_ALL_OK", ok)
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
