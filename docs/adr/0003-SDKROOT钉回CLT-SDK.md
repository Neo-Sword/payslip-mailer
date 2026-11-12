# ADR-0003 · 把 SDKROOT 钉回 CLT 自带 SDK

日期：2026-11-11 · 状态：accepted

## 背景

本机 `/Library/Developer/CommandLineTools/SDKs/` 下同时存在多个 SDK：

```
MacOSX.sdk    -> MacOSX26.5.sdk     ← 符号链接，CLT 自带
MacOSX26.5.sdk
MacOSX27.0.sdk                       ← 单独多装的一个
MacOSX27.sdk  -> MacOSX27.0.sdk
```

`xcrun --show-sdk-path` 默认选**版本号最高**的，即 `MacOSX27.0.sdk`。
而它的 `libSystem.B.tbd` 用了 `arm64e.x1-macos` 这类新架构写法，
CLT 自带的链接器 `ld-1267` 不认识，于是**任何链接都失败**：

```
ld: tapi error: malformed file
.../MacOSX27.0.sdk/usr/lib/libSystem.B.tbd:4:20: error: unknown architecture
```

症状出现在最不可疑的地方：`cargo check` 能过（不链接），一到 `cargo build` / `tauri build` 就炸，
报错信息里全是 `.tbd` 与 `unknown architecture`，很容易被误读成 Rust 工具链坏了。

## 决策

在 `payslip-mailer/.cargo/config.toml` 里为**本项目**钉住 SDK：

```toml
[env]
SDKROOT = { value = "/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk" }
```

刻意**不加 `force = true`**：若使用者外部显式设置了 `SDKROOT`（例如装了完整 Xcode 的同事），
以外部为准。这条配置是修一台机器的环境偏差，不是给全项目定一条构建政策。

## 后果

- 正面：链接恢复；配置随仓库走，同一台机器上重新 clone 不用再踩一遍。
- 负面：钉的是 `MacOSX.sdk` 这个**符号链接**而非具体版本，语义是「CLT 认为的默认 SDK」。
  若将来 CLT 自己把链接指向一个链接器不认的 SDK，同样的症状会回来。
  届时改这一行即可，不需要动源码或构建脚本。
- 注意：`scripts/env.sh` 只补 PATH，**不碰 SDKROOT**，避免两处互相覆盖。

## 重新评估的触发条件

- 安装完整 Xcode 并 `xcode-select -s` 切过去 → 删掉这一段。
- CLT 升级后 `MacOSX.sdk` 指向的 SDK 能被链接 → 删掉这一段。
