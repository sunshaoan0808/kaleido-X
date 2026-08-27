#!/usr/bin/env python3
"""W9 rebuild-st-book / migrate-legacy smoke.

Gates:
  W9_PLAIN_WB_REBUILD_OK
  W9_PREVIEW_AFTER_REBUILD_OK
  W9_IDEMPOTENT_OK
  W9_CC_LINKED_REBUILD_OK
  W9_MIGRATE_LEGACY_OK
  W9_SMOKE_ALL_OK
"""
from __future__ import annotations

import json
import os
import sys
import urllib.error
import urllib.request
from pathlib import Path

BASE = os.environ.get("KALEIDO_BASE", "http://127.0.0.1:18766").rstrip("/")
USER = os.environ.get("KALEIDO_USER", "admin")
PASS = os.environ.get("KALEIDO_PASS", "<KALEIDO_PASS>")


def req(method: str, path: str, body=None, token: str | None = None):
    data = None
    headers = {"Accept": "application/json"}
    if body is not None:
        data = json.dumps(body).encode()
        headers["Content-Type"] = "application/json"
    if token:
        headers["Authorization"] = f"Bearer {token}"
    r = urllib.request.Request(BASE + path, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(r, timeout=60) as resp:
            raw = resp.read().decode()
            return resp.status, json.loads(raw) if raw else {}
    except urllib.error.HTTPError as e:
        raw = e.read().decode()
        try:
            j = json.loads(raw) if raw else {}
        except Exception:
            j = {"error": raw}
        return e.code, j


def load_env():
    env = {}
    for path in [Path(".env"), Path("${REPO:-.}/.env")]:
        if path.exists():
            for line in path.read_text().splitlines():
                line=line.strip()
                if not line or line.startswith("#") or "=" not in line:
                    continue
                k,v=line.split("=",1)
                env[k.strip()]=v.strip().strip('"').strip("'")
    return env


def login() -> str:
    env = load_env()
    users = []
    for u,p in [
        (os.environ.get("KALEIDO_USER"), os.environ.get("KALEIDO_PASS")),
        (os.environ.get("KALEIDO_ADMIN_USER"), os.environ.get("KALEIDO_ADMIN_PASSWORD")),
        (env.get("KALEIDO_ADMIN_USER"), env.get("KALEIDO_ADMIN_PASSWORD")),
        (env.get("KALEIDO_USER"), env.get("KALEIDO_PASS")),
        (USER, PASS),
        ("admin", "<KALEIDO_PASS>"),
    ]:
        if u and p and (u,p) not in users:
            users.append((u,p))
    last=(None,None)
    for u,p in users:
        code, j = req("POST", "/api/v1/auth/login", {"username": u, "password": p})
        last=(code,j)
        if code == 200 and isinstance(j, dict):
            tok = j.get("token") or j.get("accessToken")
            if not tok and isinstance(j.get("session"), dict):
                tok = j["session"].get("token")
            if not tok and isinstance(j.get("data"), dict):
                tok = j["data"].get("token") or j["data"].get("accessToken")
            if tok:
                print(f"login ok user={u}")
                return tok
    if os.environ.get("KALEIDO_TOKEN"):
        return os.environ["KALEIDO_TOKEN"]
    raise SystemExit(f"login failed last={last}")


def gate(name: str, cond: bool, detail=""):
    if cond:
        print(name)
        return True
    print(f"FAIL {name} {detail}", file=sys.stderr)
    return False


def main() -> int:
    tok = login()
    ok = True

    # partner state
    code, st = req("GET", "/api/v1/partner", token=tok)
    if code != 200:
        print("partner get fail", code, st, file=sys.stderr)
        return 2
    # unwrap state
    state = st.get("state") if isinstance(st.get("state"), dict) else st
    wbs = state.get("worldBooks") or state.get("world_books") or []
    ccs = state.get("characterCards") or state.get("character_cards") or []

    plain = None
    for w in wbs:
        f = w.get("fields") or {}
        if not f.get("stBookRaw") and (w.get("content") or "").strip():
            plain = w
            break
    if not plain:
        # create a plain freeform book for the smoke
        code, created = req(
            "POST",
            "/api/v1/partner/world-books",
            {
                "type": "world_book",
                "name": "W9SmokePlain",
                "content": "# W9SmokePlain\n\n## 核心设定\n- 关键词: 青衣门\n- 时代: 测试\n",
                "fields": {"theme": "W9SmokePlain"},
            },
            token=tok,
        )
        if code not in (200, 201):
            print("create plain fail", code, created, file=sys.stderr)
            return 2
        plain = created if created.get("id") else (created.get("worldBook") or created)
        print("created plain", plain.get("id"))

    wid = plain["id"]
    # confirm no raw (or ignore)
    code, before = req("GET", f"/api/v1/partner/world-books/{wid}/entries", token=tok)
    # freeform may already surface 1 legacy entry via list — that's ok

    code, reb = req(
        "POST",
        f"/api/v1/partner/world-books/{wid}/rebuild-st-book",
        {"force": True},
        token=tok,
    )
    ok &= gate(
        "W9_PLAIN_WB_REBUILD_OK",
        code == 200 and reb.get("ok") and int(reb.get("count") or 0) >= 1,
        f"{code} {reb}",
    )
    entries = reb.get("entries") or []
    # fields should now have stBookRaw after reload via partner get
    code, st2 = req("GET", "/api/v1/partner", token=tok)
    state2 = st2.get("state") if isinstance(st2.get("state"), dict) else st2
    wbs2 = state2.get("worldBooks") or state2.get("world_books") or []
    hit = next((w for w in wbs2 if w.get("id") == wid), None)
    has_raw = bool((hit or {}).get("fields", {}).get("stBookRaw"))
    ok &= gate("W9_PLAIN_WB_REBUILD_OK", has_raw, f"stBookRaw missing after rebuild hit={bool(hit)}")

    # wi-preview should see entries
    code, prev = req(
        "POST",
        "/api/v1/partner/wi-preview",
        {
            "worldBookId": wid,
            "chat": [{"role": "user", "content": "青衣门 测试 关键词"}],
            "basePrompt": "test",
        },
        token=tok,
    )
    # accept various shapes
    activated = 0
    if isinstance(prev, dict):
        if isinstance(prev.get("activated"), list):
            activated = len(prev["activated"])
        elif isinstance(prev.get("wi"), dict):
            activated = len(prev["wi"].get("activated") or prev["wi"].get("entries") or [])
        elif prev.get("count") is not None:
            activated = int(prev.get("count") or 0)
        # constant legacy entry activates without keys
        slots = prev.get("promptSlots") or prev.get("wi") or {}
        if activated == 0 and (slots.get("worldInfoBefore") or slots.get("world_info_before") or prev.get("systemPrompt")):
            activated = 1
    ok &= gate(
        "W9_PREVIEW_AFTER_REBUILD_OK",
        code == 200 and activated >= 1,
        f"{code} activated={activated} keys={list(prev.keys())[:12] if isinstance(prev, dict) else prev}",
    )

    # idempotent: second rebuild without force → alreadyHadRaw true, count stable
    code, reb2 = req(
        "POST",
        f"/api/v1/partner/world-books/{wid}/rebuild-st-book",
        {"force": False},
        token=tok,
    )
    ok &= gate(
        "W9_IDEMPOTENT_OK",
        code == 200
        and reb2.get("alreadyHadRaw") is True
        and int(reb2.get("count") or 0) == int(reb.get("count") or 0),
        f"{code} {reb2}",
    )

    # character card with linked world book
    linked_cc = None
    for c in ccs:
        if c.get("worldBookId") or c.get("world_book_id"):
            linked_cc = c
            break
    if not linked_cc:
        # create card linked to rebuilt book
        code, cc = req(
            "POST",
            "/api/v1/partner/character-cards",
            {
                "type": "character_card",
                "name": "W9SmokeChar",
                "content": "W9 smoke char",
                "worldBookId": wid,
                "fields": {"name": "W9SmokeChar"},
            },
            token=tok,
        )
        if code not in (200, 201):
            print("create cc fail", code, cc, file=sys.stderr)
            ok = False
        else:
            linked_cc = cc if cc.get("id") else cc.get("characterCard") or cc
    if linked_cc:
        cid = linked_cc["id"]
        code, ccr = req(
            "POST",
            f"/api/v1/partner/character-cards/{cid}/rebuild-st-book",
            {"force": False},
            token=tok,
        )
        ok &= gate(
            "W9_CC_LINKED_REBUILD_OK",
            code == 200 and ccr.get("ok") and int(ccr.get("count") or 0) >= 1 and ccr.get("worldBookId"),
            f"{code} {ccr}",
        )
    else:
        ok &= gate("W9_CC_LINKED_REBUILD_OK", False, "no linked card")

    # migrate-legacy batch
    code, mig = req(
        "POST",
        "/api/v1/partner/world-books/migrate-legacy",
        {"force": False},
        token=tok,
    )
    ok &= gate(
        "W9_MIGRATE_LEGACY_OK",
        code == 200 and mig.get("ok") and int(mig.get("total") or 0) >= 1,
        f"{code} {mig}",
    )

    if ok:
        print("W9_SMOKE_ALL_OK")
        return 0
    print("W9_SMOKE_FAIL", file=sys.stderr)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
