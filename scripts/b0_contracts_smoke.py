#!/usr/bin/env python3
"""B0 contract pack presence + light live samples (no new features).

Gates:
  B0_DOCS_OK
  B0_API_SURFACE_OK
  B0_HEALTH_OK
  B0_ERROR_SHAPE_OK   (unauth 401 has error)
  B0_SMOKE_ALL_OK
"""
from __future__ import annotations

import json
import os
import sys
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
BASE = os.environ.get("KALEIDO_BASE", "http://127.0.0.1:18766").rstrip("/")

REQUIRED = [
    ROOT / "docs/contracts/B0_CONTRACTS.md",
    ROOT / "docs/contracts/API_SURFACE.md",
    ROOT / "docs/contracts/ERROR_BODY.md",
    ROOT / "docs/contracts/SSE_EVENTS.md",
    ROOT / "docs/contracts/SESSION_DEADLOCK.md",
]


def gate(name: str, ok: bool, detail: str = "") -> bool:
    print(f"{name}_{'OK' if ok else 'FAIL'}" + (f" {detail}" if detail else ""))
    return ok


def req(method: str, path: str, token: str | None = None, body=None, timeout=20):
    data = None
    headers = {}
    if body is not None:
        data = json.dumps(body).encode()
        headers["content-type"] = "application/json"
    if token:
        headers["authorization"] = f"Bearer {token}"
    r = urllib.request.Request(BASE + path, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(r, timeout=timeout) as resp:
            raw = resp.read().decode("utf-8", "replace")
            try:
                j = json.loads(raw) if raw else {}
            except json.JSONDecodeError:
                j = {"_raw": raw}
            return resp.status, j
    except urllib.error.HTTPError as e:
        raw = e.read().decode("utf-8", "replace")
        try:
            j = json.loads(raw) if raw else {}
        except json.JSONDecodeError:
            j = {"_raw": raw}
        return e.code, j


def main() -> int:
    ok_all = True

    missing = [str(p.relative_to(ROOT)) for p in REQUIRED if not p.is_file()]
    ok_all &= gate("B0_DOCS", not missing, f"missing={missing}" if missing else f"n={len(REQUIRED)}")

    surface = ROOT / "docs/contracts/API_SURFACE.md"
    text = surface.read_text(encoding="utf-8") if surface.is_file() else ""
    need = [
        "/api/v1/auth/login",
        "/api/v1/story-tavern/sessions/{id}/turn",
        "/api/v1/jobs/{run_id}/stream",
        "/api/v1/works/limits",
        "/api/v1/background/start",
        "/api/v1/book-travel/pipeline",
    ]
    hits = [p for p in need if p in text]
    ok_all &= gate("B0_API_SURFACE", len(hits) == len(need), f"hits={len(hits)}/{len(need)}")

    code, health = req("GET", "/health")
    ok_all &= gate("B0_HEALTH", code == 200, f"status={code}")

    code, body = req("GET", "/api/v1/me")
    err_ok = code in (401, 403) and isinstance(body, dict) and bool(body.get("error"))
    ok_all &= gate("B0_ERROR_SHAPE", err_ok, f"status={code} body_keys={list(body)[:6] if isinstance(body, dict) else body}")

    if ok_all:
        print("B0_SMOKE_ALL_OK")
        return 0
    print("B0_SMOKE_ALL_FAIL")
    return 1


if __name__ == "__main__":
    sys.exit(main())
