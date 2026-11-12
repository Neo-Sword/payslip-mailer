#!/usr/bin/env bash
# 薪邮 · 构建 → 制 dmg → 校验和 → 产物验证 → 发布记录
#
# 用法：
#   bash scripts/release.sh              # 未签名（默认，决策 D8）
#   bash scripts/release.sh --signed     # 签名 + 公证（需 Developer ID 证书，当前不购买）
#
# 未签名产物的分发注意（ADR-0028）：
#   · 分发判据是 quarantine 属性，不是介质名称：
#     U 盘 / 局域网共享拷贝不带 → 双击直接打开；
#     网盘 / 浏览器下载带 → 每台 Mac 首次需「系统设置 → 隐私与安全性 → 仍要打开」。
#   · 每个新版本都要再放行一次。
#   · 升级后 Keychain 授权会重置（未签名二进制的 ACL 认 cdhash），弹窗必须点「允许」。
#   · spctl 预期失败 —— 那说明「没签名」，不是「包坏了」。

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
DMG_NAME="薪邮-$VERSION.dmg"

# 注意：变量后紧跟全角括号时必须加花括号。
# bash 会把多字节字符的首字节并入变量名，配合 `set -u` 会报 “unbound variable”。
echo "▸ [1/6] 构建 ${TARGET}（v${VERSION}）"
npm run tauri build -- --target "$TARGET"

APP_PATH="src-tauri/target/$TARGET/release/bundle/macos/薪邮.app"
if [ ! -e "$APP_PATH" ]; then
  APP_PATH="src-tauri/target/$TARGET/release/bundle/macos/payslip-mailer.app"
fi
[ -e "$APP_PATH" ] || { echo "✗ 未找到构建产物" >&2; exit 1; }
echo "  产物：$APP_PATH"

# 打包后的 Info.plist 补一个键：`CFBundleSupportedPlatforms`。
# 别的通用二进制 app（如本机的 Amphetamine）都有它，我们的 bundle 里没有。
# 它不是「能不能跑」的关键（实测进程是原生 ARM64），但缺了它会让 LaunchServices
# 对这个未签名 bundle 的判断更保守 —— macOS 首次启动时给出
# 「在搭载 Apple 芯片的 Mac 上使用基于 Intel 的 App」的一次性提示。
# 补上它是为了让 bundle 元数据与同类 app 对齐，不是为了绕过签名校验。
APP_PLIST="$APP_PATH/Contents/Info.plist"
if [ -f "$APP_PLIST" ]; then
  if ! /usr/libexec/PlistBuddy -c "Print :CFBundleSupportedPlatforms" "$APP_PLIST" >/dev/null 2>&1; then
    /usr/libexec/PlistBuddy -c "Add :CFBundleSupportedPlatforms array" "$APP_PLIST" >/dev/null
    /usr/libexec/PlistBuddy -c "Add :CFBundleSupportedPlatforms:0 string MacOSX" "$APP_PLIST" >/dev/null
    echo "    · Info.plist 补上 CFBundleSupportedPlatforms = [MacOSX]"
  fi
fi

if [ "$SIGNED" = "1" ]; then
  echo "▸ [2/6] 代码签名"
  bash scripts/sign.sh "$APP_PATH"

  echo "▸ [3/6] Apple 公证 + 装订"
  bash scripts/notarize.sh "$APP_PATH"
else
  echo "▸ [2-3/6] 跳过签名与公证（未签名构建，符合决策 D8 / ADR-0028）"
  echo "  注意：未签名 + 网盘分发需每台机器手动放行；升级后 Keychain 授权会重置"
fi

echo "▸ [4/6] 制作 dmg"
bash scripts/make-dmg.sh "$APP_PATH" "$OUT_DIR"
DMG_PATH="$OUT_DIR/$DMG_NAME"
[ -e "$DMG_PATH" ] || { echo "✗ 未找到 dmg：$DMG_PATH" >&2; exit 1; }

# ── 产物验证 ────────────────────────────────────────────────────────────
# 这一段不是「顺手打印一下」，而是把 TC-600 的验收标准 3/4/6 变成可执行检查：
# 包里有没有说明和 Applications 链接、校验和能不能自校验、quarantine 在不在。
echo "▸ [5/6] 产物验证"

echo "  · 架构"
APP_BIN="$(find "$APP_PATH/Contents/MacOS" -maxdepth 1 -type f | head -1)"
ARCHS="$(lipo -archs "$APP_BIN" 2>/dev/null || echo '(读取失败)')"
echo "    $ARCHS"
# 架构集合必须**恰好**是 x86_64 + arm64 两个（多了少了都不是 universal 包）。
# 顺序随机器而变，所以按「两个都在」判；个数用词数卡。
ARCH_COUNT="$(echo "$ARCHS" | wc -w | tr -d ' ')"
case "$ARCHS" in
  *x86_64*arm64*|*arm64*x86_64*)
    if [ "$ARCH_COUNT" != "2" ]; then
      echo "✗ 架构不是恰好两个（期望 x86_64 + arm64），实际为：$ARCHS" >&2
      exit 1
    fi
    ;;
  *)
    echo "✗ 架构不是 universal（x86_64 + arm64），实际为：$ARCHS" >&2
    exit 1
    ;;
