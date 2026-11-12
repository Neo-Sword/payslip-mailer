#!/usr/bin/env bash
# 薪邮 · 构建 → 制 dmg → 校验和
#
# 用法：
#   bash scripts/release.sh              # 未签名（默认，决策 D8）
#   bash scripts/release.sh --signed     # 签名 + 公证（需 Developer ID 证书，当前不购买）
#
# 未签名产物的分发注意：
#   · U 盘 / 局域网共享拷贝不会打 quarantine 属性 → 双击直接打开
#   · 网盘 / 浏览器下载会打 quarantine → 首次需「系统设置 → 隐私与安全性 → 仍要打开」

set -euo pipefail

cd "$(dirname "$0")/.."
# shellcheck source=./env.sh
. "$(dirname "$0")/env.sh"

SIGNED=0
[ "${1:-}" = "--signed" ] && SIGNED=1

TARGET="universal-apple-darwin"
VERSION="$(node -p "require('./package.json').version")"

# 发布产物**不放 dist/**：dist 是 vite 的输出目录（tauri.conf.json 的 frontendDist），
# 而 vite 构建前会清空 outDir。若 dmg 也放那里，下一次 `npm run build` 会把 dmg 静默删掉。
OUT_DIR="./artifacts"

# 注意：变量后紧跟全角括号时必须加花括号。
# bash 会把多字节字符的首字节并入变量名，配合 `set -u` 会报 “unbound variable”。
echo "▸ [1/5] 构建 ${TARGET}（v${VERSION}）"
npm run tauri build -- --target "$TARGET"

APP_PATH="src-tauri/target/$TARGET/release/bundle/macos/薪邮.app"
if [ ! -e "$APP_PATH" ]; then
  APP_PATH="src-tauri/target/$TARGET/release/bundle/macos/payslip-mailer.app"
fi
[ -e "$APP_PATH" ] || { echo "✗ 未找到构建产物" >&2; exit 1; }
echo "  产物：$APP_PATH"

if [ "$SIGNED" = "1" ]; then
  echo "▸ [2/5] 代码签名"
  bash scripts/sign.sh "$APP_PATH"

  echo "▸ [3/5] Apple 公证 + 装订"
  bash scripts/notarize.sh "$APP_PATH"
else
  echo "▸ [2-3/5] 跳过签名与公证（未签名构建，符合决策 D8）"
  echo "  注意：未签名 + 网盘分发需每台机器手动放行；升级后 Keychain 授权会重置"
fi

echo "▸ [4/5] 制作 dmg"
bash scripts/make-dmg.sh "$APP_PATH" "$OUT_DIR"

echo "▸ [5/5] Gatekeeper 检查"
if spctl -a -vv "$APP_PATH" 2>&1; then
  echo "  ✓ Gatekeeper 通过"
else
  if [ "$SIGNED" = "1" ]; then
    echo "  ✗ 已签名构建却未通过 Gatekeeper，检查证书与公证状态" >&2
    exit 1
  fi
  echo "  （未签名构建被 Gatekeeper 拒绝，属预期行为）"
fi

echo
echo "✓ 发布完成。产物："
ls -lh "$OUT_DIR"
echo
echo "下一步：把 dmg + .sha256 一起交付；建议走 U 盘或局域网共享以避开 quarantine。"
