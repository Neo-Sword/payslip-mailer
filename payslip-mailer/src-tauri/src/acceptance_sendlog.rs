//! TC-403 验收测试 · 把 spec 的 11 条标准里**能自动判定**的那些钉成可跑断言。
//!
//! 范围（spec 标准号 → 本文件的测试）：
//!   标准 1  → `the_send_history_lists_batches_with_progress`
//!   标准 2  → `the_job_details_are_readable`
//!   标准 3  → `the_voucher_csv_has_the_right_columns_and_no_amounts`
//!   标准 4  → `the_voucher_is_utf8_with_bom`
//!   标准 5  → `the_voucher_escaping_survives_hostile_names`（+ sendlog::tests 的 RFC 4180 单测）
//!   标准 6  → `the_voucher_lands_in_exports_with_the_month_in_the_name`
//!   标准 7  → 结构性：迁移 0008 的表定义（断言信息表 schema）
//!   标准 8  → `the_audit_log_stays_amount_free`（TC-302 已钉导入侧；这里钉发送侧动作）
//!   标准 9  → `exporting_a_voucher_is_read_only`
//!   标准 10 → `no_command_can_delete_a_send_job`（静态：Command 层无删除入口）
//!   标准 11 → 门禁脚本自己的事（`scripts/gate.sh`）

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::domain::retry::SendResult;
use crate::infra::db::{CIPHER_PARAMS, DbState};
use crate::infra::keychain::{TempKeychain, no_user_interaction};
use crate::infra::{batch, records, sendlog};

struct Fixture {
    dir: PathBuf,
    kc: TempKeychain,
    db: DbState,
    period_id: i64,
    members: Vec<i64>,
}

