#!/usr/bin/env bash
# 本地构建脚本的公共环境。被 gate.sh / release.sh 用 `source` 引入。
#
# 为什么需要这个文件：rustup 只把 cargo 加进交互式 shell 的 PATH。
# 从非登录 shell、cron、或某些 GUI 包装里调用脚本时 PATH 里没有 cargo，
# 而 `npm run tauri build` 会在内部 shell 出 cargo，报错却是 Tauri 的
# 「failed to run 'cargo metadata'」—— 看着像工程配置坏了，其实是 PATH。
# CI 不受影响（dtolnay/rust-toolchain 会自己配好 PATH）。
#
# 注意：这里只补 PATH，不猜 SDKROOT。本机曾出现过 CLT SDK 与 Xcode SDK 冲突，
# 那个问题的处置记录在 docs/adr/0003，不要在这里顺手覆盖使用者的显式设置。

if [ -d "$HOME/.cargo/bin" ]; then
  case ":$PATH:" in
    *":$HOME/.cargo/bin:"*) ;;
    *) export PATH="$HOME/.cargo/bin:$PATH" ;;
  esac
fi
