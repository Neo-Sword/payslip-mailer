#!/usr/bin/env bash
# 任务卡验收门禁 —— 每张卡的 spec 都引用同一组检查，避免「验收标准各写各的」。
#
#   bash scripts/gate.sh          # 全部跑完再汇总（不是遇错即停，一次看全）
#   bash scripts/gate.sh --fast   # 跳过前端构建与 Rust 编译，只跑秒级检查
#
# 退出码：0 = 全绿；1 = 至少一条失败。

set -uo pipefail

cd "$(dirname "$0")/.."
# shellcheck source=./env.sh
. "$(dirname "$0")/env.sh"

FAST=0
[ "${1:-}" = "--fast" ] && FAST=1

FAILED=0

run_gate() {
  local name="$1" cmd="$2"
  printf '\n▸ %s\n  $ %s\n' "$name" "$cmd"
  if bash -c "$cmd"; then
    printf '  ✓ %s\n' "$name"
  else
    printf '  ✗ %s 失败\n' "$name"
    FAILED=1
  fi
}

# ── 秒级检查 ────────────────────────────────────────────────────────────
run_gate "Rust 格式" "cargo fmt --manifest-path src-tauri/Cargo.toml --check"
run_gate "前端类型检查" "npm run typecheck"

# 设计 Token 红线：除 tokens.css 以外不许出现硬编码色值。
# 确需脱离 Token 的颜色（例如仿 macOS 红绿灯），在行尾加 `/* no-token: 理由 */` 显式豁免，
# 这样豁免是「可 grep、可审计」的，而不是靠关掉整条检查。
#
# 扫描范围曾经是 `src/components`，于是漏掉了 App.tsx 的内联色值与 global.css 的滚动条 ——
# 门禁全绿，规矩却没守住。范围必须是「整个 src，只放行 tokens.css」，
# 因为「色值只有一个来源」这条规则的价值就在于它没有例外目录。
printf '\n▸ 设计 Token（色值只允许出现在 tokens.css）\n'
LEAKS="$(grep -RIn --include='*.css' --include='*.tsx' --include='*.ts' -E '#[0-9a-fA-F]{3,8}' src 2>/dev/null \
  | grep -v 'src/styles/tokens.css' | grep -v 'no-token' || true)"
if [ -z "$LEAKS" ]; then
  printf '  ✓ 组件层无未豁免的硬编码色值\n'
else
  printf '  ✗ 以下色值应改为 var(--token)，或加 /* no-token: 理由 */ 豁免：\n'
  printf '%s\n' "$LEAKS" | sed 's/^/      /'
  FAILED=1
fi

# Token 引用必须能解析到声明：组件里写错一个 Token 名不会报错，只会「没有颜色」，
# 在深色背景上表现为一片空白，肉眼很容易漏过去。这里把拼写错误变成构建期失败。
printf '\n▸ Token 引用可解析\n'
DANGLING="$(python3 - <<'PY'
import re, pathlib
declared = set()
for name in ("src/styles/tokens.css", "src/styles/global.css"):
    declared |= set(re.findall(r'(--[a-zA-Z0-9-]+)\s*:', pathlib.Path(name).read_text(encoding="utf8")))
bad = []
for f in sorted(pathlib.Path("src").rglob("*")):
    if f.suffix not in {".css", ".tsx", ".ts"}:
        continue
    for i, line in enumerate(f.read_text(encoding="utf8").splitlines(), 1):
        for tok in re.findall(r'var\((--[a-zA-Z0-9-]+)', line):
            if tok not in declared:
                bad.append(f"{f}:{i} 未声明的 {tok}")
print("\n".join(bad))
PY
)"
if [ -z "$DANGLING" ]; then
  printf '  ✓ 每个 var(--x) 都能解析到 Token 声明\n'
else
  printf '  ✗ 以下 Token 引用解析不到声明（会静默失去颜色）：\n'
  printf '%s\n' "$DANGLING" | sed 's/^/      /'
  FAILED=1
