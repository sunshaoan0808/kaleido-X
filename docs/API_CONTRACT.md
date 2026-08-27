# Kaleido 前端 API 契约（核对清单）

> 自动核对：前端 `src/js/*.js` 中的 `api('/...')` 与 `stApi('/...')` 调用 vs 后端 `crates/kaleido-server/src/*.rs` 中 `.route(...)` 与 `Router::new().route(...)` 实际路由。
>
> 核对日期：2026-08-16。后端为 axum；路径变量以 `{name}` 表示。

## 1. 认证与会话

| 前端调用 | 后端路由 | 状态 |
| --- | --- | --- |
| `POST /api/v1/auth/login` | `POST /api/v1/auth/login` | ✓ |
| `POST /api/v1/auth/logout` | `POST /api/v1/auth/logout` | ✓ |
| `POST /api/v1/auth/sse-ticket` | `POST /api/v1/auth/sse-ticket` | ✓ |
| `GET /api/v1/me` | `GET /api/v1/me` | ✓ |

## 2. Partner（角色卡 / 世界书 / 正则）

| 前端调用 | 后端路由 | 状态 |
| --- | --- | --- |
| `GET /api/v1/partner` | `GET /api/v1/partner` | ✓ |
| `PUT /api/v1/partner` | `PUT /api/v1/partner` | ✓ |
| `POST /api/v1/partner/select` | `POST /api/v1/partner/select` | ✓ |
| `GET /api/v1/partner/prompt-preview?...` | `GET /api/v1/partner/prompt-preview` | ✓ |
| `POST /api/v1/partner/st-import` | `POST /api/v1/partner/st-import` | ✓ |
| `POST /api/v1/partner/st-export` | `POST /api/v1/partner/st-export`（st_export.rs） | ✓ |
| `POST /api/v1/partner/wi-preview` | `POST /api/v1/partner/wi-preview` | ✓ |
| `POST /api/v1/partner/world-books` | `POST /api/v1/partner/world-books` | ✓ |
| `POST /api/v1/partner/character-cards` | `POST /api/v1/partner/character-cards` | ✓ |
| `DELETE /api/v1/partner/world-books/{id}` | 见 chat_shelf.rs（合并路由） | ✓ |
| `DELETE /api/v1/partner/character-cards/{id}` | 见 chat_shelf.rs（合并路由） | ✓ |
| `POST /api/v1/partner/analyze-memory` | `POST /api/v1/partner/analyze-memory`（deai.rs 合并） | ✓ |
| `POST /api/v1/partner/optimize-memory` | `POST /api/v1/partner/optimize-memory`（deai.rs 合并） | ✓ |
| `GET/POST /api/v1/regex-library` | `GET/POST /api/v1/regex-library` + `/api/v1/regex-library/import` | ✓ |

> 注意：`/api/v1/partner/sample` 前端历史曾误调，已在本次修订中移除——改为调用 `/api/v1/story-tavern/packs/demo` 安装示例剧本包。

## 3. Story Tavern（故事馆）

前缀 `/api/v1/story-tavern/*`，由 `story_tavern.rs` 合并路由挂载。

| 前端调用 | 后端路由 | 状态 |
| --- | --- | --- |
| `GET /api/v1/story-tavern/packs` | `GET /api/v1/story-tavern/packs` | ✓ |
| `POST /api/v1/story-tavern/packs/demo` | `POST /api/v1/story-tavern/packs/demo` | ✓ |
| `POST /api/v1/story-tavern/packs/import` | `POST /api/v1/story-tavern/packs/import` | ✓ |
| `GET /api/v1/story-tavern/packs/{id}` | `GET /api/v1/story-tavern/packs/{id}` | ✓ |
| `GET /api/v1/story-tavern/sessions` | `GET /api/v1/story-tavern/sessions` | ✓ |
| `POST /api/v1/story-tavern/sessions` | `POST /api/v1/story-tavern/sessions` | ✓ |
| `GET /api/v1/story-tavern/sessions/{id}` | `GET /api/v1/story-tavern/sessions/{id}` | ✓ |
| `POST /api/v1/story-tavern/sessions/{id}/turn` | `POST /api/v1/story-tavern/sessions/{id}/turn` | ✓ |
| `POST /api/v1/story-tavern/sessions/{id}/stop` | `POST /api/v1/story-tavern/sessions/{id}/stop` | ✓ |
| `GET /api/v1/story-tavern/sessions/{id}/stream` | `GET /api/v1/story-tavern/sessions/{id}/stream` | ✓ |
| `POST /api/v1/story-tavern/sessions/{id}/saves` | `POST /api/v1/story-tavern/sessions/{id}/saves` | ✓ |
| `POST /api/v1/story-tavern/sessions/{id}/saves/{save_id}/restore` | `POST .../saves/{save_id}/restore` | ✓ |
| `GET /api/v1/story-tavern/works/{wid}/compass` | 见 `compass.rs`（compass 子路由） | ✓ |

