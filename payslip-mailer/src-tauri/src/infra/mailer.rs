//! SMTP 引擎（TC-500 · ADR-0027）
//!
//! 三块：**配置**（`smtp_settings` + Keychain 凭据）、**邮件构建**
//! （multipart/alternative）、**发送**（每封新建 SMTP 会话 → lettre → `SendResult`）。
//!
//! 验收纪律（ch.3）：自动化测试全部跑在 lettre 的 `StubTransport` / 纯函数上 ——
//! **绝不真发**。lettre 的 `Error` 没有公开构造器（外部无法模拟 452/550），
//! 所以映射拆成两层：lettre Error → [`TransportOutcome`]（薄翻译，8 行，
//! 归 TC-501 真机验证）→ `SendResult`（纯函数，本卡单测钉住）。
//!
//! TLS 后端 rustls + ring（ADR-0027 决策 3）：`otool -L` 不见 libcrypto，
//! 门禁第 9 项继续过。

use lettre::message::{Mailbox, MultiPart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::{Error as SmtpError, SmtpTransport};
use lettre::{Message, Transport};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;

use crate::domain::retry::SendResult;
use crate::error::{AppError, AppResult};
use crate::infra::batch::EmailMessage;
use crate::infra::db::DbState;
use crate::infra::keychain::{KeychainStore, SecretKind};

/// TLS 形态（ch.7 邮箱配置矩阵；文件在迁移 0010 的注释里）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Security {
    /// implicit TLS（465 —— 飞书/QQ/163 主路径）。
    Ssl,
    /// 明文连接后升级（587）。
    StartTls,
    /// 无 TLS（仅内网 —— ch.7「自建/其他」）。
    None,
}

impl Security {
    pub fn parse(text: &str) -> AppResult<Self> {
        match text {
            "ssl" => Ok(Self::Ssl),
            "starttls" => Ok(Self::StartTls),
            "none" => Ok(Self::None),
            other => Err(AppError::ValidationError(format!(
                "加密方式只能是 ssl / starttls / none，写的是 {other}"
            ))),
        }
    }
}

/// 发信邮箱配置（`smtp_settings` 表的行；单行表 id=1）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SmtpSettings {
    pub host: String,
    pub port: i64,
    pub security: String,
    pub username: String,
    pub from_name: String,
    pub from_email: String,
    pub reply_to: Option<String>,
    pub rate_per_min: i64,
    /// Keychain 条目名 —— **不含密码本体**（ch.6 原话）。
    pub credential_ref: String,
}

/// Keychain 条目名（单行表 → 固定 account）。
pub const CREDENTIAL_REF: &str = "smtp-credential-1";

/// 读配置（没有 → `None`）。
pub fn get_settings(db: &DbState) -> AppResult<Option<SmtpSettings>> {
    db.read(|conn| {
        conn.query_row(
            "SELECT host, port, security, username, from_name, from_email, reply_to, rate_per_min, credential_ref \
             FROM smtp_settings WHERE id = 1",
            [],
            |row| {
                Ok(SmtpSettings {
                    host: row.get(0)?,
                    port: row.get(1)?,
                    security: row.get(2)?,
                    username: row.get(3)?,
                    from_name: row.get(4)?,
                    from_email: row.get(5)?,
                    reply_to: row.get(6)?,
                    rate_per_min: row.get(7)?,
                    credential_ref: row.get(8)?,
                })
            },
        )
        .optional()
        .map_err(|e| AppError::Database(e.to_string()))
    })
}

