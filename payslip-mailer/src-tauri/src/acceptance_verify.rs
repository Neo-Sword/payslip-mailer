//! TC-401 验收测试 · 把 spec 的 11 条标准里**能自动判定**的那些钉成可跑断言。
//!
//! 范围（spec 标准号 → 本文件的测试）：
//!   标准 1  → `an_identity_unverified_row_is_blocked_from_the_scope`
//!   标准 2  → `an_inactive_member_is_blocked`
//!   标准 3  → `a_row_with_a_validation_problem_is_blocked`
//!   标准 4  → `an_empty_required_builtin_blocks_the_member`
//!   标准 5  → `a_missing_required_field_blocks_the_member`
//!   标准 6  → `an_empty_optional_variable_passes`
//!   标准 7  → `an_unknown_variable_blocks_the_member_at_the_engine`
//!   标准 8  → `the_checksum_comparison_works_both_ways`
//!   标准 9  → `the_preview_and_the_verification_share_one_render`
//!   标准 10 → `the_render_context_follows_the_declared_sources`
//!   标准 11 → 门禁脚本自己的事（`scripts/gate.sh`）

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::AppResult;
use crate::infra::db::{CIPHER_PARAMS, DbState};
use crate::infra::keychain::{TempKeychain, no_user_interaction};
use crate::infra::{records, verify};

/// 一套「临时数据目录 + 临时钥匙串 + DbState」。
struct Fixture {
    dir: PathBuf,
    kc: TempKeychain,
    db: DbState,
    period_id: i64,
}

/// 默认模板（TC-202 写入的「标准中文模板」）。
const SUBJECT: &str = crate::domain::template::DEFAULT_TEMPLATE_SUBJECT;
const BODY: &str = crate::domain::template::DEFAULT_TEMPLATE_BODY;

impl Fixture {
    fn new(label: &str) -> Self {
        no_user_interaction();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let dir = std::env::temp_dir().join(format!(
            "xinyou-acc-verify-{}-{nanos}-{label}",
            std::process::id()
        ));
        let kc = TempKeychain::new(label);
        let db = DbState::new();
        db.open(&kc.store, &dir, &CIPHER_PARAMS).expect("开库失败");
        crate::infra::fields::ensure_defaults(&db).expect("写默认字段失败");
        crate::infra::templates::ensure_defaults(&db).expect("写默认模板失败");
        let period = crate::infra::periods::create(&db, "2026-10").expect("建账套失败");
        Self {
            dir,
            kc,
            db,
            period_id: period.id,
        }
    }

    fn member(&self, name: &str, email: &str) -> i64 {
        crate::infra::members::create_member(&self.db, name, email, Some("E001"), &[])
            .expect("建成员失败")
            .id
    }

