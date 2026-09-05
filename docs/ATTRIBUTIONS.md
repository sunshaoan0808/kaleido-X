# Kaleido 吞噬与借鉴致谢清单（ATTRIBUTIONS）

> 本文档记录 Kaleido 在开发过程中通过 **Morphling 能力吸收方法论**（锁定目标 → 拆解 → 提取测例 → 组件分解 → 调用/重写/舍弃 → 对照验证 → 固化）从开源项目吸收的能力，以及 UI/交互层面的设计对标借鉴。
>
> 发布 Kaleido 时，请在本清单基础上向以下项目致谢（见文末致谢模板）。

---

## 一、代码级吞噬（morphling）

### 1. Liyuan（梨园）— [weidu12123/Liyuan](https://github.com/weidu12123/Liyuan)

Node/TS 编写的 agent 角色扮演引擎（分轮演出流程 / 台上引擎 / 工具 schema）。Morphling 多波吞噬（S9.7-S9.11 五波 + T1 记忆账本），吸收时将其测试逐条移植为 Rust 单测。

| 吞噬功能 | 源模块 | Kaleido 落点 |
|---|---|---|
| 场记摘要 | `compaction.ts` | `memory_weaver.rs`：叙事过滤 + 前情提要/人物/承诺伏笔/事实账/场景守卫 + 增量合并 |
| 本地 embed 哈希兜底 | `memory/embed.ts` | `embed_hash.rs` + `llm_stream.rs::get_embedding`：NFKC+bigram+FNV-1a 第三级兜底 |
| 世界线（存档分叉） | `worldline.ts`（372 行） | `TavernSessionStore`：回档后 turn 前进再存档→新世界线，WorldlineView 视图 |
| 【询问】停笔协议 | `director.ts`（499 行，ask_director） | `build_tavern_system_prompt`：重大转折纯询问回合 + 决策门禁 |
| 【面板】可视化面板 | `panels.ts` | `TavernPanel` + `split_panels_from_narrative`：markdown/svg/html 三档，随会话持久化 |
| 剧情助手（双 agent 分治） | `assistant-gateway.ts` | `assistant_chat`：独立 LLM 会话、全局视角、**绝不代写剧情** |
| story_command | 梨园 story_command | `/reroll` 重生成、`/rewind N` 回退命令 |
| 【程序】内嵌交互块 | 梨园 show_html | 消息内嵌可交互 HTML，沙箱 iframe 渲染 |
| 【工具】MCP 工具块 | 梨园 mcp.ts | 提取【工具】块执行、结果存会话待下轮回填 |
| 记忆账本 ledger 模块 | T1 吞噬波 | `ledger.rs` |
| Memory Harness | 早期 morphling 波 | `harness.rs`：进程/叙事分离（53-63% token 节省）+ 结构化记忆 CRUD |
| Decision Cards / Dynamic Panels | 早期 morphling 波 | 决策卡 + 动态面板（Liyuan innovations） |

### 2. denova — [alfredxw/denova](https://github.com/alfredxw/denova)

Go 编写的面向创作者的 AI 创作工具（导演台 / 检定 / 事件卡 / 上下文管理）。Kaleido 导演台差距收口（G1-G16 系列）主要吞噬源。

