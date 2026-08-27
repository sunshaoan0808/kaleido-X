# Kaleido API Surface (B0)

**Generated:** 2026-07-28 from `crates/kaleido-server/src/**/*.rs` `.route(...)` scan.
**Live:** `http://127.0.0.1:18766` · public `https://kaleido.example.com`
**Auth default:** `Authorization: Bearer <token>` from `POST /api/v1/auth/login`.
SSE / some media also accept `?token=` (EventSource / `<img>`).

> Not OpenAPI. Path × method × auth × source file. Re-run scan after route changes.

**Path count:** 144

| Method | Auth | Path | Source |
|--------|------|------|--------|
| `GET,POST` | bearer | `/api/v1/agent/sessions` | agent_sessions.rs |
| `GET,PUT,PATCH,DELETE` | bearer | `/api/v1/agent/sessions/{id}` | agent_sessions.rs |
| `POST` | bearer | `/api/v1/agent/sessions/{id}/run` | agent_sessions.rs |
| `POST,PATCH` | bearer | `/api/v1/agent/sessions/{id}/title` | agent_sessions.rs |
| `GET,PUT` | bearer | `/api/v1/agent/sessions/{id}/todos` | agent_todo.rs |
| `POST` | bearer | `/api/v1/agent/tools/bash` | agent_tools.rs |
| `POST` | bearer | `/api/v1/agent/tools/edit` | agent_tools.rs |
| `POST` | bearer | `/api/v1/agent/tools/glob` | agent_tools.rs |
| `POST` | bearer | `/api/v1/agent/tools/grep` | agent_tools.rs |
| `POST` | bearer | `/api/v1/agent/tools/list` | agent_tools.rs |
| `POST` | bearer | `/api/v1/agent/tools/read` | agent_tools.rs |
| `POST` | bearer | `/api/v1/agent/tools/todo` | agent_todo.rs |
| `POST` | bearer | `/api/v1/agent/tools/write` | agent_tools.rs |
| `GET,PUT` | bearer | `/api/v1/app-state` | user_app_state.rs |
| `GET,POST,DELETE` | bearer|?token= | `/api/v1/appearance/wallpaper` | appearance.rs |
| `POST` | public | `/api/v1/auth/login` | main.rs |
| `POST` | bearer | `/api/v1/auth/logout` | main.rs |
| `GET,POST` | bearer | `/api/v1/author/projects` | author.rs |
| `GET,PATCH,DELETE` | bearer | `/api/v1/author/projects/{id}` | author.rs |
| `POST` | bearer | `/api/v1/author/projects/{id}/bind-session` | author.rs |
| `POST` | bearer | `/api/v1/author/projects/{id}/compose` | author.rs |
| `POST` | bearer | `/api/v1/author/projects/{id}/inject` | author.rs |
| `POST` | bearer | `/api/v1/author/projects/{id}/launch` | author.rs |
| `POST` | bearer | `/api/v1/author/projects/{id}/publish` | author.rs |
| `POST` | bearer | `/api/v1/background/apply` | main.rs |
| `POST` | bearer | `/api/v1/background/start` | main.rs |
| `POST` | bearer | `/api/v1/background/stop` | main.rs |
| `GET` | bearer|?token= | `/api/v1/background/stream` | main.rs |
| `POST` | bearer | `/api/v1/background/{stage}` | main.rs |
| `POST` | bearer | `/api/v1/book-travel/classify` | main.rs |
| `POST` | bearer | `/api/v1/book-travel/pipeline` | main.rs |
| `GET` | bearer | `/api/v1/book-travel/runs` | main.rs |
| `GET` | bearer | `/api/v1/book-travel/runs/{id}` | main.rs |
| `POST` | bearer | `/api/v1/book-travel/start` | main.rs |
| `POST` | bearer | `/api/v1/book-travel/stop` | main.rs |
| `GET` | bearer|?token= | `/api/v1/book-travel/stream` | main.rs |
| `POST` | bearer | `/api/v1/book-travel/{step}` | main.rs |
| `POST` | bearer | `/api/v1/chat/start` | main.rs |
| `POST` | bearer | `/api/v1/crawler/chat-to-shelf` | chat_shelf.rs |
| `POST` | bearer | `/api/v1/crawler/chat-to-shelf/run-due` | chat_shelf.rs |
| `GET,PUT` | bearer | `/api/v1/crawler/chat-to-shelf/schedule` | chat_shelf.rs |
| `POST` | bearer | `/api/v1/crawler/fanqie` | main.rs |
| `GET,POST` | bearer | `/api/v1/crawler/novels` | main.rs |
| `GET` | bearer | `/api/v1/crawler/novels/{slug}/content` | main.rs |
| `GET` | bearer|?token= | `/api/v1/crawler/novels/{slug}/cover` | main.rs |
| `GET` | bearer | `/api/v1/crawler/novels/{slug}/export` | main.rs |
| `POST` | bearer | `/api/v1/crawler/novels/{slug}/to-pack` | main.rs |
| `POST` | bearer | `/api/v1/data/ping` | main.rs |
| `POST` | bearer | `/api/v1/deai/summarize` | deai.rs |
| `GET` | bearer | `/api/v1/embed/status` | main.rs |
| `POST` | bearer | `/api/v1/embeddings` | main.rs |
| `GET,POST` | bearer | `/api/v1/jobs` | main.rs |
| `POST` | bearer | `/api/v1/jobs/cancel-all` | main.rs |
| `POST` | bearer | `/api/v1/jobs/drain` | main.rs |
| `GET` | bearer | `/api/v1/jobs/{run_id}` | main.rs |
| `POST` | bearer | `/api/v1/jobs/{run_id}/cancel` | main.rs |
| `GET` | bearer|?token= | `/api/v1/jobs/{run_id}/stream` | main.rs |
| `GET` | bearer | `/api/v1/llm/models` | llm_test.rs |
| `POST` | bearer | `/api/v1/llm/test` | llm_test.rs |
| `GET` | bearer | `/api/v1/me` | main.rs |
| `POST` | bearer | `/api/v1/outline/reverse/analyze` | main.rs |
| `POST` | bearer | `/api/v1/outline/reverse/finalize` | main.rs |
| `POST` | bearer | `/api/v1/outline/reverse/preview` | main.rs |
| `POST` | bearer | `/api/v1/outline/reverse/save` | main.rs |
| `GET,PUT` | bearer | `/api/v1/partner` | main.rs |
| `POST` | bearer | `/api/v1/partner/analyze-memory` | deai.rs |
| `GET,DELETE` | bearer | `/api/v1/partner/automation-triggers` | main.rs |
| `POST` | bearer | `/api/v1/partner/character-cards` | main.rs |
| `DELETE` | bearer | `/api/v1/partner/character-cards/{id}` | main.rs |
| `POST` | bearer | `/api/v1/partner/character-cards/{id}/rebuild-st-book` | main.rs |
| `POST` | bearer | `/api/v1/partner/optimize-memory` | deai.rs |
| `GET` | bearer | `/api/v1/partner/prompt-preview` | main.rs |
| `POST` | bearer | `/api/v1/partner/select` | main.rs |
| `POST` | bearer | `/api/v1/partner/st-export` | st_export.rs |
| `POST` | bearer | `/api/v1/partner/st-import` | main.rs |
| `POST` | bearer | `/api/v1/partner/tokenize/estimate` | main.rs |
| `POST` | bearer | `/api/v1/partner/vector-query` | main.rs |
| `POST` | bearer | `/api/v1/partner/wi-preview` | main.rs |
| `POST` | bearer | `/api/v1/partner/world-books` | main.rs |
| `POST` | bearer | `/api/v1/partner/world-books/migrate-legacy` | main.rs |
| `DELETE` | bearer | `/api/v1/partner/world-books/{id}` | main.rs |
| `GET,POST,PUT` | bearer | `/api/v1/partner/world-books/{id}/entries` | main.rs |
| `PATCH,DELETE` | bearer | `/api/v1/partner/world-books/{id}/entries/{entry_id}` | main.rs |
| `POST` | bearer | `/api/v1/partner/world-books/{id}/rebuild-st-book` | main.rs |
| `GET` | bearer | `/api/v1/partner/world-books/{id}/vector-index` | main.rs |
| `POST` | bearer | `/api/v1/partner/world-books/{id}/vector-index/rebuild` | main.rs |
| `GET` | public | `/api/v1/public/info` | main.rs |
| `GET,PUT` | bearer | `/api/v1/regex-library` | main.rs |
| `POST` | bearer | `/api/v1/regex-library/import` | main.rs |
| `POST` | bearer | `/api/v1/sessions/prune` | main.rs |
| `GET` | bearer | `/api/v1/sessions/stats` | main.rs |
| `GET,PATCH` | bearer | `/api/v1/settings` | main.rs |
| `GET,POST` | bearer | `/api/v1/skills` | skills.rs |
| `GET,DELETE` | bearer | `/api/v1/skills/{name}` | skills.rs |
| `GET` | bearer | `/api/v1/stats/interactions` | stats.rs |
| `GET` | bearer | `/api/v1/stats/work-summary` | stats.rs |
| `GET` | bearer | `/api/v1/stats/writing` | stats.rs |
| `GET,POST` | bearer | `/api/v1/story-tavern/packs` | story_tavern.rs |
| `POST` | bearer | `/api/v1/story-tavern/packs/demo` | story_tavern.rs |
| `POST` | bearer | `/api/v1/story-tavern/packs/import` | story_tavern.rs |
| `GET,DELETE` | bearer | `/api/v1/story-tavern/packs/{id}` | story_tavern.rs |
| `GET,PUT` | bearer | `/api/v1/story-tavern/packs/{id}/chapters/{*rel}` | story_tavern.rs |
| `GET` | bearer | `/api/v1/story-tavern/packs/{id}/export.zip` | story_tavern.rs |
| `GET,PUT` | bearer | `/api/v1/story-tavern/persona/{character_id}` | story_tavern.rs |
| `GET,POST` | bearer | `/api/v1/story-tavern/sessions` | story_tavern.rs |
| `GET,PUT,PATCH,DELETE` | bearer | `/api/v1/story-tavern/sessions/{id}` | story_tavern.rs |
| `POST` | bearer | `/api/v1/story-tavern/sessions/{id}/focus` | story_tavern.rs |
| `POST` | bearer | `/api/v1/story-tavern/sessions/{id}/mode` | story_tavern.rs |
| `POST` | bearer | `/api/v1/story-tavern/sessions/{id}/rebind-vessel` | story_tavern.rs |
| `GET,POST` | bearer | `/api/v1/story-tavern/sessions/{id}/saves` | story_tavern.rs |
| `DELETE` | bearer | `/api/v1/story-tavern/sessions/{id}/saves/{save_id}` | story_tavern.rs |
| `POST` | bearer | `/api/v1/story-tavern/sessions/{id}/saves/{save_id}/restore` | story_tavern.rs |
| `POST` | bearer | `/api/v1/story-tavern/sessions/{id}/stop` | story_tavern.rs |
| `GET` | bearer|?token= | `/api/v1/story-tavern/sessions/{id}/stream` | story_tavern.rs |
| `POST` | bearer | `/api/v1/story-tavern/sessions/{id}/turn` | story_tavern.rs |
| `POST` | bearer | `/api/v1/story/start` | main.rs |
| `POST` | bearer | `/api/v1/story/stop` | main.rs |
| `GET` | bearer|?token= | `/api/v1/story/stream` | main.rs |
| `GET,PUT` | bearer | `/api/v1/style-presets` | style_presets.rs |
| `POST` | bearer | `/api/v1/tokenize/estimate` | main.rs |
| `GET,POST,DELETE` | bearer | `/api/v1/versions` | versions.rs |
| `POST` | bearer | `/api/v1/versions/ai` | versions.rs |
| `GET` | bearer | `/api/v1/versions/content` | versions.rs |
| `GET,DELETE` | bearer | `/api/v1/works` | main.rs |
| `POST` | bearer | `/api/v1/works/create-untitled` | works_ext.rs |
| `POST` | bearer | `/api/v1/works/dir` | main.rs |
| `GET` | bearer | `/api/v1/works/export` | works_ext.rs |
| `GET,PUT` | bearer | `/api/v1/works/file` | main.rs |
| `GET` | bearer|?token= | `/api/v1/works/image-data-url` | works_ext.rs |
| `GET` | bearer | `/api/v1/works/limits` | main.rs |
| `POST` | bearer | `/api/v1/works/move` | works_ext.rs |
| `POST` | bearer | `/api/v1/works/rename` | main.rs |
| `GET` | bearer | `/api/v1/works/stat` | main.rs |
| `POST` | mobile-compat | `/api/mobile/chat/start` | main.rs |
| `POST` | mobile-compat | `/api/mobile/chat/stop` | main.rs |
| `GET,POST` | mobile-compat | `/api/mobile/sessions` | main.rs |
| `GET,DELETE` | mobile-compat | `/api/mobile/sessions/{id}` | main.rs |
| `PUT` | mobile-compat | `/api/mobile/sessions/{id}/title` | main.rs |
| `GET,POST` | mobile-compat | `/api/mobile/state/{name}` | main.rs |
| `GET` | mobile-compat | `/api/mobile/status` | main.rs |
| `POST` | mobile-compat | `/api/mobile/story/start` | main.rs |
| `GET` | bearer|?token= | `/api/mobile/stream` | main.rs |
| `GET` | public | `/` | main.rs |
| `GET` | public | `/health` | main.rs |

