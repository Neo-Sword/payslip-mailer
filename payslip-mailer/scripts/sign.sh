#!/usr/bin/env bash
# 薪邮 · 代码签名
#
# 用法：bash scripts/sign.sh <path/to/薪邮.app>
#
# 需要：Developer ID Application 证书（不是 Apple Distribution / Developer ID Installer）
# 检查：security find-identity -v -p codesigning | grep "Developer ID Application"

set -euo pipefail

APP_PATH="${1:?用法: bash scripts/sign.sh <path/to/薪邮.app>}"
IDENTITY="${SIGN_IDENTITY:-Developer ID Application}"

if [ ! -e "$APP_PATH" ]; then
  echo "✗ 找不到 $APP_PATH" >&2
  exit 1
fi

if ! security find-identity -v -p codesigning | grep -q "Developer ID Application"; then
  echo "✗ 没有可用的 Developer ID Application 证书。" >&2
  echo "  本机当前有效身份：" >&2
  security find-identity -v -p codesigning >&2 || true
  echo "  请先加入 Apple Developer Program 并创建 Developer ID Application 证书。" >&2
  exit 1
fi

echo "▸ 使用身份：$(security find-identity -v -p codesigning | grep 'Developer ID Application' | head -1)"

# --options runtime 开启 Hardened Runtime（公证要求）
# --timestamp 让签名在证书过期后依然有效（公证要求）
codesign --force \
  --sign "$IDENTITY" \
  --options runtime \
  --timestamp \
  --deep \
  "$APP_PATH"

# 校验签名
codesign --verify --deep --strict --verbose=2 "$APP_PATH"
# 查看 entitlements，确认没有多申请权限
echo "▸ entitlements："
codesign --display --entitlements - "$APP_PATH" 2>/dev/null || true

echo "✓ 签名完成：$APP_PATH"