/// 保存配置（**密码只进 Keychain**，ADR-0027 决策 4）。
///
/// 单行表：已存在就整行更新；`credential_ref` 固定。
/// 发信速率顺手同步进 `app_meta.rate_per_min`（TC-400 的令牌桶读那里 ——
/// 两处合一，避免「设置页改了、桶没跟着变」）。
pub fn save_settings(
    db: &DbState,
    store: &KeychainStore,
    settings: &SmtpSettings,
    password: &str,
) -> AppResult<SmtpSettings> {
    Security::parse(&settings.security)?;
    if settings.host.trim().is_empty() {
        return Err(AppError::ValidationError("SMTP 主机不能为空".into()));
    }
    if !(1..=65535).contains(&settings.port) {
        return Err(AppError::ValidationError("端口要在 1–65535 之间".into()));
    }
    if settings.username.trim().is_empty() || settings.from_email.trim().is_empty() {
        return Err(AppError::ValidationError(
            "SMTP 用户名与发件地址不能为空".into(),
        ));
    }
    if password.trim().is_empty() {
        return Err(AppError::ValidationError("专用密码不能为空".into()));
    }
    let now = now_secs();
    let mut s = settings.clone();
    s.credential_ref = CREDENTIAL_REF.to_string();
    db.write(|conn| {
        conn.execute(
            "INSERT INTO smtp_settings \
             (id, host, port, security, username, from_name, from_email, reply_to, rate_per_min, credential_ref, created_at, updated_at) \
             VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10) \
             ON CONFLICT(id) DO UPDATE SET \
               host=excluded.host, port=excluded.port, security=excluded.security, \
               username=excluded.username, from_name=excluded.from_name, from_email=excluded.from_email, \
               reply_to=excluded.reply_to, rate_per_min=excluded.rate_per_min, updated_at=excluded.updated_at",
            params![
                s.host,
                s.port,
                s.security,
                s.username,
                s.from_name,
                s.from_email,
                s.reply_to,
                s.rate_per_min,
                s.credential_ref,
                now
            ],
        )
        .map_err(|e| AppError::Database(format!("写 SMTP 配置失败：{e}")))?;
        conn.execute(
            "INSERT INTO app_meta (key, value) VALUES ('rate_per_min', ?1) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![s.rate_per_min.to_string()],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    })?;
    // 密码只进 Keychain（写或覆盖；ADR-0009 第 2 条的先 update 后 add 已在 store 内部）
    store.put(
        SecretKind::SmtpCredential,
        CREDENTIAL_REF,
        &crate::infra::keychain::Secret::new(password.as_bytes().to_vec()),
    )?;
    Ok(s)
}

/// SMTP 已配置？（表有行 **且** Keychain 有凭据 —— TC-402 的探针升级）。
pub fn smtp_configured(db: &DbState, store: &KeychainStore) -> bool {
    let has_row = get_settings(db).map(|s| s.is_some()).unwrap_or(false);
    // 条目不存在 → Err（未配置）；读到了才算配置过
    let has_credential = load_password(store).is_ok();
    has_row && has_credential
}

/// 发送那一刻从 Keychain 现读专用密码（ADR-0027 决策 4：现读现用，不在进程里长期持有）。
fn load_password(store: &KeychainStore) -> AppResult<String> {
    let secret = store.get(SecretKind::SmtpCredential, CREDENTIAL_REF)?;
    String::from_utf8(secret.as_bytes().to_vec()).map_err(|_| {
        AppError::Keychain("发信凭据不是合法的文本（Keychain 里的条目可能被别的程序写过）".into())
    })
}

/// 按配置构建一个 transport（**每封新建会话** —— ADR-0027 决策 7）。
fn build_transport(settings: &SmtpSettings, password: &str) -> AppResult<SmtpTransport> {
    let security = Security::parse(&settings.security)?;
    let builder = match security {
        Security::Ssl => SmtpTransport::relay(&settings.host),
        Security::StartTls => SmtpTransport::starttls_relay(&settings.host),
        // 仅内网：无 TLS（ch.7「自建/其他：无（仅内网）」）
        Security::None => Ok(SmtpTransport::builder_dangerous(&settings.host)),
    }
    .map_err(|e| AppError::Io(format!("连接 SMTP 服务器失败：{e}")))?;
    Ok(builder
        .port(settings.port as u16)
        .credentials(Credentials::new(
            settings.username.clone(),
            password.to_string(),
        ))
        .build())
}

/// 构建一封 MIME 邮件（ADR-0027 决策 6）。
///
/// **收件人恰好一人**（MIME 结构级红线 1 —— 没有第二个收件人的位置）；
/// multipart/alternative（text/plain + text/html 双部分，ch.8「强制生成 text/plain 兜底」）；
/// Reply-To 按配置（可选）。
pub fn build_mime(settings: &SmtpSettings, message: &EmailMessage) -> AppResult<Message> {
    let from = if settings.from_name.trim().is_empty() {
        Mailbox {
            name: None,
            email: settings
                .from_email
                .parse()
                .map_err(|e| AppError::ValidationError(format!("发件地址不合法：{e}")))?,
        }
    } else {
        Mailbox {
            name: Some(settings.from_name.trim().to_string()),
            email: settings
                .from_email
                .parse()
                .map_err(|e| AppError::ValidationError(format!("发件地址不合法：{e}")))?,
        }
    };
    let to: Mailbox = message
        .to_email
        .parse()
        .map_err(|e| AppError::ValidationError(format!("收件地址不合法：{e}")))?;

    let mut builder = Message::builder()
        .from(from)
        .to(to)
        .subject(&message.subject);
    if let Some(reply_to) = settings.reply_to.as_deref()
        && !reply_to.trim().is_empty()
    {
        let mbox: Mailbox = reply_to
            .parse()
            .map_err(|e| AppError::ValidationError(format!("回信地址不合法：{e}")))?;
        builder = builder.reply_to(mbox);
    }
    builder
        .multipart(MultiPart::alternative_plain_html(
            message.text.clone(),
            message.html.clone(),
        ))
        .map_err(|e| AppError::Internal(format!("构建邮件失败：{e}")))
}

/// Transport 层的结果（**可测**的中间表示 —— ADR-0027 决策 4）。
///
/// lettre 的 `Error` 没有公开构造器，外造模拟不了 452/550；
/// 于是「lettre 结果 → TransportOutcome」的薄翻译（8 行，归 TC-501 真机验证）
/// 与「TransportOutcome → SendResult」的纯函数（本卡单测钉住）分开。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportOutcome {
    /// 2xx：服务器收下了。
    Accepted,
    /// 4xx：稍后再试。
    Transient(u16),
    /// 5xx：服务器拒绝。
    Permanent(u16),
    /// 连接层失败（超时 / TLS / DNS / 认证握手崩了）。
    Failure,
}

/// TransportOutcome → SendResult（**纯函数**，分类判定只有 domain::retry 一份）。
pub fn outcome_to_send_result(outcome: TransportOutcome) -> SendResult {
    match outcome {
        TransportOutcome::Accepted => SendResult::Accepted,
        TransportOutcome::Transient(c) => SendResult::Rejected(c),
        TransportOutcome::Permanent(c) => SendResult::Rejected(c),
        TransportOutcome::Failure => SendResult::Network,
    }
}

/// lettre 的 smtp Error → TransportOutcome（**薄翻译**，归 TC-501 真机验证）。
fn smtp_error_to_outcome(err: &SmtpError) -> TransportOutcome {
    if let Some(code) = err.status() {
        // Code → u16（lettre 自带 From：severity*100 + category*10 + detail）
        let n = u16::from(code);
        return match n {
            400..500 => TransportOutcome::Transient(n),
            500..600 => TransportOutcome::Permanent(n),
            _ => TransportOutcome::Failure,
        };
    }
    // 没有回码 = 连接层 / TLS / 客户端错误
    TransportOutcome::Failure
}

/// 真发送器（ADR-0027 决策 7：每封新建 SMTP 会话）。
pub struct SmtpSender {
    settings: SmtpSettings,
    store: KeychainStore,
}

impl SmtpSender {
    pub fn new(settings: SmtpSettings, store: KeychainStore) -> Self {
        Self { settings, store }
    }
}

impl crate::infra::batch::Sender for SmtpSender {
    fn send(&mut self, message: &EmailMessage) -> SendResult {
        // 现读密码（不在进程里长期持有）
        let password = match load_password(&self.store) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, "读发信凭据失败");
                return SendResult::Network;
            }
        };
        let email = match build_mime(&self.settings, message) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(error = %e, "构建邮件失败");
                return SendResult::Network;
            }
        };
        // 每封新建会话（连接 + TLS + AUTH + MAIL + RCPT + DATA + QUIT）
        let transport = match build_transport(&self.settings, &password) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(error = %e, "建 SMTP 会话失败");
                return SendResult::Network;
            }
        };
        let outcome = match transport.send(&email) {
            Ok(_) => TransportOutcome::Accepted,
            Err(ref e) => smtp_error_to_outcome(e),
        };
        outcome_to_send_result(outcome)
    }
}

