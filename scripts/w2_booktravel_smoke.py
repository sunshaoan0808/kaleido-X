#!/usr/bin/env python3
"""W2 BookTravel pipeline smoke — API only, no UI.

Env:
  KALEIDO_BASE  default http://127.0.0.1:18766
  KALEIDO_USER / KALEIDO_PASS  or read from .env ADMIN_* / first login pair
  KALEIDO_TOKEN optional bearer

Prints BOOKTRAVEL_JOB_OK and/or BOOKTRAVEL_CANCEL_OK on success.
"""
from __future__ import annotations

import json
import os
import sys
import time
import urllib.error
import urllib.parse
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


def req(method, path, token=None, body=None, timeout=60):
    data = None if body is None else json.dumps(body).encode()
    headers = {"Content-Type": "application/json", "Accept": "application/json"}
    if token:
        headers["Authorization"] = f"Bearer {token}"
    r = urllib.request.Request(
        BASE + path, data=data, headers=headers, method=method
    )
    try:
        with urllib.request.urlopen(r, timeout=timeout) as resp:
            raw = resp.read().decode()
            return resp.status, json.loads(raw) if raw else {}
    except urllib.error.HTTPError as e:
        raw = e.read().decode()
        try:
            j = json.loads(raw) if raw else {}
        except Exception:
            j = {"error": raw}
        return e.code, j


def login(env):
    if os.environ.get("KALEIDO_TOKEN"):
        return os.environ["KALEIDO_TOKEN"]
    user = (
        os.environ.get("KALEIDO_USER")
        or os.environ.get("KALEIDO_ADMIN_USER")
        or env.get("KALEIDO_USER")
        or env.get("KALEIDO_ADMIN_USER")
        or env.get("ADMIN_USER")
    )
    pw = (
        os.environ.get("KALEIDO_PASS")
        or os.environ.get("KALEIDO_ADMIN_PASSWORD")
        or env.get("KALEIDO_PASS")
        or env.get("KALEIDO_ADMIN_PASSWORD")
        or env.get("ADMIN_PASS")
    )
    candidates = []
    if user and pw:
        candidates.append((user, pw))
    if env.get("KALEIDO_ADMIN_USER") and env.get("KALEIDO_ADMIN_PASSWORD"):
        candidates.append((env["KALEIDO_ADMIN_USER"], env["KALEIDO_ADMIN_PASSWORD"]))
    code, j = 0, {}
    for u, p in candidates:
        code, j = req("POST", "/api/v1/auth/login", body={"username": u, "password": p})
        if code == 200 and isinstance(j, dict) and (j.get("token") or j.get("accessToken")):
            return j.get("token") or j.get("accessToken")
        if code == 429:
            print("login 429, try prune sessions.json manually", file=sys.stderr)
    raise SystemExit(f"login failed last={code} {j}")


def poll_run(token, run_id, timeout_s=180):
    t0 = time.time()
    last = {}
    while time.time() - t0 < timeout_s:
        code, j = req("GET", f"/api/v1/book-travel/runs/{run_id}", token=token)
        if code != 200:
            code, j = req("GET", f"/api/v1/jobs/{run_id}", token=token)
        last = j
        st = (j.get("status") or "").lower()
        if st in ("succeeded", "failed", "cancelled", "error", "done", "stopped"):
            return j
        time.sleep(0.4)
    return last