## Module merges

`main.rs` also `.merge(mod::router())` for: chat_shelf, agent_tools, agent_sessions, agent_todo, skills, deai, stats, st_export, versions, llm_test, works_ext, user_app_state, appearance, style_presets, author, story_tavern — routes above include those modules.

## Domain groups (quick index)

### Auth / sessions (login tokens)

- `POST` `/api/v1/auth/login`
- `POST` `/api/v1/auth/logout`
- `GET` `/api/v1/me`
- `POST` `/api/v1/sessions/prune`
- `GET` `/api/v1/sessions/stats`

### Health / public

- `GET` `/api/v1/embed/status`
- `POST` `/api/v1/embeddings`
- `GET` `/api/v1/public/info`
- `GET` `/health`

### Jobs / SSE hub

- `GET,POST` `/api/v1/jobs`
- `POST` `/api/v1/jobs/cancel-all`
- `POST` `/api/v1/jobs/drain`
- `GET` `/api/v1/jobs/{run_id}`
- `POST` `/api/v1/jobs/{run_id}/cancel`
- `GET` `/api/v1/jobs/{run_id}/stream`

### Background (W1)

- `POST` `/api/v1/background/apply`
- `POST` `/api/v1/background/start`
- `POST` `/api/v1/background/stop`
- `GET` `/api/v1/background/stream`
- `POST` `/api/v1/background/{stage}`

