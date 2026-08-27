#!/usr/bin/env python3
"""W10 agent tool policy smoke.

Gates:
  W10_SETTINGS_DEFAULTS_OK
  W10_READ_OK
  W10_WRITE_DEFAULT_OFF_OK
  W10_WRITE_NO_CONFIRM_OK
  W10_WRITE_WITH_CONFIRM_OK
  W10_BASH_DEFAULT_OFF_OK
  W10_BASH_NO_CONFIRM_OK
  W10_BASH_WITH_CONFIRM_OK
  W10_TOOLS_KILL_SWITCH_OK
  W10_SMOKE_ALL_OK
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
SMOKE_PATH = f"state/w10_smoke_{int(time.time())}.txt"


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


def req(method: str, path: str, body: Any = None, token: str | None = None, timeout: int = 30):
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
    candidates += [("admin", "<KALEIDO_PASS>"), ("admin", "admin"), ("admin", "<KALEIDO_PASS>")]
    last = (0, {})
    for user, password in candidates:
        code, j = req("POST", "/api/v1/auth/login", {"username": user, "password": password})
        last = (code, j)
        tok = j.get("token") or j.get("accessToken") or (j.get("data") or {}).get("token")
        if code < 300 and tok:
            print(f"login ok user={user}")
            return tok
    raise SystemExit(f"login failed: {last}")


def gate(name: str, ok: bool, detail: str = "") -> None:
    status = "OK" if ok else "FAIL"
    print(f"{name}_{status}" + (f" {detail}" if detail else ""))
    if not ok:
        raise SystemExit(1)


def patch_settings(tok: str, body: dict) -> dict:
    code, j = req("PATCH", "/api/v1/settings", body, token=tok)
    if code >= 300 or not j.get("ok"):
        raise SystemExit(f"settings patch failed code={code} body={j}")
    return j


def get_settings(tok: str) -> dict:
    code, j = req("GET", "/api/v1/settings", token=tok)
    if code >= 300:
        raise SystemExit(f"settings get failed code={code} body={j}")
    return j


def main() -> None:
    tok = login()

    # Snapshot + restore later
    orig = get_settings(tok)
    restore = {
        "agentToolsEnabled": bool(orig.get("agentToolsEnabled", True)),
        "agentWriteEnabled": bool(orig.get("agentWriteEnabled", False)),
        "agentConfirmDangerous": bool(orig.get("agentConfirmDangerous", True)),
        "bashSandboxEnabled": bool(orig.get("bashSandboxEnabled", False)),
    }

    try:
        # Force known baseline for smoke
        base = patch_settings(
            tok,
            {
                "agentToolsEnabled": True,
                "agentWriteEnabled": False,
                "agentConfirmDangerous": True,
                "bashSandboxEnabled": False,
            },
        )
        gate(
            "W10_SETTINGS_DEFAULTS",
            base.get("agentToolsEnabled") is True
            and base.get("agentWriteEnabled") is False
            and base.get("agentConfirmDangerous") is True
            and base.get("bashSandboxEnabled") is False,
            f"echo={ {k: base.get(k) for k in ('agentToolsEnabled','agentWriteEnabled','agentConfirmDangerous','bashSandboxEnabled')} }",
        )

        # Safe read still works
        code, j = req("POST", "/api/v1/agent/tools/list", {"path": "state"}, token=tok)
        gate(
            "W10_READ",
            code == 200 and (j.get("ok") is True or "entries" in j or "items" in j or isinstance(j, dict)),
            f"code={code} keys={list(j)[:6]}",
        )

        # Write default OFF → 403 agent_write_disabled
        code, j = req(
            "POST",
            "/api/v1/agent/tools/write",
            {"path": SMOKE_PATH, "content": "nope", "confirmDangerous": True},
            token=tok,
        )
        gate(
            "W10_WRITE_DEFAULT_OFF",
            code == 403
            and (
                j.get("error") == "agent_write_disabled"
                or j.get("code") == "AGENT_WRITE_DISABLED"
            ),
            f"code={code} err={j.get('error')} codef={j.get('code')}",
        )

        # Enable write but omit confirm → 403 confirm_required
        patch_settings(tok, {"agentWriteEnabled": True, "agentConfirmDangerous": True})
        code, j = req(
            "POST",
            "/api/v1/agent/tools/write",
            {"path": SMOKE_PATH, "content": "need-confirm"},
            token=tok,
        )
        gate(
            "W10_WRITE_NO_CONFIRM",
            code == 403
            and (j.get("error") == "confirm_required" or j.get("code") == "CONFIRM_REQUIRED"),
            f"code={code} err={j.get('error')}",
        )

        # With confirm → 200
        code, j = req(
            "POST",
            "/api/v1/agent/tools/write",
            {"path": SMOKE_PATH, "content": "w10-ok", "confirmDangerous": True},
            token=tok,
        )
        gate(
            "W10_WRITE_WITH_CONFIRM",
            code == 200 and j.get("ok") is True,
            f"code={code} body={j}",
        )

        # Bash default OFF
        patch_settings(tok, {"bashSandboxEnabled": False, "agentConfirmDangerous": True})
        code, j = req(
            "POST",
            "/api/v1/agent/tools/bash",
            {"command": "echo hi", "confirmDangerous": True},
            token=tok,
        )
        gate(
            "W10_BASH_DEFAULT_OFF",
            code == 403 and j.get("error") == "bash_disabled",
            f"code={code} err={j.get('error')}",
        )

        # Bash on, no confirm
        patch_settings(tok, {"bashSandboxEnabled": True, "agentConfirmDangerous": True})
        code, j = req(
            "POST",
            "/api/v1/agent/tools/bash",
            {"command": "echo hi"},
            token=tok,
        )
        gate(
            "W10_BASH_NO_CONFIRM",
            code == 403
            and (j.get("error") == "confirm_required" or j.get("code") == "CONFIRM_REQUIRED"),
            f"code={code} err={j.get('error')}",
        )

        # Bash on + confirm
        code, j = req(
            "POST",
            "/api/v1/agent/tools/bash",
            {"command": "echo w10", "confirmDangerous": True},
            token=tok,
        )
        gate(
            "W10_BASH_WITH_CONFIRM",
            code == 200 and j.get("ok") is True and "w10" in str(j.get("stdout", "")),
            f"code={code} stdout={j.get('stdout')!r} err={j.get('error')}",
        )

        # Global tools kill switch still blocks read
        patch_settings(tok, {"agentToolsEnabled": False})
        code, j = req("POST", "/api/v1/agent/tools/list", {"path": "state"}, token=tok)
        gate(
            "W10_TOOLS_KILL_SWITCH",
            code == 403
            and (
                j.get("error") == "agent_tools_disabled"
                or j.get("code") == "AGENT_TOOLS_DISABLED"
            ),
            f"code={code} err={j.get('error')}",
        )

        print("W10_SMOKE_ALL_OK")
    finally:
        # Best-effort restore
        try:
            patch_settings(tok, restore)
        except Exception as e:
            print(f"restore warn: {e}", file=sys.stderr)
        # cleanup smoke file if written
        try:
            data_root = Path(os.environ.get("KALEIDO_DATA", str(ROOT / "data")))
            fp = data_root / SMOKE_PATH
            if fp.exists():
                fp.unlink()
        except Exception:
            pass


if __name__ == "__main__":
    main()
