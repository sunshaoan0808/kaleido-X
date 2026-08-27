---
name: novel-standard
description: 默认写作流程，由主 Agent 写作和修订，审稿子 Agent 严格审稿，在质量和速度之间取得平衡。
kind: writing
tier: standard
agent: ide
parents: ["tavern"]
---

# novel-standard

写作 Skill（standard 档）。默认写作流程：主 Agent 初稿 → 审稿子 Agent 审稿 → 主 Agent 修订，在质量与速度之间取得平衡。

## 写作范围判断

- 从用户实际指令判断写作范围；没有独立 `writing_scope` 字段，用户消息是唯一来源。
- 多段写作先制定整体计划与分章计划（简略，用于指导初稿）。

## 流程

主 Agent 写初稿 -> 审稿子 Agent 审稿 -> 主 Agent 修订和更新状态 -> 最终输出

标准流程只使用两个 Agent：主 Agent 与审稿子 Agent（reviewer）。不启动 `writer`/`fixer`/其他额外写作子流程。

## 工具使用要求

- 写作前读取必要上下文：CREATOR/outline/progress/character-states/章节组细纲/最近章节/lore 相关条目。
- 整章覆盖用 write_file，局部修订用 edit_file（old_string 来自最近 read_file 实际内容，无行号前缀）。
- 每次写文件后校验工具结果，失败重新读取修正后重试，不宣称已完成。
- 最终输出前 read_file 回读关键片段确认落盘。

## 审稿要求

- 审稿必须经 `task` 委派 `reviewer`：description 写明目标/章节路径/必要上下文/重点/输出格式，`reviewer` 只审不改文件。
- reviewer 严格检查连续性、资料库匹配、节奏、文风、人物动机、剧情逻辑及创作规则遵守；不输出赞扬。
- 输出结构化问题（severity/dimension/problem/证据位置/影响/fix_instruction/keep）。

## 修订要求

- 主 Agent 只修真问题，保留原文强段落、有效情节节点、人物声线与连续性。
- 修订后立即同轮更新 `setting/progress.md` 与 `setting/character-states.md`。
- 只有长期稳定设定重大变化才提出资料库更新建议（经确认后执行）。

## 最终输出

- 返回最终正文或用户要求的写作产物。
- 除非用户要求，不输出 reviewer 报告或内部修订说明。