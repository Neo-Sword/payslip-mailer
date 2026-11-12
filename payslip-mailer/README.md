# 薪邮 Payslip Mailer

macOS 本机运行的**工资条群发工具**。配置一次 → 每月导出空 Excel 模板 → 财务填好导入 → 逐人独立成信发送 → 全程留痕可举证。

> 数据加密保存在本机，除 SMTP 发信外不外联，无遥测、无崩溃上报、不上 App Store。

---

## 技术栈

| 层 | 选型 |
|---|---|
| 外壳 | Tauri 2（WKWebView + Rust） |
| 后端 | Rust 2024 |
| 前端 | React + TypeScript + Vite |
| 数据库 | SQLite + SQLCipher（AES-256，bundled 静态编译） |
| 凭据 | macOS Keychain |
| Excel | `calamine`（读）/ `rust_xlsxwriter`（写） |
| 邮件 | `lettre`（SMTP over SSL 465） |
| 模板 | `minijinja`（Jinja2 语法，沙箱化） |

---

## 目录结构

```
payslip-mailer/
├── src/                        # React 前端
│   ├── components/
│   │   ├── layout/             # AppShell（标题栏 / 侧边栏 / 内容区）
│   │   └── ui/                 # 设计系统基元（CSS Modules）
│   ├── styles/
│   │   ├── tokens.css          # Design Token（深色主题色板）
│   │   └── global.css          # reset + 排版
│   └── App.tsx
├── src-tauri/
│   ├── src/
│   │   ├── commands/           # IPC 边界：参数校验 + DTO 组装
│   │   ├── domain/             # 实体、状态机、校验规则、幂等键
│   │   ├── infra/              # SQLCipher / Keychain / Excel / 模板 / SMTP
│   │   ├── error.rs            # 统一错误码 → 中文文案
│   │   └── lib.rs
│   ├── capabilities/           # Tauri 权限白名单
│   └── tauri.conf.json
├── scripts/                    # sign.sh / notarize.sh / make-dmg.sh / release.sh
└── package.json
```

---

## 本机开发

### 首次准备

```bash
# Rust 工具链（本项目唯一的硬性前置）
curl --proto '=https' --tlsv1.2 https://sh.rustup.rs -sSf | sh
source "$HOME/.cargo/env"

# universal dmg 需要双架构目标
rustup target add aarch64-apple-darwin x86_64-apple-darwin
```

其余依赖：Xcode Command Line Tools（`xcode-select --install`）、Node.js ≥ 20、Git。

### 日常命令

```bash
npm install

npm run dev            # 仅前端（浏览器预览设计系统，不含 Rust 能力）
npm run tauri dev      # 完整桌面应用
npm run build          # 前端类型检查 + 构建
npm run typecheck      # 只做类型检查
npm run rust:check     # Rust 编译检查
npm run rust:lint      # clippy（-D warnings）
npm run rust:test      # Rust 单测
```

### 发布

```bash
npm run release            # 未签名构建 → dmg → SHA-256（默认路径，决策 D8）
npm run release:signed     # 签名 → 公证 → dmg（需 Developer ID 证书，当前不购买）
```

未签名产物的分发要点：U 盘 / 局域网共享拷贝不会打 `quarantine` 属性，双击即可打开；
网盘或浏览器下载则需要在每台 Mac 上「系统设置 → 隐私与安全性 → 仍要打开」放行一次。
每次升级后 Keychain 授权会重置，弹窗必须点「允许」。

---

## 界面规范

所有颜色、字号、间距、圆角走 `src/styles/tokens.css` 的 CSS 变量。
**v1 只交付深色主题**；浅色主题延后到 v1.1，届时只替换色板、不改布局。

可视化规范与 15 张高保真原型见仓库根目录的 `payslip-mailer-design.html`。

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
