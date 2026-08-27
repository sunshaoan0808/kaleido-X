#!/usr/bin/env python3
"""W8 world-book entry CRUD smoke.

Gates:
  W8_ENTRIES_LIST_OK
  W8_ENTRY_CREATE_OK
  W8_ENTRY_PATCH_OK
  W8_ENTRY_DELETE_OK
  W8_WI_PREVIEW_KEY_OK
  W8_SMOKE_ALL_OK
"""
from __future__ import annotations

import json
import os
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

BASE = os.environ.get("KALEIDO_BASE", "http://127.0.0.1:18766").rstrip("/")
ROOT = Path(__file__).resolve().parents[1]


def load_env() -> dict:
    env = {}
    for p in [ROOT / ".env", Path("${HOME}/.env")]:
        if not p.exists():
            continue
        for line in p.read_text(encoding="utf-8", errors="ignore").splitlines():
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            k, v = line.split("=", 1)
            env[k.strip()] = v.strip().strip('"').strip("'")
    return env


def req(method: str, path: str, token: str | None = None, body=None, timeout=30):
    data = None
    headers = {"Accept": "application/json"}
    if body is not None:
        data = json.dumps(body, ensure_ascii=False).encode("utf-8")
        headers["Content-Type"] = "application/json"
    if token:
        headers["Authorization"] = f"Bearer {token}"
    r = urllib.request.Request(BASE + path, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(r, timeout=timeout) as resp:
            raw = resp.read().decode("utf-8", errors="replace")
            try:
                return resp.status, json.loads(raw) if raw else {}
            except json.JSONDecodeError:
                return resp.status, {"_raw": raw}
    except urllib.error.HTTPError as e:
        raw = e.read().decode("utf-8", errors="replace")
        try:
            return e.code, json.loads(raw) if raw else {"error": raw}
        except json.JSONDecodeError:
            return e.code, {"error": raw}


def login(env: dict) -> str:
    if os.environ.get("KALEIDO_TOKEN"):
        return os.environ["KALEIDO_TOKEN"]
    user = (
        os.environ.get("KALEIDO_USER")
        or os.environ.get("KALEIDO_ADMIN_USER")
        or env.get("KALEIDO_USER")
        or env.get("KALEIDO_ADMIN_USER")
        or "admin"
    )
    password = (
        os.environ.get("KALEIDO_PASS")
        or os.environ.get("KALEIDO_ADMIN_PASSWORD")
        or env.get("KALEIDO_PASS")
        or env.get("KALEIDO_ADMIN_PASSWORD")
        or "admin"
    )
    candidates = [(user, password)]
    if env.get("KALEIDO_ADMIN_USER") and env.get("KALEIDO_ADMIN_PASSWORD"):
        candidates.append((env["KALEIDO_ADMIN_USER"], env["KALEIDO_ADMIN_PASSWORD"]))
    code, body = 0, {}
    for u, pw in candidates:
        code, body = req("POST", "/api/v1/auth/login", body={"username": u, "password": pw})
        if code == 200 and isinstance(body, dict):
            tok = body.get("token") or body.get("accessToken")
            if tok:
                print(f"login ok user={u}")
                return tok
        if code == 429:
            raise SystemExit("login 429, prune sessions.json + restart")
    raise SystemExit(f"login failed last={code} {body}")


def main():
    env = load_env()
    token = login(env)
    stamp = int(time.time())
    wb_id = f"wb-w8-smoke-{stamp}"
    name = f"W8冒烟世界书-{stamp}"

    # create empty-ish world book
    code, wb = req(
        "POST",
        "/api/v1/partner/world-books",
        token,
        {
            "id": wb_id,
            "name": name,
            "type": "world_book",
            "content": f"# {name}\n\nseed",
            "fields": {},
        },
    )
    if code >= 300:
        print("create wb fail", code, wb)
        sys.exit(2)
    print("WB_CREATE_OK", wb_id)

    # list (may be empty or derived from content)
    code, listed = req("GET", f"/api/v1/partner/world-books/{wb_id}/entries", token)
    if code != 200 or not listed.get("ok"):
        print("list fail", code, listed)
        sys.exit(2)
    print("W8_ENTRIES_LIST_OK count=", listed.get("count"))

    # create key entry
    key_tag = f"W8KEY{stamp}"
    code, created = req(
        "POST",
        f"/api/v1/partner/world-books/{wb_id}/entries",
        token,
        {
            "keys": [key_tag],
            "content": f"条目内容命中{key_tag}应激活",
            "comment": "w8-smoke-entry",
            "constant": False,
            "order": 10,
            "position": 0,
        },
    )
    if code >= 300 or not created.get("ok"):
        print("create entry fail", code, created)
        sys.exit(2)
    entry = created.get("entry") or {}
    eid = entry.get("uid") or entry.get("id")
    if not eid:
        print("no entry uid", created)
        sys.exit(2)
    print("W8_ENTRY_CREATE_OK", eid)

    # list again
    code, listed2 = req("GET", f"/api/v1/partner/world-books/{wb_id}/entries", token)
    if code != 200 or listed2.get("count", 0) < 1:
        print("list after create fail", code, listed2)
        sys.exit(2)

    # patch keys + content
    new_key = f"W8PATCH{stamp}"
    code, patched = req(
        "PATCH",
        f"/api/v1/partner/world-books/{wb_id}/entries/{eid}",
        token,
        {"keys": [new_key], "content": f"已改键为{new_key}", "comment": "w8-patched"},
    )
    if code >= 300 or not patched.get("ok"):
        print("patch fail", code, patched)
        sys.exit(2)
    pentry = patched.get("entry") or {}
    pkeys = pentry.get("keys") or pentry.get("key") or []
    if new_key not in pkeys:
        print("patch keys not applied", pentry)
        sys.exit(2)
    print("W8_ENTRY_PATCH_OK", pkeys)

    # wi-preview: message with new_key should activate
    code, prev = req(
        "POST",
        "/api/v1/partner/wi-preview",
        token,
        {
            "worldBookId": wb_id,
            "messages": [{"role": "user", "content": f"请描述一下{new_key}相关设定"}],
            "dryRun": True,
            "basePrompt": "You are a test harness.",
            "maxContextTokens": 4096,
        },
    )
    if code >= 300 or not prev.get("ok"):
        print("wi-preview fail", code, prev)
        sys.exit(2)
    activated = int(prev.get("wiActivated") or 0)
    before = prev.get("worldInfoBefore") or ""
    after = prev.get("worldInfoAfter") or ""
    blob = before + after + (prev.get("systemPrompt") or "")
    if activated < 1 and new_key not in blob and "已改键" not in blob:
        # constant-less key miss is hard fail
        print("wi-preview no activation", activated, prev)
        sys.exit(2)
    print("W8_WI_PREVIEW_KEY_OK activated=", activated)

    # put replace whole table with constant entry
    code, putb = req(
        "PUT",
        f"/api/v1/partner/world-books/{wb_id}/entries",
        token,
        {
            "entries": [
                {
                    "uid": "const-1",
                    "keys": [],
                    "content": "常驻设定：青衣门禁地",
                    "comment": "constant",
                    "constant": True,
                    "order": 1,
                },
                {
                    "uid": eid,
                    "keys": [new_key],
                    "content": f"保留补丁条目 {new_key}",
                    "comment": "kept",
                    "constant": False,
                    "order": 2,
                },
            ]
        },
    )
    if code >= 300 or not putb.get("ok") or putb.get("count") != 2:
        print("put entries fail", code, putb)
        sys.exit(2)
    print("W8_ENTRIES_PUT_OK count=", putb.get("count"))

    # delete one
    code, deleted = req(
        "DELETE",
        f"/api/v1/partner/world-books/{wb_id}/entries/{eid}",
        token,
    )
    if code >= 300 or not deleted.get("ok"):
        print("delete fail", code, deleted)
        sys.exit(2)
    code, listed3 = req("GET", f"/api/v1/partner/world-books/{wb_id}/entries", token)
    uids = [
        (e.get("uid") or e.get("id"))
        for e in (listed3.get("entries") or [])
    ]
    if eid in uids:
        print("delete not persisted", listed3)
        sys.exit(2)
    print("W8_ENTRY_DELETE_OK remaining=", listed3.get("count"))

    # cleanup world book
    code, _ = req("DELETE", f"/api/v1/partner/world-books/{wb_id}", token)
    print("cleanup wb", code)
    print("W8_SMOKE_ALL_OK")


if __name__ == "__main__":
    main()
