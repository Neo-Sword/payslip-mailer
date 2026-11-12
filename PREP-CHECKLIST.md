# 薪邮 Payslip Mailer · 开发前准备清单

> 基线：`payslip-mailer-design.html` v1.0（2026-11-09） + 2026-11-11 决策补充
> 本机实测：macOS 26.6 / Apple Silicon arm64 / Node v22.14.0 / Xcode CLT
> 用法：勾选项按 P0→P6 阶段推进；带 🔴 的是硬阻塞，不解决则对应阶段无法验收。

---

## 0. 一句话项目目标

给中小团队 HR/财务做一台**本机**的发薪最后一公里工具：
配置一次 → 每月导出空 Excel 模板 → 财务填好导入 → 逐人独立成信发工资条 → 全程留痕可举证。
**数据不出本机**（SQLCipher 加密库 + macOS Keychain），**零抄送零密送**，**默认只发深色主题**，**不上 App Store**。

---

## 1. 已拍板的决策（2026-11-11）

| # | 决策 | 对开发的约束 |
|---|---|---|
| D1 | 技术栈 Tauri 2 + Rust + React 18/TS + Vite | 已锁定，不可再评估其他方案 |
| D2 | 发件邮箱用**飞书邮箱**（`smtp.feishu.cn:465` SSL + 专用密码） | OAuth2 仅作扩展；设置页要有专用密码失效引导 |
| D3 | v1 **只做深色主题** | 全部颜色走 CSS 变量，浅色延后到 v1.1 |
| D4 | v1 **不做 PDF 附件** | 正文即工资条；省掉 PDF 渲染 / 口令分发 / 临时明文清理三条链路 |
| D5 | 团队模式 **A：一台管理机发全公司** | 单管理员 + 单机加密库；不引入共享存储 / RBAC / 并发锁 |
| D6 | 真实发件账号**后置**，P0–P4 全走 mock SMTP | 测试环境绝不真发；P5 再做真实联调 |
| D7 | **不上 Mac App Store**，内部分发 dmg | 不需要 App Sandbox |
| D8 | **不购买 Apple Developer Program（$99/年）** | 用未签名构建 + 本地构建/U 盘分发；不做公证。代价：网盘下载的分发需手动放行、升级后 Keychain 授权会重置 |

> D4/D5 是本轮新增，会让 v1 工期比原估算更短；D6 把飞书账号的依赖从「开工前」挪到「P5 前」；
> D8 取消了签名公证依赖，P6 从「必须等证书」变成「立即可做」，但换来分发时的手动放行成本。

---

## 2. 本机环境实测状态（2026-11-11 复核）

| 工具 | 状态 | 用途 | 备注 |
|---|---|---|---|
| macOS 26.6 / arm64 | ✅ 已装 | Tauri 2 最低 macOS 10.15 | 原生跑 arm64 |
| Xcode Command Line Tools | ✅ 已装 `/Library/Developer/CommandLineTools` | clang / linker / codesign | 桌面应用**无需**完整 Xcode |
| Node.js v22.14.0 + npm 10.9.2 | ✅ 已装 | 前端构建 / Tauri CLI | 满足官方 ≥20 要求 || Git 2.50.1 (Apple Git) | ✅ 已装 | 版本管理 / CI | — |
| `notarytool` / `stapler` | ✅ 已装（随 CLT） | 公证 / 装订 | — |
| **Rust 工具链** | 🔄 **本轮已装**（rustup, stable, `--no-modify-path`） | Tauri 后端全部逻辑 | 需 `source ~/.cargo/env` 或重开终端 |
| **双架构 target** | ⬜ 待装 | universal dmg（Intel + ARM） | `rustup target add aarch64-apple-darwin x86_64-apple-darwin` |
| **Developer ID 证书** | ⛔ **不购买**（决策 D8） | dmg 签名公证 | 无可用身份（0 valid identities）。v1 走未签名 + 本地构建/U 盘分发 |
| Homebrew / create-dmg | ⬜ 未装（**可选**） | 定制 dmg 背景与拖拽引导 | Tauri 自带 bundler 可先出 dmg，发布前再装 |
| pnpm | ⬜ 未装（**可选**） | 前端依赖管理 | 用 npm 亦可，非必需 |
| SQLCipher 系统库 | ✅ 免装 | 加密数据库 | 用 Rust crate 的 `bundled` 特性静态编译 |
| CocoaPods / Android SDK | ✅ 不需要 | 仅 iOS/Android | 本项目只做 macOS |