| 吞噬功能 | 源能力 | Kaleido 落点 |
|---|---|---|
| S1 演出机骨架 | stage machine 数据契约 | `StoryPack.stage_director` / `TavernSession.actor_states`（serde 向后兼容） |
| S2 Actor 状态机 | actor state machine | `ActorFieldValue.update_instruction` / `ActorStateUpdate` / `build_context_text` / `apply_updates` + 【状态更新】块解析应用 |
| S3 规则检定 | TRPG 检定 | `roll_check`：DC 五档 5/8/12/15/18 + roll_mode + state_bindings + critical_success/failure |
| S3c 检定卡渲染 | 检定结果 UI | 前端【检定结果】块 → 骰面/DC/成败徽章卡片 |
| S4 导演计划 | 导演调度 | 导演计划调度 + 红线下沉（不改 locked_beats） |
| P1-1/P1-3 导演 agent | interactive_director | `generate_director_plan_llm` + 原著剖析 outline 牵引主线 |
| P1-2 主线强度档位 | mainline_strength | strong_arc / balanced / soft 三档 |
| G1 导演计划三文档 | DirectorPlanDocs | plan / agent_brief / lore_context |
| G2 计划状态机 | — | ready / running / conflict 三态 |
| G3 开局导演规划 | opening_plan | 首回合自动生成开局三文档 |
| G4 上下文预算拟合 | fitTextToTokenBudget | `fit_text_to_token_budget`：超预算头尾保留中间省略 |
| G5 导演上下文账本 | ContextLedger | `DirectorLedgerEntry` 审计登记 |
| G6/G7 事件卡包 | TellerEventCard | `event_packages`：category/tags/intensity/cooldownTurns + 导演事件目录 |
| G8 导演策略枚举 | story_directors.go | failure_policy / pacing_curve / event_frequency / rule_visibility_mode / branch_planning_turns |
| G10 回合提交幂等 | 提交幂等 | `turn_submit_guard` 纯函数 + 幂等回执 |
| G11 module_refs 开关 | director_modules.go | 5 个 `*_disabled` 开关（关闭保留原 ID） |
| G13/G14 导演工具面 | 后台任务组 | `DirectorTaskGroup` 串行 + run 后台化 |
| G15 守卫事件保留 | retainedTurnsForInteractiveCompaction | `retain_guard_events`：超窗优先淘汰 med、high 保底 |
| G16 前端配置面 | 策略表单 | 导演台编辑策略（PUT director-config） |
| S5/S6 stage machine | event packages + director config | 事件卡包 + 导演配置 + actor archive API + wand 面板 |
| P0 写作三档 | novel-lite/standard/heavy SKILL | 回合生成 quality 档位（lite=单次直出 / standard=审稿修订 / heavy=writer→reviewer→fixer→final-gate 管道） |
| P1 图像管线调研 | imagegen/illustration/bookcover/loreimage/imagepreset/interactiveimage | `image_pipeline.rs` 图像消费管线（书封/章节插图/资料配图/预设库）设计依据 |
| P3 自动化编排 | 任务调度/管道 runner/子 agent 协作 | 自动化编排调研（任务队列/子 agent 协议/恢复重试） |
| memory-patcher | memory patcher | QUALITY_MEMORY_SYS 四类状态补丁（progress/character_state/world_state/foreshadowing）+ 记忆落盘 |
| 世界状态 | world_state | U2 世界事件（EntityCreated/Updated、flag/counter/relationship/meta） |

### 3. xiami（虾米）— [zhangxunvvv/xiami](https://github.com/zhangxunvvv/xiami)

Tauri2 + Rust 小说叙事引擎（剧情质检 / 大纲补丁 / 角色卡蒸馏）。X1-X7 七波全链路吞噬（含 server 接线、前端展示、全格式验收）。

| 吞噬功能 | 波次 | Kaleido 落点 |
|---|---|---|
| 剧情因果推演校验 | X1 | `st_simulation.rs`：开场四要素/因果节拍/六字段完整性/可行性六项/占位任务检测 |
| 读者速读分析 | X1 | `st_skimming.rs`：超短段/文字墙/纯对白/重复句首检测 |
| 情绪钩子引擎 | X1 | `st_emotional_hooks.rs`：12 种同构结尾信号 + 三份合同（规划/执行/结算） |
| 三大质检 server 接线 | X2 | 速读→质量管道、钩子合同→系统提示、推演→导演计划 |
| 质检前端展示 | X3 | 导演台 🧪 虾米质检区（P1 红/P2 黄 + 修复建议） |
| 大纲补丁系统 | X4 | `st_outline.rs`：补丁影响分析（direct/indirect BFS）+ 章节执行合同 + 导演台注入 |
| 写作风格分析 | X5 | 文笔样本采样 + 12 维风格分析端点 |
| 角色卡蒸馏（全格式输入） | X6 | `st_card_webp.rs`：WEBP RIFF EXIF/XMP + JPEG APP1 纯 Rust 解析 + V1 平铺 + 世界书管线打通 |
| 卡片插图 / 外链 / 章节模板 | X7 | `st_card_illustrations.rs`（catbox 外链）+ `st_card_skill.rs`（13 章节 skill 模板）+ assets 提取 |

