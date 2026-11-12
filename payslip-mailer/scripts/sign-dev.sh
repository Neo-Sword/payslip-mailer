#!/usr/bin/env bash
# 给 debug 构建签名 —— 目的只有一个：**让钥匙串条目记住的是「证书 + bundle id」，
# 而不是「这段二进制的哈希」**。
#
# 为什么必须有这步（2026-11-11 实测，TC-103 验收时撞上的）：
# macOS 钥匙串的 ACL 里存着「信任哪个程序」的**代码要求**。
#   · 未签名程序 → 按 cdhash（二进制哈希）标识 → **每重编译一次就变**
#     → 每次启动都要弹授权框 → 开发时每张卡都要重编译，于是每张卡都要弹一次
#   · 已签名程序 → 按 designated requirement（证书 + bundle id）标识 → 重编译不变
# 所以签名不是「开发环境的图方便」，它就是把**生产环境要用的那套机制**提前用起来：
# TC-600 的签名、TC-601 的升级演练，用的是同一件事。
#
# 身份是本机自签的开发证书，放在专用钥匙串里，不碰登录钥匙串。
# 下面的口令只保护这把**本地开发签名私钥**，不是任何凭据，
# 也不在 git 里（钥匙串文件本身在 ~/Library/Keychains/，不在仓库内）。
#
# 用法：bash scripts/sign-dev.sh [二进制路径]

set -euo pipefail

cd "$(dirname "$0")/.."

KEYCHAIN="$HOME/Library/Keychains/xinyou-signing.keychain-db"
# 身份名用 ASCII：中文名在脚本与 codesign 之间会被编码搞坏（实测过）。
IDENTITY="Xinyou Dev Signing"
BIN="${1:-src-tauri/target/debug/payslip-mailer}"

if [ ! -f "$KEYCHAIN" ]; then
  printf '✗ 找不到开发签名钥匙串 %s\n' "$KEYCHAIN"
  printf '  按 scripts/setup-dev-signing.sh 的步骤建一次即可。\n'
  exit 1
fi

if [ ! -f "$BIN" ]; then
  printf '✗ 找不到要签名的二进制 %s（先 cargo build）\n' "$BIN"
  exit 1
fi

# 钥匙串必须解锁，否则 codesign 取私钥会静默失败
security unlock-keychain -p "xinyou-dev-signing" "$KEYCHAIN"

# `--identifier` 要跟 `tauri.conf.json` 的 bundle id 一致：
# designated requirement 是「identifier + 证书」，两个都得对。
codesign --force --sign "$IDENTITY" --identifier cn.payslipmailer.desktop \
  --timestamp=none "$BIN"

# 验一下：签名在、designated requirement 是稳定的（按证书而不是按哈希）
printf '→ 签名完成：%s\n' "$BIN"
printf '  designated requirement: '
codesign -d -r- "$BIN" 2>&1 | sed -n 's/^designated => //p'