**结论：当前没有硬阻塞，P0 可以立刻开工。**
Rust 工具链本轮已装好；签名证书按决策 D8 不购买，P6 改为未签名分发。

> ⚠️ 已解决的环境坑：本机 `CommandLineTools/SDKs/` 下混入了一个 `MacOSX27.0.sdk`，`xcrun` 会优先选它，
> 而它的 `libSystem.tbd` 用了 CLT 自带 `ld` 不认识的新架构写法（`arm64e.x1-macos`），
> 导致**任何** Rust/C 链接都报 `ld: tapi error: malformed file`。
> 已在 `payslip-mailer/.cargo/config.toml` 里把 `SDKROOT` 钉回 CLT 自带的 `MacOSX.sdk`。
> 若日后装了完整 Xcode 并切换，删掉该配置即可。

### 2.1 Rust 装完后的固定动作

```bash
source "$HOME/.cargo/env"          # rustup 用了 --no-modify-path，不会自动写 shell 配置
rustc --version && cargo --version
rustup target add aarch64-apple-darwin x86_64-apple-darwin
```

> 建议把 `source "$HOME/.cargo/env"` 手动加进 `~/.zshrc`，否则每个新终端都要手动 source。

### 2.2 网络：npm 走国内镜像

本机实测 `registry.npmjs.org` 单包元数据请求需 6–19 秒，`registry.npmmirror.com` 约 1.5 秒（快约 4 倍）。
`npm install` 建议带上：

```bash
npm install --registry=https://registry.npmmirror.com --no-audit --no-fund
```

如果团队有企业代理或要求全部依赖走官方源，删掉 `--registry` 即可，代价是首次安装会从几十分钟拉长到 1 小时以上。

> 镜像只影响下载速度；`package-lock.json` 会记录每个包的 `integrity` 哈希，后续安装仍然逐包校验。

### 2.3 脚手架实际版本（与设计文档的差异）

`create-tauri-app` 当前模板给出的版本比设计文档 ch.3 写的略新，属正常演进，不影响架构：

| 项 | 设计文档 | 脚手架实际 |
|---|---|---|
| React | 18 | 19.1 |
| TypeScript | 5 | 6.0 |
| Vite | — | 8.0 |
| Tauri CLI | 2 | 2.x |
| Rust edition | 2021 | 2024 |

---

## 3. 开发前必须落实的业务输入（🔴 缺了会返工）

这些不是技术环境，是**内容**。P2/P3 开始前必须拿到，否则要回头改表结构和模板。

- [ ] **工资条字段清单**（真实口径，10 项左右）
  期望：`基本工资 / 绩效奖金 / 餐补 / 社保个人 / 公积金 / 个税 / 实发工资(公式) / 备注`
  需确认：哪些必填、哪些是扣款项（填负数）、实发公式到底是 `应发合计 + 扣款合计` 还是别的口径
  → 决定 `salary_fields` 初始种子数据与 Excel 空模板表头
- [ ] **成员工名单**（姓名 + 邮箱 + 工号 + 所属组）
  决定 `members` / `group_members` 种子数据与「预填成员名单」导出的效果
  ⚠️ 名单含敏感个人信息，**只放本机，不要进 git**（用 `.gitignore` + 独立的 seed 文件）
- [ ] **公司名称 / 单位名** 与 **回信邮箱**
  → 写入 `email_templates` 默认值与 `{{公司名称}}` 变量
- [ ] **邮件主题与正文文案**（中文）
  → 设计文档第 8 章已给示例稿，可作 v1 初版；确认「实发工资」显示口径
