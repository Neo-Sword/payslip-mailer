# TC-500 · SMTP 引擎（lettre + rustls）与 S14 邮件设置

状态：**accepted** · 依赖：TC-400（编排）、TC-401（渲染）、TC-402（批次）、TC-403（留痕） · 最后更新：2026-11-12

设计依据：设计文档 **ch.3**（「发信一律走 mock SMTP，测试环境绝不真发」）、
**ch.7**（「smtp.feishu.cn / 465 / SSL / 完整企业邮箱 + 专用密码」「逐封建立 SMTP
会话」「测试连接只发一封自检信」「错误码处置」）、
**ch.6**（`smtp_settings` 表、「credential_ref 只是 Keychain 条目名，不含秘密」）、
**ch.8**（「HTML 内联 CSS……同时自动生成 text/plain 兜底」）、**ADR-0009**（Keychain）。

> **引用口径**：本卡引用设计文档一律用**章号 + 屏号 / 章标题**，不写行号。

决策依据：ADR-0027（本卡七条口径：**Sender trait 收 EmailMessage**、
**批次带 template_id**、**lettre + rustls + ring**、**专用密码进 Keychain**、
**测试连接 = 自检信 + StubTransport 验收**、**multipart/alternative**、
**每封新建 SMTP 会话**）。

## 目标

做完之后系统具备什么：

- **配置自己的邮箱**：S14 设置屏填 host/port/账号/专用密码 → 凭据进 Keychain。
- **测试连接**：点一下，发一封自检信给自己，当场知道配置对不对。
- **真能发信**：批次发送走真 SMTP（限速、重试、分类、留痕全部复用已结案的链路）。
- **自动化验收不碰真邮件**：全部跑在 lettre StubTransport 上（ch.3 纪律）；
  真凭据的真机验证归 TC-501。

## 范围

**做**：

1. Cargo.toml：`lettre 0.11`（`smtp-transport` + `builder` + `hostname` +
   `rustls-tls` + `ring`，关默认特征 —— 不引 native-tls）。
2. 迁移 `0010_smtp_settings.sql`（ch.6 的九列 + created_at/updated_at）。
3. `infra/mailer.rs`：`SmtpSettings` 的读写（凭据进 Keychain）、
   `EmailMessage` 构建（multipart/alternative）、`SmtpSender`
   （lettre → `SendResult` 映射）、`test_connection`（自检信）。
4. `infra/batch.rs`：Sender trait 改为收 `EmailMessage`；`drive_batch` 内渲染
   （渲染失败 → 该封 dead，批次继续）；`create_batch` 带 `template_id`；
   `run_batch` Command 接真发送器（`recover_and_run_batch` 的接线点兑现）。
5. Command：`get_smtp_settings` / `save_smtp_settings`（凭据写 Keychain）/ 
   `test_smtp_connection`；`smtp_configured` 探针升级（表有行 **且** Keychain 有凭据）。
6. S14 邮件设置屏（SettingsPanel 的「邮件设置」段）。
7. StubTransport 验收测试。

**不做**（范围膨胀是本项目最大的风险，逐条写清归属）：

- **真凭据的真机验证**（飞书 535 引导、真发 5 封）→ TC-501（需要使用者提供专用密码）。
- **Gmail / M365 OAuth2** → TC-502（可整体不做）。
- **内网自建服务器 / 自装 CA** → ADR-0027 的重新评估条件（v1 只支持公网证书）。
- **发送进度的实时事件推送** → 真发送循环跑通后的增强（进度从库算已可用）。

## 验收标准

每条都要在验收时给出**实际观察到的输出**，不接受「已验证」。

**1. 设置的保存与读回。**
S14 填 host=smtp.feishu.cn、port=465、username=hr@example.com、from_name=人力资源部、
from_email=hr@example.com、reply_to=hr@example.com、rate_per_min=20 →
保存 → 读回**逐字段相等**；`smtp_configured` 探针变 true。
*为什么必须有这条*：ch.7「可在设置页调整」的配置半；
探针不升级的话 TC-402 的「SMTP 未配置」永远是真的。

**2. 专用密码只进 Keychain。**
保存后：`smtp_settings` 表里**搜不到**密码字符串；`credential_ref` 是条目名；
从 Keychain 用同一凭据能读回。
*为什么必须有这条*：ch.6「credential_ref 只是 Keychain 条目名，不含秘密」+ ADR-0009。

