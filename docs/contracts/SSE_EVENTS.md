# B0 — Job / SSE event enum

**Date:** 2026-07-28  
**Transport:** `text/event-stream` via `stream_job_sse` (`main.rs`)  
**Endpoints (same payload shape):**

| Stream URL | Job kind / use |
|------------|----------------|
| `GET /api/v1/jobs/{runId}/stream` | generic |
| `GET /api/v1/background/stream?id=` | background |
| `GET /api/v1/book-travel/stream?id=` | book_travel |
| `GET /api/v1/story-tavern/sessions/{id}/stream?runId=` | ST turn (jobs hub) |
| `GET /api/v1/story/stream` · chat | legacy thin shells |

**Auth:** Bearer **or** `?token=` (EventSource).

---

## Wire frame

Each SSE `event:` name ≈ `eventType`. Data JSON:

```json
{
  "runId": "…",
  "eventType": "delta|progress|event|done|error|thinking_delta|…",
  "message": "optional human/stage label",
  "progress": 0.0,
  "data": { },
  "status": "queued|running|succeeded|failed|cancelled",
  "ts": "…",
  "result": { }
}
```

Terminal: job status terminal **or** `eventType` ∈ `done` | `error`.  
If job ends with empty events, server synthesizes `done`/`error` once.

**Late subscribe:** may get non-SSE JSON error if single-shot hub already finished — poll `GET /jobs/{id}` / session (see S8 stream soak).

---

## Shared `eventType` values

| eventType | Meaning | Typical `data` |
|-----------|---------|----------------|
| `progress` | numeric/stage progress | message often `"pipeline:…"`, progress 0–1 |
| `delta` | LLM token chunk | `{ "delta": "…", "stage"\|"step": "…" }` |
| `thinking_delta` | reasoning stream (ST) | `{ "delta": "…" }` reasoning text |
| `event` | stage milestone / misc | stage payload / result fragment |
| `done` | success terminal (or cancel-as-done) | may include `result` on synthetic terminal |
| `error` | failure terminal or mid error | `message` |

Job **status** (REST) canonical: `queued | running | succeeded | failed | cancelled`  
(Legacy chat: `done`→succeeded, `error`→failed, `stopped`→cancelled — `JOBS_V2.md`)

---

## Background (W1)

| Signal | Where | Notes |
|--------|-------|-------|
| `progress` | start / pipeline | e.g. `pipeline:{mode}`, `pipeline:{mode}:start` in message/data |
| `delta` | LLM stream | `data.delta` + `data.stage` |
| `event` | stage complete | `pipeline:{mode}:done` |
| `done` / job succeeded | end | result: worldBooks, characterNames/Cards, `generationMode` llm\|heuristic |

Stages: `stage_one` · `items` · `character_card` · `pipeline`  
Routes: `POST /background/start` · `/{stage}` · `stop` · `apply` · `stream`

---

## BookTravel (W2)

| Signal | Notes |
|--------|-------|
| `progress` | `pipeline:{step}`, `pipeline:progress:N` |
| `delta` | `data.delta` + `data.step` |
| `event` | `{step} complete`, `pipeline:{step}:done`, `pipeline:persist` |
| cancel | `POST …/stop` + job cancel → status `cancelled` (must not be overwritten by complete) |

Steps: classify · assemble · plan_scene · writers · ending · memory · **pipeline**  
`preferHeuristic: true` for fast smoke (cancel tests: **avoid** too-fast heuristic).

---

## Story Tavern turn

| eventType | Notes |
|-----------|-------|
| `delta` | narrative tokens |
| `thinking_delta` | model reasoning; UI may show “思考中” without looking hung |
| `error` | LLM/stream failure; **must** clear `activeRunId` |
| `done` | turn finished (also on cancel path sometimes); then poll session — **SSE done ≠ turn++ yet** (race: poll commit) |

Flow:

1. `POST …/sessions/{id}/turn` `{message}` → `{runId}`  
2. Immediate `GET …/stream?runId=`  
3. Optional `POST …/stop` `{runId}` → cancel hub + **clear activeRunId**  
4. `GET …/sessions/{id}` for `messages[].options`, turn, state  

Markers stripped server-side: `【选项】` → `message.options`; `【节点推进:id】` → node advance.

---

## Client switch skeleton

```js
switch (ev.eventType) {
  case 'delta': append(ev.data?.delta); break;
  case 'thinking_delta': showThinking(ev.data?.delta); break;
  case 'progress': setProgress(ev.progress, ev.message); break;
  case 'event': handleStage(ev); break;
  case 'done': finalize(ev); break;
  case 'error': fail(ev); break;
  default: /* ignore or log */
}
```

---

## See also

- `docs/JOBS_V2.md`  
- `docs/STREAM_PARITY_NOTES.md`  
- `docs/S8_STREAM_SOAK_NOTES.md`  
- `docs/contracts/SESSION_DEADLOCK.md`
