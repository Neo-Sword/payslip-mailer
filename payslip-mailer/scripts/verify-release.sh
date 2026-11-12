#!/usr/bin/env bash
# 薪邮 · 发布产物验收检查（TC-600）
#
# 用法：bash scripts/verify-release.sh [产物目录，默认 ./artifacts]
#
# 这个脚本把 TC-600 spec 里**能自动化**的验收标准变成可重复执行的检查，
# 输出即证据（每条对应 spec 的一条标准号）。
# 需要真人在场的两条（S9 卸载、S10 首启）不在这里 —— 它们验的是「装了之后的状态」，
# 机器自己装自己不算数。

set -uo pipefail

cd "$(dirname "$0")/.."
# shellcheck source=./env.sh
. "$(dirname "$0")/env.sh"

OUT_DIR="${1:-./artifacts}"
REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
GUIDE="$REPO_ROOT/docs/安装说明.md"
VERSION="$(node -p "require('./package.json').version")"
DMG="$OUT_DIR/薪邮-$VERSION.dmg"
SHA="$DMG.sha256"
RECORD="$OUT_DIR/发布记录-$VERSION.md"

FAILED=0
ok()   { printf '  ✓ %s\n' "$1"; }
bad()  { printf '  ✗ %s\n' "$1"; FAILED=1; }
note() { printf '  · %s\n' "$1"; }

echo "▸ S1 产物齐备"
for f in "$DMG" "$SHA" "$RECORD"; do
  if [ -e "$f" ]; then ok "存在：$f"; else bad "缺失：$f"; fi
done

echo "▸ S2 universal 架构"
APP="src-tauri/target/universal-apple-darwin/release/bundle/macos/薪邮.app"
if [ -d "$APP" ]; then
  BIN="$(find "$APP/Contents/MacOS" -maxdepth 1 -type f | head -1)"
  ARCHS="$(lipo -archs "$BIN" 2>/dev/null || echo '(读取失败)')"
  note "lipo -archs → $ARCHS"
  COUNT="$(echo "$ARCHS" | wc -w | tr -d ' ')"
  case "$ARCHS" in
    *x86_64*arm64*|*arm64*x86_64*)
      if [ "$COUNT" = "2" ]; then ok "恰好两个架构（x86_64 + arm64）"; else bad "架构个数不对：$COUNT"; fi ;;
    *) bad "不是 universal 包：$ARCHS" ;;
  esac
else
  bad "找不到构建产物 ${APP}（先跑 scripts/release.sh）"
fi

echo "▸ S3 dmg 内容（应用 + 安装说明 + Applications 链接）"
MOUNT_DIR="$(mktemp -d)"
if hdiutil attach "$DMG" -nobrowse -mountpoint "$MOUNT_DIR" >/dev/null 2>&1; then
  ls -1 "$MOUNT_DIR" | sed 's/^/    /'
  for item in "薪邮.app" "安装说明.md"; do
    if [ -e "$MOUNT_DIR/$item" ]; then ok "包内有 $item"; else bad "包内没有 $item"; fi
  done
  if [ -L "$MOUNT_DIR/Applications" ]; then ok "包内有 Applications 符号链接"; else bad "包内没有 Applications 符号链接"; fi
  # 包里的说明必须与仓库里的**当前版本一致** —— 否则发出去的是上一版的说明，
  # 而使用者根本不会知道（dmg 里的说明才是他手上那份）。
  if diff -q "$MOUNT_DIR/安装说明.md" "$GUIDE" >/dev/null 2>&1; then
    ok "包内安装说明与仓库当前版本逐字一致"
  else
    bad "包内安装说明与仓库当前版本不一致（dmg 是用旧说明打的）"
  fi
  hdiutil detach "$MOUNT_DIR" >/dev/null 2>&1 || true
else
  bad "dmg 挂载失败"
fi
rm -rf "$MOUNT_DIR"

echo "▸ S4 校验和（自校验 + 移动后仍能校验）"
if (cd "$OUT_DIR" && shasum -a 256 -c "$(basename "$SHA")" 2>&1 | sed 's/^/    /'); then
  ok "原地校验通过"
else
  bad "原地校验失败"
fi
TMP_MOVED="$(mktemp -d)"
cp "$DMG" "$SHA" "$TMP_MOVED/" 2>/dev/null
if (cd "$TMP_MOVED" && shasum -a 256 -c "$(basename "$SHA")" >/dev/null 2>&1); then
  ok "产物移到别的目录后仍能校验（校验文件里只有文件名）"
