#!/usr/bin/env python3
"""W12 auth session cap smoke.

Gates:
  W12_STATS_OK
  W12_SETTINGS_CAP_OK
  W12_PRUNE_EXPIRED_OK
  W12_PRUNE_OLDEST_OK
  W12_AUTO_EVICT_OK
  W12_REJECT_CAP_OK
  W12_SMOKE_ALL_OK

Note: login rate window (default 10/300s) is in-memory. Cap flood tests
optionally restart kaleido-server once to clear the map (W12_RESTART=1 default).
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any

BASE = os.environ.get("KALEIDO_BASE", "http://127.0.0.1:18766").rstrip("/")
ROOT = Path(__file__).resolve().parents[1]
DO_RESTART = os.environ.get("W12_RESTART", "1") not in ("0", "false", "no")


def load_env() -> None:
    for p in [
        ROOT / ".env",
        ROOT / "data" / "Kaleido" / ".env",
        Path("${HOME}/.env"),
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


def req(
    method: str,
    path: str,
    body: Any = None,
    token: str | None = None,
    timeout: int = 60,
):
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


def creds() -> list[tuple[str, str]]:
    load_env()
    out: list[tuple[str, str]] = []
    u = os.environ.get("KALEIDO_ADMIN_USER") or os.environ.get("KALEIDO_USER")
    pw = (
        os.environ.get("KALEIDO_ADMIN_PASS")
        or os.environ.get("KALEIDO_ADMIN_PASSWORD")
        or os.environ.get("KALEIDO_PASSWORD")
    )
    if u and pw:
        out.append((u, pw))
    out += [("admin", "admin"), ("aiclaw", "Aa123151")]
    # dedupe
    seen = set()
    uniq = []
    for c in out:
        if c not in seen:
            seen.add(c)
            uniq.append(c)
    return uniq


def login() -> str:
    if os.environ.get("KALEIDO_TOKEN"):
        return os.environ["KALEIDO_TOKEN"]
    last = (0, {})
    for user, pwd in creds():
        code, j = req("POST", "/api/v1/auth/login", {"username": user, "password": pwd})
        last = (code, j)
        if code == 200 and j.get("token"):
            return str(j["token"])
        if code == 429 and "login attempts" in str(j.get("error", "")):
            time.sleep(1.5)
    raise SystemExit(f"login failed: {last}")


def wait_health(timeout: float = 30.0) -> None:
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            code, j = req("GET", "/health", timeout=3)
            if code == 200 and j.get("ok"):
                return
        except Exception:
            pass
        time.sleep(0.4)
    raise SystemExit("health not ready after restart")


def restart_server() -> None:
    """Clear in-memory login rate map so cap flood tests are not RateLimited."""
    if not DO_RESTART:
        print("W12_RESTART=0 — skip restart (rate window may block flood)", file=sys.stderr)
        return
    r = subprocess.run(
        ["systemctl", "restart", "kaleido-server"],
        capture_output=True,
        text=True,
    )
    if r.returncode != 0:
        print(f"systemctl restart warn: {r.stderr or r.stdout}", file=sys.stderr)
    wait_health()


def gate(name: str, ok: bool, detail: str = "") -> None:
    if ok:
        print(name)
    else:
        print(f"FAIL {name}: {detail}", file=sys.stderr)
        raise SystemExit(1)


def main() -> None:
    load_env()
    tok = login()

    code, stats = req("GET", "/api/v1/sessions/stats", token=tok)
    gate(
        "W12_STATS_OK",
        code == 200
        and stats.get("ok") is True
        and isinstance(stats.get("active"), int)
        and isinstance(stats.get("cap"), int)
        and int(stats.get("cap") or 0) >= 1
        and "policy" in stats
        and "actions" in stats,
        f"status={code} body={stats}",
    )
    orig_cap = int(stats["cap"])
    orig_policy = str(stats.get("policy") or "auto_evict")

    # settings: patch cap + policy, verify stats
    test_cap = max(5, min(orig_cap, 20))
    code, sp = req(
        "PATCH",
        "/api/v1/settings",
        {"sessionMax": test_cap, "sessionCapPolicy": "auto_evict"},
        token=tok,
    )
    code2, stats2 = req("GET", "/api/v1/sessions/stats", token=tok)
    code3, sget = req("GET", "/api/v1/settings", token=tok)
    sm = sget.get("sessionMax")
    spc = sget.get("sessionCapPolicy")
    gate(
        "W12_SETTINGS_CAP_OK",
        code == 200
        and sp.get("ok") is True
        and code2 == 200
        and int(stats2.get("cap", -1)) == test_cap
        and stats2.get("policy") == "auto_evict"
        and code3 == 200
        and int(sm or 0) == test_cap
        and spc == "auto_evict"
        and sp.get("sessionMax") == test_cap,
        f"patch={code}/{sp} stats={stats2} settings={sget}",
    )

    code, pr = req("POST", "/api/v1/sessions/prune", {"mode": "expired"}, token=tok)
    gate(
        "W12_PRUNE_EXPIRED_OK",
        code == 200 and pr.get("ok") is True and "removed" in pr and "active" in pr,
        f"{code} {pr}",
    )

    # one extra session then prune oldest — only +1 login to spare rate budget
    t2 = login()
    code, pr2 = req(
        "POST",
        "/api/v1/sessions/prune",
        {"mode": "oldest", "count": 1},
        token=tok if tok != t2 else t2,
    )
    if code == 401:
        tok = login()
        code, pr2 = req(
            "POST",
            "/api/v1/sessions/prune",
            {"mode": "oldest", "count": 1},
            token=tok,
        )
    gate(
        "W12_PRUNE_OLDEST_OK",
        code == 200 and pr2.get("ok") is True and isinstance(pr2.get("removed"), int),
        f"{code} {pr2}",
    )

    # --- rate window may be warm; restart once before flood ---
    # Persist desired test settings before restart (settings-store file)
    tok = login()
    req(
        "PATCH",
        "/api/v1/settings",
        {"sessionMax": 2, "sessionCapPolicy": "auto_evict"},
        token=tok,
    )
    restart_server()
    # boot reloads sessionMax from settings-store
    tok = login()
    code, st0 = req("GET", "/api/v1/sessions/stats", token=tok)
    gate(
        "W12_AUTO_EVICT_OK",
        code == 200 and int(st0.get("cap", -1)) == 2 and st0.get("policy") == "auto_evict",
        f"post-restart stats={st0}",
    )

    ok_logins = 0
    last_login: tuple[int, Any] = (0, {})
    # 4 logins under cap=2 auto_evict — all should be 200 (evict oldest)
    user, pwd = creds()[0]
    for _ in range(4):
        last_login = req("POST", "/api/v1/auth/login", {"username": user, "password": pwd})
        if last_login[0] != 200:
            # try next cred
            for u2, p2 in creds()[1:]:
                last_login = req("POST", "/api/v1/auth/login", {"username": u2, "password": p2})
                if last_login[0] == 200:
                    break
        if last_login[0] == 200 and last_login[1].get("token"):
            ok_logins += 1
            tok = str(last_login[1]["token"])
        time.sleep(0.15)

    code, st_ae = req("GET", "/api/v1/sessions/stats", token=tok)
    if code == 401:
        tok = login()
        code, st_ae = req("GET", "/api/v1/sessions/stats", token=tok)
    gate(
        "W12_AUTO_EVICT_OK",
        ok_logins >= 3
        and last_login[0] == 200
        and code == 200
        and int(st_ae.get("active", 99)) <= int(st_ae.get("cap", 0))
        and int(st_ae.get("cap", 0)) == 2,
        f"ok_logins={ok_logins} last={last_login} stats={st_ae}",
    )

    # REJECT: cap=1, keep current session, next login → SESSION_CAP
    code, _ = req(
        "PATCH",
        "/api/v1/settings",
        {"sessionMax": 1, "sessionCapPolicy": "reject"},
        token=tok,
    )
    if code == 401:
        tok = login()
        req(
            "PATCH",
            "/api/v1/settings",
            {"sessionMax": 1, "sessionCapPolicy": "reject"},
            token=tok,
        )
    # ensure active>=1
    code, st_r = req("GET", "/api/v1/sessions/stats", token=tok)
    if code == 401:
        tok = login()
        code, st_r = req("GET", "/api/v1/sessions/stats", token=tok)
    # if free slots (active=0 somehow), one login to fill
    if int(st_r.get("active") or 0) < 1:
        tok = login()

    code_r, body_r = req(
        "POST",
        "/api/v1/auth/login",
        {"username": user, "password": pwd},
    )
    # if first cred is same user and somehow replaced — try alternate user
    if code_r == 200:
        for u2, p2 in creds():
            if (u2, p2) == (user, pwd):
                continue
            code_r, body_r = req(
                "POST",
                "/api/v1/auth/login",
                {"username": u2, "password": p2},
            )
            if code_r == 429:
                break
        if code_r == 200:
            # still succeeded — fill until reject (cap=1 should block 2nd distinct)
            code_r, body_r = req(
                "POST",
                "/api/v1/auth/login",
                {"username": user, "password": pwd},
            )

    gate(
        "W12_REJECT_CAP_OK",
        code_r == 429
        and body_r.get("code") == "SESSION_CAP"
        and isinstance(body_r.get("actions"), list)
        and len(body_r.get("actions") or []) >= 1
        and int(body_r.get("cap") or 0) == 1
        and body_r.get("policy") == "reject",
        f"status={code_r} body={body_r}",
    )

    # restore original cap/policy for live host
    # may need login if only reject path left us without new token — use existing tok
    code, _ = req(
        "PATCH",
        "/api/v1/settings",
        {"sessionMax": orig_cap, "sessionCapPolicy": orig_policy},
        token=tok,
    )
    if code == 401:
        # under reject cap=1 we still have tok from before
        # if tok dead, login might SESSION_CAP — prune via file? restart + login
        restart_server()
        # after restart sessions reloaded; settings still reject/1 until patch
        # temporarily: write settings via second chance — login may work if sessions expired empty
        try:
            tok = login()
        except SystemExit:
            # force prune sessions file then restart
            spath = ROOT / "data" / "state" / "sessions.json"
            if spath.exists():
                bak = spath.with_suffix(".json.bak_w12_smoke")
                spath.replace(bak)
                spath.write_text("{}")
            restart_server()
            tok = login()
        req(
            "PATCH",
            "/api/v1/settings",
            {"sessionMax": orig_cap, "sessionCapPolicy": orig_policy},
            token=tok,
        )
    req("POST", "/api/v1/sessions/prune", {"mode": "expired"}, token=tok)

    print("W12_SMOKE_ALL_OK")


if __name__ == "__main__":
    main()