### 4. tavern-card-distiller — [leigegehaha/tavern-card-distiller](https://github.com/leigegehaha/tavern-card-distiller)（MIT）

SillyTavern 角色卡蒸馏器（把角色卡蒸馏成 13 章节 AI skill）。X6/X7 输入侧补全源（吸收可移植纯逻辑，不搬 Python 依赖）。

### 5. SoulLink — [Rosa9527/SoulLink](https://github.com/Rosa9527/SoulLink)

SillyTavern 角色扮演辅助插件（对话驱动增量档案系统）。

| 吞噬功能 | 波次 | Kaleido 落点 |
|---|---|---|
| CharacterArchive 档案结构 | Wave1 | 标量字段 + 5 分节（personality/worldview/family/relationships/memory）+ apply_diff/apply_refine/purge_by_source 纯函数 |
| 档案维护端点 + 提示词 | Wave2b | `POST /packs/{id}/archive/analyze|refine`、`DELETE /archive/purge/{source}` + 5 套提示词（prompts.generated.js v1.3.1 逐字移植） |
| 角色卡档案面板 | Wave2c | 前端 scalars/5 分节展示 + 分析/精编按钮 |

### 6. novel2hermes_jp — [sunshaoan0808/novel2hermes_jp](https://github.com/sunshaoan0808/novel2hermes_jp)

日轻 → Hermes 叙事管线项目。

| 吞噬功能 | 波次 | Kaleido 落点 |
|---|---|---|
| 情感曲线 + 角色弧 + 36 项卡 | T5 | `emotion_curve.rs`/`character_arc.rs` → `POST /api/v1/analysis/emotion-curve` + `GET /api/v1/analysis/character-arc`（2026-08-14 打通孤岛） |
| 通用叙事工程 | T6 | `novel_workflow.rs`（stage 流转） |

### 7. ai-novel-screenplay-analyzer — [ops120/ai-novel-screenplay-analyzer](https://github.com/ops120/ai-novel-screenplay-analyzer)

AI 小说剧本分析器。

| 吞噬功能 | 波次 | Kaleido 落点 |
|---|---|---|
| 关系演化图谱 | T4 | `relation_evolution.rs` → `GET /api/v1/analysis/relation-evolution`（2026-08-14 打通孤岛 + 修复 relation_count 除2 bug） |

### 8. hermes-fake-moa — [kgmkm/hermes-fake-moa](https://github.com/kgmkm/hermes-fake-moa)

MoA（Mixture-of-Agents）模拟服务。

| 吞噬功能 | 波次 | Kaleido 落点 |
|---|---|---|
| MoA 对比面板 | T2 | `moa_comparison.rs`（后续扩展为真聚合：aggregator LLM 并排共存 + 持久化） |

### 9. TavernWeave — [LiarMTTT/TavernWeave](https://github.com/LiarMTTT/TavernWeave)

SillyTavern 角色卡蒸馏方法论（方法论级吸收）。

| 吞噬功能 | Kaleido 落点 |
|---|---|
| 演出层字段 | `PackCharacterRef` 新增 gender/appearance/opening_scene/opening_lines |
| CoT 模块契约 | 蒸馏提示词重构为 M1-M8 八模块契约（身份/性格/台词/动机/关系/认知/演出/证据，含输入/产出/降级规则） |
| A1 四层验证门 | A1-static / evidence / cover / fffd |
| 世界书 lore 路由契约 | lore 条目 activation/depth 契约字段 |
| variable-systems 字段生命周期 | `ActorStatePackConfig` 生命周期文档 |

### 10. Openwrite — [LiPu-jpg/Openwrite](https://github.com/LiPu-jpg/Openwrite)

AI 写作工具（对标任务书「Openwrite + 四图谱」；U 系列任务参考）。

| 参考能力 | Kaleido 落点 |
|---|---|
| long-text semantic chunking + progressive compression | U1 长文本语义切块，渐进式压缩替代硬截断 |
| 图像消费模块 | U10 `image_pipeline.rs`：书封/章节插图/资料配图/预设库（与 denova P1 图像管线调研共同作为设计依据） |
| epoch 上下文压缩 | U11 上下文阈值压缩 + 会话累计账本 |
| **伏笔 DAG** | Kaleido 原为平铺 store，受 Openwrite 伏笔 DAG 启发实现：权重（1-10）/父依赖/环检测 + 统计/依赖 API + 前端依赖链编辑 |
| 能力对标 | 对标差距报告（9 项差距 + 二轮补充）用于功能收口 |