else
  bad "产物移动后校验失败（校验文件里不该写死路径）"
fi
rm -rf "$TMP_MOVED"

echo "▸ S5 Gatekeeper 实测输出 + 与安装说明逐字相符"
SPCTL_OUT="$(spctl -a -vv "$APP" 2>&1 || true)"
printf '%s\n' "$SPCTL_OUT" | sed 's/^/    /'
if printf '%s' "$SPCTL_OUT" | grep -q 'rejected'; then
  ok "未签名构建被拒绝（ADR-0028 决策五：预期行为）"
else
  note "spctl 未被拒绝 —— 若这是 --signed 构建，这里应当通过"
fi
if grep -q 'source=no usable signature' "$GUIDE"; then
  ok "安装说明里原样引用了 spctl 的输出"
else
  bad "安装说明里没有引用 spctl 的实际输出（标准 5 要求逐字相符）"
fi

echo "▸ S6 quarantine 判据"
TMP_Q="$(mktemp -d)"
cp "$DMG" "$TMP_Q/本地拷贝.dmg"
cp "$DMG" "$TMP_Q/模拟下载.dmg"
if xattr -l "$TMP_Q/本地拷贝.dmg" 2>/dev/null | grep -q 'com.apple.quarantine'; then
  note "本地拷贝居然带 quarantine（少见，需查明来源）"
else
  ok "本地拷贝不带 quarantine → 双击直接开"
fi
xattr -w com.apple.quarantine "0080;5f0000;app;0" "$TMP_Q/模拟下载.dmg" 2>/dev/null
if xattr -l "$TMP_Q/模拟下载.dmg" 2>/dev/null | grep -q 'com.apple.quarantine'; then
  ok "人为写入 quarantine 后能看到该属性 → 这一路需要手动放行"
else
  bad "写不进 quarantine 属性，判据不可测"
fi
rm -rf "$TMP_Q"

echo "▸ S7 安装说明：一页、六节齐全"
if [ -f "$GUIDE" ]; then
  ok "存在：$GUIDE"
  for sec in "装之前先校验" "第一次打开" "首启三步" "卸载" "出问题怎么办"; do
    # 变量后紧跟全角标点必须写 ${sec}：bash 会把多字节字符的首字节并进变量名。
    if grep -q "$sec" "$GUIDE"; then ok "含「${sec}」"; else bad "缺「${sec}」"; fi
  done
  HANZI="$(python3 -I - "$GUIDE" <<'PY'
import re, sys
lines = [l for l in open(sys.argv[1], encoding="utf-8").read().split("\n") if not l.startswith("#")]
print(len(re.findall(r"[一-鿿]", "\n".join(lines))))
PY
)"
  note "正文汉字数 = ${HANZI}（上限 900）"
  if [ "$HANZI" -le 900 ]; then ok "一页装得下"; else bad "超过一页（$HANZI 汉字）"; fi
else
  bad "缺安装说明：$GUIDE"
fi

echo "▸ S8 说明里的路径 / 条目名与代码一致"
for pair in "cn.payslipmailer.desktop:src-tauri/tauri.conf.json" \
            "cn.payslipmailer.db-key:src-tauri/src/infra/keychain.rs"; do
  NAME="${pair%%:*}"; CODE="${pair##*:}"
  if grep -q "$NAME" "$GUIDE"; then ok "说明里写了 ${NAME}"; else bad "说明里没有 ${NAME}"; fi
  if grep -q "$NAME" "$CODE"; then ok "代码里有 ${NAME}（${CODE}）"; else bad "代码里找不到 ${NAME}（${CODE}）"; fi
done

echo "▸ S12 产物不入库"
if git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  if git ls-files | grep -q '^payslip-mailer/artifacts/'; then
    bad "artifacts/ 里有文件进了版本库"
  else
    ok "artifacts/ 未纳入版本库"
  fi
else
  note "（非 git 仓库，跳过）"
fi

echo
echo "▸ 需要真人在场的两条（脚本跑不了，本卡人工验）"
echo "  S9  卸载 = 删 app，数据仍在原处（数据目录 + 钥匙串条目都还在）"
echo "  S10 首启能走到主界面，且不被 Gatekeeper 拦"
echo "  S11 门禁九项全绿：FAST=0 bash scripts/gate.sh"

echo
if [ "$FAILED" = "0" ]; then
  echo "✓ 自动化部分全过"
else
  echo "✗ 有检查未通过"
fi
exit "$FAILED"