    /// 给一个人录入工资（走 TC-303 的单格保存 —— 与生产同一路径）。
    fn record_for(&self, member_id: i64, base: i64) {
        for (key, value) in [
            ("base_salary", base.to_string()),
            ("performance", "3200".to_string()),
            ("meal_allowance", "400".to_string()),
            ("social_insurance", "-2100".to_string()),
            ("housing_fund", "-1800".to_string()),
        ] {
            records::update_cell(&self.db, self.period_id, member_id, key, &value)
                .expect("录入失败");
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.db.close();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// 标准 1：身份未验证的行被拦（TC-302 的持久化缺口随本卡修复）。
#[test]
fn an_identity_unverified_row_is_blocked_from_the_scope() {
    let fx = Fixture::new("std01");
    // 直接造一条 identity_verified = 0 的记录（模拟 TC-302 导入时判出的情形）
    let zhang = fx.member("张三", "zhangsan@example.com");
    fx.record_for(zhang, 18000);
    fx.db
        .write(|conn| {
            conn.execute(
                "UPDATE salary_records SET identity_verified = 0 WHERE member_id = ?1",
                [zhang],
            )
            .map_err(|e| crate::error::AppError::Database(e.to_string()))?;
            Ok(())
        })
        .expect("改标记失败");

    let report =
        verify::verify_scope(&fx.db, fx.period_id, &[zhang], SUBJECT, BODY).expect("核对失败");
    assert!(!report.ok, "有身份存疑的行，范围不该通过");
    let blocked = report
        .members
        .iter()
        .find(|m| m.member_id == zhang)
        .expect("张三该在报告里");
    assert!(
        blocked
            .blocked_reasons
            .iter()
            .any(|r| r.contains("身份待核对")),
        "{:?}",
        blocked.blocked_reasons
    );
    assert!(!blocked.sendable);

    // 对照：身份正常的人通过（对照组也要配 org_name，否则被「公司名称是空的」拦 —— 那是标准 4 的事）
    let li = fx.member("李四", "lisi@example.com");
    fx.record_for(li, 15000);
    fx.db
        .write(|conn| {
            conn.execute(
                "INSERT INTO app_meta (key, value) VALUES ('org_name', '测试单位甲')                  ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [],
            )
            .map_err(|e| crate::error::AppError::Database(e.to_string()))?;
            Ok(())
        })
        .expect("写单位名称失败");
    let report =
        verify::verify_scope(&fx.db, fx.period_id, &[li], SUBJECT, BODY).expect("核对失败");
    assert!(
        report.ok,
        "李四该通过：blocked={:?} vars={:?}",
        report.members[0].blocked_reasons, report.members[0].variable_problems
    );
}

/// 标准 2：停用成员被拦。
#[test]
fn an_inactive_member_is_blocked() {
    let fx = Fixture::new("std02");
    let wang = fx.member("王五", "wangwu@example.com");
    fx.record_for(wang, 16000);
    crate::infra::members::set_member_active(&fx.db, wang, false).expect("停用失败");

    let report =
        verify::verify_scope(&fx.db, fx.period_id, &[wang], SUBJECT, BODY).expect("核对失败");
    let m = &report.members[0];
    assert!(
        m.blocked_reasons.iter().any(|r| r.contains("停用")),
        "{:?}",
        m.blocked_reasons
    );
}

/// 标准 3：校验列有问题的行被拦（必填缺失）。
#[test]
fn a_row_with_a_validation_problem_is_blocked() {
    let fx = Fixture::new("std03");
    let zhang = fx.member("张三", "zhangsan@example.com");
    fx.record_for(zhang, 18000);
    // 清空必填的基本工资 → 校验列「基本工资缺失」
    records::update_cell(&fx.db, fx.period_id, zhang, "base_salary", "").expect("清空失败");

    let report =
        verify::verify_scope(&fx.db, fx.period_id, &[zhang], SUBJECT, BODY).expect("核对失败");
    let m = &report.members[0];
    assert!(
        m.blocked_reasons.iter().any(|r| r.contains("基本工资")),
        "{:?}",
        m.blocked_reasons
    );
}

/// 标准 4：required 内置变量为空 → 拦（单位名称没配置 → 公司名称空）。
#[test]
fn an_empty_required_builtin_blocks_the_member() {
    let fx = Fixture::new("std04");
    let zhang = fx.member("张三", "zhangsan@example.com");
    fx.record_for(zhang, 18000);
    // **不设置** org_name → {{公司名称}} 是空的

    let report =
        verify::verify_scope(&fx.db, fx.period_id, &[zhang], SUBJECT, BODY).expect("核对失败");
    let m = &report.members[0];
    assert!(
        m.variable_problems.iter().any(|p| p.contains("公司名称")),
        "{:?}",
        m.variable_problems
    );
    assert!(!m.sendable);

    // 配上单位名称 → 通过
    fx.db
        .write(|conn| {
            conn.execute(
                "INSERT INTO app_meta (key, value) VALUES ('org_name', '测试单位甲') \
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [],
            )
            .map_err(|e| crate::error::AppError::Database(e.to_string()))?;
            Ok(())
        })
        .expect("写单位名称失败");
    let report =
        verify::verify_scope(&fx.db, fx.period_id, &[zhang], SUBJECT, BODY).expect("核对失败");
    assert!(
        report.members[0].sendable,
        "{:?}",
        report.members[0].variable_problems
    );
}

/// 标准 5：required 字段缺 → 拦。
#[test]
fn a_missing_required_field_blocks_the_member() {
    let fx = Fixture::new("std05");
    fx.db
        .write(|conn| {
            conn.execute(
                "INSERT INTO app_meta (key, value) VALUES ('org_name', '测试单位甲') \
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [],
            )
            .map_err(|e| crate::error::AppError::Database(e.to_string()))?;
            Ok(())
        })
        .expect("写单位名称失败");
    let zhang = fx.member("张三", "zhangsan@example.com");
    // 只录绩效，不录必填的基本工资 → 行是新建的，基本工资键不存在
    records::update_cell(&fx.db, fx.period_id, zhang, "performance", "3200").expect("录绩效失败");

    let report =
        verify::verify_scope(&fx.db, fx.period_id, &[zhang], SUBJECT, BODY).expect("核对失败");
    let m = &report.members[0];
    assert!(
        m.variable_problems.iter().any(|p| p.contains("基本工资")),
        "{:?}",
        m.variable_problems
    );
}

/// 标准 6：可选变量为空 → 放行（不属于任何组 → {{组名}} 空）。
#[test]
fn an_empty_optional_variable_passes() {
    let fx = Fixture::new("std06");
    fx.db
        .write(|conn| {
            conn.execute(
                "INSERT INTO app_meta (key, value) VALUES ('org_name', '测试单位甲') \
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [],
            )
            .map_err(|e| crate::error::AppError::Database(e.to_string()))?;
            Ok(())
        })
        .expect("写单位名称失败");
    let zhang = fx.member("张三", "zhangsan@example.com");
    fx.record_for(zhang, 18000);
    // 张三不属于任何组 → {{组名}} 空（模板里没写组名，但 context 是空的）
    let ctx = verify::context_for(&fx.db, fx.period_id, zhang).expect("取上下文失败");
    assert_eq!(
        ctx.builtins.get("组名").map(|s| s.as_str()),
        Some(""),
        "组名该是空"
    );

    // 模板里写上组名也能过（可选变量空 → 放行）
    let body = "{{姓名}} 您好：您的组是【{{组名}}】。";
    let report =
        verify::verify_scope(&fx.db, fx.period_id, &[zhang], SUBJECT, body).expect("核对失败");
    assert!(
        report.members[0].sendable,
        "可选变量空不该拦：{:?}",
        report.members[0].variable_problems
    );
}

/// 标准 7：模板引用未知变量 → 该行被拦（引擎级兜底）。
#[test]
fn an_unknown_variable_blocks_the_member_at_the_engine() {
    let fx = Fixture::new("std07");
    fx.db
        .write(|conn| {
            conn.execute(
                "INSERT INTO app_meta (key, value) VALUES ('org_name', '测试单位甲')",
                [],
            )
            .map_err(|e| crate::error::AppError::Database(e.to_string()))?;
            Ok(())
        })
        .expect("写单位名称失败");
    let zhang = fx.member("张三", "zhangsan@example.com");
    fx.record_for(zhang, 18000);

    let body = "{{姓名}} 实发 {{不存在的变量}}";
    let report =
        verify::verify_scope(&fx.db, fx.period_id, &[zhang], SUBJECT, body).expect("核对失败");
    let m = &report.members[0];
    assert!(!m.variable_problems.is_empty(), "未知变量该被拦");
    assert!(
        m.variable_problems[0].contains("不存在的变量") || m.variable_problems[0].contains("渲染"),
        "{:?}",
        m.variable_problems
    );
    // **渲染结果不存在** —— 绝不发出字面量
    assert!(m.preview.is_none() || !m.preview.as_ref().unwrap().html.contains("{{"));
}

/// 标准 8：checksum 比对两个方向都对。
#[test]
fn the_checksum_comparison_works_both_ways() {
    let fx = Fixture::new("std08");
    let zhang = fx.member("张三", "zhangsan@example.com");
    fx.record_for(zhang, 18000);

    let expected = verify::row_checksum(&fx.db, fx.period_id, zhang).expect("算 checksum 失败");
    assert!(
        verify::checksum_matches(&fx.db, fx.period_id, zhang, &expected).expect("比对失败"),
        "没改数据时该一致"
    );

    // 改一格 → 旧值比对失败（TC-402 发送前逐封比对的基础）
    records::update_cell(&fx.db, fx.period_id, zhang, "base_salary", "19000").expect("改值失败");
    assert!(
        !verify::checksum_matches(&fx.db, fx.period_id, zhang, &expected).expect("比对失败"),
        "数据改了，旧 checksum 不该再匹配"
    );
}

/// 标准 9：逐人预览与核对**同一份渲染**。
#[test]
fn the_preview_and_the_verification_share_one_render() {
    let fx = Fixture::new("std09");
    fx.db
        .write(|conn| {
            conn.execute(
                "INSERT INTO app_meta (key, value) VALUES ('org_name', '测试单位甲')",
                [],
            )
            .map_err(|e| crate::error::AppError::Database(e.to_string()))?;
            Ok(())
        })
        .expect("写单位名称失败");
    let zhang = fx.member("张三", "zhangsan@example.com");
    fx.record_for(zhang, 18000);

    let report =
        verify::verify_scope(&fx.db, fx.period_id, &[zhang], SUBJECT, BODY).expect("核对失败");
    let from_scope = report.members[0].preview.as_ref().expect("该有预览");
    let from_command = verify::verify_member(&fx.db, fx.period_id, zhang, SUBJECT, BODY)
        .expect("核对失败")
        .preview
        .expect("该有预览");

    assert_eq!(from_scope.html, from_command.html, "同一份渲染该逐字节相同");
    assert_eq!(from_scope.subject, from_command.subject);
    assert_eq!(from_scope.checksum, from_command.checksum);
    // 渲染出的正文里该有真的数值（不是模板字面量）
    assert!(from_scope.html.contains("18,000.00"), "{}", from_scope.html);
    assert!(!from_scope.html.contains("{{"), "正文里不该有模板字面量");
}

/// 标准 10：渲染上下文的口径。
#[test]
fn the_render_context_follows_the_declared_sources() {
    let fx = Fixture::new("std10");
    fx.db
        .write(|conn| {
            conn.execute(
                "INSERT INTO app_meta (key, value) VALUES ('org_name', '测试单位甲')",
                [],
            )
            .map_err(|e| crate::error::AppError::Database(e.to_string()))?;
            Ok(())
        })
        .expect("写单位名称失败");
    let zhang = fx.member("张三", "zhangsan@example.com");
    fx.record_for(zhang, 18000);
    let group = crate::infra::members::create_group(&fx.db, "研发组", None).expect("建组失败");
    crate::infra::members::set_member_groups(&fx.db, zhang, &[group.id]).expect("入组失败");

    let ctx = verify::context_for(&fx.db, fx.period_id, zhang).expect("取上下文失败");
    // 成员表
    assert_eq!(ctx.builtins.get("姓名").map(|s| s.as_str()), Some("张三"));
    assert_eq!(
        ctx.builtins.get("邮箱").map(|s| s.as_str()),
        Some("zhangsan@example.com")
    );
    // 账套
    assert_eq!(
        ctx.builtins.get("年月").map(|s| s.as_str()),
        Some("2026 年 10 月")
    );
    assert_eq!(ctx.builtins.get("年份").map(|s| s.as_str()), Some("2026"));
    assert_eq!(ctx.builtins.get("月份").map(|s| s.as_str()), Some("10"));
    // 设置
    assert_eq!(
        ctx.builtins.get("公司名称").map(|s| s.as_str()),
        Some("测试单位甲")
    );
    // 组
    assert_eq!(ctx.builtins.get("组名").map(|s| s.as_str()), Some("研发组"));
    // 汇总（18000+3200+400=21600 / -3900 / 17700），千分位格式化
    assert_eq!(
        ctx.builtins.get("应发合计").map(|s| s.as_str()),
        Some("21,600.00")
    );
    assert_eq!(
        ctx.builtins.get("扣款合计").map(|s| s.as_str()),
        Some("-3,900.00")
    );
    assert_eq!(
        ctx.builtins.get("实发工资").map(|s| s.as_str()),
        Some("17,700.00")
    );
    // 字段值（千分位）
    assert_eq!(
        ctx.fields.get("base_salary").map(|s| s.as_str()),
        Some("18,000.00")
    );
    assert_eq!(
        ctx.fields.get("net_pay").map(|s| s.as_str()),
        Some("17,700.00")
    );
    // 工资条表格是 HTML
    assert!(ctx.salary_table_html.contains("<table"));
    assert!(ctx.salary_table_html.contains("基本工资"));
}

/// 空 values 的求值兜底：`BTreeMap` 空袋走 `evaluate_row` 不炸（汇总 0）。
#[test]
fn an_empty_values_map_yields_zero_totals() {
    let specs: Vec<crate::domain::record::FieldSpec> = Vec::new();
    let values: BTreeMap<String, String> = BTreeMap::new();
    let ev = crate::domain::record::evaluate_row(&specs, &values).expect("求值失败");
    assert_eq!(ev.total_earnings, 0.0);
    assert!(ev.computed.is_empty());
    let _ = AppResult::<()>::Ok(());
}
