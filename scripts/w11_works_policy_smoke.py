#!/usr/bin/env python3
"""W11 Works policy smoke — limits + coded errors.

Gates:
  W11_LIMITS_OK
  W11_TOO_LARGE_CODE_OK
  W11_PARENT_MISSING_CODE_OK
  W11_TRAVERSAL_CODE_OK
  W11_NOT_FOUND_CODE_OK
  W11_ROOT_FORBIDDEN_CODE_OK
  W11_SMOKE_ALL_OK
"""
from __future__ import annotations

import json
import os
import sys
import urllib.error
import urllib.request

BASE = os.environ.get("KALEIDO_BASE", "http://127.0.0.1:18766").rstrip("/")
USER = os.environ.get("KALEIDO_USER", "admin")
PASS = os.environ.get("KALEIDO_PASS", "<KALEIDO_PASS>")
PREFIX = "w11-smoke"


def req(method: str, path: str, token: str | None = None, body: dict | None = None, query: str = ""):
    url = f"{BASE}{path}"
    if query:
        url += ("&" if "?" in url else "?") + query
    data = None
    headers = {"Accept": "application/json"}
    if body is not None:
        data = json.dumps(body).encode("utf-8")
        headers["Content-Type"] = "application/json"
    if token:
        headers["Authorization"] = f"Bearer {token}"
    r = urllib.request.Request(url, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(r, timeout=20) as resp:
            raw = resp.read().decode("utf-8", errors="replace")
            try:
                j = json.loads(raw) if raw else {}
            except json.JSONDecodeError:
                j = {"_raw": raw}
            return resp.status, j, dict(resp.headers)
    except urllib.error.HTTPError as e:
        raw = e.read().decode("utf-8", errors="replace")
        try:
            j = json.loads(raw) if raw else {}
        except json.JSONDecodeError:
            j = {"_raw": raw}
        return e.code, j, dict(e.headers)


def login() -> str:
    st, j, _ = req("POST", "/api/v1/auth/login", body={"username": USER, "password": PASS})
    if st != 200 or not j.get("token"):
        # try alternate shape
        st, j, _ = req("POST", "/api/v1/auth/login", body={"user": USER, "pass": PASS})
    if st != 200:
        # common Kaleido shape
        st, j, _ = req("POST", "/api/v1/auth/login", body={"username": USER, "password": PASS})
    tok = j.get("token") or (j.get("session") or {}).get("token")
    if not tok:
        # try password field names used historically
        for body in (
            {"username": USER, "password": PASS},
            {"name": USER, "password": PASS},
        ):
            st, j, _ = req("POST", "/api/v1/auth/login", body=body)
            tok = j.get("token") or j.get("accessToken")
            if tok:
                break
    if not tok:
        raise SystemExit(f"login failed: {st} {j}")
    return tok


def gate(name: str, ok: bool, detail: str = ""):
    mark = "OK" if ok else "FAIL"
    print(f"{name}_{mark}" + (f" {detail}" if detail else ""))
    return ok


def main() -> int:
    tok = login()
    ok_all = True

    # 1) limits
    st, j, _ = req("GET", "/api/v1/works/limits", token=tok)
    limits_ok = (
        st == 200
        and j.get("ok") is True
        and j.get("maxFileBytes") == 2 * 1024 * 1024
        and j.get("maxListDepth") == 8
        and isinstance(j.get("codes"), list)
        and "WORKS_FILE_TOO_LARGE" in j.get("codes", [])
        and j.get("parentsMustExist") is True
    )
    ok_all &= gate("W11_LIMITS", limits_ok, f"status={st} keys={list(j)[:8]}")
    max_bytes = int(j.get("maxFileBytes") or (2 * 1024 * 1024))

    # cleanup / setup dirs
    req("DELETE", "/api/v1/works", token=tok, query=f"path={PREFIX}&recursive=true")
    st, j, _ = req("POST", "/api/v1/works/dir", token=tok, body={"path": PREFIX})
    if st not in (200, 201):
        print(f"setup mkdir fail {st} {j}")
        return 1

    # 2) content too large on write
    # Payload is max+1 chars; route body limit is ~2x max so handler (not axum 413) rejects.
    huge = "x" * (max_bytes + 1)
    st, j, _ = req(
        "PUT",
        "/api/v1/works/file",
        token=tok,
        body={"path": f"{PREFIX}/huge.txt", "content": huge},
    )
    too_large_ok = (
        st == 400
        and j.get("code") == "WORKS_CONTENT_TOO_LARGE"
        and j.get("maxBytes") == max_bytes
        and "error" in j
    )
    # If still 413, body-limit layer not raised enough — fail loud with body.
    if st == 413:
        too_large_ok = False
    ok_all &= gate("W11_TOO_LARGE_CODE", too_large_ok, f"status={st} body={j}")

    # 3) parent missing
    st, j, _ = req(
        "PUT",
        "/api/v1/works/file",
        token=tok,
        body={"path": f"{PREFIX}/no-such-dir/a.txt", "content": "hi"},
    )
    parent_ok = st == 400 and j.get("code") == "WORKS_PARENT_MISSING"
    ok_all &= gate("W11_PARENT_MISSING_CODE", parent_ok, f"status={st} body={j}")

    # 4) traversal
    st, j, _ = req(
        "GET",
        "/api/v1/works/file",
        token=tok,
        query="path=../etc/passwd",
    )
    trav_ok = st in (400, 403) and j.get("code") in (
        "WORKS_PATH_TRAVERSAL",
        "WORKS_ABSOLUTE_PATH",
        "WORKS_PATH_ESCAPE",
    )
    ok_all &= gate("W11_TRAVERSAL_CODE", trav_ok, f"status={st} body={j}")

    # 5) not found
    st, j, _ = req(
        "GET",
        "/api/v1/works/file",
        token=tok,
        query=f"path={PREFIX}/missing-nope.txt",
    )
    nf_ok = st == 404 and j.get("code") == "WORKS_NOT_FOUND"
    ok_all &= gate("W11_NOT_FOUND_CODE", nf_ok, f"status={st} body={j}")

    # 6) root forbidden write
    st, j, _ = req(
        "PUT",
        "/api/v1/works/file",
        token=tok,
        body={"path": "", "content": "x"},
    )
    # empty path may be bad request before coded; also try "."
    if j.get("code") != "WORKS_ROOT_FORBIDDEN":
        st, j, _ = req(
            "PUT",
            "/api/v1/works/file",
            token=tok,
            body={"path": ".", "content": "x"},
        )
    root_ok = st == 400 and j.get("code") == "WORKS_ROOT_FORBIDDEN"
    ok_all &= gate("W11_ROOT_FORBIDDEN_CODE", root_ok, f"status={st} body={j}")

    # cleanup
    req("DELETE", "/api/v1/works", token=tok, query=f"path={PREFIX}&recursive=true")

    ok_all &= gate("W11_SMOKE_ALL", ok_all)
    return 0 if ok_all else 1


if __name__ == "__main__":
    sys.exit(main())
