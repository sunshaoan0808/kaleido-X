#!/usr/bin/env python3
"""W19 crawler diagnostic smoke — no live fanqie fetch.

Gates:
  W19_DEFAULT_OFF_OK
  W19_DISABLED_CODE_OK
  W19_BAD_REQUEST_OK
  W19_UNSUPPORTED_HOST_OK
  W19_SETTINGS_FIELD_OK
  W19_SMOKE_ALL_OK
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


def load_env():
    env = {}
    for p in (ROOT / ".env", Path("${HOME}/.env")):
        if not p.is_file():
            continue
        for line in p.read_text().splitlines():
            if "=" in line and not line.strip().startswith("#"):
                k, v = line.split("=", 1)
                env[k.strip()] = v.strip().strip('"').strip("'")
    return env


def req(method, path, token=None, body=None, timeout=30):
    data = None if body is None else json.dumps(body).encode()
    headers = {"Content-Type": "application/json", "Accept": "application/json"}
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
            j = {"_raw": raw}
        return e.code, j


def login(env) -> str:
    if os.environ.get("KALEIDO_TOKEN"):
        return os.environ["KALEIDO_TOKEN"]
    candidates = []
    u = os.environ.get("KALEIDO_USER") or env.get("KALEIDO_ADMIN_USER") or env.get("ADMIN_USER")
    pw = (
        os.environ.get("KALEIDO_PASS")
        or env.get("KALEIDO_ADMIN_PASSWORD")
        or env.get("ADMIN_PASS")
    )
    if u and pw:
        candidates.append((u, pw))
    candidates += [("admin", "<KALEIDO_PASS>"), ("admin", "admin")]
    last = (0, {})
    for user, password in candidates:
        code, j = req("POST", "/api/v1/auth/login", body={"username": user, "password": password})
        last = (code, j)
        tok = j.get("token") or j.get("accessToken")
        if code == 200 and tok:
            return str(tok)
    raise SystemExit(f"login failed: {last}")


def gate(name: str, ok: bool, detail: str = "") -> bool:
    print(f"{name}_{'OK' if ok else 'FAIL'}" + (f" {detail}" if detail else ""))
    return ok


def main() -> int:
    env = load_env()
    tok = login(env)
    ok_all = True

    # Ensure default-off for test (restore false if someone left it on)
    code, st = req("GET", "/api/v1/settings", token=tok)
    field_ok = code == 200 and "crawlerEnabled" in st
    ok_all &= gate("W19_SETTINGS_FIELD", field_ok, f"status={code}")
    if st.get("crawlerEnabled") is True:
        req("PATCH", "/api/v1/settings", token=tok, body={"crawlerEnabled": False})
        code, st = req("GET", "/api/v1/settings", token=tok)

    default_off = st.get("crawlerEnabled") is False
    ok_all &= gate("W19_DEFAULT_OFF", default_off, f"crawlerEnabled={st.get('crawlerEnabled')}")

    code, j = req(
        "POST",
        "/api/v1/crawler/fanqie",
        token=tok,
        body={"url": "https://fanqienovel.com/reader/1"},
    )
    dis_ok = (
        code == 403
        and j.get("code") == "CRAWLER_DISABLED"
        and j.get("ok") is False
        and j.get("defaultOff") is True
        and "hint" in j
    )
    ok_all &= gate("W19_DISABLED_CODE", dis_ok, f"status={code} body={j}")

    code, j = req("POST", "/api/v1/crawler/fanqie", token=tok, body={})
    # still disabled first — but body missing url should still be disabled gate
    # enable briefly only for bad-request / host tests
    req("PATCH", "/api/v1/settings", token=tok, body={"crawlerEnabled": True})
    try:
        code, j = req("POST", "/api/v1/crawler/fanqie", token=tok, body={})
        bad_ok = code == 400 and j.get("code") == "CRAWLER_BAD_REQUEST"
        ok_all &= gate("W19_BAD_REQUEST", bad_ok, f"status={code} body={j}")

        code, j = req(
            "POST",
            "/api/v1/crawler/fanqie",
            token=tok,
            body={"url": "https://example.com/foo"},
        )
        host_ok = (
            code == 400
            and j.get("code") == "CRAWLER_UNSUPPORTED_HOST"
            and j.get("retryable") is False
            and j.get("stage") == "ssrf_guard"
        )
        ok_all &= gate("W19_UNSUPPORTED_HOST", host_ok, f"status={code} body={j}")
    finally:
        req("PATCH", "/api/v1/settings", token=tok, body={"crawlerEnabled": False})

    ok_all &= gate("W19_SMOKE_ALL", ok_all)
    return 0 if ok_all else 1


if __name__ == "__main__":
    sys.exit(main())