## 4. Works（文稿文件系统）

| 前端调用 | 后端路由 | 状态 |
| --- | --- | --- |
| `GET /api/v1/works?...` | `GET/DELETE /api/v1/works` | ✓ |
| `GET/PUT /api/v1/works/file?path=` | `GET/PUT /api/v1/works/file`（带 body limit） | ✓ |
| `POST /api/v1/works/dir` | `POST /api/v1/works/dir` | ✓ |
| `POST /api/v1/works/move` | `POST /api/v1/works/move`（works_ext.rs） | ✓ |
| `POST /api/v1/works/rename` | `POST /api/v1/works/rename` | ✓ |
| `POST /api/v1/works/create-untitled` | `POST /api/v1/works/create-untitled`（works_ext.rs） | ✓ |
| `GET/POST /api/v1/versions?path=` | `GET/POST /api/v1/versions`（versions.rs） | ✓ |

## 5. Graph / Foreshadow / Analysis（作品级工具）

| 前端调用 | 后端路由 | 状态 |
| --- | --- | --- |
| `GET /api/v1/works/{wid}/graph` | `GET /api/v1/works/{work_id}/graph`（graph.rs） | ✓ |
| `POST /api/v1/works/{wid}/graph/characters` | `POST .../graph/characters` | ✓ |
| `PUT/DELETE /api/v1/works/{wid}/graph/characters/{id}` | `PUT/DELETE .../graph/characters/{id}` | ✓ |
| `GET .../graph/characters/candidates?q=` | `GET .../graph/characters/candidates` | ✓ |
| `POST/PUT/DELETE .../graph/relationships[/{id}]` | ✓ | ✓ |
| `GET/POST /api/v1/works/{wid}/foreshadows` | `foreshadow.rs` 合并 | ✓ |
| `POST/DELETE .../foreshadows/{id}/occurrences[/{occId}]` | ✓ | ✓ |
| `GET /api/v1/analysis/kinds` | `GET /api/v1/analysis/kinds`（analysis.rs） | ✓ |
| `GET/POST /api/v1/works/{wid}/analysis/tasks[/{id}/...]` | ✓ | ✓ |
| `GET /api/v1/analysis/tasks/{id}` | ✓ | ✓ |
| `GET /api/v1/analysis/character-arc?work_id=` | `GET /api/v1/analysis/character-arc` | ✓ |
| `GET /api/v1/analysis/relation-evolution?work_id=` | `GET /api/v1/analysis/relation-evolution` | ✓ |
| `POST /api/v1/analysis/emotion-curve` | `POST /api/v1/analysis/emotion-curve` | ✓ |

## 6. Agent / Skills / DeAI / LLM Test / MoA

| 前端调用 | 后端路由 | 状态 |
| --- | --- | --- |
| `POST /api/v1/agent/sessions/{id}/run` | `agent_sessions.rs` 合并 | ✓ |
| `POST /api/v1/agent/tools/{read|write|bash|list}` | `agent_tools.rs` 合并 | ✓ |
| `GET/POST/DELETE /api/v1/skills[/{name}]` | `skills.rs` 合并 | ✓ |
| `POST /api/v1/deai/summarize` | `POST /api/v1/deai/summarize`（deai.rs） | ✓ |
| `POST /api/v1/llm/test` | `POST /api/v1/llm/test`（llm_test.rs） | ✓ |
| MoA：`/api/v1/moa/...` | `moa_api.rs` 合并 | ✓ |

## 7. Crawler（番茄爬虫，默认关闭）

| 前端调用 | 后端路由 | 状态 |
| --- | --- | --- |
| `POST /api/v1/crawler/fanqie` | `POST /api/v1/crawler/fanqie` | ✓（403 当 `crawlerEnabled != true`） |
| `GET /api/v1/crawler/fanqie/meta` | `GET /api/v1/crawler/fanqie/meta` | ✓ |
| `GET /api/v1/crawler/fanqie/search?q=` | `GET /api/v1/crawler/fanqie/search` | ✓ |
| `GET /api/v1/crawler/fanqie/progress` | `GET /api/v1/crawler/fanqie/progress` | ✓ |
| `GET /api/v1/crawler/novels` | `GET/POST /api/v1/crawler/novels` | ✓ |

> 前端 `friendlyError()` 会把 `crawler_disabled` 映射为「功能未启用，请在设置中开启」。

## 8. Outline（反拆大纲，预览版）