esac

echo "  · dmg 内容"
MOUNT_DIR="$(mktemp -d)"
if hdiutil attach "$DMG_PATH" -nobrowse -mountpoint "$MOUNT_DIR" >/dev/null 2>&1; then
  ls -1 "$MOUNT_DIR" | sed 's/^/    /'
  for item in "薪邮.app" "安装说明.md"; do
    if [ ! -e "$MOUNT_DIR/$item" ]; then
      echo "✗ dmg 里没有 $item" >&2
      hdiutil detach "$MOUNT_DIR" >/dev/null 2>&1 || true
      rm -rf "$MOUNT_DIR"
      exit 1
    fi
  done
  if [ ! -L "$MOUNT_DIR/Applications" ]; then
    echo "✗ dmg 里没有 Applications 符号链接（拖放引导）" >&2
    hdiutil detach "$MOUNT_DIR" >/dev/null 2>&1 || true
    rm -rf "$MOUNT_DIR"
    exit 1
  fi
  hdiutil detach "$MOUNT_DIR" >/dev/null 2>&1 || true
  rm -rf "$MOUNT_DIR"
  echo "    ✓ 应用 + 安装说明 + Applications 链接齐全"
else
  echo "✗ dmg 无法挂载" >&2
  rm -rf "$MOUNT_DIR"
  exit 1
fi

echo "  · 校验和自校验"
(
  cd "$OUT_DIR"
  shasum -a 256 -c "$DMG_NAME.sha256"
)

echo "  · quarantine 判据（分发时是否需要手动放行）"
if xattr -l "$DMG_PATH" 2>/dev/null | grep -q 'com.apple.quarantine'; then
  QUARANTINE="有（下载/网盘分发 → 每台 Mac 首次需手动放行）"
else
  QUARANTINE="无（本地/ U 盘 / 局域网共享拷贝 → 双击直接开）"
fi
echo "    $QUARANTINE"

# ── Gatekeeper + 发布记录 ────────────────────────────────────────────────
echo "▸ [6/6] Gatekeeper 检查 + 发布记录"
SPCTL_OUT="$(spctl -a -vv "$APP_PATH" 2>&1 || true)"
printf '%s\n' "$SPCTL_OUT" | sed 's/^/    /'
if [ "$SIGNED" = "1" ]; then
  if ! printf '%s' "$SPCTL_OUT" | grep -q 'accepted'; then
    echo "✗ 已签名构建却未通过 Gatekeeper，检查证书与公证状态" >&2
    exit 1
  fi
  echo "    ✓ Gatekeeper 通过"
else
  echo "    （未签名构建被 Gatekeeper 拒绝，属预期行为 —— ADR-0028 决策五）"
fi

SHA_LINE="$(cat "$OUT_DIR/$DMG_NAME.sha256")"
DMG_SIZE="$(du -h "$DMG_PATH" | cut -f1 | tr -d ' ')"
APP_SIZE="$(du -sh "$APP_PATH" | cut -f1 | tr -d ' ')"
RECORD="$OUT_DIR/发布记录-$VERSION.md"

# 注意：heredoc 里凡是变量后面紧跟全角标点，一律写成 ${VAR}。
# bash 会把多字节字符的首字节并入变量名（$ARCHS）→ 解析成变量 "ARCHS\xef"），
# 配合 set -u 直接报 “unbound variable”。这条坑本脚本上方已经踩过一次。
cat > "$RECORD" <<EOF
# 薪邮 $VERSION 发布记录

| 项 | 值 |
| --- | --- |
| 生成时间 | $(date '+%Y-%m-%d %H:%M:%S %Z') |
| 版本 | $VERSION |
| 构建目标 | ${TARGET}（${ARCHS}） |
| 应用体积 | $APP_SIZE |
| dmg | ${DMG_NAME}（${DMG_SIZE}） |
| SHA-256 | \`$SHA_LINE\` |
| 签名状态 | $([ "$SIGNED" = "1" ] && echo "已签名 + 已公证" || echo "**未签名、未公证**（决策 D8 / ADR-0028）") |
| quarantine | $QUARANTINE |
| 包内附带 | 薪邮.app、安装说明.md、Applications 符号链接 |

## Gatekeeper 实测输出

\`\`\`
$SPCTL_OUT
\`\`\`

未签名构建这一条**预期是 rejected**（ADR-0028 决策五）：
它说明「这个包没签名」，不是「这个包坏了」。
安装说明里原样引用了这段输出。

## 分发

- dmg 与 .sha256 **一起**交付；把 .sha256 放在 dmg 旁边即可（校验文件里只有文件名）。
- **首选 U 盘 / 局域网共享**（不带 quarantine，双击直接开）；
  网盘与内网下载页次选（每台 Mac 首次要手动放行一次）。
- 装之前先 \`shasum -a 256 -c *.sha256\`。
- 升级后 Keychain 授权会重置（未签名二进制的 ACL 认 cdhash），
  弹窗**点「允许」**；点「拒绝」会让人锁在门外。升级演练见 TC-601。
EOF

echo
echo "✓ 发布完成。产物："
ls -lh "$OUT_DIR"
echo
echo "下一步：把 dmg + .sha256 一起交付；建议走 U 盘或局域网共享以避开 quarantine。"
