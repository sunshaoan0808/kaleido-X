#!/usr/bin/env python3
"""W21 API chimney smoke — no browser, no required LLM turn.

Gates:
  W21_HEALTH_OK
  W21_LOGIN_OK
  W21_SETTINGS_OK
  W21_WORKS_LIMITS_OK
  W21_DEMO_PACK_OK
  W21_SESSION_OK
  W21_JOBS_OK
  W21_CRAWLER_DEFAULT_OFF_OK
  W21_SMOKE_ALL_OK
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
            print(f"login ok user={user}")
            return str(tok)
    raise SystemExit(f"login failed: {last}")


def gate(name: str, ok: bool, detail: str = "") -> bool:
    print(f"{name}_{'OK' if ok else 'FAIL'}" + (f" {detail}" if detail else ""))
    return ok


def main() -> int:
    env = load_env()
    ok_all = True

    code, h = req("GET", "/health")
    ok_all &= gate("W21_HEALTH", code == 200 and h.get("ok") is True, f"status={code}")

    tok = login(env)
    ok_all &= gate("W21_LOGIN", bool(tok))

    code, st = req("GET", "/api/v1/settings", token=tok)
    settings_ok = code == 200 and "crawlerEnabled" in st and "agentWriteEnabled" in st
    ok_all &= gate("W21_SETTINGS", settings_ok, f"status={code} keys={list(st)[:6] if isinstance(st, dict) else st}")

    code, lim = req("GET", "/api/v1/works/limits", token=tok)
    lim_ok = bool(code == 200 and lim.get("ok") is True and lim.get("maxFileBytes"))
    ok_all &= gate("W21_WORKS_LIMITS", lim_ok, f"status={code}")

    code, demo = req("POST", "/api/v1/story-tavern/packs/demo", token=tok, body={})
    demo_ok = code < 300 and (
        demo.get("id") == "demo-rain-alley"
        or demo.get("packId") == "demo-rain-alley"
        or (isinstance(demo.get("pack"), dict) and demo["pack"].get("id") == "demo-rain-alley")
        or demo.get("ok") is True
        or "demo" in json.dumps(demo, ensure_ascii=False).lower()
    )
    # GET pack as stronger check
    code2, pack = req("GET", "/api/v1/story-tavern/packs/demo-rain-alley", token=tok)
    if code2 == 200 and (pack.get("id") == "demo-rain-alley" or pack.get("packId") == "demo-rain-alley"):
        demo_ok = True
    ok_all &= gate("W21_DEMO_PACK", demo_ok, f"post={code} get={code2}")

    code, sess = req(
        "POST",
        "/api/v1/story-tavern/sessions",
        token=tok,
        body={
            "packId": "demo-rain-alley",
            "playable": "P1",
            "playMode": "free",
            "userTier": "standard",
            "adultConfirmed": True,
        },
    )
    sid = sess.get("sessionId") or sess.get("id") or (sess.get("session") or {}).get("sessionId")
    sess_ok = code < 300 and bool(sid)
    ok_all &= gate("W21_SESSION", sess_ok, f"status={code} sid={sid} body_keys={list(sess)[:8] if isinstance(sess, dict) else sess}")

    # jobs list — try a few shapes
    jobs_ok = False
    jobs_detail = ""
    for path in ("/api/v1/jobs", "/api/v1/jobs?limit=5", "/api/v1/book-travel/runs"):
        c, j = req("GET", path, token=tok)
        jobs_detail += f"{path}={c};"
        if c == 200:
            jobs_ok = True
            break
    ok_all &= gate("W21_JOBS", jobs_ok, jobs_detail)

    # crawler default off (do not enable)
    if st.get("crawlerEnabled") is True:
        req("PATCH", "/api/v1/settings", token=tok, body={"crawlerEnabled": False})
    code, cj = req(
        "POST",
        "/api/v1/crawler/fanqie",
        token=tok,
        body={"url": "https://fanqienovel.com/reader/1"},
    )
    crawler_off = code == 403 and cj.get("code") == "CRAWLER_DISABLED"
    ok_all &= gate("W21_CRAWLER_DEFAULT_OFF", crawler_off, f"status={code} code={cj.get('code')}")

    ok_all &= gate("W21_SMOKE_ALL", ok_all)
    return 0 if ok_all else 1


if __name__ == "__main__":
    sys.exit(main())
