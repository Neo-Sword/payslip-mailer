#!/usr/bin/env bash
# 薪邮 · 依赖漏洞扫描（TC-602 · 发布门槛「无高危依赖漏洞」）
#
# 用法：bash scripts/audit.sh
#
# 为什么要有这个脚本（而不是在 CI 里敲一行）：设计文档 ch.13 的发布门槛写着
# 「无高危 clippy / 依赖漏洞」，但**这句话此前从未被执行过** ——
# 现在它是门禁的一项，每次验收都会跑。
#
# 豁免写在 `.cargo/audit.toml` 里（可 grep、可评审），**只有一条**：
# RUSTSEC-2026-0459（CVE-2026-119375）。理由与依据见 docs/adr/0031：
# 我们不用 WAL（journal_mode = delete），所以 SQLite **不会**创建 `-wal` 兄弟文件，
# 而那条 CVE 的触发前提正是「库旁边有一个 page size 不同的 -wal」。
# 依据的那条测试（db_journal_mode_stays_delete）在 Rust 单测里，失败会红。

set -uo pipefail

cd "$(dirname "$0")/.."
# shellcheck source=./env.sh
. "$(dirname "$0")/env.sh"

LOCK="src-tauri/Cargo.lock"

if ! command -v cargo-audit >/dev/null 2>&1 && [ ! -x "$HOME/.cargo/bin/cargo-audit" ]; then
  cat >&2 <<'EOF'
✗ 没装 cargo-audit —— 这一项门禁**无法执行**（不能当成通过）。
  装法（官方预编译包，aarch64 版）：
    curl -sSL https://api.github.com/repos/rustsec/cargo-audit/releases/latest \
      | python3 -c "import sys,json;d=json.load(sys.stdin);print(next(a['browser_download_url'] for a in d['assets'] if 'aarch64-apple-darwin' in a['name']))" \
      | xargs curl -sSL -o /tmp/ca.tgz && tar xzf /tmp/ca.tgz -C /tmp \
      && cp /tmp/cargo-audit-*/cargo-audit ~/.cargo/bin/
EOF
  exit 1
fi

OUT="$(mktemp)"
trap 'rm -f "$OUT"' EXIT

# 先走正常路径（拉最新 advisory 库）。
if cargo audit --file "$LOCK" >"$OUT" 2>&1; then
  cat "$OUT"
  echo "  ✓ 依赖漏洞扫描：无未豁免的漏洞"
  exit 0
fi

# 拉库失败 ≠ 有漏洞：分清楚，不能把网络问题报成「有洞」。
if grep -q "couldn't fetch advisory database" "$OUT"; then
  echo "  ⚠ advisory 数据库拉取失败（本网络到 github.com 的 git 协议不通）"
  echo "    → 改用本机已有的库（~/.cargo/advisory-db），它可能不是最新的"
  if cargo audit --no-fetch --stale --file "$LOCK" >"$OUT" 2>&1; then
    cat "$OUT"
    echo "  ✓ 依赖漏洞扫描（用本机库）：无未豁免的漏洞"
    exit 0
  fi
  cat "$OUT"
  echo "  ✗ 依赖漏洞扫描未通过"
  exit 1
fi

cat "$OUT"
echo "  ✗ 依赖漏洞扫描未通过"
exit 1