### 11. 叙界（scriverse / kaleido-xujie）— 本地姊妹项目（无公开仓库）

Kaleido 的姊妹项目（Rust 服务，`<REFERENCE_PROJECT>`，无 git remote；英文名 scriverse）。以下功能从叙界吸收。

| 吞噬功能 | Kaleido 落点 |
|---|---|
| P2 生成后多维守卫 | `story_tavern.rs`：ST-26 人名黑名单扩展为多维守卫（high=打回阻止跳章推进 / med=提示不阻断）+ Canon tag 回归原著注入 |
| P2-1 情绪字段枚举 | 10 项情绪枚举（平静/开心/愤怒/悲伤/害羞/惊讶/恐惧/厌恶/疲惫/心动）+ 【状态更新】emotion 注入 + 前端 emoji 角标 + 立绘渲染（按情绪切图/生成立绘工具） |
| P2-3 守卫事件回放 | 前端叙界守卫事件回放（guardEvents 最近 20 条，high/med 格式消息） |
| DOCX 导入 | 书架导入链路（mammoth 解析 DOCX，扩展 accept .docx） |

### 12. Legado（阅读）— [gedoor/legado](https://github.com/gedoor/legado)（46.9k★）

开源阅读 App。移植纯算法模块（编码识别 + 目录切分）。

| 移植功能 | Kaleido 落点 |
|---|---|
| ICU4J CharsetDetector 统计识别 | `encoding_sniff.rs`：Big5/GB18030 逐字节状态机 + commonChars 高频字表加权打分（decode_text 兜底路径） |
| multi-rule TOC splitting | `split_novel_chapters`：txtTocRule.json 核心启用规则移植（去 Rust regex 不支持特性） |

### 13. OpenHanako（HanaAgent）— [liliMozi/openhanako](https://github.com/liliMozi/openhanako)（Apache 2.0）

TS/Electron 桌面 AI Agent（589K 行，记忆/工具/沙盒/插件体系）。H1 波次（2026-08-14）吸收记忆契约层；其余 Agent 平台能力（computer-use/vision/沙盒/凭据/插件/多平台接入）因产品定位差异舍弃。

| 吞噬功能 | 源模块 | Kaleido 落点 |
|---|---|---|
| 记忆输出结构契约 + 写前校验 + 修复循环 | `lib/memory/rolling-summary-format.ts` | `st_memory_contract.rs`：四类记忆补丁 JSON 契约校验 + 场记摘要五节结构校验 + 修复 prompt/输入构造（MAX_REPAIRS=1） |
| 分类省略统计 | `core/lossy-local-compaction.ts`（OmissionCounts） | `memory_weaver.rs::serialize_for_summary_with_stats`：program/reasoning/empty/other/kept 分类统计 |
| 修复循环接线 | memory reflection 侧任务模式 | `run_quality_refine` memory_patch 校验→修复重试；compact 端点场记校验→修复→降级 |

---

### 14. Front Porch AI — [linux4life1/front-porch-AI](https://github.com/linux4life1/front-porch-AI)（AGPL-3.0，重实现未搬代码）

Flutter 本地优先角色扮演应用（Realism Engine 活人感引擎 + The Stoop 社区角色站）。P1→收口 + 全自动事件提取吞噬（2026-09-02→09-05）。

