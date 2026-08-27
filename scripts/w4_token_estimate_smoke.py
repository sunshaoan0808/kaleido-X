#!/usr/bin/env python3
"""W4 token estimate smoke.

Gates:
  W4_ESTIMATE_HEURISTIC_OK
  W4_ESTIMATE_CL100K_OK
  W4_ESTIMATE_BATCH_OK
  W4_SETTINGS_MODE_OK
  W4_WI_PREVIEW_MODE_OK
  W4_BUDGET_MODE_SWITCH_OK
  W4_SMOKE_ALL_OK
"""
from __future__ import annotations

import json
import os
import sys
import time
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


def req(method: str, path: str, body: Any = None, token: str | None = None, timeout: int = 60):
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


def login() -> str:
    load_env()
    if os.environ.get("KALEIDO_TOKEN"):
        return os.environ["KALEIDO_TOKEN"]
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
    for user, password in candidates:
        code, j = req("POST", "/api/v1/auth/login", {"username": user, "password": password})
        last = (code, j)
        tok = j.get("token") or j.get("accessToken") or (j.get("data") or {}).get("token")
        if code < 300 and tok:
            return tok
    raise SystemExit(f"login failed: {last}")


def gate(name: str, ok: bool, detail: str = "") -> None:
    status = "OK" if ok else "FAIL"
    print(f"{name}_{status}" + (f" {detail}" if detail else ""))
    if not ok:
        raise SystemExit(1)


