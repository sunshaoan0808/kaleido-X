# ADR: 存储边界与 Repository 抽象裁定（P2-6 / P-07）

**日期:** 2026-08-24 · **状态:** 已裁定 · **关联:** docs/DECISIONS.md W20, CODE_REVIEW_FINDINGS P-07/P-11

## 裁定一句话
**单机 SQLite 是产品边界；不引入 Repository trait 全量抽象，不做 PG 迁移路径。**
以「trait 化成本 >> 收益」为由显式关闭 P-07 的"先抽 trait 再谈迁移"路线，改为三条低成本护栏。

## 论据
1. W20 已锚定：本机/单实例 dogfood 单用户服务。多租户/分布式 = 新产品形态 = 新 ADR。
2. 现状盘点（2026-08-24）：core 内 SQLite 访问集中在 `db.rs`(406L) + 7 个 store 模块；
   lib.rs 无直接 Connection 使用——数据访问已天然收敛，全量 trait 抽象是重复劳动。
3. JobStore 全局 Mutex 队列 + KALEIDO_MAX_CONCURRENT_JOBS(1–2)：P-11 天花板由单机假设决定，
   抽象掉 SQLite 不改变该天花板；真到瓶颈时正确动作是「Jobs 外置队列」专项，而非 ORM 化。

## 三条护栏（替代 trait）
| # | 护栏 | 验收 |
|---|------|------|
| G1 | 新增 store 必须放 core、经 `db.rs` 打开的连接池/工厂，禁止 handler 层直连 | review checklist |
| G2 | schema 变更只走 `ensure_layout()` 增量分支（幂等 ALTER/CREATE IF NOT EXISTS），禁止启动外迁移脚本 | db.rs 单一入口 |
| G3 | 若未来出现第二个存储后端需求 → 重开 ADR，届时按 store 逐个 trait 化（strangler），不做一次性大爆炸 | — |

## 与 Jobs 分布式评估（P-11）的关系
同日裁定：**不评估分布式队列**。触发重估条件（满足其一才立项）：
- 单机并发 >4 job 且排队延迟成为用户可感痛点
- 出现第二实例需求（此时同时触发 W20 重审）
