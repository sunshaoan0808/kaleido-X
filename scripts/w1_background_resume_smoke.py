#!/usr/bin/env python3
"""W1+ Background pipeline: checkpoint / multi-char deepen / resume / schema.

Gates:
  W1_START_OK
  W1_CHECKPOINT_OK
  W1_MULTI_CHAR_OK
  W1_RESUME_OK
  W1_SCHEMA_OK
  W1_APPLY_OK
  W1_SMOKE_ALL_OK
"""
from __future__ import annotations

import json
import os
import sys
import time
import urllib.error
import urllib.request

BASE = os.environ.get("KALEIDO_BASE", "http://127.0.0.1:18766").rstrip("/")
USER = os.environ.get("KALEIDO_USER", "admin")
PASS = os.environ.get("KALEIDO_PASS", "<KALEIDO_PASS>")


def req(method: str, path: str, token: str | None = None, body: dict | None = None, timeout: float = 30):
    data = None
    headers = {"Accept": "application/json"}
    if body is not None:
        data = json.dumps(body).encode()
        headers["Content-Type"] = "application/json"
    if token:
        headers["Authorization"] = f"Bearer {token}"
    r = urllib.request.Request(BASE + path, data=data, headers=headers, method=method)
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


def login() -> str:
    code, j = req("POST", "/api/v1/auth/login", body={"username": USER, "password": PASS})
    assert code == 200, f"login {code} {j}"
    tok = j.get("token") or j.get("accessToken") or (j.get("session") or {}).get("token")
    assert tok, j
    return tok


def wait_job(token: str, run_id: str, want_terminal=True, timeout=60.0):
    t0 = time.time()
    last = {}
    while time.time() - t0 < timeout:
        code, j = req("GET", f"/api/v1/background/runs/{run_id}", token=token)
        assert code == 200, f"get_run {code} {j}"
        last = j
        st = j.get("status")
        if want_terminal and st in ("succeeded", "failed", "cancelled"):
            return j
        if not want_terminal and j.get("checkpoint"):
            return j
        time.sleep(0.25)
    return last