- [ ] **飞书发件账号**（P5 前）
  - 必须是一个**真实分配了企业邮箱席位的成员账号**（如 `payroll@company.com`），**不能用邮件组**
  - 企业管理员需开启「第三方邮箱客户端登录」
  - 桌面客户端生成专用密码：飞书 → 头像 → 设置 → 邮箱 → 第三方邮箱客户端登录 → 生成
  - 确认该账号**不是**邮件组别名（否则第三方客户端登不上）

---

## 4. 分阶段准备与验收

### P0 工程骨架（当前阶段）
- [x] Rust 工具链安装
- [ ] 双架构 target 安装
- [x] git 仓库初始化（仓库根 = `工资条群发 app/`，设计文档与代码同仓）
- [ ] Tauri 2 + React/TS 脚手架（`payslip-mailer/`）
- [ ] Design Token 抽取（从 `payslip-mailer-design.html` 的 `.w-*` 样式反推）
- [ ] 组件库基线（Button / Input / Card / Chip / Table / Switch / Modal / Field）
- [ ] 窗口尺寸对齐规格：默认 1200×800，最小 1024×680；侧边栏 186px，可折叠 56px
- [ ] CI 流水线（`test → build → sign → notarize → dmg`）
- [ ] `sign.sh` / `notarize.sh` / `make-dmg.sh` 脚本
- [ ] 产出**未签名** dmg，确认 `tauri build` 链路通

**P0 验收**：干净 macOS 虚拟机双击 dmg 可安装；arm64 与 x86_64 通用包均可运行；
`tauri dev` 冷启动 < 2s；主界面交互响应 < 100ms。

### P1 身份与存储
- [ ] SQLCipher 连接池 + 迁移机制（版本化 SQL）
- [ ] macOS Keychain 接入（DB key / SMTP 凭据 / OAuth token）
- [ ] Argon2id 管理员口令 + 5 次失败锁定 60s + 空闲 10 分钟自动锁定
- [ ] 初始化向导（S01）/ 登录解锁（S02）
- [ ] 加密备份导出 / 恢复（先自动备份当前库）

**P1 验收**：直接打开 `data.db` 读不到明文；Keychain 中无明文口令；重启后数据完整；备份可异地恢复。

### P2 配置模块
- [ ] 工资条项目 CRUD（拖拽排序 / key 唯一 / 停用不删）
- [ ] 邮件组与成员（批量粘贴 TSV/CSV / 同组邮箱唯一 / 启停用）
- [ ] 中文模板编辑器 + 变量白名单 + 实时预览 + 试发
- [ ] 字段公式求值器（只允许 `+ - * / ( )` 与本行字段引用）

**P2 验收**：改字段后导出表头同步变化；未知变量模板无法设为默认；同组邮箱重复能被拦截。

### P3 数据闭环
- [ ] 月份账套状态机（draft → ready → sending → sent → archived）
- [ ] 空模板导出（`rust_xlsxwriter`，表头随字段配置动态生成）
- [ ] Excel 导入向导（`calamine`，表头映射 / 行级校验 / 冲突策略 / 错误明细）
- [ ] 逐格编辑 + 公式列自动重算 + 合计行

**P3 验收**：42 行 × 10 列样例导入 100% 正确；错误行阻断入库；事务失败整批回滚；5000 行导入 < 8s。

### P4 发送引擎（mock SMTP）
- [ ] 令牌桶限速（默认 30 封/分）+ 指数退避（30s / 2min / 10min）
- [ ] 三重核对（身份一致 / 内容一致 checksum / 变量完整）
- [ ] 幂等键 `hash(batch_id + member_id + period_id + content_checksum)`
- [ ] 中断恢复（队列持久化，强退重启只补未发）
- [ ] 发送日志（脱敏，不含工资数值与正文）+ 凭证 CSV 导出

**P4 验收**：重复点发送不产生第二封；421 自动退避并降速 50%；强退重启后只补未发；日志 grep 不到工资数值。

### P5 真实邮箱联调（🔴 依赖第 3 节飞书账号）
- [ ] 飞书 `smtp.feishu.cn:465` 实连 + 专用密码失效（535）引导
- [ ] QQ / 163 / 企业邮授权码各 5 封
- [ ] Gmail / M365 OAuth2（PKCE 环回）— 可降级为「暂不做」

