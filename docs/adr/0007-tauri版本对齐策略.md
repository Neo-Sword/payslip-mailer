# ADR-0007 · Tauri 版本对齐与 MSRV 策略

日期：2026-11-11 · 状态：accepted

> 本条被 `src-tauri/Cargo.toml` 第 10 行与第 27 行的注释直接引用，必须存在。

## 背景

Tauri 有**三条互相独立的版本线**，各自升级节奏不同：

| 位置 | 当前值 | 约束 |
|---|---|---|
| npm `@tauri-apps/api` | `~2.12.4` | 与 Rust `tauri` 的 major/minor 必须一致，否则 `tauri build` 拒绝构建 |
| Rust `tauri` | `~2.12`（lock 里解析到 2.12.5） | 同上 |
| Rust `tauri-build` | `2` | **独立版本线**，不与 `tauri` 同步，不能一起抬 |

另外 `rust-version = "1.88"` 配合 `resolver = "3"` 的 MSRV-aware 解析：
cargo 会按 1.88 挑选**能被这个 rustc 编译**的依赖版本，
所以 `cargo update` 不会悄悄引入需要更新工具链的传递依赖。

## 决策

1. **锁 minor 不锁 patch**：两侧都写 `~2.12`。patch 版本（2.12.4 → 2.12.5）自动跟随，
   因为它们不含破坏性变更；minor 必须人工同步升级。
2. **升级必须一次改两处**：`package.json` 与 `Cargo.toml` 在同一个 commit 里改，
   改完立刻跑 `npm run tauri build`，**不能只跑 `cargo check`** ——
   版本不匹配是 `tauri build` 阶段才校验的，`cargo check` 会一路绿灯。
3. **`tauri-build` 单独评估**，不跟着 `tauri` 一起抬。
4. **rust-version 只在真的升级了工具链后才抬**，且抬之前先确认 CI 的
   `rust-toolchain` 与本机 `rustc` 都已到位。

## 为什么不用 `latest` / `^2`

`^2` 会让某次普通的 `npm install`（或 CI 缓存失效后的重装）把 minor 抬上去，
而 Rust 侧的 `Cargo.lock` 没同步 → 构建在 CI 里炸，且报错发生在打包阶段，
离「我刚装了个 npm 包」这个原因很远，排查成本高。

## 后果

- 正面：版本漂移被 lockfile + `~` 双重限制；升级是一个显式动作，有明确的验证命令。
- 负面：拿不到 Tauri 新 minor 的特性，直到我们主动跟。本项目只用到窗口、Command、
  Keychain、文件系统这些稳定能力，不追新。
- 注意：`scripts/env.sh` 只补 PATH，不碰工具链版本；CI 用 `dtolnay/rust-toolchain@stable`，
  与本机 rustup 解析到的版本可能不同 —— 若出现「本地过、CI 不过」，先查这里。

## 重新评估的触发条件

- 需要 Tauri 新 minor 的某个具体能力 → 一次改两处并跑完整 `tauri build`。
- 若 `tauri-build` 与 `tauri` 官方宣布合并版本线 → 删掉第 3 条。
