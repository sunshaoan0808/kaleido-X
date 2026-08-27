#!/usr/bin/env python3
"""Kaleido S8 stress harness — concurrent hot paths (no LLM stream).

Usage:
  python3 scripts/s8_stress.py [BASE]
Env:
  KALEIDO_ADMIN_USER / KALEIDO_ADMIN_PASSWORD (or .env)
  S8_WORKERS (default 16)
  S8_ROUNDS  (default 3)
  S8_HEALTH_P95_MS (default 80)
  S8_AUTH_P95_MS   (default 200)
  S8_WORKS_P95_MS  (default 400)
Exit 0 on PASS, 1 on FAIL. Prints JSON summary last line as S8_STRESS_JSON=...
"""
from __future__ import annotations

import json
import os
import statistics
import sys
import threading
import time
import urllib.error
import urllib.request
from urllib.parse import quote
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
BASE = sys.argv[1] if len(sys.argv) > 1 else os.environ.get("KALEIDO_BASE", "http://127.0.0.1:18766")
WORKERS = int(os.environ.get("S8_WORKERS", "16"))
ROUNDS = int(os.environ.get("S8_ROUNDS", "3"))
HEALTH_P95 = float(os.environ.get("S8_HEALTH_P95_MS", "80"))
AUTH_P95 = float(os.environ.get("S8_AUTH_P95_MS", "200"))
WORKS_P95 = float(os.environ.get("S8_WORKS_P95_MS", "400"))
AUTHOR_P95 = float(os.environ.get("S8_AUTHOR_P95_MS", "800"))

# load .env
env_path = ROOT / ".env"
if env_path.exists():
    for line in env_path.read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        k, v = line.split("=", 1)
        os.environ.setdefault(k.strip(), v.strip().strip('"').strip("'"))

USER = os.environ.get("KALEIDO_ADMIN_USER", "admin")
PASS = os.environ.get("KALEIDO_ADMIN_PASSWORD", "")
if not PASS:
    print("[FAIL] no KALEIDO_ADMIN_PASSWORD")
    sys.exit(1)

_lock = threading.Lock()
_errors: list[str] = []


def fail(msg: str) -> None:
    with _lock:
        _errors.append(msg)
    print(f"[FAIL] {msg}")


def p95(xs: list[float]) -> float:
    if not xs:
        return 0.0
    s = sorted(xs)
    i = min(len(s) - 1, max(0, int(round(0.95 * (len(s) - 1)))))
    return s[i]


def req(method: str, path: str, token: str | None = None, body: dict | None = None, timeout: float = 30.0):
    data = None if body is None else json.dumps(body).encode()
    headers = {"Content-Type": "application/json"}
    if token:
        headers["Authorization"] = f"Bearer {token}"
    r = urllib.request.Request(BASE + path, data=data, headers=headers, method=method)
    t0 = time.perf_counter()
    try:
        with urllib.request.urlopen(r, timeout=timeout) as resp:
            raw = resp.read().decode()
            ms = (time.perf_counter() - t0) * 1000
            return resp.status, (json.loads(raw) if raw else {}), ms
    except urllib.error.HTTPError as e:
        ms = (time.perf_counter() - t0) * 1000
        raw = e.read().decode(errors="replace")
        try:
            j = json.loads(raw) if raw else {}
        except Exception:
            j = {"raw": raw[:300]}
        return e.code, j, ms


def login() -> tuple[str, float]:
    code, j, ms = req("POST", "/api/v1/auth/login", body={"username": USER, "password": PASS})
    if code != 200 or not j.get("token"):
        raise RuntimeError(f"login {code} {j}")
    return j["token"], ms


def bench_health(n: int) -> list[float]:
    lats: list[float] = []

    def one(_i: int):
        t0 = time.perf_counter()
        with urllib.request.urlopen(BASE + "/health", timeout=5) as resp:
            raw = resp.read()
        ms = (time.perf_counter() - t0) * 1000
        d = json.loads(raw)
        if not d.get("ok"):
            fail(f"health not ok: {d}")
        return ms

    with ThreadPoolExecutor(max_workers=WORKERS) as ex:
        futs = [ex.submit(one, i) for i in range(n)]
        for f in as_completed(futs):
            try:
                lats.append(f.result())
            except Exception as e:
                fail(f"health exc: {e}")
    return lats


