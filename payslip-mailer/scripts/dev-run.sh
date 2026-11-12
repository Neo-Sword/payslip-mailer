#!/usr/bin/env bash
# 开发用的「构建 → 签名 → 起 app → 自动处理钥匙串授权」一条龙。
#
# 为什么要有这个脚本（2026-11-11 实测，TC-103 验收时撞上的）：
# macOS 钥匙串给每个条目配 ACL，「谁可以读」写在里面。默认 ACL 信任
# **创建它的那个程序** —— 对未签名程序来说身份是二进制的 cdhash，
# **每重编译一次就变** → 每次启动都弹
# 「payslip-mailer 想要访问你钥匙串中的密钥 cn.payslipmailer.db-key。
#   若要给予许可，请输入"xinyou-dev"钥匙串的密码。」
# 要人输口令 —— 而那口令其实是本脚本自己定的常量，根本不该让人来输。
#
# 这个脚本做三件事：
#   1. 编译 + 用开发证书签名（签名让 designated requirement 稳定）
#   2. 起 app
#   3. 钥匙串授权框一出现就自动填入口令 + 点「始终允许」，不用人管
#
# 用法：bash scripts/dev-run.sh
# （停掉：pkill -f 'target/debug/payslip-mailer'）

set -uo pipefail

cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
export SDKROOT="${SDKROOT:-/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk}"

DEV_KEYCHAIN_PASSWORD="xinyou-dev-keychain"
BIN="src-tauri/target/debug/payslip-mailer"

printf '▸ 编译\n'
if ! cargo build --manifest-path src-tauri/Cargo.toml 2>&1 | tail -3; then
  printf '✗ 编译失败\n'
  exit 1
fi

printf '▸ 签名\n'
bash scripts/sign-dev.sh >/dev/null 2>&1 || {
  printf '（签名失败：先跑一次 scripts/setup-dev-signing.sh）\n'
}

printf '▸ 启动\n'
pkill -f 'target/debug/payslip-mailer' >/dev/null 2>&1
sleep 1
nohup "$BIN" > /tmp/xinyou-dev-app.log 2>&1 &

# 钥匙串授权框：出现就自动授权，最多等 40 秒。
# 用 AppleScript 走无障碍接口（按控件定位，不是按坐标）。
# 说明：这里没用 Computer Use —— 那个在本机连续超时不可用；
# 无障碍接口同样是按名字定位控件，精度不差。
#
# 为什么必须自动授权：macOS 钥匙串条目 ACL 里的「信任谁」记的是二进制的 **cdhash**，
# 每重编译一次就变 → 每次启动都要重新授权。Apple 锚定的 Developer ID 签名能解决
# （DR 按证书 + bundle id，重编译不变），但那需要付费开发者计划（路线 A 不买）。
# 试过「给开发构建自签一份」：DR 确实稳定了，但 macOS 评估自签根证书时仍不放行，
# 照样弹 —— 所以自签这条路不通，只能自动授权。
printf '▸ 处理钥匙串授权（自动）\n'
for _ in $(seq 1 100); do
  sleep 0.4
  OUT="$(osascript <<'AS' 2>/dev/null
tell application "System Events"
  if not (exists process "SecurityAgent") then return ""
  tell process "SecurityAgent"
    set handled to 0
    repeat with w in windows
      try
        set value of first text field of w to "xinyou-dev-keychain"
        delay 0.2
        click button "始终允许" of w
        set handled to handled + 1
      end try
    end repeat
    return handled
  end tell
end tell
AS
)"
  case "$OUT" in
    0|'') : ;;
    *) printf '  已授权 %s 个弹窗\n' "$OUT" ;;
  esac
done

printf '▸ 应用窗口：'
for _ in $(seq 1 10); do
  sleep 1
  N="$(osascript -e 'tell application "System Events" to tell process "payslip-mailer" to count windows' 2>/dev/null || echo 0)"
  if [ "$N" != "0" ]; then
    printf '%s 个\n' "$N"
    exit 0
  fi
done
printf '0 个（应用还在启动或卡住了，看 /tmp/xinyou-dev-app.log）\n'
