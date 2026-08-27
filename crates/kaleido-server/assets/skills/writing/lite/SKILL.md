---
name: novel-lite
description: 快速续写、灵感初稿和低延迟正文生成；由主 Agent 直接输出结果，不启动审稿或修稿子流程。
kind: writing
tier: lite
agent: ide
parents: ["tavern"]
---

# novel-lite

写作 Skill（lite 档）。用于低延迟正文生成：单 Agent 直出最终正文，不启动 reviewer/fixer/task 等子流程。

## 写作范围判断

- 从用户的实际指令判断写作范围，例如“续写一段”“写一个场景”“写一章”或用户自定义目标。
- 用户消息是判断范围、目标、约束和输出形态的唯一来源。
- 若要求一次写多段，做轻量内部拆分后按用户要求的规模创作。

## 流程

main agent -> final output

## 工具使用要求

- 若需作品连续性，先读取相关状态（outline / progress / character-states / 最近章节 / 相关 lore 条目）。
- 对话内片段或灵感稿直接输出，不写入文件。
- 写文件用 write_file / edit_file，并校验工具结果；[tool error]/string not found/截断 时不得宣称已完成。
- 本轮写完整章节或实质剧情改写时，同轮同步更新 progress 与 character-states；纯错字/标点/措辞润色不更新。

## 规则

- 只由主 Agent 直接写出最终结果。
- 不启动 reviewer、fixer、task、General SubAgent 或任何已配置 subagent 流程。
- 可做轻量内部自检（连续性、用户要求、明显文句问题），但不输出审稿过程。
- 保留用户的控制感，不把用户要的初稿改写成另一个故事。

## 输出

- 直接输出用户要求的创作结果。
- 仅当用户要求说明或存在无法满足的重要约束时，补充简短说明。