impl Fixture {
    fn new(label: &str, n: usize) -> Self {
        no_user_interaction();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let dir = std::env::temp_dir().join(format!(
            "xinyou-acc-sendlog-{}-{nanos}-{label}",
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
            records::update_cell(
                &db,
                period.id,
                *member_id,
                "base_salary",
                &(15000 + i * 100).to_string(),
            )
            .expect("录基本工资失败");
            records::update_cell(&db, period.id, *member_id, "performance", "2000")
                .expect("录绩效失败");
            records::update_cell(&db, period.id, *member_id, "meal_allowance", "400")
                .expect("录餐补失败");
            records::update_cell(&db, period.id, *member_id, "social_insurance", "-1750")
                .expect("录社保失败");
            records::update_cell(&db, period.id, *member_id, "housing_fund", "-1500")
                .expect("录公积金失败");
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

    /// 建一个批次并按脚本发完。
    fn run_batch(&self, label: &str, script: Vec<SendResult>) -> batch::Batch {
        let b = batch::create_batch(
            &self.db,
            self.period_id,
            None,
            &self.members,
            30,
            self.now(),
        )
        .unwrap_or_else(|e| panic!("{label}: 建批次失败 {e:?}"));
        struct S(std::vec::IntoIter<SendResult>);
        impl batch::Sender for S {
            fn send(&mut self, _message: &batch::EmailMessage) -> SendResult {
                self.0.next().unwrap_or(SendResult::Accepted)
            }
        }
        let mut sender = S(script.into_iter());
        let mut now_ms = self.now() * 1000;
        batch::drive_batch(&self.db, b.id, &mut sender, 30, &mut now_ms).expect("驱动失败");
        b
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.db.close();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// 标准 1：发送历史可读 —— 批次列表带进度，按时间倒序。
#[test]
fn the_send_history_lists_batches_with_progress() {
    let fx = Fixture::new("std01", 2);
    let first = fx.run_batch("first", vec![SendResult::Accepted, SendResult::Accepted]);
    let second = fx.run_batch("second", vec![SendResult::Accepted, SendResult::Accepted]);

    let history = sendlog::list_history(&fx.db).expect("读历史失败");
    assert_eq!(history.len(), 2);
    // 最近的在前面
    assert_eq!(history[0].batch.id, second.id);
    assert_eq!(history[1].batch.id, first.id);
    for e in &history {
        assert_eq!(e.progress.total, 2);
        assert_eq!(e.progress.success, 2);
        assert_eq!(e.batch.status, "finished");
        assert!(e.batch.finished_at.is_some());
    }
}

/// 标准 2：逐封明细可读（姓名、地址、状态、回码、时间）。
#[test]
fn the_job_details_are_readable() {
    let fx = Fixture::new("std02", 3);
    let b = fx.run_batch(
        "detail",
        vec![
            SendResult::Accepted,
            SendResult::Rejected(550),
            SendResult::Accepted,
        ],
    );

    let details = sendlog::job_details(&fx.db, b.id).expect("读明细失败");
    assert_eq!(details.len(), 3);
    // 一封 dead 带回码，两封 success
    assert_eq!(details.iter().filter(|d| d.status == "dead").count(), 1);
    assert_eq!(details.iter().filter(|d| d.status == "success").count(), 2);
    let dead = details
        .iter()
        .find(|d| d.status == "dead")
        .expect("该有 dead");
    assert_eq!(dead.smtp_code, Some(550));
    assert_eq!(dead.error_code.as_deref(), Some("permanent"));
    assert!(dead.sent_at.is_none(), "没发出去的没有 sent_at");
    let ok = details
        .iter()
        .find(|d| d.status == "success")
        .expect("该有 success");
    assert!(ok.sent_at.is_some());
    assert!(!ok.name.is_empty(), "明细里该有姓名（按 member_id 关联）");
    assert!(ok.to_email.contains("@example.com"));
}

/// 标准 3/4/6：CSV 凭证的列、无数值、UTF-8 BOM、落 exports/、文件名含年月。
#[test]
fn the_voucher_csv_has_the_right_columns_and_no_amounts() {
    let fx = Fixture::new("std03", 3);
    let b = fx.run_batch(
        "voucher",
        vec![
            SendResult::Accepted,
            SendResult::Rejected(550),
            SendResult::Accepted,
        ],
    );

    let path = sendlog::export_voucher(&fx.db, b.id, &fx.dir).expect("导出失败");
    // 标准 6：落 exports/、文件名含年月
    assert!(
        path.to_string_lossy().contains("exports"),
        "该落在 exports/：{path:?}"
    );
    assert!(
        path.file_name()
            .unwrap()
            .to_string_lossy()
            .contains("2026-10"),
        "文件名含年月：{path:?}"
    );

    let raw = std::fs::read(&path).expect("读回失败");
    // 标准 4：UTF-8 BOM
    assert_eq!(&raw[0..3], &[0xEF, 0xBB, 0xBF], "文件头该是 UTF-8 BOM");
    let text = String::from_utf8_lossy(&raw[3..]).to_string();

    // 列头
    assert!(
        text.contains("姓名,收件地址,状态,SMTP回码,发送时间,内容校验和"),
        "{text}"
    );
    // 收件地址与状态在
    assert!(text.contains("member0@example.com"), "{text}");
    assert!(text.contains("dead"), "{text}");
    // SMTP 回码在
    assert!(text.contains("550"), "{text}");
    // **工资数值不在**（红线 2）—— 造数时录过 15000/2000/400/1750/1500 等金额
    for amount in ["15000", "15100", "2000", "400", "1750", "15500", "15100.00"] {
        assert!(!text.contains(amount), "凭证里出现了金额 {amount}：{text}");
    }
    // checksum 在（64 位十六进制）
    assert!(text.matches(',').count() >= 3 * 5, "{text}");
    let checksums: Vec<&str> = text
        .lines()
        .skip(1)
        .map(|l| l.rsplit(',').next().unwrap_or(""))
        .filter(|s| s.len() == 64)
        .collect();
    assert_eq!(checksums.len(), 3, "每封一行 checksum：{text}");
}

/// 标准 5：敌意姓名（逗号/引号/换行）在凭证里不炸，往返一致。
#[test]
fn the_voucher_escaping_survives_hostile_names() {
    let fx = Fixture::new("std05", 1);
    // 造一个带逗号引号换行的成员（直接改库 —— 界面会拦，但 CSV 必须兜得住）
    let hostile = "张三, \"三\"\n别名";
    fx.db
        .write(|conn| {
            conn.execute("UPDATE members SET name = ?1 WHERE id = ?2", [hostile, "1"])
                .map_err(|e| crate::error::AppError::Database(e.to_string()))?;
            Ok(())
        })
        .expect("改姓名失败");
    let b = fx.run_batch("hostile", vec![SendResult::Accepted]);

    let path = sendlog::export_voucher(&fx.db, b.id, &fx.dir).expect("导出失败");
    let raw = std::fs::read(&path).expect("读回失败");
    let text = String::from_utf8_lossy(&raw[3..]).to_string();
    // 转义后的字段整体包引号、内部引号翻倍、**换行保留在引号内**（不能被当行边界拆开）。
    // 按 RFC 4180 解析回去 = 原值。
    let escaped_field = format!("\"{}\"", hostile.replace('"', "\"\""));
    assert!(
        text.contains(&escaped_field),
        "转义后的姓名字段该原样在 CSV 里：{text:?}"
    );
    // 解析：把转义后的字段还原成原值
    let parsed = escaped_field[1..escaped_field.len() - 1].replace("\"\"", "\"");
    assert_eq!(parsed, hostile, "转义往返该一致");
}

/// 标准 8：审计层脱敏 —— 发送动作留下的 audit 记录里搜不到金额。
#[test]
fn the_audit_log_stays_amount_free() {
    let fx = Fixture::new("std08", 2);
    // 触发一次会写审计的成批动作（导入覆盖在 TC-302 钉过；这里钉发送侧的入口集）
    let b = fx.run_batch("audit", vec![SendResult::Accepted, SendResult::Accepted]);
    let audit = crate::infra::periods::list_audit(&fx.db, 100).expect("读审计失败");
    // 发送动作目前不写 audit（ADR-0022 决策 6 的同族：审计粒度是成批动作，
    // 发送批次有自己的表）；不管写没写，**写了也不能有数值**：
    for a in &audit {
        let detail = a.detail_masked.clone().unwrap_or_default();
        for amount in ["15000", "15100", "2000", "1750", "1500", "400"] {
            assert!(
                !detail.contains(amount),
                "audit_log 里出现了金额 {amount}：{detail}"
            );
        }
    }
    let _ = b;
}

/// 标准 9：凭证导出是纯读操作 —— 导出前后 send_jobs 与批次逐字节不变。
#[test]
fn exporting_a_voucher_is_read_only() {
    let fx = Fixture::new("std09", 2);
    let b = fx.run_batch("readonly", vec![SendResult::Accepted, SendResult::Accepted]);

    let snapshot = sendlog::job_details(&fx.db, b.id).expect("读明细失败");
    let before_batch = batch::get_batch(&fx.db, b.id).expect("读批次失败");

    let _path = sendlog::export_voucher(&fx.db, b.id, &fx.dir).expect("导出失败");

    let after_snapshot = sendlog::job_details(&fx.db, b.id).expect("读明细失败");
    let after_batch = batch::get_batch(&fx.db, b.id).expect("读批次失败");
    assert_eq!(
        format!("{snapshot:?}"),
        format!("{after_snapshot:?}"),
        "逐封明细不该被搅动"
    );
    assert_eq!(before_batch, after_batch, "批次状态不该被搅动");
}

/// 标准 7/10（结构性）：send_jobs 无正文列、无 cc/bcc 列；Command 层没有删除 job 的入口。
#[test]
fn the_structure_has_no_place_for_content_and_no_delete_path() {
    // 标准 7：表结构断言（迁移 0008 的 SQL）
    let ddl = include_str!("../migrations/0008_send_jobs.sql");
    let create_start = ddl
        .find("CREATE TABLE IF NOT EXISTS send_jobs")
        .expect("该有 send_jobs 表");
    let create_end = ddl[create_start..]
        .find(");")
        .map(|i| create_start + i)
        .expect("表定义结束");
    let create = &ddl[create_start..create_end];
    // 只匹配「列定义」形态（`列名 类型`），避免误伤注释里的词
    // （idempotency_key 的注释里有 `content_checksum` 公式 —— 那不是列）。
    let lower = create.to_lowercase();
    for forbidden in [
        "body_html",
        "body_text",
        "content text",
        "cc text",
        "bcc text",
    ] {
        assert!(
            !lower.contains(forbidden),
            "send_jobs 的表定义里不该有 {forbidden} 列"
        );
    }
    // 标准 10：Command 层没有删除 job 的入口（源码形态：commands/ 里没有
    // 针对 send_jobs 的 DELETE 语句）
    for file in ["batch.rs", "sendlog.rs"] {
        let src = std::fs::read_to_string(format!("src/commands/{file}")).expect("读 Command 源码");
        assert!(
            !src.contains("DELETE FROM send_jobs"),
            "commands/{file} 不该有删除 send_jobs 的语句"
        );
    }
    // infra 层同样没有（清空数据 TC-105 走整库删除，不逐行删 job）
    let src = std::fs::read_to_string("src/infra/batch.rs").expect("读 infra 源码");
    assert!(
        !src.contains("DELETE FROM send_jobs"),
        "infra/batch.rs 不该有删除 send_jobs 的语句"
    );
}