def main():
    env = load_env()
    token = login(env)
    ok_job = False
    ok_cancel = False

    # Clear concurrency queue so smoke is not stuck in queued.
    code_ca, ca = req("POST", "/api/v1/jobs/cancel-all", token=token, body={})
    print("cancel-all", code_ca, ca.get("count") if isinstance(ca, dict) else ca)

    # --- cancel: start WITHOUT preferHeuristic so worker is slow enough for stop;
    #     fallback: fill queue + cancel a queued job (stop before run). ---
    code, created2 = req(
        "POST",
        "/api/v1/book-travel/pipeline",
        token=token,
        body={
            "title": "W2取消测",
            "premise": "应被取消的长流水线，请慢慢写。",
            "preferHeuristic": False,
        },
    )
    run2 = (created2.get("runId") or created2.get("id")) if code in (200, 201) else None
    if run2:
        # immediate stop — must beat LLM stages (not heuristic which finishes <50ms)
        c3, stopped = req(
            "POST",
            "/api/v1/book-travel/stop",
            token=token,
            body={"id": run2},
        )
        job2 = poll_run(token, run2, timeout_s=45)
        st2 = (job2.get("status") or stopped.get("status") or "").lower()
        print("cancel status", c3, st2, "progress", job2.get("progress"))
        if st2 in ("cancelled", "stopped"):
            time.sleep(0.4)
            job2b = poll_run(token, run2, timeout_s=5)
            st2b = (job2b.get("status") or "").lower()
            if st2b in ("cancelled", "stopped"):
                print("BOOKTRAVEL_CANCEL_OK")
                ok_cancel = True
            else:
                print("CANCEL_RACE", st2b)
        else:
            print("CANCEL_FAST_PATH_MISS", st2, "— try queued cancel")
            # Fallback: cancel a job that is still queued (never started worker)
            fillers = []
            queued_id = None
            for i in range(8):
                c, cr = req(
                    "POST",
                    "/api/v1/book-travel/pipeline",
                    token=token,
                    body={
                        "title": f"fill-{i}",
                        "premise": "fill concurrency",
                        "preferHeuristic": False,
                    },
                )
                rid = (cr or {}).get("runId")
                if rid:
                    fillers.append(rid)
                    if (cr or {}).get("status") == "queued" or (cr or {}).get("progressMessage") == "queued":
                        queued_id = rid
                        break
                    # re-get
                    _, jx = req("GET", f"/api/v1/book-travel/runs/{rid}", token=token)
                    if (jx.get("status") or "").lower() == "queued":
                        queued_id = rid
                        break
            if queued_id:
                req("POST", "/api/v1/book-travel/stop", token=token, body={"id": queued_id})
                jq = poll_run(token, queued_id, timeout_s=15)
                if (jq.get("status") or "").lower() in ("cancelled", "stopped"):
                    print("BOOKTRAVEL_CANCEL_OK")
                    ok_cancel = True
                else:
                    print("CANCEL_QUEUED_FAIL", jq.get("status"))
            else:
                print("CANCEL_FAIL no queued slot", st2)
            # cleanup fillers
            for rid in fillers:
                req("POST", "/api/v1/book-travel/stop", token=token, body={"id": rid})
            req("POST", "/api/v1/jobs/cancel-all", token=token, body={})
    else:
        print("cancel start fail", code, created2)

    # --- pipeline job (preferHeuristic for deterministic gate) ---
    code, created = req(
        "POST",
        "/api/v1/book-travel/pipeline",
        token=token,
        body={
            "title": "W2烟测雾港",
            "premise": "迷雾港口的契约与一次不被记载的登船。",
            "userInput": "偏冷峻，短场景。",
            "preferHeuristic": True,
        },
        timeout=30,
    )
    if code not in (200, 201):
        print("pipeline start fail", code, created)
        sys.exit(1)
    run_id = created.get("runId") or created.get("id")
    if not run_id:
        print("no runId", created)
        sys.exit(1)
    if not created.get("pipeline") and created.get("mode") != "pipeline":
        print("warn: start response missing pipeline flag", created)

    job = poll_run(token, run_id, timeout_s=120)
    if not isinstance(job, dict):
        print("bad job", job)
        sys.exit(1)
    st = (job.get("status") or "").lower()
    result = job.get("result") if isinstance(job.get("result"), dict) else {}
    print("pipeline status", st, "progress", job.get("progress"), "persisted", result.get("persisted"))
    if st in ("succeeded", "done") and result.get("pipeline") and int(result.get("stageCount") or 0) >= 6:
        wp = result.get("workPath") or result.get("resultPath")
        if wp:
            c, wj = req(
                "GET",
                f"/api/v1/works/file?path={urllib.parse.quote(wp)}",
                token=token,
            )
            content = ""
            if isinstance(wj, dict):
                content = wj.get("content") or wj.get("text") or ""
            print("works read", c, "path", wp, "len", len(content))
            if c == 200 and len(content) > 20 and result.get("persisted") is not False:
                print("BOOKTRAVEL_JOB_OK")
                ok_job = True
            else:
                print("JOB_PERSIST_FAIL", c, result)
        else:
            print("JOB_NO_WORKPATH", result)
    else:
        print("JOB_FAIL", json.dumps(job, ensure_ascii=False)[:1200])

    # list runs smoke
    cl, lst = req("GET", "/api/v1/book-travel/runs?limit=5", token=token)
    items = []
    if isinstance(lst, dict):
        items = lst.get("items") or lst.get("runs") or lst.get("jobs") or []
    print("list_runs", cl, "n", len(items) if isinstance(items, list) else type(lst))

    if not (ok_job and ok_cancel):
        sys.exit(2)
    print("W2_SMOKE_ALL_OK")


if __name__ == "__main__":
    main()
