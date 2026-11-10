#!/bin/sh
# 结构门槛：源文件行数预算（AGENTS.md「结构铁律」的机器执法）。
# 超过预算 = 红，先拆文件再继续写。约定靠自觉，脚本不会忘。
# 用法：sh scripts/check-structure.sh（或挂在提交流程里）
LIMIT=${1:-500}
CSS_LIMIT=${CSS_LIMIT:-900}  # CSS 是声明式样式表（无逻辑分支、高冗余），单独放宽预算
cd "$(dirname "$0")/.."

fail=0

# Rust 源（含子模块）、前端源（含子目录）。测试文件同预算（tests 也是代码）。
for f in src-tauri/src/*.rs src-tauri/src/*/*.rs src/*.tsx src/*.ts src/*/*.tsx src/*/*.ts; do
  [ -f "$f" ] || continue
  case "$f" in
    *.d.ts) continue ;;
  esac
  lines=$(wc -l < "$f" | tr -d ' ')
  if [ "$lines" -gt "$LIMIT" ]; then
    echo "✗ $f = $lines 行（预算 $LIMIT）"
    fail=1
  fi
done

# CSS 单独走 CSS_LIMIT
for f in src/*.css src/*/*.css; do
  [ -f "$f" ] || continue
  lines=$(wc -l < "$f" | tr -d ' ')
  if [ "$lines" -gt "$CSS_LIMIT" ]; then
    echo "✗ $f = $lines 行（CSS 预算 $CSS_LIMIT）"
    fail=1
  fi
done

if [ "$fail" -eq 1 ]; then
  echo ""
  echo "结构门槛未过：先拆文件再继续（AGENTS.md 结构铁律：新功能优先新模块；"
  echo "往现有文件加代码前先问'这属于它的职责吗'）。"
  exit 1
fi
echo "结构门槛通过：Rust/TS ≤ $LIMIT 行，CSS ≤ $CSS_LIMIT 行"
