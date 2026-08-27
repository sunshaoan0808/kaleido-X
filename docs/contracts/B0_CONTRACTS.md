# B0 — 契约盘点与稳定化

**Status:** done 2026-07-28  
**Scope:** documentation only — freeze surface for UI 专人 / regression  
**UI:** 无代码改动

---

## Deliverables

| 任务 | 产出 | 验收 |
|------|------|------|
| 公开 API 清单 | [`API_SURFACE.md`](./API_SURFACE.md) | path × method × auth × source；与 `.route` 扫描一致（~144 paths） |
| 统一错误体 | [`ERROR_BODY.md`](./ERROR_BODY.md) | canonical `{error, code?, …}` + `map_core_err` + 关键路径样例 |
| Job/SSE 事件枚举 | [`SSE_EVENTS.md`](./SSE_EVENTS.md) | Background / BookTravel / ST turn `eventType` 表 |
| 会话/死锁策略 | [`SESSION_DEADLOCK.md`](./SESSION_DEADLOCK.md) | auth vs tavern；`activeRunId` 自动清；stop 语义 |

**Index:** this file · plan mark in `docs/BACKEND_REFACTOR_PLAN_UI_DEFERRED.md`

---

## Non-goals (B0)

- OpenAPI/Swagger codegen  
- Rewriting every handler to coded errors  
- Playwright UI E2E (see W21 API chimney only)  
- W1+ resume implementation  

---

## Refresh recipe

After adding routes:

```bash
# re-scan (or re-run parent B0 route script) → docs/contracts/API_SURFACE.md
rg '\.route\(' crates/kaleido-server/src -g'*.rs' | wc -l
```

Smoke optional: `python3 scripts/w21_api_chimney_smoke.py` (surface alive, not full contract lint).

---

## Related wave docs

W2 / W8–W12 / W10 / W11 / W19 · `JOBS_V2.md` · `ST` turn unlock references
