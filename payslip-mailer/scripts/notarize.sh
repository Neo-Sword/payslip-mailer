#!/usr/bin/env bash
# 薪邮 · Apple 公证 + 装订（stapler）
#
# 用法：bash scripts/notarize.sh <path/to/薪邮.app>
#
# 前置：
#   1. 应用已用 Developer ID Application 签好名
#   2. 已存好公证凭据：
#      xcrun notarytool store-credentials "AC_PASSWORD" \
#        --apple-id <APPLE_ID> --team-id <TEAM_ID> --password <APP_SPECIFIC_PASSWORD>
#      或使用 App Store Connect API Key：
#      xcrun notarytool store-credentials "AC_PASSWORD" --keychain-profile ...
#
# 环境变量：NOTARY_PROFILE（默认 AC_PASSWORD）

set -euo pipefail

APP_PATH="${1:?用法: bash scripts/notarize.sh <path/to/薪邮.app>}"
PROFILE="${NOTARY_PROFILE:-AC_PASSWORD}"

if [ ! -e "$APP_PATH" ]; then
  echo "✗ 找不到 $APP_PATH" >&2
  exit 1
fi

if ! xcrun notarytool history --keychain-profile "$PROFILE" >/dev/null 2>&1; then
  # 变量后紧跟全角标点必须写 ${PROFILE}：bash 会把多字节字符的首字节并进变量名，
  # 配合 set -u 会报 “unbound variable”（TC-600 实测，同一处坑 release.sh 也踩过）。
  echo "✗ 没有名为「${PROFILE}」的公证凭据。" >&2
  echo "  先执行：xcrun notarytool store-credentials \"$PROFILE\" --apple-id ... --team-id ... --password ..." >&2
  exit 1
fi

TMP_ZIP="$(mktemp -t payslip-notarize).zip"
trap 'rm -f "$TMP_ZIP"' EXIT

echo "▸ 打包上传：$TMP_ZIP"
ditto -c -k --keepParent "$APP_PATH" "$TMP_ZIP"

echo "▸ 提交 Apple 公证（可能需要几分钟）…"
xcrun notarytool submit "$TMP_ZIP" --keychain-profile "$PROFILE" --wait

echo "▸ 装订公证票据（stapler）…"
xcrun stapler staple "$APP_PATH"
xcrun stapler validate "$APP_PATH"

echo "✓ 公证完成：$APP_PATH"
echo "  离线验证：spctl -a -vv \"$APP_PATH\""