| 吞噬功能 | 源模块 | Kaleido 落点 |
|---|---|---|
| 口袋/衣物/暂存堆 | `pockets.dart` 889行 | `pockets.rs` 732行 + 会话/播种/提示词/API/导演台 + À la carte 独立开关 |
| Needs 六维 + 灾变 | `needs_simulation.dart` 692行 | `needs.rs` + auto-tick 衰减 + 提示词联动 |
| Journal 物理/存量/召回 | `journal_physics/store/injection` | `journal_physics.rs` + `journal_store.rs`（heat/冷卡召回/物品卡） |
| 成长年轮 + 物理阈值 | `growth_service` | `character_arc.rs` GrowthRing/GrowthStore + tier/注入选择 |
| 世界气候 | `world.dart` atmosphere/gravity | `world_climate.rs` + 提示词守卫 |
| Chaos/Chance Time | `chaos_mode_service` | `chaos.rs` + auto-tick 压力 |
| 羁绊/里程碑/目标/夜梦 | `relationship/objective/dream` | `relationship.rs` + `objectives.rs` + `dreams.rs` + 承诺债务 `promise.rs` |
| 心情基线/在场/场景渐隐/偏好 | `mood_baseline/presence/scenario/preference` | `mood_presence.rs` + prompt 加权 |
| 定时效应 sticky/cooldown | `lorebook_timed_effects` | `st_world_info.rs` 酒馆链路 + pill |
| 全自动事件提取 | （自研，Front Porch 无对应） | 回合末后台 LLM 直写 + remerge/CAS + resolve_cid |

---

## 一·五、评估过但未落地代码的候选（避免遗漏争议）

以下项目经代码级评估（功能交叉对比、吞噬候选清单），**未产生代码级吸收**（仓库中无引用），仅调研/评估过，特此声明：

| 项目 | 地址 | 评估内容 |
|---|---|---|
（当前无——OpenHanako 已于 2026-08-14 转正为主清单第 13 项）

---

## 二、UI / 交互设计对标借鉴（非代码级）

| 项目 | 地址 | 借鉴点 |
|---|---|---|
| Agnai | [agnaistic/agnai](https://github.com/agnaistic/agnai) | 角色背景沉浸模式（当前发言角色立绘模糊背景） |
| Omate | 未公开地址 | 内心独白折叠区块、事件书剧情链面板 |
| RisuAI | [kwaroran/Risuai](https://github.com/kwaroran/Risuai) | 角色卡自动摘要展示、`{{image::URL}}` 内联媒体 |
| SillyTavern | [SillyTavern/SillyTavern](https://github.com/SillyTavern/SillyTavern) | 消息书签、部分消息编辑、聊天宽度滑杆、消息时间戳/token 显示、气泡三风格、Swipe 备选回复（含 PR #5304 风格的历史选择器弹窗） |

---

## 三、致谢模板（发布时使用）

```
Kaleido 的部分能力吸收自以下开源项目（Morphling 能力吸收方法论），特此致谢：

- Liyuan（梨园）— https://github.com/weidu12123/Liyuan（场记摘要/世界线/可视化面板/剧情助手等）
- denova — https://github.com/alfredxw/denova（导演台/规则检定/事件卡包/上下文管理）
- xiami（虾米）— https://github.com/zhangxunvvv/xiami（剧情质检/大纲补丁/角色卡蒸馏）
- tavern-card-distiller — https://github.com/leigegehaha/tavern-card-distiller（MIT，角色卡蒸馏输入管线）
- SoulLink — https://github.com/Rosa9527/SoulLink（角色档案系统）
- novel2hermes_jp — https://github.com/sunshaoan0808/novel2hermes_jp（情感曲线/叙事工程）
- ai-novel-screenplay-analyzer — https://github.com/ops120/ai-novel-screenplay-analyzer（关系演化图谱）
- hermes-fake-moa — https://github.com/kgmkm/hermes-fake-moa（MoA 聚合）
- TavernWeave — https://github.com/LiarMTTT/TavernWeave（角色卡蒸馏方法论）
- Openwrite — https://github.com/LiPu-jpg/Openwrite（长文本语义切块/图像消费）
- 叙界 — 本地姊妹项目 kaleido-xujie（生成后多维守卫/情绪枚举/立绘渲染）
- Legado（阅读）— https://github.com/gedoor/legado（Big5/GB18030 编码识别/目录切分）
- Front Porch AI — https://github.com/linux4life1/front-porch-AI（AGPL-3.0，思路重实现：口袋/Needs/Journal/羁绊/事件提取等活人感系统）
- OpenHanako（HanaAgent）— https://github.com/liliMozi/openhanako（记忆输出契约/修复循环/省略统计）

UI/交互设计参考：Agnai、Omate、RisuAI、SillyTavern 及其生态。
```

---

*清单生成日期：2026-08-14。吞噬记录代码实证来源：`grep -rn "吸收自" crates/ src/` + `git log --grep="吞噬|morphling|absorb"`。*
