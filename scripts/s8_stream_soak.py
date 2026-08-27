#!/usr/bin/env python3
"""Kaleido S8+ stream soak — real LLM Story Tavern turns (NOT part of hard gate).

Drains SSE on POST /turn → GET /stream?runId= against live LLM (settings-store
or env). Independent of s8_stress / s8_gate so CPA cost/noise stay out of CI.

Usage:
  python3 scripts/s8_stream_soak.py [BASE]
  # lighter default
  SOAK_SESSIONS=2 SOAK_TURNS=2 SOAK_CONCURRENCY=2 python3 scripts/s8_stream_soak.py
  # heavier
  SOAK_SESSIONS=4 SOAK_TURNS=3 SOAK_CONCURRENCY=2 SOAK_TTFT_P95_MS=45000 \\
    python3 scripts/s8_stream_soak.py

Env:
  KALEIDO_ADMIN_USER / KALEIDO_ADMIN_PASSWORD (or .env)
  S8_TOKEN                 reuse gate/login token (skip login)
  SOAK_SESSIONS            parallel sessions (default 2)
  SOAK_TURNS               sequential turns per session (default 2)
  SOAK_CONCURRENCY         max in-flight turns (default 2; server max_concurrent_jobs=2)
  SOAK_STREAM_TIMEOUT_S    per-turn SSE budget (default 180)
  SOAK_TTFT_P95_MS         first-delta budget (default 60000)
  SOAK_TURN_P95_MS         full-turn budget (default 180000)
  SOAK_MIN_CHARS           min assistant chars for PASS (default 20)
  SOAK_PACK_ID             default demo-rain-alley
  SOAK_PLAYABLE            default P1
  SOAK_PLAY_MODE           default free (no node advance noise)
  SOAK_MESSAGE             base user text (default short Chinese soak line)

Exit 0 on PASS, 1 on FAIL. Last line: S8_STREAM_SOAK_JSON=...
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
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
BASE = sys.argv[1] if len(sys.argv) > 1 else os.environ.get("KALEIDO_BASE", "http://127.0.0.1:18766")
SESSIONS_N = int(os.environ.get("SOAK_SESSIONS", "2"))
TURNS_N = int(os.environ.get("SOAK_TURNS", "2"))
CONCURRENCY = int(os.environ.get("SOAK_CONCURRENCY", "2"))
STREAM_TIMEOUT = float(os.environ.get("SOAK_STREAM_TIMEOUT_S", "180"))
TTFT_P95 = float(os.environ.get("SOAK_TTFT_P95_MS", "60000"))
TURN_P95 = float(os.environ.get("SOAK_TURN_P95_MS", "180000"))
MIN_CHARS = int(os.environ.get("SOAK_MIN_CHARS", "20"))
PACK_ID = os.environ.get("SOAK_PACK_ID", "demo-rain-alley")
PLAYABLE = os.environ.get("SOAK_PLAYABLE", "P1")
PLAY_MODE = os.environ.get("SOAK_PLAY_MODE", "free")
BASE_MSG = os.environ.get(
    "SOAK_MESSAGE",
    "雨巷里继续。沈棠抬眼看你，短句回：今晚先不走。",
)

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

_lock = threading.Lock()
_errors: list[str] = []
_sem = threading.Semaphore(max(1, CONCURRENCY))


def fail(msg: str) -> None:
    with _lock:
        _errors.append(msg)
    print(f"[FAIL] {msg}", flush=True)


def p95(xs: list[float]) -> float:
    if not xs:
        return 0.0
    s = sorted(xs)
    i = min(len(s) - 1, max(0, int(round(0.95 * (len(s) - 1)))))
    return s[i]


def req(
    method: str,
    path: str,
    token: str | None = None,
    body: dict | None = None,
    timeout: float = 30.0,
) -> tuple[int, Any, float]:
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
            j = {"raw": raw[:400]}
        return e.code, j, ms


def login() -> str:
    code, j, _ = req("POST", "/api/v1/auth/login", body={"username": USER, "password": PASS})
    if code != 200 or not j.get("token"):
        raise RuntimeError(f"login {code} {j}")
    return j["token"]


def ensure_demo(token: str) -> None:
    code, j, _ = req("POST", "/api/v1/story-tavern/packs/demo", token=token, body={})
    if code >= 400:
        fail(f"packs/demo {code} {j}")


def create_session(token: str) -> str:
    body = {
        "packId": PACK_ID,
        "playable": PLAYABLE,
        "playMode": PLAY_MODE,
        "userTier": "standard",
        "adultConfirmed": True,
    }
    code, j, _ = req("POST", "/api/v1/story-tavern/sessions", token=token, body=body, timeout=30)
    sid = j.get("sessionId")
    if code >= 400 or not sid:
        raise RuntimeError(f"session create {code} {j}")
    return sid


def get_session(token: str, sid: str) -> dict:
    code, j, _ = req("GET", f"/api/v1/story-tavern/sessions/{sid}", token=token, timeout=15)
    if code >= 400:
        raise RuntimeError(f"session get {code} {j}")
    return j if isinstance(j, dict) else {}


def parse_sse_line(line: str) -> dict | None:
    line = line.strip("\r")
    if not line or line.startswith(":"):
        return None
    data = line[5:].lstrip() if line.startswith("data:") else line
    try:
        obj = json.loads(data)
    except Exception:
        return None
    return obj if isinstance(obj, dict) else None


def drain_stream(token: str, sid: str, run_id: str, timeout_s: float) -> dict:
    """Connect SSE and accumulate deltas until done/error/timeout.

    Returns metrics dict. Late-subscribe (receiver already gone) surfaces as
    type=result subtype=error — caller should fall back to session poll.
    """
    url = f"{BASE}/api/v1/story-tavern/sessions/{sid}/stream?runId={urllib.parse.quote(run_id)}"
    headers = {
        "Accept": "text/event-stream",
        "Authorization": f"Bearer {token}",
        "Cache-Control": "no-store",
    }
    r = urllib.request.Request(url, headers=headers, method="GET")
    t0 = time.perf_counter()
    out: dict[str, Any] = {
        "ok": False,
        "late": False,
        "error": None,
        "deltas": 0,
        "chars": 0,
        "ttft_ms": None,
        "total_ms": None,
        "text": "",
        "events": [],
    }
    text_parts: list[str] = []
    try:
        with urllib.request.urlopen(r, timeout=timeout_s) as resp:
            # Non-SSE JSON fallback (job finished / no receiver)
            ctype = (resp.headers.get("Content-Type") or "").lower()
            if "text/event-stream" not in ctype and "json" in ctype:
                raw = resp.read().decode(errors="replace")
                out["total_ms"] = (time.perf_counter() - t0) * 1000
                try:
                    j = json.loads(raw)
                except Exception:
                    j = {"raw": raw[:300]}
                out["late"] = True
                out["error"] = f"non-sse {j}"
                out["events"].append(j)
                return out

            buf = ""
            deadline = t0 + timeout_s
            while True:
                if time.perf_counter() > deadline:
                    out["error"] = "stream timeout"
                    break
                chunk = resp.read(256)
                if not chunk:
                    break
                buf += chunk.decode("utf-8", errors="replace")
                while "\n" in buf:
                    line, buf = buf.split("\n", 1)
                    obj = parse_sse_line(line)
                    if not obj:
                        continue
                    # ignore wrong run
                    if obj.get("runId") and obj.get("runId") != run_id:
                        continue
                    et = obj.get("type") or obj.get("event_type") or ""
                    out["events"].append(et or list(obj.keys())[:3])

                    # late-subscribe JSON-shaped result on SSE path (rare)
                    if et == "result":
                        out["late"] = True
                        out["error"] = f"result {obj.get('subtype')}:{obj.get('result')}"
                        out["total_ms"] = (time.perf_counter() - t0) * 1000
                        return out

                    if et == "delta" and obj.get("delta"):
                        d = str(obj["delta"])
                        text_parts.append(d)
                        out["deltas"] += 1
                        out["chars"] += len(d)
                        if out["ttft_ms"] is None:
                            out["ttft_ms"] = (time.perf_counter() - t0) * 1000
                    elif et == "thinking_delta":
                        # count but don't require content chars
                        out["deltas"] += 1
                        if out["ttft_ms"] is None:
                            out["ttft_ms"] = (time.perf_counter() - t0) * 1000
                    elif et == "done":
                        out["ok"] = True
                        out["total_ms"] = (time.perf_counter() - t0) * 1000
                        out["text"] = "".join(text_parts)
                        return out
                    elif et == "error":
                        out["error"] = obj.get("message") or "stream error event"
                        out["total_ms"] = (time.perf_counter() - t0) * 1000
                        out["text"] = "".join(text_parts)
                        return out
    except Exception as e:
        out["error"] = f"stream exc: {e}"
        out["total_ms"] = (time.perf_counter() - t0) * 1000
        out["text"] = "".join(text_parts)
        return out

    out["total_ms"] = (time.perf_counter() - t0) * 1000
    out["text"] = "".join(text_parts)
    if out["chars"] > 0 and out["error"] is None:
        # stream closed without explicit done — treat as soft ok if we got text
        out["ok"] = True
    elif out["error"] is None:
        out["error"] = "stream ended without done"
    return out


# urllib.parse used in drain_stream
import urllib.parse  # noqa: E402  (kept near drain for clarity of quote)


def poll_turn_complete(
    token: str,
    sid: str,
    turn_before: int,
    timeout_s: float = 120.0,
) -> dict:
    """Fallback when SSE missed: wait until turn advances + assistant msg grows."""
    t0 = time.perf_counter()
    last: dict = {}
    while time.perf_counter() - t0 < timeout_s:
        try:
            last = get_session(token, sid)
        except Exception as e:
            time.sleep(1.5)
            last = {"_err": str(e)}
            continue
        turn = int(last.get("turn") or 0)
        active = last.get("activeRunId") or last.get("active_run_id")
        msgs = last.get("messages") or []
        asst = next((m for m in reversed(msgs) if m.get("role") == "assistant"), None)
        chars = len((asst or {}).get("content") or "") if asst else 0
        if turn > turn_before and active in (None, "") and chars >= MIN_CHARS:
            return {
                "ok": True,
                "late_ok": True,
                "turn": turn,
                "chars": chars,
                "total_ms": (time.perf_counter() - t0) * 1000,
                "text": ((asst or {}).get("content") or "")[:200],
            }
        time.sleep(1.5)
    return {
        "ok": False,
        "late_ok": False,
        "turn": last.get("turn"),
        "chars": 0,
        "total_ms": (time.perf_counter() - t0) * 1000,
        "error": f"poll timeout last={ {k: last.get(k) for k in ('turn','activeRunId')} }",
    }


def one_turn(token: str, sid: str, turn_idx: int, session_idx: int) -> dict:
    """Run one real LLM turn with stream drain + poll fallback. Honours concurrency sem."""
    with _sem:
        msg = f"{BASE_MSG} [s{session_idx} t{turn_idx} {int(time.time())%10000}]"
        try:
            before = get_session(token, sid)
            turn_before = int(before.get("turn") or 0)
            # wait if previous run still sticky
            for _ in range(40):
                active = before.get("activeRunId") or before.get("active_run_id")
                if not active:
                    break
                time.sleep(1.5)
                before = get_session(token, sid)
                turn_before = int(before.get("turn") or 0)
        except Exception as e:
            fail(f"pre-turn get s{session_idx}: {e}")
            return {"ok": False, "error": str(e), "session": session_idx, "turn_idx": turn_idx}

        t0 = time.perf_counter()
        code, j, start_ms = req(
            "POST",
            f"/api/v1/story-tavern/sessions/{sid}/turn",
            token=token,
            body={"message": msg},
            timeout=60,
        )
        run_id = j.get("runId")
        if code >= 400 or not run_id:
            fail(f"turn start s{session_idx}t{turn_idx} {code} {j}")
            return {
                "ok": False,
                "error": f"turn start {code}",
                "start_ms": start_ms,
                "session": session_idx,
                "turn_idx": turn_idx,
            }

        # stream ASAP — receiver is single-shot
        stream = drain_stream(token, sid, run_id, STREAM_TIMEOUT)
        # Server sends SSE "done" BEFORE session turn++ / assistant append
        # (story_tavern.rs spawn). Always poll for durable turn commit.
        used_poll = True
        poll = poll_turn_complete(
            token,
            sid,
            turn_before,
            timeout_s=min(90.0, max(15.0, STREAM_TIMEOUT * 0.5)),
        )
        turn_after = turn_before
        if poll.get("ok"):
            stream["ok"] = True
            stream["poll_ok"] = True
            stream["chars"] = max(int(stream.get("chars") or 0), int(poll.get("chars") or 0))
            if stream.get("ttft_ms") is None:
                stream["ttft_unknown"] = True
            stream["error"] = None
            if not stream.get("text"):
                stream["text"] = poll.get("text") or ""
            turn_after = int(poll.get("turn") or (turn_before + 1))
        else:
            # last-chance session read (race with post-turn save)
            try:
                after_soft = get_session(token, sid)
                turn_after = int(after_soft.get("turn") or 0)
                msgs = after_soft.get("messages") or []
                asst = next(
                    (m for m in reversed(msgs) if m.get("role") == "assistant"),
                    None,
                )
                if asst:
                    stream["chars"] = max(
                        int(stream.get("chars") or 0),
                        len(asst.get("content") or ""),
                    )
                    if not stream.get("text"):
                        stream["text"] = (asst.get("content") or "")[:200]
            except Exception:
                turn_after = turn_before
            if turn_after <= turn_before or int(stream.get("chars") or 0) < MIN_CHARS:
                stream["ok"] = False
                stream["error"] = (
                    stream.get("error")
                    or poll.get("error")
                    or "turn not committed"
                )
            else:
                stream["ok"] = True
                stream["error"] = None
        stream["total_ms"] = (time.perf_counter() - t0) * 1000

        # final snapshot for chars / turn
        try:
            after = get_session(token, sid)
            turn_after = max(turn_after, int(after.get("turn") or 0))
            msgs = after.get("messages") or []
            asst = next((m for m in reversed(msgs) if m.get("role") == "assistant"), None)
            if asst:
                stream["chars"] = max(
                    int(stream.get("chars") or 0),
                    len(asst.get("content") or ""),
                )
                if not stream.get("text"):
                    stream["text"] = (asst.get("content") or "")[:200]
        except Exception:
            pass

        ok = (
            bool(stream.get("ok"))
            and turn_after > turn_before
            and int(stream.get("chars") or 0) >= MIN_CHARS
        )
        if not ok:
            fail(
                f"turn s{session_idx}t{turn_idx} ok={stream.get('ok')} "
                f"turn {turn_before}->{turn_after} chars={stream.get('chars')} "
                f"err={stream.get('error')}"
            )

        _total = stream.get("total_ms")
        if _total is None:
            _total = (time.perf_counter() - t0) * 1000
        _ttft = stream.get("ttft_ms")
        row = {
            "ok": ok,
            "session": session_idx,
            "turn_idx": turn_idx,
            "sid": sid,
            "run_id": run_id,
            "start_ms": round(float(start_ms), 2),
            "ttft_ms": round(float(_ttft), 2) if _ttft is not None else None,
            "total_ms": round(float(_total), 2),
            "deltas": stream.get("deltas") or 0,
            "chars": stream.get("chars") or 0,
            "used_poll": used_poll,
            "late": bool(stream.get("late")),
            "turn_before": turn_before,
            "turn_after": turn_after,
            "preview": (stream.get("text") or "")[:80],
            "error": stream.get("error"),
        }
        tag = "OK" if ok else "FAIL"
        print(
            f"[soak] {tag} s{session_idx}t{turn_idx} ttft={row['ttft_ms']}ms "
            f"total={row['total_ms']}ms deltas={row['deltas']} chars={row['chars']} "
            f"poll={used_poll}",
            flush=True,
        )
        return row


def run_session(token: str, session_idx: int) -> list[dict]:
    try:
        sid = create_session(token)
    except Exception as e:
        fail(f"create session s{session_idx}: {e}")
        return [{"ok": False, "session": session_idx, "error": str(e)}]
    print(f"[soak] session s{session_idx}={sid}", flush=True)
    rows = []
    for t in range(TURNS_N):
        rows.append(one_turn(token, sid, t, session_idx))
        # small gap so post-turn extraction/embed doesn't stampede job slots
        time.sleep(0.4)
    return rows


def main() -> int:
    print(
        f"[soak] base={BASE} sessions={SESSIONS_N} turns={TURNS_N} "
        f"concurrency={CONCURRENCY} pack={PACK_ID} mode={PLAY_MODE}",
        flush=True,
    )

    # health
    try:
        with urllib.request.urlopen(BASE + "/health", timeout=5) as resp:
            health = json.loads(resp.read())
    except Exception as e:
        fail(f"health {e}")
        print("S8_STREAM_SOAK_JSON=" + json.dumps({"pass": False, "errors": list(_errors)}))
        return 1
    print(
        f"[soak] health ok={health.get('ok')} phase={health.get('phase')} "
        f"max_jobs={health.get('max_concurrent_jobs')} llm={health.get('llm_configured')}",
        flush=True,
    )
    if not health.get("ok"):
        fail(f"health not ok {health}")
    if not health.get("llm_configured"):
        fail("llm_configured=false")

    if not PASS and not os.environ.get("S8_TOKEN"):
        fail("no KALEIDO_ADMIN_PASSWORD / S8_TOKEN")
        print("S8_STREAM_SOAK_JSON=" + json.dumps({"pass": False, "errors": list(_errors)}))
        return 1

    token = os.environ.get("S8_TOKEN", "").strip() or None
    if token:
        print("[soak] using S8_TOKEN", flush=True)
    else:
        for attempt in range(10):
            try:
                token = login()
                break
            except Exception as e:
                msg = str(e)
                if "429" in msg or "too many" in msg:
                    wait = min(30, 3 + attempt * 3)
                    print(f"[info] login lockout, wait {wait}s…", flush=True)
                    time.sleep(wait)
                    continue
                fail(f"login {e}")
                break
    if not token:
        print("S8_STREAM_SOAK_JSON=" + json.dumps({"pass": False, "errors": list(_errors)}))
        return 1

    ensure_demo(token)

    t_wall0 = time.perf_counter()
    all_rows: list[dict] = []
    # sessions run in parallel; turns inside each session are sequential
    with ThreadPoolExecutor(max_workers=max(1, SESSIONS_N)) as ex:
        futs = [ex.submit(run_session, token, i) for i in range(SESSIONS_N)]
        for f in as_completed(futs):
            try:
                all_rows.extend(f.result())
            except Exception as e:
                fail(f"session worker exc: {e}")

    wall_ms = (time.perf_counter() - t_wall0) * 1000
    ok_rows = [r for r in all_rows if r.get("ok")]
    bad_rows = [r for r in all_rows if not r.get("ok")]
    ttfts = [r["ttft_ms"] for r in ok_rows if r.get("ttft_ms") is not None]
    totals = [r["total_ms"] for r in ok_rows if r.get("total_ms") is not None]
    chars = [r.get("chars") or 0 for r in ok_rows]
    poll_n = sum(1 for r in all_rows if r.get("used_poll"))

    summary: dict[str, Any] = {
        "base": BASE,
        "phase": health.get("phase"),
        "sessions": SESSIONS_N,
        "turns_per_session": TURNS_N,
        "concurrency": CONCURRENCY,
        "planned_turns": SESSIONS_N * TURNS_N,
        "completed_ok": len(ok_rows),
        "failed": len(bad_rows),
        "wall_ms": round(wall_ms, 2),
        "ttft": {
            "n": len(ttfts),
            "p50": round(statistics.median(ttfts), 2) if ttfts else None,
            "p95": round(p95(ttfts), 2) if ttfts else None,
            "max": round(max(ttfts), 2) if ttfts else None,
        },
        "turn_total": {
            "n": len(totals),
            "p50": round(statistics.median(totals), 2) if totals else None,
            "p95": round(p95(totals), 2) if totals else None,
            "max": round(max(totals), 2) if totals else None,
        },
        "chars": {
            "n": len(chars),
            "p50": round(statistics.median(chars), 2) if chars else None,
            "min": min(chars) if chars else None,
            "max": max(chars) if chars else None,
        },
        "used_poll_fallback": poll_n,
        "rows": all_rows,
    }

    # gates
    if len(ok_rows) < SESSIONS_N * TURNS_N:
        fail(f"ok turns {len(ok_rows)} < planned {SESSIONS_N * TURNS_N}")
    if ttfts and p95(ttfts) > TTFT_P95:
        fail(f"ttft p95 {p95(ttfts):.0f} > {TTFT_P95}")
    if totals and p95(totals) > TURN_P95:
        fail(f"turn total p95 {p95(totals):.0f} > {TURN_P95}")

    # post health still ok
    try:
        with urllib.request.urlopen(BASE + "/health", timeout=5) as resp:
            h2 = json.loads(resp.read())
        if not h2.get("ok"):
            fail(f"post-soak health {h2}")
        summary["post_health"] = {
            "ok": h2.get("ok"),
            "running_jobs": h2.get("running_jobs"),
            "queued_jobs": h2.get("queued_jobs"),
        }
    except Exception as e:
        fail(f"post health {e}")

    summary["errors"] = list(_errors)
    summary["pass"] = len(_errors) == 0
    print(
        f"[soak] ok={len(ok_rows)}/{SESSIONS_N * TURNS_N} "
        f"ttft_p95={summary['ttft']['p95']} turn_p95={summary['turn_total']['p95']} "
        f"wall={summary['wall_ms']}ms poll_fallback={poll_n}",
        flush=True,
    )
    print("S8_STREAM_SOAK_JSON=" + json.dumps(summary, ensure_ascii=False), flush=True)
    if _errors:
        print(f"[FAIL] S8 stream soak errors={len(_errors)}", flush=True)
        return 1
    print("[PASS] S8 stream soak", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