def bench_auth(n: int) -> list[float]:
    """Serial logins — concurrent logins trip IP rate limit (by design)."""
    lats: list[float] = []
    n = min(n, 3)
    for i in range(n):
        try:
            _tok, ms = login()
            lats.append(ms)
            time.sleep(0.15)
        except Exception as e:
            # 429 under residual lockout is soft — record and stop
            msg = str(e)
            if "429" in msg or "too many login" in msg:
                print(f"[info] auth rate-limited after {i} ok logins (expected under lockout)")
                break
            fail(f"auth exc: {e}")
    return lats


def bench_works(token: str, n: int) -> list[float]:
    lats: list[float] = []
    # parent dir required by works FS
    code_m, jm, _ = req("POST", "/api/v1/works/dir", token=token, body={"path": "s8-stress"})
    if code_m >= 400 and "exist" not in str(jm).lower():
        # ignore already exists variants
        if "already" not in str(jm).lower() and code_m not in (200, 201, 409):
            # still try — concurrent mkdir may race
            pass

    def one(i: int):
        path = f"s8-stress/w-{os.getpid()}-{i}-{int(time.time()*1000)%100000}.md"
        body = {"path": path, "content": f"# s8 {i}\n\nline\n"}
        code, j, ms = req("PUT", "/api/v1/works/file", token=token, body=body)
        if code >= 400:
            # race: parent missing — mkdir once more
            req("POST", "/api/v1/works/dir", token=token, body={"path": "s8-stress"})
            code, j, ms = req("PUT", "/api/v1/works/file", token=token, body=body)
        if code >= 400:
            fail(f"works put {code} {j}")
            return ms
        code2, j2, ms2 = req(
            "GET",
            f"/api/v1/works/file?path={quote(path)}",
            token=token,
        )
        if code2 >= 400:
            fail(f"works read {code2} {j2}")
        return ms + ms2

    with ThreadPoolExecutor(max_workers=WORKERS) as ex:
        futs = [ex.submit(one, i) for i in range(n)]
        for f in as_completed(futs):
            try:
                lats.append(f.result())
            except Exception as e:
                fail(f"works exc: {e}")
    return lats


def bench_author(token: str, n: int) -> list[float]:
    lats: list[float] = []
    # one character for all
    code, partner, _ = req("GET", "/api/v1/partner", token=token)
    if code >= 400:
        fail(f"partner {code} {partner}")
        return lats
    cards = partner.get("characterCards") or partner.get("character_cards") or []
    if not cards:
        fail("no character cards for author stress")
        return lats
    cc = cards[0]["id"]

    def one(i: int):
        t0 = time.perf_counter()
        code, j, _ = req(
            "POST",
            "/api/v1/author/projects",
            token=token,
            body={
                "title": f"S8-{i}-{int(time.time()*1000)%10000}",
                "characterIds": [cc],
                "livePolicy": {"enabled": True, "everyN": 2, "writeTurns": False},
            },
        )
        proj = j.get("project") or j
        pid = proj.get("id")
        if not pid:
            fail(f"author create {code} {j}")
            return (time.perf_counter() - t0) * 1000
        root = (proj.get("worksRoot") or f"projects/{pid}").rstrip("/")
        path = f"{root}/imports/s8.md"
        req("PUT", "/api/v1/works/file", token=token, body={"path": path, "content": f"# s8 set\n\n设定 {i}\n"})
        code_c, jc, _ = req(
            "POST",
            f"/api/v1/author/projects/{pid}/compose",
            token=token,
            body={"playable": "P1", "characterIds": [cc]},
        )
        if code_c >= 400 or not (jc.get("packId") or (jc.get("project") or {}).get("packId")):
            fail(f"compose {code_c} {jc}")
        code_p, jp, _ = req(
            "POST",
            f"/api/v1/author/projects/{pid}/publish",
            token=token,
            body={"kind": "lore", "path": path},
        )
        if code_p >= 400 or not jp.get("ok"):
            fail(f"publish {code_p} {jp}")
        code_l, jl, _ = req(
            "POST",
            f"/api/v1/author/projects/{pid}/launch",
            token=token,
            body={"playable": "P1", "adultConfirmed": True, "liveEnabled": True, "liveEveryN": 2},
        )
        sid = jl.get("sessionId")
        if code_l >= 400 or not sid:
            fail(f"launch {code_l} {jl}")
            return (time.perf_counter() - t0) * 1000
        code_i, ji, _ = req(
            "POST",
            f"/api/v1/author/projects/{pid}/inject",
            token=token,
            body={"sessionId": sid, "path": path},
        )
        mc = ji.get("messageCount")
        try:
            mc_n = int(mc or 0)
        except Exception:
            mc_n = 0
        if code_i >= 400 or mc_n < 1:
            fail(f"inject {code_i} {ji}")
        return (time.perf_counter() - t0) * 1000

    # author is heavier — cap concurrency
    with ThreadPoolExecutor(max_workers=min(6, WORKERS)) as ex:
        futs = [ex.submit(one, i) for i in range(n)]
        for f in as_completed(futs):
            try:
                lats.append(f.result())
            except Exception as e:
                fail(f"author exc: {e}")
    return lats


