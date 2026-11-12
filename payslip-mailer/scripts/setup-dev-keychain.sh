#!/usr/bin/env bash
# 预置**开发用**的钥匙串与库密钥，让开发时不再弹钥匙串授权框。
#
# 为什么需要它（2026-11-11 实测，TC-103 验收时撞上的）：
# macOS 给每个钥匙串条目配一份 ACL，「谁可以读」写在里面。默认 ACL 信任
# **创建它的那个程序** —— 对未签名程序来说身份是二进制的 cdhash，
# **每重编译一次就变** → 每次启动都弹
# 「想要使用你储存在钥匙串中的机密信息」，要人输登录钥匙串口令。
# 开发时每张卡都要重编译，于是每张卡都被这个弹窗打断一次。
#
# 试过「给开发构建自签一份」：designated requirement 确实稳定了
# （按证书 + bundle id，不按哈希），但 macOS 评估自签根证书时仍不放行，
# 照样弹。Apple 锚定的 Developer ID 才行（那需要付费开发者计划）。
#
# 所以开发构建改用一个**专用钥匙串文件**：
#   · 口令固定（只保护这台开发机上的开发密钥，不是任何凭据）
#   · 条目由本脚本以「允许任意程序」建好 → **永远不弹**
#   · **发布构建完全不受影响**：仍然走登录钥匙串 + 默认 ACL，
#     ADR-0002 记的「未签名分发升级会重置授权」照旧成立
#   · 应用侧的 put/get/delete 是同一段代码（ADR-0009 第 5 条），
#     只有「用哪个钥匙串实例」不同
#
# 这个脚本会**重建**开发库的密钥 → 旧的开发库文件要一起删掉（它是用旧密钥加密的）。
# 开发库里只有一张空的 admins 表，没有真实数据，删掉是安全的。
#
# 用法：bash scripts/setup-dev-keychain.sh

set -euo pipefail

SERVICE="cn.payslipmailer.db-key"
ACCOUNT="default"
KC="$HOME/Library/Keychains/xinyou-dev.keychain-db"
PW="xinyou-dev-keychain"
DATA_DIR="$HOME/Library/Application Support/cn.payslipmailer.desktop"

printf '▸ 建专用开发钥匙串 %s\n' "$KC"
rm -f "$KC"
security create-keychain -p "$PW" "$KC"
security unlock-keychain -p "$PW" "$KC"
security set-keychain-settings -lut 21600 "$KC"

printf '▸ 生成一把新的开发库密钥\n'
# 必须**恰好 32 字节**：应用侧 `apply_key_and_params` 硬性要求
# （`key.len() != KEY_BYTES` → Internal 错误）。
# `security add-generic-password -w` 只能存字符串，所以要挑一个
# 字节数正好等于 32 的随机串：`openssl rand -base64 24` → 24 随机字节
# → 32 个 base64 字符（24 是 3 的倍数，不补 '='）→ 正好 32 字节。
KEY="$(openssl rand -base64 24)"
if [ "${#KEY}" -ne 32 ]; then
  printf '✗ 生成的密钥是 %s 字节，不是 32 字节 —— 应用会拒收\n' "${#KEY}"
  exit 1
fi

printf '▸ 把条目写进开发钥匙串，ACL 设为「允许任意程序」（开发用）\n'
# -A = allow any application to access this item without warning
# 钥匙串是**位置参数**（`security` 的 add-generic-password 没有 -k 选项）
security add-generic-password -s "$SERVICE" -a "$ACCOUNT" -w "$KEY" -A "$KC" >/dev/null
unset KEY

printf '▸ 清掉旧的开发库文件（它是用旧密钥加密的）\n'
rm -rf "$DATA_DIR"

printf '✓ 完成。下次起 app 不会再弹钥匙串授权框。\n'
printf '  （登录钥匙串里那条旧条目还在，需要的话可以手动删：\n'
printf '    security delete-generic-password -s %s -a %s）\n' "$SERVICE" "$ACCOUNT"
