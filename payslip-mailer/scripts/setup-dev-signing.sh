#!/usr/bin/env bash
# 建一个**本机自签的开发代码签名身份**，让 debug 构建有稳定的钥匙串授权。
#
# 为什么需要它（2026-11-11 实测，TC-103 验收时撞上的）：
# macOS 钥匙串的 ACL 里存着「信任哪个程序」的**代码要求**。
#   · 未签名程序 → 按 cdhash（二进制哈希）标识 → **每重编译一次就变**
#     → 每次启动都弹「想要使用你储存在钥匙串中的机密信息」
#   · 已签名程序 → 按 designated requirement（证书 + bundle id）标识 → 重编译不变
# 开发时每张卡都要重编译，所以不签名就是每张卡都要被弹窗打断。
#
# 这不是「开发环境的图方便」：TC-600 的签名、TC-601 的升级演练，
# 用的是同一套机制。这里只是把它提前用起来。
#
# 身份是自签的，私钥在本机专用钥匙串里，**不碰登录钥匙串**。
# 下面的口令只保护这把本地开发签名私钥，不是任何凭据。
#
# 用法：bash scripts/setup-dev-signing.sh

set -euo pipefail

KC="$HOME/Library/Keychains/xinyou-signing.keychain-db"
PW="xinyou-dev-signing"
NAME="Xinyou Dev Signing"
IDENTIFIER="cn.payslipmailer.desktop"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

if [ -f "$KC" ] && security find-identity -v -p codesigning "$KC" 2>/dev/null | grep -q "$NAME"; then
  printf '✓ 开发签名身份已存在，跳过创建：%s\n' "$KC"
else
  printf '▸ 建专用签名钥匙串 %s\n' "$KC"
  rm -f "$KC"
  security create-keychain -p "$PW" "$KC"
  security unlock-keychain -p "$PW" "$KC"
  security set-keychain-settings -lut 21600 "$KC"

  printf '▸ 生成自签代码签名证书\n'
  openssl req -x509 -newkey rsa:2048 -nodes \
    -keyout "$TMP/key.pem" -out "$TMP/cert.pem" \
    -days 3650 \
    -subj "/CN=$NAME/O=Xinyou/OU=$IDENTIFIER" \
    -addext "extendedKeyUsage=codeSigning" \
    -addext "keyUsage=digitalSignature" >/dev/null 2>&1

  # 证书与私钥必须**一起**导入（PKCS#12），否则系统不知道它们是一对，
  # `find-identity` 会显示「0 valid identities」。
  printf '▸ 导入身份\n'
  openssl pkcs12 -export -inkey "$TMP/key.pem" -in "$TMP/cert.pem" \
    -out "$TMP/id.p12" -passout "pass:$PW" -name "$NAME" >/dev/null 2>&1
  security import "$TMP/id.p12" -k "$KC" -T /usr/bin/codesign -T /usr/bin/security -P "$PW" >/dev/null
  security add-trusted-cert -r trustRoot -k "$KC" "$TMP/cert.pem" >/dev/null 2>&1 || true
fi

# 放进搜索列表：codesign 找不到不在列表里的钥匙串里的身份
security list-keychains -d user -s "$KC" "$HOME/Library/Keychains/login.keychain-db" >/dev/null 2>&1 || true

# **关键一步**：信任设置必须落在**登录钥匙串**里。
# 自签证书给了稳定的 designated requirement（按证书 + bundle id，不按二进制哈希），
# 但 macOS 在评估这个 requirement 时要能**验证**那张根证书 ——
# 信任设置只在自建的签名钥匙串里是不够的（实测：那样每次重编译仍然弹授权框；
# 放进登录钥匙串后，重编译多少次都不弹了）。
# 登录钥匙串里加的只是一张「信任这张证书可以签代码」的设置，不是任何凭据。
security add-trusted-cert -r trustRoot \
  -k "$HOME/Library/Keychains/login.keychain-db" "$TMP/cert.pem" >/dev/null 2>&1 || \
  printf '  （提示：把证书加进登录钥匙串的信任设置失败，需要手动执行一次）\n'

# 允许 codesign 使用这把私钥。不做这步，签名会报 errSecInternalComponent。
security unlock-keychain -p "$PW" "$KC"
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$PW" "$KC" >/dev/null 2>&1

printf '✓ 有效代码签名身份：\n'
security find-identity -v -p codesigning "$KC" 2>&1 | grep -E '^\s+[0-9]+\)' | sed 's/^/  /'
printf '下一步：bash scripts/sign-dev.sh\n'