def bench_tavern_sessions(token: str, n: int) -> list[float]:
    """Create N sessions concurrently against demo pack (may hit session cap — tolerate + count)."""
    lats: list[float] = []
    ok = 0
    rate_limited = 0

    def one(i: int):
        nonlocal ok, rate_limited
        code, j, ms = req(
            "POST",
            "/api/v1/story-tavern/sessions",
            token=token,
            body={
                "packId": "demo-rain-alley",
                "playable": "P1",
                "playMode": "free",
                "userTier": "standard",
                "adultConfirmed": True,
            },
        )
        if code == 200 and j.get("sessionId"):
            with _lock:
                ok += 1
            return ms
        err = str(j.get("error") or j)
        if "too many" in err.lower() or code == 429:
            with _lock:
                rate_limited += 1
            return ms
        fail(f"session create {code} {j}")
        return ms

    # ensure demo
    req("POST", "/api/v1/story-tavern/packs/demo", token=token, body={})
    with ThreadPoolExecutor(max_workers=min(8, WORKERS)) as ex:
        futs = [ex.submit(one, i) for i in range(n)]
        for f in as_completed(futs):
            try:
                lats.append(f.result())
            except Exception as e:
                fail(f"session exc: {e}")
    print(f"[info] tavern sessions ok={ok} rate_limited={rate_limited} n={n}")
    if ok < 1:
        fail("tavern session create zero success")
    return lats