| 前端调用 | 后端路由 | 状态 |
| --- | --- | --- |
| `POST /api/v1/outline/reverse/preview` | `POST /api/v1/outline/reverse/preview` | ✓ |
| `POST /api/v1/outline/reverse/analyze` | `POST /api/v1/outline/reverse/analyze` | ✓ |
| `POST /api/v1/outline/reverse/finalize` | `POST /api/v1/outline/reverse/finalize` | ✓ |
| `POST /api/v1/outline/reverse/save` | `POST /api/v1/outline/reverse/save` | ✓ |

> 前端标签页头标注「预览版」并给用户提示当前为启发式拆章。

## 9. Background / Book-Travel / Jobs

| 前端调用 | 后端路由 | 状态 |
| --- | --- | --- |
| `POST /api/v1/background/apply` | `POST /api/v1/background/apply` | ✓ |
| `POST /api/v1/background/stop` | `POST /api/v1/background/stop` | ✓ |
| `POST /api/v1/book-travel/classify` | `POST /api/v1/book-travel/classify` | ✓ |
| `POST /api/v1/book-travel/stop` | `POST /api/v1/book-travel/stop` | ✓ |
| `GET /api/v1/jobs?limit=50` | `GET /api/v1/jobs` | ✓ |
| `POST /api/v1/jobs` | `POST /api/v1/jobs` | ✓ |
| `GET /api/v1/jobs/{id}` | `GET /api/v1/jobs/{run_id}` | ✓ |
| `POST /api/v1/jobs/{id}/cancel` | `POST /api/v1/jobs/{run_id}/cancel` | ✓ |
| `POST /api/v1/jobs/cancel-all` | `POST /api/v1/jobs/cancel-all` | ✓ |

## 10. Settings / App-State / Appearance / Style

| 前端调用 | 后端路由 | 状态 |
| --- | --- | --- |
| `GET/POST /api/v1/settings` | `GET/POST /api/v1/settings` | ✓ |
| `GET/POST /api/v1/app-state` | `GET/POST /api/v1/app-state`（user_app_state.rs） | ✓ |
| `POST /api/v1/appearance/wallpaper` | `appearance.rs` 合并 | ✓ |
| `GET/POST /api/v1/style-presets` | `style_presets.rs` 合并 | ✓ |

## 11. Embed / Embeddings / Search / Author / 图像

| 前端调用 | 后端路由 | 状态 |
| --- | --- | --- |
| `GET /api/v1/embed/status` | `GET /api/v1/embed/status` | ✓ |
| `POST /api/v1/embeddings` | `POST /api/v1/embeddings` | ✓ |
| `GET /api/v1/search?q=` | `GET /api/v1/search` | ✓ |
| `GET /api/v1/author/projects` | `author.rs` 合并 | ✓ |
| `POST /api/v1/author/projects[/{id}/...]` | ✓ | ✓ |
| `POST /api/v1/kaleido-tools/image` | `POST /api/v1/kaleido-tools/image` | ✓ |

## 12. Mobile 兼容（旧路径，仅保留）

| 前端调用 | 后端路由 | 状态 |
| --- | --- | --- |
| `GET /api/mobile/sessions?prefix=...` | `GET /api/mobile/sessions` | ✓ |
| `GET /api/mobile/sessions/{id}` | `GET /api/mobile/sessions/{id}` | ✓ |
| `POST /api/mobile/sessions` | `POST /api/mobile/sessions` | ✓ |
| `POST /api/mobile/chat/start` | `POST /api/mobile/chat/start` | ✓ |
| `POST /api/mobile/chat/stop` | `POST /api/mobile/chat/stop` | ✓ |

## 13. 评审 / 审稿（已修复项回顾）

- `POST /reviews/check/post-check`：已由主 agent 修复（B3），后端路由在 `review_tavern.rs` 合并中存在。
- 前端 `_review-part.js` 已使用 `/api/v1/reviews/check/post-check`（核对：`review_tavern.rs` 内的合并 router 提供）。

## 14. 已知差异 / 历史修复

- ~~`/api/v1/partner/sample`~~：前端曾误调，本次改为调用 `/api/v1/story-tavern/packs/demo`（A3 空态「使用示例角色」按钮）。
- ~~`POST /reviews/check`~~：重复路径，已由主 agent 修复为 `POST /reviews/check/post-check`（B3）。

## 15. 维护约定

1. 新增前端 `api('/...')` 调用时，先在此文件追加对应行；若后端路由尚未实现，标 `✗ 待实现`。
2. 新增后端路由时，回头在此文件补一行，避免前端漏调。
3. 路径前缀保持 `/api/v1/*`（故事馆 `/api/v1/story-tavern/*`；移动兼容 `/api/mobile/*`）。Jobs/SSE 复用既有协议，不新增并行 stream 路径。
4. 变量段统一用 axum `{name}` 写法；`{*rel}` 表示 catch-all。
