# B0 — Auth sessions vs Tavern activeRunId / stop

**Date:** 2026-07-28  
**Purpose:** prevent “网络错误” / permanent 409 from **stuck locks**, and distinguish two “session” concepts.

---

## Two different “sessions”

| Kind | Store | API | Cap |
|------|-------|-----|-----|
| **Auth session** (login token) | `data/state/sessions.json` | login / logout / `GET /sessions/stats` / `POST /sessions/prune` | **W12** `sessionMax` + `sessionCapPolicy` (`auto_evict`\|`reject`) → 429 `SESSION_CAP` |
| **Tavern session** (story play) | `data/tavern-sessions/*.json` | `/api/v1/story-tavern/sessions/*` | not W12; per-play state |

Do **not** prune tavern files when fixing login 429 — wrong store.

---

## `activeRunId` lock (Story Tavern)

Field: `TavernSession.active_run_id` (`activeRunId` in JSON).

| Phase | Behavior |
|-------|----------|
| Turn start | If previous id **still running** in JobStore → **409** `{ error, activeRunId }` |
| Turn start | If previous id **dead/missing** → **auto-clear** then continue |
| After accept | set `pending-{turn}` then real `runId` |
| Stream end / error / empty / cancel | `clear_session_active_run` |
| Stop | `hub.cancel` + `jobs.cancel` + clear **this runId** + clear **any** lock on session |

```text
clear_session_active_run(store, sessionId, Some(runId))
  → clear if current == runId OR current starts with "pending-"
clear_session_active_run(store, sessionId, None)
  → force clear any active_run_id (stop path)
```

### Client rules

1. On 409: call **stop** (with `activeRunId` if known) then **retry turn once**.  
2. Stop even if `runId` lost — body may send last known; server force-unlocks session.  
3. “网络错误” with health OK + AxonHub OK → **assume lock**, not WAN down.  
4. After SSE `done`, **poll GET session** until turn++ / assistant row (commit lag).  
5. Long `thinking_delta` only ≠ hung lock; still allow Stop.

### Manual recovery

```bash
# inspect
jq '.activeRunId' data/tavern-sessions/<id>.json
# clear lock (then optional restart)
# edit activeRunId → null  OR delete key
```

Regression gate name: **`ST_TURN_UNLOCK_OK`** (historical fix `9af3316` · `references/st-turn-unlock.md`).

---

## Stop semantics matrix

| Action | Hub | JobStore | activeRunId |
|--------|-----|----------|-------------|
| `POST …/sessions/{id}/stop` `{runId}` | cancel | cancel | clear (specific + force) |
| `POST /api/v1/jobs/{id}/cancel` | — | cancel | **does not** by itself clear tavern lock — prefer ST stop |
| `POST /api/v1/jobs/cancel-all` | bulk | bulk | may leave tavern locks if not paired — use ST stop per session |
| Background / BookTravel `…/stop` | job cancel | cancel | N/A (no tavern activeRunId) |

Mode / focus / rebind / restore-save while turn active → **409** (same lock family).

---

## Auth session deadlock-ish (W12)

| Symptom | Fix |
|---------|-----|
| login 429 `SESSION_CAP` | prune / raise `sessionMax` / `auto_evict` |
| login 429 rate window | process-local 10/300s; restart clears map; serial logins in stress |

Not the same as tavern `activeRunId`.

---

## Checklist for UI 专人

- [ ] Turn button disabled while local `runId` set  
- [ ] Stop always enabled during stream / thinking  
- [ ] 409 → stop + one retry  
- [ ] On error/empty SSE → clear local runId (server should too)  
- [ ] Don’t map all fetch failures to “网络错误” without status/body
