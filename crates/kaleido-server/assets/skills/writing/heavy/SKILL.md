---
name: novel-heavy
description: 关键内容、复杂剧情和长篇连续性要求高的写作流程；先规划、综合审稿、再生成状态更新。
kind: writing
tier: heavy
agent: ide
parents: ["tavern"]
---

# novel-heavy

写作 Skill（heavy 档）。用于关键场景、复杂剧情、长链路连续性高、需同步更新作品状态的写作任务。

## 写作范围判断

- 从用户实际指令判断写作范围；用户消息是判断范围、目标、约束和输出形态的唯一来源。
- 一次写 N 章或 arc 时，Context Plan 必须包含整体计划与分章计划。

## 流程

context-planner -> writer -> reviewer -> fixer -> final-gate -> memory-patcher -> final output

## 工具使用要求

- 写作前读取必要上下文：CREATOR/outline/progress/character-states/章节组细纲/最近章节/lore 相关条目。
- 所有角色 subagent 都通过 `task` 委派，description 写明角色名/目标/上下文来源/文件路径/允许禁止写入/期望输出格式/交付物。
- context-planner、reviewer、final-gate、memory-patcher 默认只返回计划/审稿/检查/patch，不直接改文件；writer、fixer 是否写文件由委派说明决定。主 Agent 对最终落盘负责。
- 整章 write_file、局部 edit_file、状态文件优先 edit_file；每次写后校验工具结果，Final Gate 通过后 read_file 回验。
- 长期资料库与短期状态分离；只有稳定 canon 重大变化才请求确认后进资料库。

## Context Plan

写作前先产生轻量计划（fixed structure）：

```md
# Context Plan
## Writing Scope
## Goal
## Required Beats
## Character State
## Canon Constraints
## Style Constraints
## Risks
```

多章补充 `整体计划` + `分章计划`（章节目标/关键事件/POV 焦点/结尾钩子）。

## 审稿协议

reviewer 返回结构化问题，每项包含：

- `severity`: `blocker` / `major` / `minor`
- `dimension`: `continuity` / `character_voice` / `pacing` / `prose` / `dialogue` / `plot_logic` / `style` / `user_requirement`
- `problem`
- `fix_instruction`
- `keep`

## Final Gate

- 仅当修订稿满足用户要求、Context Plan、canon 约束、风格约束与明显连续性时才 pass。
- 存在 blocker 时带明确指令交回 fixer 一次；不要新增额外 reviewer agent。

## Memory Patch

最终稿完成后生成可应用补丁：

- `progress`: 剧情/时间线/地点/风险/未解决线索的变化。
- `character_state`: 当前状态、动机、关系、伤病、已知信息、资源、承诺、秘密。
- `world_state`: 本轮即时故事状态中已变化的事实。
- `foreshadowing`: 新埋/推进/兑现/退场的伏笔。

只基于已发生内容。主 Agent 在权限允许时把 progress/character_state 写入状态文件；否则输出可应用 patch 并说明未写入原因。

## 最终输出

- 返回最终正文或用户要求的写作产物。
- 产生可持久化进展或用户要求时才附简短状态摘要；除非要求检查流程，隐藏内部角色对话。