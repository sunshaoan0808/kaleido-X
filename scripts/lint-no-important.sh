#!/usr/bin/env bash
# lint-no-important.sh — 禁止在 src/css 新增 !important（存量 556 个逐步消化，新增即拒绝）
# 用法：提交时由 .git/hooks/pre-commit 自动调用，或手动执行
#   bash scripts/lint-no-important.sh
set -uo pipefail

# 只检查暂存区中 src/css 的 .css 改动
CHANGED=$(git diff --cached --name-only | grep -E '^src/css/.*\.css$' || true)
if [ -z "$CHANGED" ]; then
  exit 0
fi

# -U0 无上下文，^\+[^+] 只取新增行（排除 +++ 文件头）
ADDED=$(git diff --cached -U0 -- $CHANGED | grep -E '^\+[^+]' | grep '!important' || true)
if [ -n "$ADDED" ]; then
  echo "❌ 禁止在 src/css 新增 !important（存量 556 个逐步消化中）："
  echo "$ADDED"
  echo ""
  echo "  替代方案："
  echo "   1. 提高选择器特异性（如 html[data-immersive] 前缀）代替 !important"
  echo "   2. 复用全局 .hidden（已带 !important 兜底）"
  echo "   3. 用 CSS 变量 / 组件层样式覆盖"
  exit 1
fi
exit 0