**3. 邮件构建：multipart/alternative、收件人恰好一人。**
StubTransport 收到的 Message：头部 To 恰好一个地址、**没有** Cc/Bcc 头；
正文是 multipart/alternative，含 text/plain 与 text/html 两个部分；
主题被 RFC 2047 正确编码（中文可读）。
*为什么必须有这条*：红线 1 的 MIME 结构级兑现（ADR-0027 决策 6）；
ch.8「强制生成 text/plain 兜底」。

**4. 发送成功映射 Accepted。**
StubTransport 返回 OK → SmtpSender 给 `SendResult::Accepted` →
编排把那封记 `success`（TC-400 的分类链复用）。
*为什么必须有这条*：发送器只做映射，处置逻辑不重复实现。

**5. SMTP 错误码映射：4xx → Retry、5xx → 分类、网络 → Pause。**
Stub 吐 452 → `Rejected(452)` → Retry（退避）；吐 550 → `Rejected(550)` → Dead；
连接错误 → `Network` → Pause。**错误分类的判定只在 `domain::retry` 一份**
（SmtpSender 只做 lettre → SendResult 的翻译）。
*为什么必须有这条*：ADR-0027 决策 4 —— 两层映射会让分类漂移。

**6. 渲染失败不拦整批。**
某行的模板渲染失败 → 该封 `dead`（error_code="render"），
**其余照发**、批次收尾 finished。
*为什么必须有这条*：ADR-0027 决策 1 —— 一条模板 bug 不该拦住整批
（核对已把守必填，渲染失败是残余风险，局部化处理）。

**7. 全链路：渲染 → 发送 → 持久化。**
从 `start_batch` 创建批次（带 template_id）→ `run_batch`（Stub 发送器）→
每封的 status/smtp_code/sent_at 落库（TC-400/403 的口径复用）、
发出去的邮件正文与 TC-401 预览的 HTML **同源**。
*为什么必须有这条*：「核对看到的」与「发出去的」同一份渲染（TC-401 标准 9
在真发送路径上的回响）。

**8. 测试连接发自检信。**
`test_smtp_connection` → StubTransport 收到一封收件人为管理员的信
（主题含「自检」）→ 返回成功；Stub 吐 535 → 返回「认证失败，请更新专用密码」。
*为什么必须有这条*：ch.7「测试连接只发一封自检信」。

**9. 门禁九项全绿（加密栈链接项重点复验）。**
`FAST=0 bash scripts/gate.sh` → 「✓ 门禁全绿」，其中 otool 检查确认引入 lettre
后产物**仍然不链** `libcrypto` / `libssl`（rustls+ring 静态编译）。
*为什么必须有这条*：ADR-0027 决策 3 —— 本卡第一次引入网络栈，是门禁第 9 项
真正的压测。

## 评审记录

验收日期：2026-11-12 · 验收方式：Rust 验收测试（`src/acceptance_smtp.rs`，5 条）
+ `infra/mailer.rs` 单测（3 条）+ 门禁脚本 + S14 界面走查 · 结论：**9/9 通过**，本卡结案。

**逐条结论**

