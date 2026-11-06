#!/bin/bash
# 打包后处理：给 .app 注入权限声明 + 托盘应用标志，再重新签名
# 用法：npm run tauri build 之后执行 scripts/postprocess-app.sh
set -e

APP="src-tauri/target/release/bundle/macos/Sayit.app"
PLIST="$APP/Contents/Info.plist"

if [ ! -d "$APP" ]; then
  echo "找不到 $APP —— 先运行 npm run tauri build"
  exit 1
fi

echo "== 注入 Info.plist =="
# 麦克风用途声明：没有它，打包版申请麦克风会被系统静默拒绝（弹窗都不弹）
/usr/libexec/PlistBuddy -c "Add :NSMicrophoneUsageDescription string Sayit 需要使用麦克风进行语音识别（本地处理，不上传）" "$PLIST" 2>/dev/null || \
  /usr/libexec/PlistBuddy -c "Set :NSMicrophoneUsageDescription Sayit 需要使用麦克风进行语音识别（本地处理，不上传）" "$PLIST"
# 托盘应用：不在 Dock 显示图标
/usr/libexec/PlistBuddy -c "Add :LSUIElement string true" "$PLIST" 2>/dev/null || \
  /usr/libexec/PlistBuddy -c "Set :LSUIElement true" "$PLIST"

echo "== 重新签名（ad-hoc）=="
codesign --force --deep --sign - "$APP"

echo "== 完成 =="
echo "应用位置: $APP"
echo "可拖入 /Applications 使用；麦克风/辅助功能授权将以 Sayit 自己的身份进行"
