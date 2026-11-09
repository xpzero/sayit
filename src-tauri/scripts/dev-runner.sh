#!/bin/sh
# dev 签名稳定器：cargo run / cargo test 的产物先以固定身份「Sayit Dev」重签再执行。
#
# 为什么：ad-hoc 签名的 designated requirement 只认 CDHash——dev 二进制每重编译一次，
# TCC（辅助功能/麦克风授权）面前就站着一个"新 App"，授权随编译失效。固定身份签名后
# TCC 授权跨编译存活（AGENTS.md「本机实测事实」AX 探测死亡陷阱的根因修复）。
#
# 证书导入（一次性，用户执行）：
#   security import /tmp/sayit-dev.p12 -k ~/Library/Keychains/login.keychain-db \
#     -P sayit-dev-tmp -T /usr/bin/codesign
# 首次 codesign 使用可能弹钥匙串访问确认，选「始终允许」。
# 身份不存在时打一行警告并照常执行——绝不让缺证书挡住 dev。
# 注意：仅在 cwd=src-tauri 时被 cargo 发现（config 相对 cwd 解析）；从仓库根跑 cargo
# 不生效且无提示（仓库根无 Cargo.toml，常规用法无碍，脚枪在此注明）。
set -e
if err=$(codesign --force -s "Sayit Dev" "$1" 2>&1); then
  :
else
  # 回显真实错误（目标只读/钥匙串锁定/证书过期……病因不同，不能一律谎报"缺身份"）
  echo "[dev-sign] 重签失败（照常执行）：$err" >&2
fi
exec "$@"
