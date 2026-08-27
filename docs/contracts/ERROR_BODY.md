# B0 — Error body contract

**Date:** 2026-07-28  
**Scope:** document-only (no mass rewrite of every handler)  
**Related:** `map_core_err` in `crates/kaleido-server/src/main.rs`

---

## Canonical shape

```json
{
  "error": "human-readable message (often Chinese)",
  "code": "MACHINE_CODE_OPTIONAL",
  "detail": { }
}
```

| Field | Required | Notes |
|-------|----------|-------|
| `error` | **yes** on failure | string; UI may show as-is |
| `code` | preferred on new paths | stable `SCREAMING_SNAKE`; switchable |
| extra keys | ok | domain fields flattened (`active`, `cap`, `host`, `hint`, …) or nested `details` |
| `ok: false` | some domains | crawler W19, works limits success uses `ok:true` |

**Success** shapes are domain-specific (`{ok:true,…}`, `{token}`, job object, session object). Do not force `{ok:true}` everywhere.

---

## HTTP status map (`map_core_err`)

| `CoreError` | HTTP | Body |
|-------------|------|------|
| `Auth` | 401 | `{error}` |
| `RateLimited` | 429 | `{error}` |
| `SessionCap{…}` | 429 | `{error, code:SESSION_CAP, active, cap, policy, hint, actions[]}` |
| `NotFound` | 404 | `{error}` |
| `Forbidden` | 403 | `{error}` |
| `BadRequest` | 400 | `{error}` |
| `Coded{code,message,details}` | by code | `{error, code, …details}` |
| other | 500 | `{error: Display}` |

### `Coded` status rules (W11+)

| Prefix / code | Status |
|---------------|--------|
| `WORKS_NOT_FOUND` | 404 |
| `WORKS_PATH_ESCAPE` / `WORKS_PATH_TRAVERSAL` | 403 |
| other `WORKS_*` | 400 |
| unknown coded | 400 |

---

## Domain codes (shipped)

| Domain | Codes | Doc |
|--------|-------|-----|
| Auth sessions | `SESSION_CAP` | `W12_SESSION_CAP.md` |
| Works FS | `WORKS_*` | `W11_WORKS_POLICY.md` |
| Crawler fanqie | `CRAWLER_*` | `W19_CRAWLER.md` |
| BookTravel input | `BT_INPUT` (partial) | `W2_BOOKTRAVEL_JOB_NOTES.md` |
| Agent tools | message keys `agent_write_disabled` / `bash_disabled` / `confirm_required` (string `error`, not always `code`) | `W10_AGENT_TOOL_POLICY.md` |
| JSON parse | `BAD_JSON` | main reject body |

---

## Critical-path samples (live shape)

### 401 missing bearer

```json
{ "error": "missing bearer" }
```

### 429 session cap

```json
{
  "error": "…",
  "code": "SESSION_CAP",
  "active": 50,
  "cap": 50,
  "policy": "reject",
  "hint": "GET /api/v1/sessions/stats; …",
  "actions": [ { "method": "POST", "path": "/api/v1/sessions/prune", "body": { "mode": "oldest", "count": 5 } } ]
}
```

### 400 Works parent missing

```json
{
  "error": "…",
  "code": "WORKS_PARENT_MISSING"
}
```

### 403 crawler off (W19)

```json
{
  "ok": false,
  "error": "crawler_disabled",
  "code": "CRAWLER_DISABLED",
  "stage": "gate",
  "retryable": false,
  "hint": "PATCH /api/v1/settings {crawlerEnabled:true} then retry; default remains off",
  "crawlerEnabled": false,
  "defaultOff": true
}
```

### 409 Story Tavern turn lock

```json
{
  "error": "turn in progress; tap 停止 then retry",
  "activeRunId": "run-…"
}
```

(Not always via `map_core_err` — handler-local.)

### 422 validation (serde)

Axum/json rejection — may be framework text or `{error, code:BAD_JSON}`. Prefer camelCase bodies (`packId`, `playMode`).

---

## Guidance for new endpoints

1. Prefer `CoreError::Coded` or existing variant → `map_core_err`.
2. Always set `error` string (Chinese OK for product paths).
3. Add stable `code` when UI/automation must branch.
4. Put machine fields as siblings or `details` object — both OK; W11 flattens `details`.
5. Do **not** silently truncate; return coded 400 (Works) or 413 only if body limit hits first.

---

## Residual (not B0 scope)

- Mass-convert every `{error}`-only path to coded form.
- OpenAPI generation from this doc.
