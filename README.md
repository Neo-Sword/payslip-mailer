# 薪邮 Payslip Mailer

macOS 本机运行的**工资条群发工具**。配置一次 → 每月导出空 Excel 模板 → 财务填好导入 → 逐人独立成信发送 → 全程留痕可举证。

> 数据加密保存在本机，除 SMTP 发信外不外联，无遥测、无崩溃上报、不上 App Store。

> 📌 **项目总纲**：[`总纲.html`](总纲.html)（浏览器直接打开）—— 进度看板、P0–P6 任务清单、全部产出的分类登记与检索入口。它独立于应用，不参与构建、不打进 dmg。

---

## 仓库结构

| 路径 | 内容 |
|---|---|
| `总纲.html` | **项目总纲**：进度看板 + 分期任务表 + 文件登记 + 全文检索 + 维护规范（单文件自包含） |
| `payslip-mailer-design.html` | **完整设计方案与 UI 原型全集**：15 章方案 + 15 张高保真 macOS 深色界面原型（S01–S15）。浏览器直接打开。 |
| `PREP-CHECKLIST.md` | **开发前准备清单**：环境实测、已定决策、分阶段验收标准、红线自查项 |
| `docs/` | **开发过程的唯一事实来源**：`TASKBOARD.md` 状态看板 + `specs/` 任务规格 + `adr/` 架构决策 |
| `docs/build-master-index.py` | 总纲生成器：从 TASKBOARD / specs / adr 重生成 `总纲.html`，自动编码并校验链接 |
| `payslip-mailer/` | 应用代码（Tauri 2 + Rust + React/TS） |
| `.github/workflows/` | CI：测试门禁 + 签名公证发布 |

---

## 快速开始

```bash
cd payslip-mailer
npm install --registry=https://registry.npmmirror.com   # 国内网络走镜像快 4 倍
npm run tauri dev
```

Rust 工具链是唯一的硬性前置：

```bash
curl --proto '=https' --tlsv1.2 https://sh.rustup.rs -sSf | sh
source "$HOME/.cargo/env"
rustup target add aarch64-apple-darwin x86_64-apple-darwin
```

更完整的说明见 [`payslip-mailer/README.md`](payslip-mailer/README.md)。

---

## 当前进度

> 状态以 [`docs/TASKBOARD.md`](docs/TASKBOARD.md) 为准；总纲的分期任务表每次生成时从这里读取，不另存副本。

| 阶段 | 内容 | 状态 |
|---|---|---|
| **P0** | 工程骨架、设计系统、CI、未签名 dmg | ✅ 已完成 |
| **P1** | 初始化向导、登录锁定、SQLCipher、Keychain、迁移、加密备份、清空数据 | 🔄 仅收口卡 TC-103 剩 3 条真人真机验收 |
| **P2** | 工资条项目、邮件组与成员、中文模板编辑器、公式求值器 | ✅ 已完成 |
| **P3** | 月份账套、空模板导出、Excel 导入、逐格编辑 | ✅ 已完成（TC-300~303，月度数据闭环） |
| **P4** | 发送引擎（mock SMTP）：限速、重试、幂等、中断恢复、日志 | ✅ 已完成（TC-400~403，Rust 侧链路） |
| P5 | 真实邮箱联调 | 🔴 真机验证仍阻塞（需飞书专用密码）；TC-500 已发出 spec 与 ADR-0027，本卡改跑 StubTransport 验收 |
| P6 | universal 未签名 dmg、安装说明、内部分发 | ⬜ 未开始（P0 已验证 dmg 链路） |

---

## 已确认的决策

1. 技术栈 **Tauri 2 + Rust + React/TS**
2. 发件邮箱用**飞书邮箱**（`smtp.feishu.cn:465` + 专用密码）
3. v1 **只做深色主题**
4. v1 **不做 PDF 附件**（正文即工资条，避免多留一份明文副本）
5. 团队模式 **A：一台管理机发全公司**（单管理员 + 单机加密库）
6. **不上 Mac App Store**，内部分发 dmg
7. **不购买 Apple Developer Program**（$99/年），v1 走未签名构建 + 本地构建/U 盘分发

---

## 开发红线

1. 绝不把多个收件人放进同一封邮件；禁止 CC / BCC。
2. 绝不把工资数值写进日志、错误上报、备份文件名。
3. 绝不提供「一键群发全部组」而不经过二次确认的入口。
4. 失败重试只允许针对**未成功的 job**，不提供整批重发。
5. 前端拿不到工资数值与凭据的长期引用；外部副作用一律走 Rust Command 层。
6. 模板引擎沙箱化，禁止文件读取 / 系统调用 / 访问环境变量。
7. 已发送账套只读；修改必须「解锁 + 生成修订记录 + 仅对未发送者补发」。
8. 字段一旦被历史数据引用只能停用不能删除；月份账套冻结 `field_snapshot`。