### BookTravel (W2)

- `POST` `/api/v1/book-travel/classify`
- `POST` `/api/v1/book-travel/pipeline`
- `GET` `/api/v1/book-travel/runs`
- `GET` `/api/v1/book-travel/runs/{id}`
- `POST` `/api/v1/book-travel/start`
- `POST` `/api/v1/book-travel/stop`
- `GET` `/api/v1/book-travel/stream`
- `POST` `/api/v1/book-travel/{step}`

### Partner / WI / regex / vector

- `GET,PUT` `/api/v1/partner`
- `POST` `/api/v1/partner/analyze-memory`
- `GET,DELETE` `/api/v1/partner/automation-triggers`
- `POST` `/api/v1/partner/character-cards`
- `DELETE` `/api/v1/partner/character-cards/{id}`
- `POST` `/api/v1/partner/character-cards/{id}/rebuild-st-book`
- `POST` `/api/v1/partner/optimize-memory`
- `GET` `/api/v1/partner/prompt-preview`
- `POST` `/api/v1/partner/select`
- `POST` `/api/v1/partner/st-export`
- `POST` `/api/v1/partner/st-import`
- `POST` `/api/v1/partner/tokenize/estimate`
- `POST` `/api/v1/partner/vector-query`
- `POST` `/api/v1/partner/wi-preview`
- `POST` `/api/v1/partner/world-books`
- `POST` `/api/v1/partner/world-books/migrate-legacy`
- `DELETE` `/api/v1/partner/world-books/{id}`
- `GET,POST,PUT` `/api/v1/partner/world-books/{id}/entries`
- `PATCH,DELETE` `/api/v1/partner/world-books/{id}/entries/{entry_id}`
- `POST` `/api/v1/partner/world-books/{id}/rebuild-st-book`
- `GET` `/api/v1/partner/world-books/{id}/vector-index`
- `POST` `/api/v1/partner/world-books/{id}/vector-index/rebuild`
- `GET,PUT` `/api/v1/regex-library`
- `POST` `/api/v1/regex-library/import`
- `POST` `/api/v1/tokenize/estimate`