def main() -> int:
    print(f"[s8] base={BASE} workers={WORKERS} rounds={ROUNDS}")
    # health phase check (accept S7 or S8 during transition)
    code, h, _ = req("GET", "/health")
    # GET without auth via urllib
    with urllib.request.urlopen(BASE + "/health", timeout=5) as resp:
        h = json.loads(resp.read())
    phase = h.get("phase")
    print(f"[s8] health phase={phase} ok={h.get('ok')}")
    if not h.get("ok"):
        fail(f"health {h}")

    summary: dict = {"base": BASE, "phase": phase, "workers": WORKERS, "rounds": ROUNDS}

    # 1 health flood
    n_h = WORKERS * ROUNDS * 4
    hl = bench_health(n_h)
    summary["health"] = {
        "n": len(hl),
        "p50": round(statistics.median(hl), 2) if hl else None,
        "p95": round(p95(hl), 2) if hl else None,
        "max": round(max(hl), 2) if hl else None,
    }
    print(f"[s8] health n={len(hl)} p95={summary['health']['p95']}ms max={summary['health']['max']}ms")
    if hl and p95(hl) > HEALTH_P95:
        fail(f"health p95 {p95(hl):.1f} > {HEALTH_P95}")

    # 2 primary login (single) then light serial auth sample
    # Prefer S8_TOKEN from outer gate so we don't burn login rate budget twice.
    token = os.environ.get("S8_TOKEN", "").strip() or None
    login_ms = 0.0
    if token:
        print("[s8] using S8_TOKEN (skip primary login)")
    else:
        for attempt in range(12):
            try:
                token, login_ms = login()
                break
            except Exception as e:
                msg = str(e)
                if "429" in msg or "too many" in msg:
                    wait = min(30, 3 + attempt * 3)
                    print(f"[info] login lockout, wait {wait}s…")
                    time.sleep(wait)
                    continue
                raise
    if not token:
        fail("could not login after lockout waits")
        summary["errors"] = list(_errors)
        summary["pass"] = False
        print("S8_STRESS_JSON=" + json.dumps(summary, ensure_ascii=False))
        return 1

    # Extra serial logins are optional — skip when reusing gate token or S8_SKIP_AUTH_BENCH=1
    skip_auth_bench = bool(token and os.environ.get("S8_TOKEN")) or os.environ.get("S8_SKIP_AUTH_BENCH") in ("1", "true", "yes")
    if skip_auth_bench:
        al = [login_ms] if login_ms else []
        summary["auth"] = {
            "n": len(al),
            "p50": round(statistics.median(al), 2) if al else None,
            "p95": round(p95(al), 2) if al else None,
            "max": round(max(al), 2) if al else None,
            "note": "auth bench skipped (S8_TOKEN reuse / rate budget)",
        }
        print(f"[s8] auth bench skipped (token reuse)")
    else:
        al = ([login_ms] if login_ms else []) + bench_auth(2)
        summary["auth"] = {
            "n": len(al),
            "p50": round(statistics.median(al), 2) if al else None,
            "p95": round(p95(al), 2) if al else None,
            "max": round(max(al), 2) if al else None,
            "note": "serial logins; concurrent login rate-limit is intentional",
        }
        print(f"[s8] auth n={len(al)} p95={summary['auth']['p95']}ms")
        if al and p95(al) > AUTH_P95 * 2:  # allow slower under bcrypt
            fail(f"auth p95 {p95(al):.1f} > {AUTH_P95*2}")

    # 3 works
    n_w = WORKERS * ROUNDS
    wl = bench_works(token, n_w)
    summary["works"] = {
        "n": len(wl),
        "p50": round(statistics.median(wl), 2) if wl else None,
        "p95": round(p95(wl), 2) if wl else None,
        "max": round(max(wl), 2) if wl else None,
    }
    print(f"[s8] works n={len(wl)} p95={summary['works']['p95']}ms")
    if wl and p95(wl) > WORKS_P95:
        fail(f"works p95 {p95(wl):.1f} > {WORKS_P95}")

    # 4 author closed loop (lighter count)
    n_auth = max(4, min(8, WORKERS // 2))
    au = bench_author(token, n_auth)
    summary["author"] = {
        "n": len(au),
        "p50": round(statistics.median(au), 2) if au else None,
        "p95": round(p95(au), 2) if au else None,
        "max": round(max(au), 2) if au else None,
    }
    print(f"[s8] author n={len(au)} p95={summary['author']['p95']}ms")
    if au and p95(au) > AUTHOR_P95:
        fail(f"author p95 {p95(au):.1f} > {AUTHOR_P95}")

    # 5 tavern sessions concurrent
    tl = bench_tavern_sessions(token, max(6, min(12, WORKERS)))
    summary["tavern_sessions"] = {
        "n": len(tl),
        "p50": round(statistics.median(tl), 2) if tl else None,
        "p95": round(p95(tl), 2) if tl else None,
        "max": round(max(tl), 2) if tl else None,
    }
    print(f"[s8] tavern sessions p95={summary['tavern_sessions']['p95']}ms")

    # final health still ok
    with urllib.request.urlopen(BASE + "/health", timeout=5) as resp:
        h2 = json.loads(resp.read())
    if not h2.get("ok"):
        fail(f"post-stress health {h2}")
    summary["errors"] = list(_errors)
    summary["pass"] = len(_errors) == 0
    print("S8_STRESS_JSON=" + json.dumps(summary, ensure_ascii=False))
    if _errors:
        print(f"[FAIL] S8 stress errors={len(_errors)}")
        return 1
    print("[PASS] S8 stress")
    return 0


if __name__ == "__main__":
    sys.exit(main())
