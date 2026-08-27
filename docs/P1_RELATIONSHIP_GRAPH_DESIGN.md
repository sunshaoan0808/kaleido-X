# P1 设计：人物关系图 + 银河图（表结构与 API 契约）

日期：2026-08-02 · 分支：feature/absorb-scriverse · 状态：待评审
参考源：refs/scriverse/src/public/relationship-graph.js（2469 行，实测）
后端约定：axum `Router::new().route(...)`（见 crates/kaleido-server/src/*.rs）；存储走 `crates/kaleido-core/src/db.rs`（rusqlite bundled，P0 已就绪）。

## 1. 数据模型（从参考源实测提取）

Edge 字段（relationship-graph.js:105-118 实测）：
```
category          family|social|emotional|conflict|uncertain（中文标签：亲属/社交/情感/冲突/未确定）
subtype          string，如 "师徒"（可空）
keywords         string[]，展示用"subtype · keyword1 · keyword2"
confirmationStatus  pending|confirmed（pending 显示"待确认"）
```
Node：角色（character），无内置固定色，由 OBSIDIAN_NODE_PALETTE（24 色）按序分配。
Galaxy 参数：GALAXY_LAYOUT_CONFIG(minimumRadius 220, radialSpan 830, repulsion 9200, desiredEdgeLength 285)、GALAXY_BASE_STAR_COUNT 7200、ROTATION 0.000012 rad/ms。

## 2. SQLite 表结构（migration v2，追加在 P0 的 v1 之后）

```sql
-- 角色节点（挂在一个 work/pack 下）
CREATE TABLE IF NOT EXISTS characters (
  id          TEXT PRIMARY KEY,            -- uuid
  work_id     TEXT NOT NULL,               -- 对应 kaleido work（pack_id 或作品 id）
  name        TEXT NOT NULL,
  aliases     TEXT NOT NULL DEFAULT '[]',  -- JSON array
  note        TEXT NOT NULL DEFAULT '',
  color_idx   INTEGER NOT NULL DEFAULT 0,  -- OBSIDIAN palette 序号（可空则自动分配）
  created_at  TEXT NOT NULL,
  updated_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_chars_work ON characters(work_id);

-- 关系边
CREATE TABLE IF NOT EXISTS relationships (
  id          TEXT PRIMARY KEY,
  work_id     TEXT NOT NULL,
  from_char   TEXT NOT NULL REFERENCES characters(id) ON DELETE CASCADE,
  to_char     TEXT NOT NULL REFERENCES characters(id) ON DELETE CASCADE,
  category    TEXT NOT NULL,               -- family|social|emotional|conflict|uncertain
  subtype     TEXT NOT NULL DEFAULT '',
  keywords    TEXT NOT NULL DEFAULT '[]',  -- JSON array
  confirmation_status TEXT NOT NULL DEFAULT 'pending',  -- pending|confirmed
  note        TEXT NOT NULL DEFAULT '',
  created_at  TEXT NOT NULL,
  updated_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_rel_work ON relationships(work_id);
CREATE INDEX IF NOT EXISTS idx_rel_pair ON relationships(from_char, to_char);
```
设计决策：
- 不建 relationship_events 独立表（P1 不引入时间线；后续 P2 伏笔管理再按需追加）
- 双向外键边：from/to 有序，正向存储；UI 层双向展示（参考源即如此）
- work_id 命名与现有 works_fs/works API 对齐（works/{id}/...）

## 3. API 契约（挂到 /api/v1/works/{work_id}/graph 下）

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | /api/v1/works/{work_id}/graph | 全量图：`{characters:[], relationships:[], palette_meta:{...}}` |
| POST | /api/v1/works/{work_id}/graph/characters | 建角色（name 必填，aliases 可选） |
| PATCH | /api/v1/works/{work_id}/graph/characters/{id} | 改名/别名/备注/颜色 |
| DELETE | /api/v1/works/{work_id}/graph/characters/{id} | 删除（级联删关系） |
| POST | /api/v1/works/{work_id}/graph/relationships | 建关系（from,to,category 必填） |
| PATCH | /api/v1/works/{work_id}/graph/relationships/{id} | 改 category/subtype/keywords/confirmation_status |
| DELETE | /api/v1/works/{work_id}/graph/relationships/{id} | 删关系 |

响应统一 `{"ok":true,"data":...}` / `{"ok":false,"error":"..."}`（与现有 API 一致）。
鉴权：复用现有 session auth 中间件（与 works 同源）。
幂等：POST characters 若同 work 同名已存在 → 返回 409 + 提示（参考源有 identity-repair 机制，P1 先做同名冲突提示）。

## 4. 前端集成

- 入口：作者区 SPA 新增「关系图」面板（D2 定稿：进主 SPA，bookshelf 不动）
- 移植范围：relationship-graph.js 的网络布局 + 银河布局 + 调色板/常量（纯函数部分逐段拷贝，避免引入其 app.js 依赖）
- 事件链：节点增删改 → PATCH 后端 → 本地图状态增量更新（不做全量重拉）
- 3D 银河：保持 canvas 2D 实现（参考源即 canvas 2D + 视差旋转），不引 three.js

## 5. 验收门禁（P1 完成条件）

1. `cargo check -p kaleido-core -p kaleido-server` 通过
2. `cargo test -p kaleido-core` 新增关系图模块测试 ≥ 5 用例（CRUD + 级联删除 + 同名冲突）
3. s8_gate.sh 仍全绿（回归）
4. 浏览器实测：关系图增删改 + 银河图渲染 + 刷新持久化（web_scan/截图佐证）
5. 提交 feature/absorb-scriverse

## 6. 工作量拆分（P1 内部顺序）

1. migration v2 + 关系图 store（kaleido-core::graph_store）→ 2-3 天
2. REST 路由（kaleido-server::graph.rs，仿 agent_todo.rs 结构）→ 1 天
3. 前端面板 + 关系图渲染移植 → 2-3 天
4. 银河图视图移植 → 1-2 天
5. 测试 + 门禁 → 1 天
