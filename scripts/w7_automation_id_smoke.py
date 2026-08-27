#!/usr/bin/env python3
"""W7 automationId consumption surface smoke.

Gates:
  W7_ENTRY_WITH_AUTO_OK
  W7_PREVIEW_IDS_OK
  W7_PREVIEW_PER_ENTRY_OK
  W7_PREVIEW_RECORD_OK
  W7_TRIGGERS_LIST_OK
  W7_TRIGGERS_CLEAR_OK
  W7_SMOKE_ALL_OK
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


def req(method: str, path: str, body: Any = None, token: str | None = None, timeout: int = 60):
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
    if os.environ.get("KALEIDO_TOKEN"):
        return os.environ["KALEIDO_TOKEN"]
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
            return str(tok)
    raise SystemExit(f"login failed last={last}")


def gate(name: str, cond: bool, detail: str = "") -> bool:
    print(f"{name} {'OK' if cond else 'FAIL'}" + (f" {detail}" if detail else ""))
    return bool(cond)


def main() -> int:
    tok = login()
    ok = True
    stamp = int(time.time())
    wb_id = f"wb-w7-auto-{stamp}"
    auto_id = f"auto.w7.hook.{stamp}"
    key_tag = f"W7AUTO{stamp}"
    session_id = f"w7-sess-{stamp}"

    # clear log first so list is deterministic-ish
    req("DELETE", "/api/v1/partner/automation-triggers", token=tok)

    code, wb = req(
        "POST",
        "/api/v1/partner/world-books",
        {
            "id": wb_id,
            "name": f"W7自动化冒烟-{stamp}",
            "type": "world_book",
            "content": f"# W7 automation {stamp}",
            "fields": {},
        },
        token=tok,
    )
    if code >= 300:
        print("create wb fail", code, wb, file=sys.stderr)
        return 2

    code, created = req(
        "POST",
        f"/api/v1/partner/world-books/{wb_id}/entries",
        {
            "keys": [key_tag],
            "content": f"自动化钩子内容 {auto_id} 已激活",
            "comment": "w7-automation-entry",
            "constant": False,
            "order": 40,
            "position": 0,
            "automationId": auto_id,
            "extensions": {"automation_id": auto_id, "automationId": auto_id},
        },
        token=tok,
    )
    if code >= 300 or not created.get("ok"):
        print("create entry fail", code, created, file=sys.stderr)
        return 2
    entry = created.get("entry") or {}
    eid = str(entry.get("uid") or entry.get("id") or "")
    # round-trip flag
    listed_auto = None
    code, listed = req("GET", f"/api/v1/partner/world-books/{wb_id}/entries", token=tok)
    for e in listed.get("entries") or []:
        if str(e.get("uid") or e.get("id") or "") == eid:
            listed_auto = e.get("automationId") or e.get("automation_id")
            if not listed_auto and isinstance(e.get("extensions"), dict):
                listed_auto = e["extensions"].get("automationId") or e["extensions"].get(
                    "automation_id"
                )
    ok &= gate(
        "W7_ENTRY_WITH_AUTO_OK",
        bool(eid) and (listed_auto == auto_id or entry.get("automationId") == auto_id
                       or (isinstance(entry.get("extensions"), dict)
                           and (entry["extensions"].get("automationId") == auto_id
                                or entry["extensions"].get("automation_id") == auto_id))),
        f"eid={eid} listed_auto={listed_auto} entry_keys={list(entry.keys())[:12]}",
    )

    # dryRun preview: ids present, no record
    code, dry = req(
        "POST",
        "/api/v1/partner/wi-preview",
        {
            "worldBookId": wb_id,
            "messages": [{"role": "user", "content": f"请触发 {key_tag}"}],
            "basePrompt": "W7 dry",
            "dryRun": True,
            "sessionId": session_id,
            "worldInfoSettings": {"depth": 4},
        },
        token=tok,
    )
    ids_dry = dry.get("automationIds") or [] if isinstance(dry, dict) else []
    act_dry = dry.get("activated") or [] if isinstance(dry, dict) else []
    per_entry = any(
        isinstance(a, dict)
        and (a.get("automationId") == auto_id or a.get("automation_id") == auto_id)
        for a in act_dry
    )
    ok &= gate(
        "W7_PREVIEW_IDS_OK",
        code == 200 and auto_id in ids_dry,
        f"{code} ids={ids_dry}",
    )
    ok &= gate(
        "W7_PREVIEW_PER_ENTRY_OK",
        code == 200 and per_entry,
        f"activated={[ (a.get('uid'), a.get('automationId') or a.get('automation_id'), a.get('reason')) for a in act_dry if isinstance(a, dict)]}",
    )

    # non-dry: should record
    code, live = req(
        "POST",
        "/api/v1/partner/wi-preview",
        {
            "worldBookId": wb_id,
            "messages": [{"role": "user", "content": f"再触发 {key_tag} 一次"}],
            "basePrompt": "W7 live",
            "dryRun": False,
            "sessionId": session_id,
            "worldInfoSettings": {"depth": 4},
        },
        token=tok,
    )
    recorded = int(live.get("automationRecorded") or 0) if isinstance(live, dict) else 0
    ids_live = live.get("automationIds") or [] if isinstance(live, dict) else []
    ok &= gate(
        "W7_PREVIEW_RECORD_OK",
        code == 200 and auto_id in ids_live and recorded >= 1,
        f"{code} recorded={recorded} ids={ids_live}",
    )

    code, lst = req("GET", f"/api/v1/partner/automation-triggers?limit=20", token=tok)
    events = lst.get("events") or [] if isinstance(lst, dict) else []
    hit = any(
        isinstance(e, dict) and e.get("automationId") == auto_id for e in events
    )
    ok &= gate(
        "W7_TRIGGERS_LIST_OK",
        code == 200 and lst.get("ok") and hit,
        f"{code} count={lst.get('count')} sample={events[:2]}",
    )

    code, clr = req("DELETE", "/api/v1/partner/automation-triggers", token=tok)
    code2, lst2 = req("GET", "/api/v1/partner/automation-triggers?limit=5", token=tok)
    empty = (lst2.get("count") or 0) == 0 if isinstance(lst2, dict) else False
    ok &= gate(
        "W7_TRIGGERS_CLEAR_OK",
        code == 200 and clr.get("ok") and empty,
        f"clear={code}/{clr} after={lst2.get('count')}",
    )

    ok &= gate("W7_SMOKE_ALL_OK", ok)
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
