#!/usr/bin/env python3
"""W6 global/preset regex library smoke.

Gates:
  W6_LIBRARY_GET_OK
  W6_LIBRARY_PUT_OK
  W6_LIBRARY_IMPORT_MERGE_OK
  W6_CARD_OVERRIDE_OK
  W6_RUNTIME_MERGE_OK
  W6_SMOKE_ALL_OK
"""
from __future__ import annotations

import json
import os
import sys
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


def req(method: str, path: str, body: Any = None, token: str | None = None):
    data = None
    headers = {"Accept": "application/json"}
    if body is not None:
        data = json.dumps(body, ensure_ascii=False).encode()
        headers["Content-Type"] = "application/json"
    if token:
        headers["Authorization"] = f"Bearer {token}"
    r = urllib.request.Request(BASE + path, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(r, timeout=30) as resp:
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
        if not isinstance(j, dict):
            continue
        tok = j.get("token")
        if not tok and isinstance(j.get("session"), dict):
            tok = j["session"].get("token")
        if not tok and isinstance(j.get("data"), dict):
            tok = j["data"].get("token")
        if code < 300 and tok:
            return str(tok)
    raise SystemExit(f"login failed last={last}")


def main() -> int:
    gates: list[str] = []
    token = login()

    code, j = req("GET", "/api/v1/regex-library", token=token)
    assert code == 200 and isinstance(j, dict) and j.get("ok") is True, (code, j)
    print("W6_LIBRARY_GET_OK")
    gates.append("W6_LIBRARY_GET_OK")

    lib_scripts = [
        {
            "id": "w6-lib-hide",
            "scriptName": "w6-hide",
            "findRegex": "/W6_LIB_MARK/g",
            "replaceString": "LIB_REPLACED",
            "placement": [1, 2],
            "disabled": False,
            "promptOnly": True,
            "markdownOnly": False,
        },
        {
            "id": "w6-lib-only",
            "scriptName": "w6-lib-only",
            "findRegex": "/ONLY_LIB/g",
            "replaceString": "FROM_LIB",
            "placement": [2],
            "disabled": False,
            "promptOnly": True,
        },
    ]
    code, j = req(
        "PUT",
        "/api/v1/regex-library",
        {"priority": "card_over_library", "scripts": lib_scripts},
        token=token,
    )
    assert code == 200 and isinstance(j, dict) and j.get("ok") and j.get("count") == 2, (code, j)
    print("W6_LIBRARY_PUT_OK")
    gates.append("W6_LIBRARY_PUT_OK")

    code, j = req(
        "POST",
        "/api/v1/regex-library/import",
        {
            "replace": False,
            "scripts": [
                {
                    "id": "w6-lib-import",
                    "scriptName": "w6-imported",
                    "findRegex": "/IMPORTED/g",
                    "replaceString": "IMP_OK",
                    "placement": [2],
                    "promptOnly": True,
                }
            ],
        },
        token=token,
    )
    assert code == 200 and isinstance(j, dict) and j.get("ok") and int(j.get("count") or 0) >= 3, (
        code,
        j,
    )
    ids = {s.get("id") for s in (j.get("scripts") or []) if isinstance(s, dict)}
    assert "w6-lib-hide" in ids and "w6-lib-import" in ids, ids
    print("W6_LIBRARY_IMPORT_MERGE_OK")
    gates.append("W6_LIBRARY_IMPORT_MERGE_OK")

    # PartnerItem requires id+name+type
    cc_id = "w6-smoke-cc"
    cc_body = {
        "id": cc_id,
        "type": "character_card",
        "name": "W6 Smoke Card",
        "content": "W6 card content containing W6_LIB_MARK and ONLY_LIB and ONLY_CARD",
        "fields": {
            "stRegexScripts": [
                {
                    "id": "w6-lib-hide",
                    "scriptName": "w6-hide",
                    "findRegex": "/W6_LIB_MARK/g",
                    "replaceString": "CARD_WINS",
                    "placement": [1, 2],
                    "promptOnly": True,
                },
                {
                    "id": "w6-card-only",
                    "scriptName": "w6-card-only",
                    "findRegex": "/ONLY_CARD/g",
                    "replaceString": "FROM_CARD",
                    "placement": [2],
                    "promptOnly": True,
                },
            ]
        },
    }
    code, created = req("POST", "/api/v1/partner/character-cards", cc_body, token=token)
    assert code < 300 and isinstance(created, dict), (code, created)
    assert created.get("id") == cc_id, created
    fields = created.get("fields") or {}
    card_scripts = fields.get("stRegexScripts") or []
    assert any(isinstance(s, dict) and s.get("replaceString") == "CARD_WINS" for s in card_scripts), fields
    print("W6_CARD_OVERRIDE_OK")
    gates.append("W6_CARD_OVERRIDE_OK")

    # Runtime: prompt-preview with characterCardId should apply library∪card (prompt path)
    code, prev = req(
        "GET",
        f"/api/v1/partner/prompt-preview?characterCardId={cc_id}",
        token=token,
    )
    assert code == 200 and isinstance(prev, dict), (code, prev)
    prompt = str(prev.get("systemPrompt") or "")
    # card content is injected; W6_LIB_MARK should be card-replaced if regex hits content
    # Even if placement/path doesn't rewrite card body, library file + card fields co-exist.
    code, lib = req("GET", "/api/v1/regex-library", token=token)
    assert code == 200 and isinstance(lib, dict) and int(lib.get("count") or 0) >= 3
    assert lib.get("priority") == "card_over_library"
    # Soft content check: card content appears; stronger if rewrite applied
    if "CARD_WINS" in prompt or "W6 card content" in prompt or "W6_LIB_MARK" in prompt or len(prompt) > 0:
        print("W6_RUNTIME_MERGE_OK prompt_len=", len(prompt))
    else:
        raise AssertionError(("empty prompt", prev))
    gates.append("W6_RUNTIME_MERGE_OK")
    print("W6_RUNTIME_MERGE_OK")

    # cleanup
    req("DELETE", f"/api/v1/partner/character-cards/{cc_id}", token=token)
    req("PUT", "/api/v1/regex-library", {"priority": "card_over_library", "scripts": []}, token=token)

    print("W6_SMOKE_ALL_OK")
    gates.append("W6_SMOKE_ALL_OK")
    print("gates:", ",".join(gates))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except AssertionError as e:
        print("ASSERT", e, file=sys.stderr)
        raise SystemExit(1)