**P5 验收**：各邮箱成功送达 5 封；中文主题/正文无乱码；HTML 与 text/plain 双版本正常；535 时立即中止整批不重发。

### P6 发布（未签名内部分发 · 决策 D8）
- [ ] `tauri build --target universal-apple-darwin` 产出 universal .app
- [ ] 制 dmg（图标 / Applications 快捷方式）+ SHA-256（`scripts/make-dmg.sh`）
- [ ] 分发方式按「是否触发 quarantine」二选一：
  - **U 盘 / 局域网共享（推荐）**：拷贝不过 Gatekeeper，双击直接开，零提示
  - **网盘 / 内网下载页**：每台 Mac 首次安装需「系统设置 → 隐私与安全性 → 仍要打开」+ 管理员密码
- [ ] 写一页安装说明：怎么放行、Keychain 授权弹窗该点「允许」、卸载与数据保留位置
- [ ] 升级流程演练：新版覆盖安装 → 确认 Keychain 授权被重置后的处理步骤

**P6 验收**：在目标 Mac 上从安装到试发一封 ≤ 10 分钟；升级后数据完整、加密库可正常打开；
`spctl -a -vv` **预期失败**（未签名，符合 D8），确认失败信息与安装说明里写的一致。

> ⚠️ 未签名的两个后果，必须在交付前跟使用者讲清：
> ① 网盘分发的每个新版本都要手动放行一次；
> ② 每次重建/升级后代码签名变化，Keychain 授权会重置——弹窗**必须点「允许」**，点「拒绝」将无法打开加密库。
> 若未来要给外部客户分发，这条决策需要重新评估。

---

## 5. 必须坚持的红线（写给每个开发 PR 的自查项）

1. **绝不**把多个收件人放进同一封邮件；禁止 CC / BCC。
2. **绝不**把工资数值写进日志、错误上报、备份文件名。
3. **绝不**提供「一键群发全部组」而不经过二次确认的入口。
4. 失败重试只允许针对**未成功的 job**，不提供整批重发。
5. 前端拿不到工资数值与凭据的长期引用；所有外部副作用走 Rust Command 层。
6. 模板引擎沙箱化，禁止文件读取 / 系统调用 / 访问环境变量。
7. 已发送账套只读；修改必须「解锁 + 生成修订记录 + 仅对未发送者补发」。
8. 字段一旦被历史数据引用**只能停用不能删除**；月份账套冻结 `field_snapshot`。

---

## 6. 尚未解决 / 需持续跟踪

| 事项 | 状态 | 何时必须解决 |
|---|---|---|
| Apple Developer Program 账号与证书 | ⛔ 不购买（D8） | 若改为对外分发则需重新评估 |
| 飞书发件账号与专用密码 | 🔴 未确认 | P5 前 |
| 真实工资字段口径与名单 | 🔴 待提供 | P2/P3 前 |
| 浅色主题 | 延后 | v1.1 |
| OAuth2 邮箱（Gmail/M365） | 延后 | v1.1，可先只做授权码 |
| 多机备份交接流程与保管责任 | 待定 | 模式 A 下优先级低 |
| `tauri-plugin-updater` 自动更新 | 延后 | v2（需签名更新清单 + HTTPS 源） |

---

## 7. 关键文档索引

| 需要查什么 | 去哪 |
|---|---|
| 完整设计（15 章 + 15 张高保真原型） | `payslip-mailer-design.html`（浏览器打开） |
| 需求 → 模块 → 界面映射 | 设计文档第 2 章 |
| 数据表定义 | 设计文档第 6 章 |
| 邮箱配置矩阵 / 飞书参数 / 错误码处置 | 设计文档第 7 章 |
| UI 栅格 / 色彩 Token / 组件规范 | 设计文档第 11 章 |
| 各界面原型 | 设计文档第 12 章（S01–S15） |
| 阶段验收标准 | 设计文档第 13 章 + 本文件第 4 节 |
| 风险与缓解 | 设计文档第 14 章 |
