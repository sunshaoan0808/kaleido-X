# Kaleido DECISIONS（工程裁定）

**Date anchor:** 2026-07-28  
**Scope:** 产品/工程不可轻易推翻的一句话裁定。细节见各波次 docs。

---

## W20 — 单用户 · 非多租户

**裁定：** Kaleido 当前产品形态是 **本机/单实例 dogfood 的单用户（或少量共享账号）Web 服务，不是多租户 SaaS。**

| 含义 | |
|------|--|
| Auth | username/password + bearer；`workspace_id` 字段为 **预留**，运行时默认单工作区 |
| 不做 | per-tenant 隔离计费、org 成员、跨租户 ACL、SaaS 配额商品化 |
| 会话 cap (W12) | **全局** auth session 上限，不是 per-user SaaS quota |
| 数据根 | 单 `$KALEIDO_DATA`；不按 tenant 分库 |
| 以后若要做多租户 | **新 ADR + 显式迁移**；不得 silent 把现网当多租户 |

**对照：** `docs/BACKEND_REFACTOR_PLAN_UI_DEFERRED.md` §4「多租户 SaaS — 不做」；`docs/WEB_PRIORITY_GAP_PLAN.md` W20。

---

## 其它已锚定（索引）

| ID | 一句 |
|----|------|
| UI 冻结 | 工程波次不改 `web/*` 业务 UI；专人接契约 |
| 壳后置 | Android / Tauri 发行不排进 B0–B4 |
| 爬虫默认 off | `crawlerEnabled=false`；见 `docs/W19_CRAWLER.md` |
| Agent 写默认 off | W10 `agentWriteEnabled=false` |
| 真源 | Story Tavern 世界书以卡内嵌/ stBookRaw 为准（Trellis 另仓） |
