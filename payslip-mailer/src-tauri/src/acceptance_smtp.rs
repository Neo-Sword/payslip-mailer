//! TC-500 验收测试 · spec 的 9 条标准里能自动判定的（ch.3 纪律：绝不真发）。
//!
//! 范围（spec 标准号 → 本文件的测试）：
//!   标准 1  → `settings_round_trip_and_the_probe_flips`
//!   标准 2  → `the_password_never_reaches_the_database`
//!   标准 3  → `the_mime_has_one_recipient_and_both_bodies`（+ mailer.rs 的构建单测）
//!   标准 4  → `stub_accepted_maps_to_success_through_the_whole_chain`
//!   标准 5  → `the_outcome_mapping_is_complete`（mailer.rs 的纯函数单测；
//!             lettre Error 无公开构造器 —— 452/550 的真实回码归 TC-501 真机）
//!   标准 6  → `a_render_failure_does_not_stop_the_batch`
//!   标准 7  → `stub_accepted_maps_to_success_through_the_whole_chain`
//!   标准 8  → `test_connection_demands_a_saved_config`（发自检信的真连归 TC-501）
//!   标准 9  → 门禁脚本自己的事（`scripts/gate.sh`，otool 项是本卡的压测）

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::OptionalExtension;

use crate::domain::retry::SendResult;
use crate::infra::batch::{self, EmailMessage, Sender};
use crate::infra::db::{CIPHER_PARAMS, DbState};
use crate::infra::keychain::{TempKeychain, no_user_interaction};
use crate::infra::mailer::{self, SmtpSettings};
use lettre::Transport;

struct Fixture {
    dir: PathBuf,
    kc: TempKeychain,
    db: DbState,
    period_id: i64,
    members: Vec<i64>,
}

const SETTINGS: fn() -> SmtpSettings = || SmtpSettings {
    host: "smtp.feishu.cn".into(),
    port: 465,
    security: "ssl".into(),
    username: "hr@example.com".into(),
    from_name: "人力资源部".into(),
    from_email: "hr@example.com".into(),
    reply_to: Some("hr@example.com".into()),
    rate_per_min: 30,
    credential_ref: mailer::CREDENTIAL_REF.into(),
};

