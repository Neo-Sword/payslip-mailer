#!/usr/bin/env bash
# 薪邮 · 制作分发 dmg + 计算 SHA-256
#
# 用法：bash scripts/make-dmg.sh <path/to/薪邮.app> [输出目录]
#
# dmg 由本脚本产出，**不由 Tauri bundler 产出**（tauri.conf.json 的 targets 只有 "app"）。
# 原因见 docs/adr/0008：Tauri 的 bundle_dmg.sh 和 create-dmg 都要用 AppleScript 指挥
# Finder 摆图标，在没有 GUI 会话或 TCC 未授权的环境里会 AppleEvent 超时，构建随之失败。
# hdiutil 是纯命令行的，同样条件下必然成功；代价是 dmg 里没有美化布局。
#
# 无论哪种方式产出的 dmg，都要在干净 macOS 上验证双击可安装、Gatekeeper 行为符合预期。

set -euo pipefail

APP_PATH="${1:?用法: bash scripts/make-dmg.sh <path/to/薪邮.app> [输出目录]}"
OUT_DIR="${2:-./dist}"
VOL_NAME="薪邮 Payslip Mailer"
VERSION="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP_PATH/Contents/Info.plist" 2>/dev/null || echo "0.0.0")"

mkdir -p "$OUT_DIR"
DMG_PATH="$OUT_DIR/薪邮-$VERSION.dmg"

if [ ! -e "$APP_PATH" ]; then
  echo "✗ 找不到 $APP_PATH" >&2
  exit 1
fi

# 纯 hdiutil：拖 Applications 的引导靠一个符号链接实现，没有图标定位。
hdiutil_dmg() {
  local staging
  staging="$(mktemp -d)"
  cp -R "$APP_PATH" "$staging/"
  ln -s /Applications "$staging/Applications"
  hdiutil create -volname "$VOL_NAME" -srcfolder "$staging" -ov -format UDZO "$DMG_PATH" >/dev/null
  rm -rf "$staging"
}

if command -v create-dmg >/dev/null 2>&1; then
  echo "▸ 使用 create-dmg 生成定制 dmg"
  rm -f "$DMG_PATH"
  # 失败不致命：create-dmg 依赖 Finder，在无 GUI 会话下会超时，此时退回 hdiutil。
  if ! create-dmg \
    --volname "$VOL_NAME" \
    --window-size 660 400 \
    --icon-size 96 \
    --icon "薪邮.app" 180 170 \
    --app-drop-link 480 170 \
    --no-internet-enable \
    "$DMG_PATH" \
    "$APP_PATH"; then
    echo "  ! create-dmg 失败（多半是 AppleScript 操作 Finder 超时），回退 hdiutil" >&2
    rm -f "$DMG_PATH"
    hdiutil_dmg
  fi
else
  echo "▸ 未安装 create-dmg，用 hdiutil 生成基础 dmg"
  hdiutil_dmg
fi

echo "▸ 计算 SHA-256 校验值"
# 校验文件里只记文件名、不记路径：使用者 `cd` 到 dmg 所在目录后
# `shasum -a 256 -c *.sha256` 就能验。写死 ./dist/... 这种前缀，
# 一旦产物被移动（换目录、下载改名）校验就会失败，而失败原因看着像内容被改过。
(
  cd "$(dirname "$DMG_PATH")"
  shasum -a 256 "$(basename "$DMG_PATH")" | tee "$(basename "$DMG_PATH").sha256"
)

echo "✓ dmg 产出：$DMG_PATH"
echo "  分发时把 dmg 与 .sha256 一起放上内网下载页 / 飞书云盘"
