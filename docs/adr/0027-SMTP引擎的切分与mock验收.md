# ADR-0027 · SMTP 引擎：lettre、rustls 后端与 mock 验收的切分

日期：2026-11-12 · 状态：**accepted**（TC-500 验收通过，2026-11-12）

## 背景

TC-400 已经把编排跑在**注入的 Sender** 上（「假发送器」），`recover_and_run_batch`
里注明了接线点。本卡把真发送器接上 —— 设计文档 ch.7 的「飞书邮箱专项」与
ch.3 的测试纪律：

> **ch.3**：Rust cargo test + Vitest + **mock SMTP** E2E —— 发信一律走 mock SMTP，
> 测试环境**绝不真发**。
> **ch.7**：smtp.feishu.cn / 465 / SSL / 完整企业邮箱 + **专用密码**；
> 「逐封建立 SMTP 会话，物理隔离工资数据」；「测试连接只发一封自检信」；
> 20–30 封/分钟。

两条硬约束决定技术路线：

- **门禁第 9 项**：产物不得链 `libcrypto` / `libssl`（ADR-0010 的取向：
  CommonCrypto，不引 OpenSSL）。lettre 默认的 `native-tls` 在 macOS 链
  Security 但依赖链里可能带 OpenSSL —— **必须用 rustls 后端**。
- **项目取向**（ADR-0010 第 4 条同族）：不为小功能多引依赖。

有五件事要定。

### 洞一：Sender trait 要不要改？

TC-400 的 `Sender::send(&mut self, to_email: &str)` 只知道地址 ——
真发送器还需要**邮件内容**（渲染结果）。两个选择：

- 编排循环里构建 MIME，trait 收 `EmailMessage`（内容已备好）；
- trait 收渲染所需的上下文，发送器自己渲染。

⇒ **trait 收 `EmailMessage`**（`to_email / subject / html / text`）。
渲染是**编排**的事（ch.7 编排第 2 步「逐人渲染」在核对之后、发送之前），
发送器只管「把这封信发出去」—— 两者混在一个类型里，
mock 发送器就要跟着懂渲染（那 mock 就不是 mock 了）。
**渲染失败 → 该封判 dead（error_code="render"），批次继续** ——
TC-401 的核对已经拦过 required 空值，渲染理论不该失败；万一失败，
一条模板 bug 不该拦住整批（其余人照样该收到）。

⇒ `drive_batch` 相应改造：取 job → 渲染 → 发 → 记结果。
批次创建（`create_batch`）要带上 `template_id`（渲染用哪套模板；
表列 TC-400 已建，之前一直空着）。

### 洞二：TLS 后端与证书

`lettre = { default-features = false, features = ["smtp-transport", "builder",
"hostname", "rustls-tls", "ring"] }`：

- `rustls-tls` = rustls + **ring**（静态编译 —— `otool -L` 不见 libcrypto，
  门禁第 9 项过）+ webpki 根证书（Mozilla 集合，覆盖飞书/QQ/163/Gmail）。
- 不用 `native-tls`（可能引入 OpenSSL）。
- ring 有 C/汇编单元 —— 但 SQLCipher 本来就是 C（bundled），
  门禁查的是「不链系统加密库」不是「零 C」，取向一致。

### 洞三：专用密码怎么存？

ADR-0009 第 3 条早就留了位置：`SecretKind::SmtpCredential`（service =
`cn.payslipmailer.smtp-credential`，account = 配置行标识）。
**密码只进 Keychain**，`smtp_settings.credential_ref` 只是条目名
（ch.6 原话「credential_ref 只是 Keychain 条目名，不含秘密」）。

读取时机：发送那一刻从 Keychain 读（`send` 内部）——
不在进程里长期持有（ch.9「发送凭据在内存中使用后清零」的 v1 近似：
每次 send 现读现用，用完释放）。

### 洞四：「测试连接」发什么？

ch.7：「『测试连接』**只发一封自检信**」。⇒ 用当前设置构建发送器，
给**管理员自己的邮箱**发一封「薪邮自检」—— 不是空连接探测
（那验证不了 AUTH 与 Data 阶段）。结果三态：成功（2xx）/ 认证失败（535）/ 其它错误（带码）。

**mock 验收**：lettre 自带 `StubTransport`（`smtp-transport` feature）——
按预设吐回码、把 Message 收进内存。**自动化测试全部跑在 Stub 上**（ch.3 纪律），
真凭据的真连验证归 TC-501（真机 5 封）。

### 洞五：邮件的 MIME 形态

ch.8：「HTML 内联 CSS，中文字体栈 PingFang SC…同时自动生成 text/plain 兜底」。

⇒ **multipart/alternative**（text/plain + text/html 双部分），
收件端选渲染能力；主题按 RFC 2047 编码（lettre builder 自带）；
**信封收件人 = To 头收件人 = 恰好一人**（红线 1 的最后一道墙 ——
MIME 层面就没有第二个收件人的位置）。

## 决策

1. **Sender trait 改为收 `EmailMessage`**；`drive_batch` 内渲染
   （TC-401 的 `render_for`），渲染失败该封 dead（`error_code="render"`）、批次继续。
2. **批次创建带 `template_id`**（表列已有；`start_batch` 传上来）。
3. **lettre 0.11 + rustls + ring**（`default-features = false`，不引 native-tls）。
4. **专用密码进 Keychain**（`SecretKind::SmtpCredential`），配置表只存
   `credential_ref`；发送时现读现用。
5. **测试连接 = 发一封自检信给管理员**；自动化验收全部跑在 lettre `StubTransport` 上。
6. **邮件 = multipart/alternative（text + html）**，收件人恰好一人（MIME 结构级）。
7. **每封新建 SMTP 会话**（ch.7「逐封建立会话」；20–30 封/分的节奏下
   连接开销可忽略，换来「一封出问题不污染下一封」的物理隔离）。

## 后果

**变简单的**：

- mock 验收闭环：TC-500 结案不需要真凭据 —— StubTransport 把「设置→渲染→发送→
  分类→持久化」整链验完；TC-501 只做真机 5 封（真凭据到位后）。
- `recover_and_run_batch` 的「SMTP 未配置」探针从查表升级为查表+凭据 ——
  接线点兑现，编排代码一行不改。

**要认的账**：

- **webpki 根证书不含企业内网 CA**：自建 SMTP 服务器（内网、无公网证书）
  在 v1 发不了信 —— ch.7 的「自建/其他」场景要用户有公网证书。
- **每封新建连接**比连接池慢（每封多 ~1s TLS 握手）——
  在 2s/封 的限速节奏里被吃掉，无感。

**重新评估的触发条件**：

- 若要支持内网自建服务器 → TLS 配置加「跳过验证/自装 CA」选项（要单独评估）。
- 若要 OAuth2（Gmail/M365）→ TC-502 的范围。

## 附：本 ADR 与其他 ADR 的关系

- **ADR-0009**（Keychain）：`SecretKind::SmtpCredential` 的兑现。
- **ADR-0023 / 0025**（编排）：Sender trait 的改造；接线点兑现。
- **ADR-0010**（加密栈取向）：rustls+ring 不链 libcrypto 的门禁约束。