/// 测试连接的结果（ch.7「测试连接只发一封自检信」）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum TestResult {
    /// 自检信已被服务器收下（2xx）。
    Ok,
    /// 认证失败（535/534）—— 引导去重新生成专用密码。
    AuthFailed,
    /// 其它失败（带 SMTP 码或「连接失败」）。
    Failed { detail: String },
}

/// 测试连接：给**管理员自己**发一封自检信（ADR-0027 决策 5）。
///
/// 不是空连接探测 —— 那验证不了 AUTH 与 DATA 阶段；
/// 一封真的自检信把配置的全链路都过了一遍。
pub fn test_connection(
    store: &KeychainStore,
    settings: &SmtpSettings,
    password: Option<&str>,
) -> AppResult<TestResult> {
    // 输入框里带了新密码（还没保存）→ 先用它测；没有 → 用钥匙串里的
    let password: String = match password {
        Some(p) if !p.trim().is_empty() => p.to_string(),
        _ => load_password(store)?,
    };
    let self_check = EmailMessage {
        to_email: settings.from_email.clone(),
        subject: "薪邮 · 测试连接自检".into(),
        html: "<p>这是一封来自薪邮的自检信 —— 收到它说明发信配置已经通了。</p>".into(),
        text: "这是一封来自薪邮的自检信 —— 收到它说明发信配置已经通了。".into(),
    };
    let email = build_mime(settings, &self_check)?;
    let transport = build_transport(settings, &password)?;
    match transport.send(&email) {
        Ok(_) => Ok(TestResult::Ok),
        Err(ref e) => {
            if e.status().map(|c| u16::from(c) >= 500).unwrap_or(false) && e.is_permanent() {
                Ok(TestResult::AuthFailed)
            } else {
                Ok(TestResult::Failed {
                    detail: e.to_string(),
                })
            }
        }
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// TransportOutcome → SendResult 的纯映射（spec 标准 5 的可测半）。
    #[test]
    fn the_outcome_mapping_is_complete() {
        assert_eq!(
            outcome_to_send_result(TransportOutcome::Accepted),
            SendResult::Accepted
        );
        assert_eq!(
            outcome_to_send_result(TransportOutcome::Transient(452)),
            SendResult::Rejected(452),
            "4xx → Rejected(码)，分类走 domain::retry → Retry"
        );
        assert_eq!(
            outcome_to_send_result(TransportOutcome::Permanent(550)),
            SendResult::Rejected(550),
            "5xx → Rejected(码)，分类走 domain::retry → Dead"
        );
        assert_eq!(
            outcome_to_send_result(TransportOutcome::Failure),
            SendResult::Network,
            "连接层失败 → Network → Pause"
        );
        // 分类判定只有 domain::retry 一份：SendResult 进 classify
        assert!(matches!(
            crate::domain::retry::classify(SendResult::Rejected(452)),
            crate::domain::retry::Disposition::Retry
        ));
        assert!(matches!(
            crate::domain::retry::classify(SendResult::Rejected(550)),
            crate::domain::retry::Disposition::Dead
        ));
        assert!(matches!(
            crate::domain::retry::classify(SendResult::Network),
            crate::domain::retry::Disposition::Pause
        ));
    }

    /// MIME 构建：multipart/alternative、To 恰好一人、无 Cc/Bcc（spec 标准 3）。
    #[test]
    fn the_mime_has_exactly_one_recipient_and_both_bodies() {
        let settings = SmtpSettings {
            host: "smtp.feishu.cn".into(),
            port: 465,
            security: "ssl".into(),
            username: "hr@example.com".into(),
            from_name: "人力资源部".into(),
            from_email: "hr@example.com".into(),
            reply_to: Some("hr@example.com".into()),
            rate_per_min: 30,
            credential_ref: CREDENTIAL_REF.into(),
        };
        let message = EmailMessage {
            to_email: "zhangsan@example.com".into(),
            subject: "2026 年 10 月工资条".into(),
            html: "<p>张三 您好：</p>".into(),
            text: "张三 您好：".into(),
        };
        let email = build_mime(&settings, &message).expect("构建失败");
        let formatted = email.formatted();
        let raw = String::from_utf8_lossy(&formatted);

        // To 恰好一个收件人；没有 Cc/Bcc 头（红线 1 的 MIME 结构级）
        assert!(raw.contains("To: zhangsan@example.com"), "{raw}");
        assert!(!raw.to_lowercase().contains("cc:"), "不得有 Cc 头：{raw}");
        assert!(!raw.to_lowercase().contains("bcc:"), "不得有 Bcc 头：{raw}");
        // multipart/alternative：两个部分都在
        assert!(raw.contains("multipart/alternative"), "{raw}");
        assert!(raw.contains("text/plain"), "{raw}");
        assert!(raw.contains("text/html"), "{raw}");
        // Reply-To 按配置
        assert!(raw.contains("Reply-To: hr@example.com"), "{raw}");
        // From 带显示名
        assert!(raw.contains("From: =?utf-8?"), "{raw}");
    }

    /// 回信地址为空时没有 Reply-To 头。
    #[test]
    fn no_reply_to_header_when_unset() {
        let settings = SmtpSettings {
            host: "smtp.feishu.cn".into(),
            port: 465,
            security: "ssl".into(),
            username: "hr@example.com".into(),
            from_name: String::new(),
            from_email: "hr@example.com".into(),
            reply_to: None,
            rate_per_min: 30,
            credential_ref: CREDENTIAL_REF.into(),
        };
        let message = EmailMessage {
            to_email: "a@example.com".into(),
            subject: "s".into(),
            html: "<p>x</p>".into(),
            text: "x".into(),
        };
        let email = build_mime(&settings, &message).expect("构建失败");
        let formatted = email.formatted();
        let raw = String::from_utf8_lossy(&formatted);
        assert!(!raw.contains("Reply-To"), "{raw}");
    }
}
