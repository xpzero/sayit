#!/bin/sh
# 发布构建包装：治 DMG 打包的挂载卡死（"hdiutil: couldn't unmount … 资源忙"）。
#
# 病因链：tauri 的 bundle_dmg.sh 挂一块可写中间镜像拷文件，Spotlight/Finder 会碰它；
# 一旦某次失败，残留挂载让**之后每次构建**都在同一块盘上"资源忙"（2026-11-11 实测：
# 连续三次构建失败，手动 force detach 后立即恢复）。
#
# 本脚本三道保险：
#   ① 构建前强制弹出所有镜像路径含 sayit 的残留挂载
#   ② 先打 .app 再打 dmg——.app 是主要产物，dmg 失败不连坐
#   ③ dmg 失败 → 再清一次挂载 → 重试一次
set -eu
cd "$(dirname "$0")/.."

say() { printf '%s\n' "$*"; }

detach_stale() {
  hdiutil info 2>/dev/null | awk '
    /image-path/ { keep = tolower($0) ~ /sayit/; next }
    /^\/dev\/disk/ { if (keep) print $1 }
  ' | while IFS= read -r d; do
    say "  清理残留挂载 $d"
    hdiutil detach "$d" -force >/dev/null 2>&1 || true
  done
}

say "[1/4] 清理残留 DMG 挂载"
detach_stale

# 注意：app 与 dmg 必须同一次调用（-b app,dmg）——拆开时 dmg 单独跑会把
# "目标不含 app" 的中间产物 .app 清掉（tauri-bundler 的 Cleaning 行为，
# 2026-11-11 实测：先 -b app 再 -b dmg，结束后 Sayit.app 消失）
say "[2/3] 构建 .app + DMG（失败自动清挂载重试一次）"
if ! pnpm tauri build -b app,dmg; then
  say "  打包失败——清理挂载后重试"
  detach_stale
  pnpm tauri build -b app,dmg
fi

say "[3/3] 产物"
ls -d src-tauri/target/release/bundle/macos/*.app 2>/dev/null || true
ls src-tauri/target/release/bundle/dmg/*.dmg 2>/dev/null || true