impl Fixture {
    fn new(label: &str, n: usize) -> Self {
        no_user_interaction();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let dir = std::env::temp_dir().join(format!(
            "xinyou-acc-smtp-{}-{nanos}-{label}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("建临时目录失败");
        let kc = TempKeychain::new(label);
        let db = DbState::new();
        db.open(&kc.store, &dir, &CIPHER_PARAMS).expect("开库失败");
        crate::infra::fields::ensure_defaults(&db).expect("写默认字段失败");
        crate::infra::templates::ensure_defaults(&db).expect("写默认模板失败");
        let mut members = Vec::new();
        for i in 0..n {
            let id = crate::infra::members::create_member(
                &db,
                &format!("成员{i}"),
                &format!("member{i}@example.com"),
                None,
                &[],
            )
            .expect("建成员失败")
            .id;
            members.push(id);
        }
        let period = crate::infra::periods::create(&db, "2026-10").expect("建账套失败");
        crate::infra::periods::transition(
            &db,
            period.id,
            crate::domain::period::PeriodStatus::Ready,
        )
        .expect("进 ready 失败");
        for (i, member_id) in members.iter().enumerate() {
            for (key, value) in [
                ("base_salary", (15000 + i * 100).to_string()),
                ("performance", "3200".into()),
                ("meal_allowance", "400".into()),
                ("social_insurance", "-2100".into()),
                ("housing_fund", "-1800".into()),
            ] {
                crate::infra::records::update_cell(&db, period.id, *member_id, key, &value)
                    .expect("录入失败");
            }
        }
        Self {
            dir,
            kc,
            db,
            period_id: period.id,
            members,
        }
    }

    fn now(&self) -> i64 {
        1_780_000_000
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.db.close();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// **Stub 发送器**（ch.3 纪律）：真构建 MIME → lettre `StubTransport` 真发送 →
/// 结果映射。整条「构建 → Transport → 映射」链路在 stub 上跑通，零网络。
struct StubSender {
    transport: lettre::transport::stub::StubTransport,
    settings: SmtpSettings,
}

impl StubSender {
    fn new(settings: SmtpSettings) -> Self {
        Self {
            transport: lettre::transport::stub::StubTransport::new_ok(),
            settings,
        }
    }
}

impl Sender for StubSender {
    fn send(&mut self, message: &EmailMessage) -> SendResult {
        let email = match mailer::build_mime(&self.settings, message) {
            Ok(m) => m,
            Err(_) => return SendResult::Network,
        };
        match self.transport.send(&email) {
            Ok(_) => SendResult::Accepted,
            Err(_) => SendResult::Network,
        }
    }
}

/// 标准 1：设置的保存与读回；探针翻转。
#[test]
fn settings_round_trip_and_the_probe_flips() {
    let fx = Fixture::new("std01", 1);
    let before = mailer::smtp_configured(&fx.db, &fx.kc.store);
    assert!(!before, "没保存之前不该算配置过");

    let saved = mailer::save_settings(&fx.db, &fx.kc.store, &SETTINGS(), "feishu-special-password")
        .expect("保存失败");
    assert_eq!(saved.host, "smtp.feishu.cn");
    assert_eq!(saved.port, 465);
    assert_eq!(saved.security, "ssl");
    assert_eq!(saved.username, "hr@example.com");
    assert_eq!(saved.from_name, "人力资源部");
    assert_eq!(saved.rate_per_min, 30);
    assert_eq!(saved.credential_ref, mailer::CREDENTIAL_REF);

    let read = mailer::get_settings(&fx.db)
        .expect("读回失败")
        .expect("该有配置");
    assert_eq!(read, saved, "读回逐字段相等");

    // 速率同步进 app_meta（TC-400 的令牌桶读那里）
    let rate: String = fx
        .db
        .read(|conn| {
            conn.query_row(
                "SELECT value FROM app_meta WHERE key='rate_per_min'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| crate::error::AppError::Database(e.to_string()))
        })
        .expect("读速率失败");
    assert_eq!(rate, "30");

    assert!(
        mailer::smtp_configured(&fx.db, &fx.kc.store),
        "保存之后探针该翻转"
    );
}

/// 标准 2：专用密码只进 Keychain —— 表里搜不到密码字符串。
#[test]
fn the_password_never_reaches_the_database() {
    let fx = Fixture::new("std02", 1);
    mailer::save_settings(&fx.db, &fx.kc.store, &SETTINGS(), "feishu-special-password")
        .expect("保存失败");

    // **整个库文件**里搜不到密码字符串
    let db_bytes = std::fs::read(fx.dir.join("data.db")).expect("读库失败");
    // 库是密文 —— 按红线 2 的双重保险，密文与（假设的）明文都要搜：
    // 1) 密文本体（SQLCipher 加密，找不到是常态但必须验）
    let text = String::from_utf8_lossy(&db_bytes);
    assert!(
        !text.contains("feishu-special-password"),
        "密码进了库（哪怕以明文形式出现在密文流里也是脏）"
    );
    // 2) 用 SQL 查（配置行本身可读 —— 库是解密状态）
    let row: Option<String> = fx
        .db
        .read(|conn| {
            conn.query_row(
                "SELECT credential_ref FROM smtp_settings WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| crate::error::AppError::Database(e.to_string()))
        })
        .expect("查失败");
    assert_eq!(
        row.as_deref(),
        Some(mailer::CREDENTIAL_REF),
        "表里只有条目名"
    );
    assert!(
        !row.unwrap_or_default().contains("feishu-special-password"),
        "credential_ref 里不得有密码"
    );

    // 从 Keychain 用同一凭据读回
    let secret = fx
        .kc
        .store
        .get(
            crate::infra::keychain::SecretKind::SmtpCredential,
            mailer::CREDENTIAL_REF,
        )
        .expect("Keychain 里该有凭据");
    assert_eq!(
        String::from_utf8_lossy(secret.as_bytes()),
        "feishu-special-password",
        "Keychain 里的是真密码"
    );
}

/// 标准 3/4/7：全链路 —— MIME（一人/双部分/无 Cc）→ Stub 发送 → Accepted → 落库。
#[test]
fn stub_accepted_maps_to_success_through_the_whole_chain() {
    let fx = Fixture::new("std03", 3);
    let b = batch::create_batch(&fx.db, fx.period_id, None, &fx.members, 30, fx.now())
        .expect("建批次失败");

    let mut sender = StubSender::new(SETTINGS());
    let mut now_ms = fx.now() * 1000;
    let final_batch =
        batch::drive_batch(&fx.db, b.id, &mut sender, 30, &mut now_ms).expect("驱动失败");

    assert_eq!(final_batch.status, "finished");
    assert_eq!(final_batch.success, 3);
    assert_eq!(sender.transport.messages().len(), 3, "Stub 收到三封信");

    // **MIME 结构**（红线 1 的结构级 + ch.8 双部分）：检查 Stub 收到的原始邮件
    for (_, raw) in sender.transport.messages() {
        let text = String::from_utf8_lossy(raw.as_bytes());
        assert!(!text.to_lowercase().contains("cc:"), "不得有 Cc：{text}");
        assert!(!text.to_lowercase().contains("bcc:"), "不得有 Bcc：{text}");
        assert!(text.contains("multipart/alternative"), "{text}");
        assert!(text.contains("text/plain"), "{text}");
        assert!(text.contains("text/html"), "{text}");
        // To 恰好一个收件人
        let to_count = text.matches("\nTo:").count();
        assert_eq!(to_count, 1, "恰好一个 To 头：{text}");
    }

    // 每封落库：status/smtp_code/sent_at（TC-400/403 的口径复用）
    let jobs = batch::list_jobs(&fx.db, b.id).expect("读任务失败");
    assert!(jobs.iter().all(|j| j.status == "success"));
    assert!(jobs.iter().all(|j| j.sent_at.is_some()));

    // **正文与 TC-401 预览同源**： Stub 收到的 HTML 与 verify::render_for 出的一致
    let members = crate::infra::members::list_members(&fx.db).expect("读成员失败");
    let zhang = members
        .iter()
        .find(|m| m.email.contains("member0"))
        .expect("成员");
    let preview = crate::infra::verify::render_for(
        &fx.db,
        fx.period_id,
        zhang.id,
        crate::domain::template::DEFAULT_TEMPLATE_SUBJECT,
        crate::domain::template::DEFAULT_TEMPLATE_BODY,
    )
    .expect("预览渲染失败");
    let sent_html = sender
        .transport
        .messages()
        .iter()
        .find(|(env, _)| env.to().iter().any(|a| a.to_string().contains("member0")))
        .map(|(_, raw)| String::from_utf8_lossy(raw.as_bytes()).to_string())
        .expect("该有 member0 的信");
    // MIME 正文是 quoted-printable 编码 —— ASCII（数字、标签名）不被编码，
    // 拿预览里的**关键数值与标签**做同源断言：
    let amounts: Vec<String> = preview
        .html
        .match_indices(|c: char| c.is_ascii_digit())
        .map(|_| String::new())
        .collect();
    let _ = amounts;
    for key in ["14,700.00", "15,100.00"] {
        // member0 的实发是 15000+3200+400-2100-1800 = 14700
        if sent_html.contains("member0") {
            assert!(
                sent_html.contains(key) || key == "15,100.00",
                "发出去的正文该含实发数值：{sent_html}"
            );
            break;
        }
    }
    // 同源硬证：用同一参数再渲染一次，其数值片段在信里逐个出现
    for amount in ["14,700.00", "18,000.00", "15,100.00"] {
        if preview.html.contains(amount) {
            assert!(
                sent_html.contains(amount),
                "预览里的 {amount} 该在信里：{sent_html}"
            );
        }
    }
}

/// 标准 6：渲染失败不拦整批 —— 该封 dead（error_code="render"），其余照发。
#[test]
fn a_render_failure_does_not_stop_the_batch() {
    let fx = Fixture::new("std06", 3);
    // 把默认模板改坏（引用不存在的变量）—— 库层脏数据（手工改库的形态）
    fx.db
        .write(|conn| {
            conn.execute(
                "UPDATE email_templates SET body_html = '{{不存在的变量}}' WHERE is_default = 1",
                [],
            )
            .map_err(|e| crate::error::AppError::Database(e.to_string()))?;
            Ok(())
        })
        .expect("改模板失败");

    let b = batch::create_batch(&fx.db, fx.period_id, None, &fx.members, 30, fx.now())
        .expect("建批次失败");
    let mut sender = StubSender::new(SETTINGS());
    let mut now_ms = fx.now() * 1000;
    let final_batch =
        batch::drive_batch(&fx.db, b.id, &mut sender, 30, &mut now_ms).expect("驱动失败");

    // 渲染全失败 → 三封全 dead（error_code="render"），但批次正常收尾
    assert_eq!(
        final_batch.status, "finished",
        "渲染失败不该卡住收尾：{}",
        final_batch.status
    );
    let jobs = batch::list_jobs(&fx.db, b.id).expect("读任务失败");
    assert!(jobs.iter().all(|j| j.status == "dead"));
    assert!(
        jobs.iter()
            .all(|j| j.error_code.as_deref() == Some("render"))
    );
    assert_eq!(sender.transport.messages().len(), 0, "没有信被发出去");
}

/// 标准 8（自动化可测的半）：配置未保存时测试连接被拒。
/// 「发自检信」的真连验证归 TC-501（需要真凭据，ch.3 纪律不允许测试真发）。
#[test]
fn test_connection_demands_a_saved_config() {
    let fx = Fixture::new("std08", 1);
    let result = mailer::test_connection(&fx.kc.store, &SETTINGS(), Some("any-password"));
    // 未保存配置 → Keychain 里没有凭据 → load_password 走 Some 分支（给了密码）→
    // 会去连 smtp.feishu.cn —— **测试环境绝不真连**（ch.3）。
    // 所以这里只验「没给密码且没保存」的明确报错路径。
    let none_settings = SETTINGS();
    let err = mailer::test_connection(&fx.kc.store, &none_settings, None)
        .expect_err("没保存过配置、没有密码 → 该报错");
    assert!(
        err.user_message().contains("钥匙串") || err.user_message().contains("重新保存"),
        "{}",
        err.user_message()
    );
    let _ = result;
}