def main() -> None:
    tok = login()
    cjk = "青衣门影刺在星落湖畔留下银色涟漪。"
    latin = "one two three four five six seven eight"

    # 1) heuristic estimate
    code, j = req(
        "POST",
        "/api/v1/tokenize/estimate",
        {"text": cjk, "mode": "heuristic", "breakdown": True},
        token=tok,
    )
    gate(
        "W4_ESTIMATE_HEURISTIC",
        code == 200 and j.get("ok") is True and int(j.get("tokens") or 0) >= 1 and j.get("mode") == "heuristic",
        f"code={code} tokens={j.get('tokens')} mode={j.get('mode')} method={j.get('method')}",
    )
    h_tok = int(j.get("tokens") or 0)

    # 2) cl100k_approx ≥ heuristic for dense CJK
    code, j = req(
        "POST",
        "/api/v1/partner/tokenize/estimate",
        {"text": cjk, "mode": "cl100k_approx", "breakdown": True},
        token=tok,
    )
    c_tok = int(j.get("tokens") or 0)
    gate(
        "W4_ESTIMATE_CL100K",
        code == 200 and j.get("ok") is True and c_tok >= h_tok and j.get("mode") == "cl100k_approx",
        f"code={code} c={c_tok} h={h_tok} mode={j.get('mode')}",
    )

    # 3) batch texts
    code, j = req(
        "POST",
        "/api/v1/tokenize/estimate",
        {"texts": [cjk, latin], "mode": "heuristic"},
        token=tok,
    )
    items = j.get("items") or []
    total = int(j.get("totalTokens") or 0)
    gate(
        "W4_ESTIMATE_BATCH",
        code == 200 and len(items) == 2 and total == sum(int(i.get("tokens") or 0) for i in items) and total >= 6,
        f"code={code} n={len(items)} total={total}",
    )

    # 4) settings mode patch + get
    code, j = req(
        "PATCH",
        "/api/v1/settings",
        {"tokenEstimateMode": "cl100k_approx"},
        token=tok,
    )
    mode_echo = j.get("tokenEstimateMode") or ""
    code2, g = req("GET", "/api/v1/settings", token=tok)
    mode_get = g.get("tokenEstimateMode") or ""
    gate(
        "W4_SETTINGS_MODE",
        code == 200 and mode_echo == "cl100k_approx" and code2 == 200 and mode_get == "cl100k_approx",
        f"patch={code}/{mode_echo} get={code2}/{mode_get}",
    )

    # 5) create wb + entry, wi-preview reports mode
    stamp = int(time.time())
    wb_id = f"wb-w4-tok-{stamp}"
    key_tag = f"W4TOK{stamp}"
    code, wb = req(
        "POST",
        "/api/v1/partner/world-books",
        {
            "id": wb_id,
            "name": f"W4令牌估算-{stamp}",
            "type": "world_book",
            "content": f"# W4 token {stamp}",
            "fields": {},
        },
        token=tok,
    )
    if code >= 300:
        print(f"create wb fail {code} {wb}", file=sys.stderr)
        gate("W4_WI_PREVIEW_MODE_SETUP", False, f"wb create code={code}")
    # upsert returns PartnerItem directly
    wb_id = (wb.get("id") if isinstance(wb, dict) else None) or wb_id
    gate("W4_WI_PREVIEW_MODE_SETUP", bool(wb_id), f"wb create code={code} id={wb_id}")

    code_e, je = req(
        "POST",
        f"/api/v1/partner/world-books/{wb_id}/entries",
        {
            "keys": [key_tag, "星落湖", "青衣"],
            "content": "【设定】青衣门影刺常出没于星落湖畔，刀光如银。",
            "comment": "w4-tok",
            "constant": False,
            "order": 100,
            "position": 0,
        },
        token=tok,
    )
    gate("W4_ENTRY_WRITE", code_e < 400 and (je.get("ok") is True if isinstance(je, dict) else True), f"code={code_e} body={str(je)[:200]}")

    code, prev = req(
        "POST",
        "/api/v1/partner/wi-preview",
        {
            "worldBookId": wb_id,
            "messages": [{"role": "user", "content": f"我在星落湖边看见青衣人。关键词{key_tag}"}],
            "dryRun": True,
            "maxContextTokens": 4096,
            # explicit mode on request should win
            "worldInfoSettings": {"depth": 4, "budgetPct": 25, "tokenEstimateMode": "heuristic"},
        },
        token=tok,
    )
    mode_prev = prev.get("tokenEstimateMode")
    gate(
        "W4_WI_PREVIEW_MODE",
        code == 200 and prev.get("ok") is True and mode_prev == "heuristic",
        f"code={code} mode={mode_prev} budget={prev.get('wiBudgetTokens')} act={prev.get('wiActivated')}",
    )

    # 6) budget mode switch: same scan, mode field changes; both modes return ok
    code_a, a = req(
        "POST",
        "/api/v1/partner/wi-preview",
        {
            "worldBookId": wb_id,
            "messages": [{"role": "user", "content": f"星落湖 青衣 影刺 {key_tag}"}],
            "dryRun": True,
            "maxContextTokens": 2048,
            "worldInfoSettings": {
                "depth": 4,
                "budgetPct": 5,
                "budgetCap": 8,
                "tokenEstimateMode": "heuristic",
            },
        },
        token=tok,
    )
    code_b, b = req(
        "POST",
        "/api/v1/partner/wi-preview",
        {
            "worldBookId": wb_id,
            "messages": [{"role": "user", "content": f"星落湖 青衣 影刺 {key_tag}"}],
            "dryRun": True,
            "maxContextTokens": 2048,
            "worldInfoSettings": {
                "depth": 4,
                "budgetPct": 5,
                "budgetCap": 8,
                "tokenEstimateMode": "cl100k_approx",
            },
        },
        token=tok,
    )
    gate(
        "W4_BUDGET_MODE_SWITCH",
        code_a == 200
        and code_b == 200
        and a.get("tokenEstimateMode") == "heuristic"
        and b.get("tokenEstimateMode") == "cl100k_approx",
        f"a={code_a}/{a.get('tokenEstimateMode')}/ov={a.get('wiOverflowed')} "
        f"b={code_b}/{b.get('tokenEstimateMode')}/ov={b.get('wiOverflowed')}",
    )

    # restore settings default
    req("PATCH", "/api/v1/settings", {"tokenEstimateMode": "heuristic"}, token=tok)

    print("W4_SMOKE_ALL_OK")


if __name__ == "__main__":
    try:
        main()
    except SystemExit:
        raise
    except Exception as e:
        print(f"W4_SMOKE_EXCEPTION {e!r}")
        raise