### Story Tavern

- `GET,POST` `/api/v1/story-tavern/packs`
- `POST` `/api/v1/story-tavern/packs/demo`
- `POST` `/api/v1/story-tavern/packs/import`
- `GET,DELETE` `/api/v1/story-tavern/packs/{id}`
- `GET,PUT` `/api/v1/story-tavern/packs/{id}/chapters/{*rel}`
- `GET` `/api/v1/story-tavern/packs/{id}/export.zip`
- `GET,PUT` `/api/v1/story-tavern/persona/{character_id}`
- `GET,POST` `/api/v1/story-tavern/sessions`
- `GET,PUT,PATCH,DELETE` `/api/v1/story-tavern/sessions/{id}`
- `POST` `/api/v1/story-tavern/sessions/{id}/focus`
- `POST` `/api/v1/story-tavern/sessions/{id}/mode`
- `POST` `/api/v1/story-tavern/sessions/{id}/rebind-vessel`
- `GET,POST` `/api/v1/story-tavern/sessions/{id}/saves`
- `DELETE` `/api/v1/story-tavern/sessions/{id}/saves/{save_id}`
- `POST` `/api/v1/story-tavern/sessions/{id}/saves/{save_id}/restore`
- `POST` `/api/v1/story-tavern/sessions/{id}/stop`
- `GET` `/api/v1/story-tavern/sessions/{id}/stream`
- `POST` `/api/v1/story-tavern/sessions/{id}/turn`