def main() -> int:
    fails = []
    token = login()

    # --- full heuristic pipeline with multi deepen ---
    code, j = req(
        "POST",
        "/api/v1/background/start",
        token=token,
        body={
            "mode": "pipeline",
            "title": "W1烟测世界",
            "premise": "雨巷剑客与茶馆老板娘的双线故事，角色：林晚、阿茶、书生周行。",
            "preferHeuristic": True,
            "deepenMax": 3,
            "deepenMode": "all",
            "includeCharacterNames": True,
        },
    )
    if code not in (200, 201, 202) or not (j.get("id") or j.get("runId")):
        fails.append(f"W1_START_OK fail {code} {j}")
        print("FAIL", fails)
        return 1
    run_id = j.get("runId") or j.get("id")
    print("W1_START_OK", run_id)

    done = wait_job(token, run_id, want_terminal=True, timeout=45)
    st = done.get("status")
    result = done.get("result") or {}
    cp = done.get("checkpoint") or (result.get("checkpoint") if isinstance(result, dict) else None)

    if st != "succeeded":
        fails.append(f"pipeline not succeeded: {st} {done.get('error')}")
    else:
        print("pipeline succeeded")

    # schema
    if not isinstance(result, dict) or result.get("schemaVersion") != 1:
        fails.append(f"W1_SCHEMA_OK result.schemaVersion missing: keys={list(result)[:20] if isinstance(result, dict) else type(result)}")
    else:
        print("W1_SCHEMA_OK")

    # multi char
    deepened = result.get("deepenedCount") if isinstance(result, dict) else None
    cards = result.get("characterCards") if isinstance(result, dict) else None
    n_cards = len(cards) if isinstance(cards, list) else 0
    if not (isinstance(deepened, int) and deepened >= 2) and n_cards < 2:
        fails.append(f"W1_MULTI_CHAR_OK expected multi deepen/cards, deepened={deepened} cards={n_cards}")
    else:
        print(f"W1_MULTI_CHAR_OK deepened={deepened} cards={n_cards}")

    # checkpoint present on completed job
    code, gr = req("GET", f"/api/v1/background/runs/{run_id}", token=token)
    if code != 200 or gr.get("schemaVersion") != 1:
        fails.append(f"get_run schema {code} {gr}")
    cp2 = gr.get("checkpoint")
    if not cp2 and not (isinstance(result, dict) and result.get("checkpoint")):
        fails.append(f"W1_CHECKPOINT_OK missing checkpoint on done job")
    else:
        print("W1_CHECKPOINT_OK", (cp2 or {}).get("completed") if isinstance(cp2, dict) else "from-result")

    # --- cancel mid pipeline then resume ---
    code, j2 = req(
        "POST",
        "/api/v1/background/start",
        token=token,
        body={
            "mode": "pipeline",
            "title": "W1续跑世界",
            "premise": "角色：甲、乙、丙。需要断点。",
            "preferHeuristic": True,
            "deepenMax": 3,
            "deepenMode": "all",
        },
    )
    run2 = (j2.get("runId") or j2.get("id")) if code in (200, 201, 202) else None
    if not run2:
        fails.append(f"second start fail {code} {j2}")
    else:
        run2 = str(run2)
        got_cp = False
        t0 = time.time()
        while time.time() - t0 < 25:
            code, mid = req("GET", f"/api/v1/background/runs/{run2}", token=token)
            if code != 200:
                time.sleep(0.1)
                continue
            cp_mid = mid.get("checkpoint") if isinstance(mid, dict) else None
            completed = (cp_mid or {}).get("completed") if isinstance(cp_mid, dict) else None
            st_mid = mid.get("status") if isinstance(mid, dict) else None
            if completed and "stage_one" in completed and st_mid == "running":
                got_cp = True
                req("POST", "/api/v1/background/stop", token=token, body={"id": run2})
                time.sleep(0.5)
                break
            if st_mid in ("succeeded", "failed", "cancelled"):
                break
            time.sleep(0.08)
        code, mid = req("GET", f"/api/v1/background/runs/{run2}", token=token)
        st2 = mid.get("status") if isinstance(mid, dict) else None
        if st2 == "succeeded":
            # fallback: already-done path still validates resume API
            code_r, rj = req(
                "POST",
                f"/api/v1/background/runs/{run2}/resume",
                token=token,
                body={"preferHeuristic": True},
            )
            if code_r == 400 and (
                rj.get("code") == "BG_ALREADY_DONE"
                or "succeeded" in str(rj.get("error", "")).lower()
            ):
                print("W1_RESUME_OK (already-done path; stop raced past)")
            else:
                fails.append(f"W1_RESUME_OK expected BG_ALREADY_DONE got {code_r} {rj}")
        else:
            if st2 == "running":
                req("POST", "/api/v1/background/stop", token=token, body={"id": run2})
                time.sleep(0.5)
                code, mid = req("GET", f"/api/v1/background/runs/{run2}", token=token)
                st2 = mid.get("status") if isinstance(mid, dict) else st2
            code_r, rj = req(
                "POST",
                f"/api/v1/background/runs/{run2}/resume",
                token=token,
                body={"preferHeuristic": True, "deepenMax": 2},
            )
            if code_r != 200 or not (isinstance(rj, dict) and rj.get("resumed")):
                fails.append(f"W1_RESUME_OK resume fail {code_r} {rj} status={st2} mid={mid}")
            else:
                fin = wait_job(token, run2, True, 45)
                if not isinstance(fin, dict) or fin.get("status") != "succeeded":
                    fails.append(
                        f"resume not succeeded {fin.get('status') if isinstance(fin, dict) else fin} {fin.get('error') if isinstance(fin, dict) else ''}"
                    )
                else:
                    res = fin.get("result") if isinstance(fin, dict) else {}
                    if not isinstance(res, dict) or res.get("schemaVersion") != 1:
                        fails.append(f"resume result schema bad {res}")
                    else:
                        print("W1_RESUME_OK", run2, "got_cp=", got_cp, "deepened=", res.get("deepenedCount"))

    # apply
    if isinstance(result, dict) and (result.get("worldBooks") or result.get("characterCards")):
        code, aj = req(
            "POST",
            "/api/v1/background/apply",
            token=token,
            body={"runId": run_id, "result": result, "prefix": "w1-", "select": False},
        )
        if code != 200 or not (isinstance(aj, dict) and aj.get("ok") and aj.get("schemaVersion") == 1):
            fails.append(f"W1_APPLY_OK {code} schema={aj.get('schemaVersion') if isinstance(aj, dict) else None} keys={list(aj)[:12] if isinstance(aj, dict) else type(aj)}")
        else:
            print("W1_APPLY_OK", aj.get("counts"))
    else:
        fails.append("W1_APPLY_OK no result to apply")

    if fails:
        print("FAILS:")
        for f in fails:
            print(" -", f)
        return 1
    print("W1_SMOKE_ALL_OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
