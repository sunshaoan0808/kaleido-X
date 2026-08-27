# Jobs v2 (S5-W0 / T0)

Unified Job/Run registry extending the S4 `JobStore` (JSON under `$KALEIDO_DATA/jobs/`).

## Concurrency policy

| Path | At capacity behavior |
|------|----------------------|
| `JobStore::try_start` (chat / mobile stream) | **429 RateLimited** — unchanged S4 contract |
| `JobStore::create` (`POST /api/v1/jobs`) | **queue** (`status=queued`) — never 429 on overflow |

- Cap: `KALEIDO_MAX_CONCURRENT_JOBS` (default **2**, clamped to **1–2**).
- When a running job finishes/cancels, oldest queued job is **promoted** to `running`.
- Chat still uses `try_start`/`finish` so concurrent chat fails fast with 429 instead of queueing LLM streams.

## Status mapping

| Legacy (S4 chat) | Canonical (v2) |
|------------------|----------------|
| `done` | `succeeded` |
| `error` | `failed` |
| `stopped` | `cancelled` |
| `running` | `running` |

Canonical set: `queued | running | succeeded | failed | cancelled`.

## Kinds

`background`, `book_travel`, `outline`, `agent`, `chat`, `other`, plus test helpers `noop` / `test` (auto-worker emits progress + done for SSE smoke).

## API

| Method | Path | Notes |
|--------|------|-------|
| GET | `/api/v1/jobs?status=&kind=&limit=` | workspace-scoped list |
| POST | `/api/v1/jobs` | `{kind, payload?, model?, meta?}` |
| GET | `/api/v1/jobs/{id}` | detail (+ chat run_id compat) |
| POST | `/api/v1/jobs/{id}/cancel` | idempotent |
| GET | `/api/v1/jobs/{id}/stream` | SSE: progress / event / done / error |

All require bearer (or one-time `?ticket=` for SSE, M-3). `?token=` removed in e454d50. Unauth → 401 via middleware.

## Persistence

JSON files `$KALEIDO_DATA/jobs/{run_id}.json` (extended schema; additive fields). No SQLite in W0.

## Features / phase

- `features.jobs_v2=true` on `/api/v1/public/info`
- `phase=S5-W0` on health + public info
- S4 routes (chat SSE, works, partner, auth) unchanged

## Curl smoke (after server restart)

```bash
TOKEN=$(curl -s -X POST http://127.0.0.1:18766/api/v1/auth/login \
  -H 'content-type: application/json' \
  -d '{"username":"admin","password":"YOUR_PASS"}' | jq -r .token)

# unauth → 401
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:18766/api/v1/jobs

# create noop
JOB=$(curl -s -X POST http://127.0.0.1:18766/api/v1/jobs \
  -H "authorization: Bearer $TOKEN" -H 'content-type: application/json' \
  -d '{"kind":"noop","payload":{"n":1}}')
ID=$(echo "$JOB" | jq -r .id)

curl -s -H "authorization: Bearer $TOKEN" "http://127.0.0.1:18766/api/v1/jobs/$ID"
curl -s -H "authorization: Bearer $TOKEN" "http://127.0.0.1:18766/api/v1/jobs?kind=noop"
curl -s -N -H "authorization: Bearer $TOKEN" "http://127.0.0.1:18766/api/v1/jobs/$ID/stream"
curl -s -X POST -H "authorization: Bearer $TOKEN" \
  "http://127.0.0.1:18766/api/v1/jobs/$ID/cancel"
```