fi

# 敏感数据不许进版本库：成员名单与真实工资表只放本机。
printf '\n▸ 敏感数据未进 git\n'
if git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  DIRTY="$(git ls-files | grep -E 'members.*\.(csv|xlsx|json)$|roster|payroll.*\.xlsx$' || true)"
  if [ -z "$DIRTY" ]; then
    printf '  ✓ 名单与工资表未被纳入版本库\n'
  else
    printf '  ✗ 以下含个人信息的文件已进 git：\n'
    printf '%s\n' "$DIRTY" | sed 's/^/      /'
    FAILED=1
  fi
else
  printf '  （非 git 仓库，跳过）\n'
fi

# 依赖漏洞扫描（发布门槛「无高危依赖漏洞」· TC-602 · ADR-0031）。
# 为什么不是「CI 才跑」：ch.13 那句话此前**从未被执行过** ——
# 现在它是门禁的一项，每次验收都会跑。豁免写在 .cargo/audit.toml（只有一条，
# 理由与依据都在那里面）。没装 cargo-audit 时这一项**红**，不当作通过。
run_gate "依赖漏洞扫描" "bash scripts/audit.sh"

# ── 编译级检查 ──────────────────────────────────────────────────────────
if [ "$FAST" = "0" ]; then
  run_gate "Rust 静态检查" "cargo clippy --manifest-path src-tauri/Cargo.toml --all-features -- -D warnings"
  run_gate "Rust 单测" "cargo test --manifest-path src-tauri/Cargo.toml"

  # 加密栈的链接证据（ADR-0010 第 2 条）。SQLCipher 必须走 Apple 的 CommonCrypto
  # （体现为链 Security.framework），不能链到 OpenSSL 的 libcrypto/libssl。
  # 为什么必须用 otool 而不是靠编译/测试：环境里一旦出现 OPENSSL_DIR，
  # rusqlite 的 build.rs 会改走 OpenSSL —— 编译照过、测试照绿，
  # 只有看链接才能发现加密栈被换掉了（ADR-0010 实测）。
  printf '\n▸ 加密栈链接（链 Security.framework，不得链 libcrypto/libssl）\n'
  CRYPTO_BIN="src-tauri/target/debug/payslip-mailer"
  if [ ! -f "$CRYPTO_BIN" ]; then
    cargo build --manifest-path src-tauri/Cargo.toml >/dev/null 2>&1 || true
  fi
  if [ ! -f "$CRYPTO_BIN" ]; then
    printf '  ✗ 找不到 %s，无法验证链接（前面的编译门禁没产出它？）\n' "$CRYPTO_BIN"
    FAILED=1
  else
    LINKS="$(otool -L "$CRYPTO_BIN")"
    OPENSSL_LINKS="$(printf '%s\n' "$LINKS" | grep -Ei 'libcrypto|libssl' || true)"
    if printf '%s\n' "$LINKS" | grep -q 'Security.framework' && [ -z "$OPENSSL_LINKS" ]; then
      printf '%s\n' "$LINKS" | grep 'Security.framework' | sed 's/^/      /'
      printf '  ✓ 加密走 CommonCrypto，未链 OpenSSL\n'
    else
      printf '  ✗ 链接不符合预期：\n'
      printf '%s\n' "$LINKS" | grep -Ei 'Security.framework|libcrypto|libssl' | sed 's/^/      /'
      printf '    （缺 Security.framework，或出现了 OpenSSL 的库 —— 检查 OPENSSL_DIR 与 build.rs）\n'
      FAILED=1
    fi
  fi

  run_gate "前端构建" "npm run build"
fi

echo
if [ "$FAILED" = "0" ]; then
  echo "✓ 门禁全绿"
else
  echo "✗ 有门禁未通过，见上方 ✗ 项"
fi
exit "$FAILED"