### Legacy story/chat stream

- `POST` `/api/v1/chat/start`
- `POST` `/api/v1/story/start`
- `POST` `/api/v1/story/stop`
- `GET` `/api/v1/story/stream`

### Works FS (W11)

- `GET,DELETE` `/api/v1/works`
- `POST` `/api/v1/works/create-untitled`
- `POST` `/api/v1/works/dir`
- `GET` `/api/v1/works/export`
- `GET,PUT` `/api/v1/works/file`
- `GET` `/api/v1/works/image-data-url`
- `GET` `/api/v1/works/limits`
- `POST` `/api/v1/works/move`
- `POST` `/api/v1/works/rename`
- `GET` `/api/v1/works/stat`

### Crawler / shelf / chat-to-shelf

- `POST` `/api/v1/crawler/chat-to-shelf`
- `POST` `/api/v1/crawler/chat-to-shelf/run-due`
- `GET,PUT` `/api/v1/crawler/chat-to-shelf/schedule`
- `POST` `/api/v1/crawler/fanqie`
- `GET,POST` `/api/v1/crawler/novels`
- `GET` `/api/v1/crawler/novels/{slug}/content`
- `GET` `/api/v1/crawler/novels/{slug}/cover`
- `GET` `/api/v1/crawler/novels/{slug}/export`
- `POST` `/api/v1/crawler/novels/{slug}/to-pack`

### Author zone

- `GET,POST` `/api/v1/author/projects`
- `GET,PATCH,DELETE` `/api/v1/author/projects/{id}`
- `POST` `/api/v1/author/projects/{id}/bind-session`
- `POST` `/api/v1/author/projects/{id}/compose`
- `POST` `/api/v1/author/projects/{id}/inject`
- `POST` `/api/v1/author/projects/{id}/launch`
- `POST` `/api/v1/author/projects/{id}/publish`

### Agent tools (W10)

- `GET,POST` `/api/v1/agent/sessions`
- `GET,PUT,PATCH,DELETE` `/api/v1/agent/sessions/{id}`
- `POST` `/api/v1/agent/sessions/{id}/run`
- `POST,PATCH` `/api/v1/agent/sessions/{id}/title`
- `GET,PUT` `/api/v1/agent/sessions/{id}/todos`
- `POST` `/api/v1/agent/tools/bash`
- `POST` `/api/v1/agent/tools/edit`
- `POST` `/api/v1/agent/tools/glob`
- `POST` `/api/v1/agent/tools/grep`
- `POST` `/api/v1/agent/tools/list`
- `POST` `/api/v1/agent/tools/read`
- `POST` `/api/v1/agent/tools/todo`
- `POST` `/api/v1/agent/tools/write`

### Settings / appearance / app-state

- `GET,PUT` `/api/v1/app-state`
- `GET,POST,DELETE` `/api/v1/appearance/wallpaper`
- `GET,PATCH` `/api/v1/settings`
- `GET,PUT` `/api/v1/style-presets`

### LLM test / models

- `GET` `/api/v1/llm/models`
- `POST` `/api/v1/llm/test`

### Mobile compat

- `POST` `/api/mobile/chat/start`
- `POST` `/api/mobile/chat/stop`
- `GET,POST` `/api/mobile/sessions`
- `GET,DELETE` `/api/mobile/sessions/{id}`
- `PUT` `/api/mobile/sessions/{id}/title`
- `GET,POST` `/api/mobile/state/{name}`
- `GET` `/api/mobile/status`
- `POST` `/api/mobile/story/start`
- `GET` `/api/mobile/stream`