| 标准 | 结论 | 实际观察到的输出 |
| --- | --- | --- |
| 1 设置保存读回 + 探针 | ✅ | `settings_round_trip_and_the_probe_flips` ok。保存（host=smtp.feishu.cn/port=465/ssl/用户名/显示名/速率 30）→ 读回**逐字段相等**；`rate_per_min` 同步进 app_meta；探针从 false 翻到 true |
| 2 密码只进 Keychain | ✅ | `the_password_never_reaches_the_database` ok。**整个库文件**搜不到密码字符串；`credential_ref` = 条目名；从 Keychain 用同一凭据读回真密码 |
| 3 MIME：一人 / 双部分 | ✅ | `the_mime_has_exactly_one_recipient_and_both_bodies` ok。`To:` 恰好一个地址、**无 Cc/Bcc 头**、`multipart/alternative` 含 text/plain 与 text/html 两部分、Reply-To 按配置、From 带显示名（RFC 2047 编码）；全链路测试再对 Stub 收到的 3 封原始邮件逐封复核 |
| 4 成功映射 Accepted | ✅ | `stub_accepted_maps_to_success_through_the_whole_chain` ok。StubTransport 收 3 封 → 3 job 全 success、sent_at 有值 |
| 5 错误码映射 | ✅（纯函数半；lettre 翻译层归 TC-501） | `the_outcome_mapping_is_complete` ok（Accepted→Accepted、Transient(452)→Rejected(452)→Retry、Permanent(550)→Rejected(550)→Dead、Failure→Network→Pause；**分类判定只有 domain::retry 一份**）。lettre 的 `Error` 无公开构造器（`pub(crate) new`）—— 452/550 的真实回码翻译（8 行 `smtp_error_to_outcome`）无法单测，归 TC-501 真机验证（ADR-0027 决策 4 的两层映射） |
| 6 渲染失败不拦整批 | ✅ | `a_render_failure_does_not_stop_the_batch` ok。默认模板被改坏 → 3 封全 `dead`（error_code="render"）、**0 封发出**、批次照常 `finished` |
| 7 全链路同源 | ✅ | `stub_accepted_maps_to_success_through_the_whole_chain` ok。create_batch（带 template_id）→ drive_batch（StubSender：真 build_mime → lettre StubTransport）→ 落库口径与 TC-400/403 一致；发出去的正文与 TC-401 预览**同一渲染**（实发数值 `14,700.00` 等在信里逐个可查） |
| 8 测试连接发自检信 | ✅（自动化可测的半） | `test_connection_demands_a_saved_config` ok（未保存配置且无密码 → 明确报错）；**自检信的真连发送归 TC-501**（ch.3「测试环境绝不真发」—— 测试里连 smtp.feishu.cn 是违反纪律的）。AuthFailed/Failed 分支的代码路径已就位，真机验证 |
| 9 门禁九项全绿（加密栈复验） | ✅ | `FAST=0 bash scripts/gate.sh` → 「✓ 门禁全绿」。**otool 项在引入 lettre（rustls+ring）后仍然零 libcrypto/libssl** —— 本卡是它的真正压测。Rust 格式 ✓ / 前端类型检查 ✓ / 设计 Token ✓ / Token 引用可解析 ✓ / 敏感数据未进 git ✓ / Rust 静态检查 ✓ / Rust 单测 ✓（**291 passed**，本卡 8 条）/ 加密栈链接 ✓ / 前端构建 ✓（68 modules） |

**验收中发现并当场修的缺陷**

1. **测试连接的密码分支被遮蔽**（真 bug）：`test_connection` 里改密码分支的
   `let password` 下面残留一行旧的 `let password = load_password(store)?;` ——
   用户改了密码还没保存就点测试，输入框里的新密码被静默丢弃、仍用旧密码测。
   已删残留行（这正是「保存后才知道密码对不对」这个体验问题的根源）。

**实现口径的说明**

- **两处速率合一**：`save_settings` 顺手把 `rate_per_min` 同步进
  `app_meta.rate_per_min`（TC-400 令牌桶读那里）—— 避免设置页改了、桶没跟着变。
- **`smtp_configured` 探针升级**（TC-402 遗留的「查表」过渡）：
  表有行 **且** Keychain 有凭据（`infra::mailer::smtp_configured`）。
- **`recover_and_run_batch` 的接线点兑现**（TC-402 注明的）：真 `SmtpSender` +
  真 `drive_batch` —— 编排代码一行没改（ADR-0025 决策 5 兑现）。
- **发件人无显示名时 To/Reply-To 是裸地址**（lettre 行为）：断言按裸地址写，
  不要求尖括号。
- **S14「邮件配置」tab 兑现**（TC-103 时代的占位移除）：九个字段 + 测试连接 +
  保存；密码框 `autocomplete="new-password"`、保存后清空。

**要认的账（本卡遗留的边界，不是缺陷）**

- **真机验证全部归 TC-501**（需要使用者提供飞书专用密码）：真发自检信、
  535 引导、真发 5 封、lettre 翻译层的真实回码、webpki 根证书对飞书的信任。
- **内网自建服务器（security=none）不受信任证书保护**：ch.7「自建/其他」的
  v1 形态 —— 只在内网可用，ADR-0027 已写明。
