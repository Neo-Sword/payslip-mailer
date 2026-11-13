//! 工资记录的读写（TC-303 · ADR-0022）
//!
//! 这一屏（S07）的四件事：**列出**某月的记录、**单格保存**（整行重算公式列）、
//! **删除整行**、**整月合计**。
//!
//! 口径全部沿用已定的决策，这里不发明新的：
//! - 「空 = 没有这一项」（ADR-0021 决策 6）：单格保存把值清空 = 从 `values_json` 删键。
//! - 行求值走 [`crate::domain::record::evaluate_row`]（唯一权威，ADR-0022 决策 2）。
//! - 编辑权在**这里**再查一次（ADR-0022 决策 5）—— 前端的 `editable` 只是提示。
//! - checksum 每次保存后整行重算（ADR-0022 决策 7）。

use std::collections::BTreeMap;

use rusqlite::params;
use serde::Serialize;

use crate::domain::field::FieldType;
use crate::domain::period::PeriodStatus;
use crate::domain::record::{self, FieldSpec};
use crate::error::{AppError, AppResult};
use crate::infra::db::DbState;
use crate::infra::periods;

/// 表格里的一行。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordRow {
    pub member_id: i64,
    pub name: String,
    pub email: String,
    pub employee_no: Option<String>,
    /// 字段 key → 值（**含公式列** —— 库里存的是导入/上次保存时算好的）。
    /// 键不存在 = 这一项没有（ADR-0021 决策 6）。
    pub values: BTreeMap<String, String>,
    /// 校验列：空 = 通过；否则是第一条问题的说明。
    pub validation: String,
    /// 校验有没有问题（表头的「待处理」计数用它）。
    pub has_problem: bool,
    /// 这一行怎么来的：import / manual。
    pub source: String,
}

/// 整月合计（S07 表底的「本页合计」）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeriodTotals {
    /// 正数金额之和。
    pub earnings: f64,
    /// 负数金额之和（负数）。
    pub deductions: f64,
    /// 实发 = 应发 + 扣款。
    pub net: f64,
}

/// 一行校验。
///
/// **必填缺失 → 警告**（不拦保存，拦发送 —— TC-401 的范围过滤）；
/// **公式重算失败 → 错误**。这是编辑期能看到的两类问题，
/// 与 TC-302 导入期的三档对齐（ADR-0022 决策 3）。
fn validate_row(specs: &[FieldSpec], values: &BTreeMap<String, String>) -> Option<String> {
    // 先报错误（比警告要紧）：公式算不出来 = 这一行的钱是坏的。
    if record::evaluate_row(specs, values).is_err() {
        return Some("公式算不出来，这一行不能发送".into());
    }
    for spec in specs {
        if spec.is_formula() {
            // 公式列由系统算，永远有值，「实发工资缺失」这种话没有意义。
            continue;
        }
        if spec.required && !values.contains_key(&spec.key) {
            return Some(format!("{}缺失", spec.label));
        }
    }
    None
}

/// 列出某月的全部记录（拼上成员信息）。
pub fn list_records(db: &DbState, period_id: i64) -> AppResult<Vec<RecordRow>> {
    let specs = specs_for(db, period_id)?;
    db.read(|conn| {
        let mut stmt = conn
            .prepare(
                "SELECT r.member_id, m.name, m.email, m.employee_no, r.values_json, r.source \
                 FROM salary_records r \
                 JOIN members m ON m.id = r.member_id \
                 WHERE r.period_id = ?1 \
                 ORDER BY m.name, m.id",
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        let rows = stmt
            .query_map(params![period_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })
            .map_err(|e| AppError::Database(e.to_string()))?;
        let mut out = Vec::new();
        for row in rows {
            let (member_id, name, email, employee_no, json, source) =
                row.map_err(|e| AppError::Database(e.to_string()))?;
            let values = record::values_from_json(&json)
                .map_err(|e| AppError::Database(format!("记录读不懂：{e}")))?;
            let validation = validate_row(&specs, &values).unwrap_or_default();
            let has_problem = !validation.is_empty();
            out.push(RecordRow {
                member_id,
                name,
                email,
                employee_no,
                values,
                validation,
                has_problem,
                source,
            });
        }
        Ok(out)
    })
}

/// 单格保存（ADR-0022 决策 1、2）。
///
/// 改一格 → 更新 → **整行重算公式列** → 重算 checksum → 返回这一行的新状态。
/// `value` 为空 = 这一项没有（从 values 里删键，不是存空串）。
pub fn update_cell(
    db: &DbState,
    period_id: i64,
    member_id: i64,
    key: &str,
    value: &str,
) -> AppResult<RecordRow> {
    // ---- 编辑权：Command 层的关卡（ADR-0022 决策 5）----
    let period = periods::get(db, period_id)?;
    if !period.status.is_editable() {
        return Err(AppError::ValidationError(format!(
            "「{}」的账套已{}，不能修改工资数值。要纠错请先解锁账套。",
            period.year_month,
            match period.status {
                PeriodStatus::Sending => "进入发送中",
                PeriodStatus::Sent => "发送完成",
                PeriodStatus::Archived => "归档",
                _ => "锁定",
            }
        )));
    }

    // ---- 公式列不可编辑（ADR-0022 / spec 标准 4）----
    let specs = specs_for(db, period_id)?;
    let spec = specs
        .iter()
        .find(|s| s.key == key)
        .ok_or_else(|| AppError::NotFound("工资条项目".into()))?;
    if spec.is_formula() {
        return Err(AppError::ValidationError(format!(
            "「{}」由系统计算，不能直接编辑",
            spec.label
        )));
    }

    // ---- 规范化新值 ----
    let trimmed = value.trim().to_string();
    let normalized = if trimmed.is_empty() {
        None // 空 = 没有这一项
    } else {
        match spec.data_type {
            FieldType::Text => Some(trimmed.clone()),
            FieldType::Percent => {
                let v = crate::domain::import::parse_percent(&trimmed)
                    .map_err(AppError::ValidationError)?;
                v.map(|n| format!("{n:.2}"))
            }
            _ => {
                let v = crate::domain::import::parse_amount(&trimmed)
                    .map_err(AppError::ValidationError)?;
                v.map(|n| format!("{n:.2}"))
            }
        }
    };

    // ---- 读现有值，改这一格 ----
    let existing: Option<(String, String)> = db
        .read(|conn| {
            conn.query_row(
                "SELECT values_json, source FROM salary_records \
                 WHERE period_id = ?1 AND member_id = ?2",
                params![period_id, member_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .map_err(|_| AppError::NotFound("记录".into()))
        })
        .ok();
    let (mut values, source) = match existing {
        Some((json, source)) => (
            record::values_from_json(&json)
                .map_err(|e| AppError::Database(format!("记录读不懂：{e}")))?,
            source,
        ),
        None => (BTreeMap::new(), "manual".to_string()),
    };
    match normalized {
        Some(v) => {
            values.insert(key.to_string(), v);
        }
        None => {
            values.remove(key);
        }
    }

    // ---- 整行重算公式列（算不出来 = 拒绝保存，旧值保留）----
    let evaluated = record::evaluate_row(&specs, &values).map_err(AppError::ValidationError)?;
    let mut final_values = values.clone();
    for (k, v) in evaluated.computed {
        final_values.insert(k, v);
    }

    // ---- 写库 + 重算 checksum（同一事务）----
    let json = record::values_to_json(&final_values);
    let email = member_email(db, member_id)?;
    let checksum = record::checksum(&email, &json);
    let now = now_secs();
    db.write(|conn| {
        conn.execute(
            "INSERT INTO salary_records \
             (period_id, member_id, values_json, checksum, source, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6) \
             ON CONFLICT(period_id, member_id) DO UPDATE SET \
               values_json = excluded.values_json, \
               checksum = excluded.checksum, \
               updated_at = excluded.updated_at",
            params![period_id, member_id, json, checksum, source, now],
        )
        .map_err(|e| AppError::Database(format!("写工资记录失败：{e}")))?;
        Ok(())
    })?;

    // ---- 返回这一行的新状态 ----
    let validation = validate_row(&specs, &final_values);
    Ok(RecordRow {
        member_id,
        name: member_name(db, member_id)?,
        email,
        employee_no: member_employee_no(db, member_id)?,
        validation: validation.clone().unwrap_or_default(),
        has_problem: validation.is_some(),
        values: final_values,
        source,
    })
}

/// 删除整行（ADR-0022 决策 8）。
pub fn delete_record(db: &DbState, period_id: i64, member_id: i64) -> AppResult<()> {
    let period = periods::get(db, period_id)?;
    if !period.status.is_editable() {
        return Err(AppError::ValidationError(
            "这个月的账套已锁定，不能删除工资记录。要纠错请先解锁账套。".into(),
        ));
    }
    let deleted = db.write(|conn| {
        conn.execute(
            "DELETE FROM salary_records WHERE period_id = ?1 AND member_id = ?2",
            params![period_id, member_id],
        )
        .map_err(|e| AppError::Database(e.to_string()))
    })?;
    if deleted == 0 {
        return Err(AppError::NotFound("工资记录".into()));
    }
    Ok(())
}

/// 整月合计（ADR-0022 决策 4）。
///
/// **公式列不进合计**（行级公式列已经是和了，再加一遍实发就重复了）；
/// 实发 = 应发 + 扣款（与行级 `NET_PAY` 口径一致）。
pub fn totals(db: &DbState, period_id: i64) -> AppResult<PeriodTotals> {
    let specs = specs_for(db, period_id)?;
    let numeric_keys: Vec<&str> = specs
        .iter()
        .filter(|s| !s.is_formula() && s.data_type != FieldType::Text)
        .map(|s| s.key.as_str())
        .collect();

    let mut earnings = 0.0f64;
    let mut deductions = 0.0f64;
    for row in list_records(db, period_id)? {
        for key in &numeric_keys {
            let Some(raw) = row.values.get(*key) else {
                continue; // 这一项没有 → 不参与合计（不是 0，是跳过）
            };
            let Ok(v) = crate::util::normalize_fullwidth(raw.trim()).parse::<f64>() else {
                continue; // 坏值在 validate_row 里已经报过，这里不重复拦
            };
            if v >= 0.0 {
                earnings += v;
            } else {
                deductions += v;
            }
        }
    }
    Ok(PeriodTotals {
        earnings,
        deductions,
        net: earnings + deductions,
    })
}

/// 某个月的字段定义（`FieldSpec` 清单）。
///
/// 复用 `periods::display_fields`（draft 用当前配置、ready 起用快照）——
/// 与 TC-301 导出、TC-302 导入同一个口径。
fn specs_for(db: &DbState, period_id: i64) -> AppResult<Vec<FieldSpec>> {
    let period = periods::get(db, period_id)?;
    let fields = periods::display_fields(db, &period)?;
    Ok(fields
        .iter()
        .filter(|f| f.visible)
        .map(|f| FieldSpec {
            key: f.key.clone(),
            label: f.label.clone(),
            data_type: FieldType::parse(&f.data_type).unwrap_or(FieldType::Text),
            formula: f.formula.clone(),
            required: f.required,
        })
        .collect())
}

/// 成员的邮箱（checksum 要用）。
fn member_email(db: &DbState, member_id: i64) -> AppResult<String> {
    db.read(|conn| {
        conn.query_row(
            "SELECT email FROM members WHERE id = ?1",
            params![member_id],
            |row| row.get::<_, String>(0),
        )
        .map_err(|_| AppError::NotFound("成员".into()))
    })
}

/// 成员的姓名。
fn member_name(db: &DbState, member_id: i64) -> AppResult<String> {
    db.read(|conn| {
        conn.query_row(
            "SELECT name FROM members WHERE id = ?1",
            params![member_id],
            |row| row.get::<_, String>(0),
        )
        .map_err(|_| AppError::NotFound("成员".into()))
    })
}

/// 成员的工号。
fn member_employee_no(db: &DbState, member_id: i64) -> AppResult<Option<String>> {
    db.read(|conn| {
        conn.query_row(
            "SELECT employee_no FROM members WHERE id = ?1",
            params![member_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .map_err(|_| AppError::NotFound("成员".into()))
    })
}

/// 墙钟 epoch 秒。
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::db::{CIPHER_PARAMS, DbState};
    use crate::infra::fields;
    use crate::infra::keychain::{TempKeychain, no_user_interaction};
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct Fx {
        dir: PathBuf,
        // TempKeychain 留在作用域里让钥匙串活着；没人读它
        #[allow(dead_code)]
        kc: TempKeychain,
        db: DbState,
        period_id: i64,
        zhang: i64,
        li: i64,
    }

    impl Fx {
        fn new(label: &str) -> Self {
            no_user_interaction();
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default();
            let dir = std::env::temp_dir().join(format!(
                "xinyou-records-test-{}-{nanos}-{label}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).expect("建临时目录失败");
            let kc = TempKeychain::new(label);
            let db = DbState::new();
            db.open(&kc.store, &dir, &CIPHER_PARAMS).expect("开库失败");
            fields::ensure_defaults(&db).expect("写默认字段失败");
            let zhang = crate::infra::members::create_member(
                &db,
                "张三",
                "zhangsan@example.com",
                Some("E001"),
                &[],
            )
            .expect("建成员失败")
            .id;
            let li = crate::infra::members::create_member(
                &db,
                "李四",
                "lisi@example.com",
                Some("E002"),
                &[],
            )
            .expect("建成员失败")
            .id;
            let period = periods::create(&db, "2026-10").expect("建账套失败");
            Self {
                dir,
                kc,
                db,
                period_id: period.id,
                zhang,
                li,
            }
        }

        /// 造一条已有记录（模拟导入的结果）。
        fn seed(&self, member_id: i64, values: &[(&str, &str)]) {
            let specs = specs_for(&self.db, self.period_id).expect("取 specs 失败");
            let map: BTreeMap<String, String> = values
                .iter()
                .map(|(k, v)| {
                    // 数值按导入路径同样的方式规范化（18000 → 18000.00）
                    let ty = specs
                        .iter()
                        .find(|s| s.key == *k)
                        .map(|s| s.data_type)
                        .unwrap_or(FieldType::Text);
                    let norm = match ty {
                        FieldType::Text => v.to_string(),
                        _ => crate::domain::import::parse_amount(v)
                            .expect("seed 的值该能解析")
                            .map(|n| format!("{n:.2}"))
                            .unwrap_or_default(),
                    };
                    (k.to_string(), norm)
                })
                .collect();
            let evaluated = record::evaluate_row(&specs, &map).expect("求值失败");
            let mut final_values = map;
            for (k, v) in evaluated.computed {
                final_values.insert(k, v);
            }
            let json = record::values_to_json(&final_values);
            let email = member_email(&self.db, member_id).expect("读邮箱失败");
            let checksum = record::checksum(&email, &json);
            self.db
                .write(|conn| {
                    conn.execute(
                        "INSERT INTO salary_records \
                         (period_id, member_id, values_json, checksum, source, created_at, updated_at) \
                         VALUES (?1, ?2, ?3, ?4, 'import', 0, 0)",
                        params![self.period_id, member_id, json, checksum],
                    )
                    .map_err(|e| AppError::Database(e.to_string()))?;
                    Ok(())
                })
                .expect("写记录失败");
        }
    }

    impl Drop for Fx {
        fn drop(&mut self) {
            self.db.close();
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// 标准 1：列出记录，公式列的值是库里存的。
    #[test]
    fn listing_shows_stored_rows_with_computed_values() {
        let fx = Fx::new("list");
        fx.seed(
            fx.zhang,
            &[
                ("base_salary", "18000"),
                ("performance", "3200"),
                ("meal_allowance", "400"),
                ("social_insurance", "-2100"),
                ("housing_fund", "-1800"),
            ],
        );
        let rows = list_records(&fx.db, fx.period_id).expect("读记录失败");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "张三");
        assert_eq!(rows[0].email, "zhangsan@example.com");
        assert_eq!(
            rows[0].values.get("net_pay").map(|s| s.as_str()),
            Some("17700.00")
        );
        assert_eq!(
            rows[0].validation, "",
            "这一行该是「通过」：{}",
            rows[0].validation
        );
        assert!(!rows[0].has_problem);
    }

    /// 标准 3：单格保存 → 公式列当场重算。
    #[test]
    fn editing_a_cell_recomputes_the_formula_column() {
        let fx = Fx::new("edit");
        fx.seed(
            fx.zhang,
            &[
                ("base_salary", "18000"),
                ("performance", "3200"),
                ("meal_allowance", "400"),
                ("social_insurance", "-2100"),
                ("housing_fund", "-1800"),
            ],
        );
        let row =
            update_cell(&fx.db, fx.period_id, fx.zhang, "base_salary", "20000").expect("保存失败");
        assert_eq!(
            row.values.get("base_salary").map(|s| s.as_str()),
            Some("20000.00")
        );
        assert_eq!(
            row.values.get("net_pay").map(|s| s.as_str()),
            Some("19700.00"),
            "实发工资该跟着重算：{:?}",
            row.values
        );
        // 库里也是新值
        let rows = list_records(&fx.db, fx.period_id).expect("读回失败");
        assert_eq!(
            rows[0].values.get("net_pay").map(|s| s.as_str()),
            Some("19700.00")
        );
    }

    /// 标准 4：公式列不可编辑。
    #[test]
    fn the_formula_column_is_not_editable() {
        let fx = Fx::new("formula");
        fx.seed(fx.zhang, &[("base_salary", "18000")]);
        let err = update_cell(&fx.db, fx.period_id, fx.zhang, "net_pay", "999")
            .expect_err("公式列不该能改");
        assert!(
            err.user_message().contains("系统计算"),
            "{}",
            err.user_message()
        );
    }

    /// 标准 5：公式算不出来 → 拒绝保存，旧值保留。
    #[test]
    fn a_value_that_breaks_the_formula_is_rejected_and_the_old_value_stays() {
        let fx = Fx::new("break");
        fx.seed(
            fx.zhang,
            &[
                ("base_salary", "18000"),
                ("performance", "3200"),
                ("meal_allowance", "400"),
                ("social_insurance", "-2100"),
                ("housing_fund", "-1800"),
            ],
        );
        // 文本字段没有公式问题；这里用「金额格填了不是数字的东西」触发拒绝
        let err = update_cell(&fx.db, fx.period_id, fx.zhang, "base_salary", "一千八")
            .expect_err("坏值该被拒");
        assert!(
            err.user_message().contains("数字"),
            "{}",
            err.user_message()
        );
        // 旧值还在
        let rows = list_records(&fx.db, fx.period_id).expect("读回失败");
        assert_eq!(
            rows[0].values.get("base_salary").map(|s| s.as_str()),
            Some("18000.00")
        );
    }

    /// 标准 6：必填缺失 → 警告（校验列），保存仍然成功。
    #[test]
    fn a_missing_required_field_warns_but_still_saves() {
        let fx = Fx::new("required");
        fx.seed(
            fx.zhang,
            &[
                ("base_salary", "18000"),
                ("performance", "3200"),
                ("meal_allowance", "400"),
                ("social_insurance", "-2100"),
                ("housing_fund", "-1800"),
            ],
        );
        // 把基本工资清空（必填）→ 警告，不拦保存
        let row = update_cell(&fx.db, fx.period_id, fx.zhang, "base_salary", "").expect("保存失败");
        assert!(
            row.validation.contains("基本工资缺失"),
            "校验列该报缺失：{}",
            row.validation
        );
        assert!(row.has_problem);
        // 值确实被清掉了（空 = 没有这一项）
        assert!(!row.values.contains_key("base_salary"));
    }

    /// 标准 7：表级合计，公式列不进合计。
    #[test]
    fn the_totals_do_not_double_count_formula_columns() {
        let fx = Fx::new("totals");
        fx.seed(
            fx.zhang,
            &[
                ("base_salary", "18000"),
                ("performance", "3200"),
                ("meal_allowance", "400"),
                ("social_insurance", "-2100"),
                ("housing_fund", "-1800"),
            ],
        );
        fx.seed(
            fx.li,
            &[
                ("base_salary", "15000"),
                ("performance", "2000"),
                ("meal_allowance", "400"),
                ("social_insurance", "-1750"),
                ("housing_fund", "-1500"),
            ],
        );
        let t = totals(&fx.db, fx.period_id).expect("算合计失败");
        // 应发：(18000+3200+400) + (15000+2000+400) = 39000
        assert!((t.earnings - 39000.0).abs() < 0.01, "应发 {t:?}");
        assert!((t.deductions + 7150.0).abs() < 0.01, "扣款 {t:?}");
        assert!((t.net - 31850.0).abs() < 0.01, "实发 {t:?}");
        // 实发不是两行 net_pay 的和（21900.00 + 14150.00 = 36050）—— 那会重复计公式列
        assert!((t.net - 36050.0).abs() > 1.0, "实发不该重复计公式列：{t:?}");
    }

    /// 标准 8：编辑权在 Command 层再查一次。
    #[test]
    fn a_locked_period_refuses_edits_at_the_command_layer() {
        let fx = Fx::new("locked");
        fx.seed(fx.zhang, &[("base_salary", "18000")]);
        crate::infra::periods::transition(&fx.db, fx.period_id, PeriodStatus::Ready)
            .expect("进 ready 失败");
        // ready 仍可编辑（TC-300 的口径）
        update_cell(&fx.db, fx.period_id, fx.zhang, "base_salary", "19000").expect("ready 该能改");
        crate::infra::periods::transition(&fx.db, fx.period_id, PeriodStatus::Sending)
            .expect("进 sending 失败");
        let err = update_cell(&fx.db, fx.period_id, fx.zhang, "base_salary", "20000")
            .expect_err("sending 不该能改");
        assert!(
            err.user_message().contains("发送中"),
            "{}",
            err.user_message()
        );
        // 删除也一样被拒
        let err = delete_record(&fx.db, fx.period_id, fx.zhang).expect_err("不该能删");
        assert!(
            err.user_message().contains("锁定"),
            "{}",
            err.user_message()
        );
        // 解锁（sent → draft）后又能改。sending → sent → unlock：
        crate::infra::periods::transition(&fx.db, fx.period_id, PeriodStatus::Sent)
            .expect("进 sent 失败");
        crate::infra::periods::unlock(&fx.db, fx.period_id).expect("解锁失败");
        update_cell(&fx.db, fx.period_id, fx.zhang, "base_salary", "20000").expect("解锁后该能改");
    }

    /// 标准 9：删除整行只删那行。
    #[test]
    fn deleting_a_row_leaves_the_others_alone() {
        let fx = Fx::new("delete");
        fx.seed(fx.zhang, &[("base_salary", "18000")]);
        fx.seed(fx.li, &[("base_salary", "15000")]);
        delete_record(&fx.db, fx.period_id, fx.zhang).expect("删除失败");
        let rows = list_records(&fx.db, fx.period_id).expect("读回失败");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].member_id, fx.li);
    }

    /// 标准 10：保存后 checksum 被整行重算。
    #[test]
    fn saving_recomputes_the_checksum() {
        let fx = Fx::new("checksum");
        fx.seed(
            fx.zhang,
            &[
                ("base_salary", "18000"),
                ("performance", "3200"),
                ("meal_allowance", "400"),
                ("social_insurance", "-2100"),
                ("housing_fund", "-1800"),
            ],
        );
        let before: String = fx
            .db
            .read(|conn| {
                conn.query_row(
                    "SELECT checksum FROM salary_records WHERE member_id = ?1",
                    params![fx.zhang],
                    |row| row.get(0),
                )
                .map_err(|e| AppError::Database(e.to_string()))
            })
            .expect("读 checksum 失败");

        update_cell(&fx.db, fx.period_id, fx.zhang, "base_salary", "20000").expect("保存失败");

        let rows = list_records(&fx.db, fx.period_id).expect("读回失败");
        let json = record::values_to_json(&rows[0].values);
        let expected = record::checksum(&rows[0].email, &json);
        let after: String = fx
            .db
            .read(|conn| {
                conn.query_row(
                    "SELECT checksum FROM salary_records WHERE member_id = ?1",
                    params![fx.zhang],
                    |row| row.get(0),
                )
                .map_err(|e| AppError::Database(e.to_string()))
            })
            .expect("读 checksum 失败");
        assert_ne!(before, after, "改了值 checksum 不该不变");
        assert_eq!(after, expected, "checksum 该是整行内容算出来的");
    }

    /// 标准 11：逐格编辑不写 audit_log。
    #[test]
    fn cell_edits_write_no_audit_rows() {
        let fx = Fx::new("audit");
        fx.seed(fx.zhang, &[("base_salary", "18000")]);
        let before = audit_count(&fx.db);
        update_cell(&fx.db, fx.period_id, fx.zhang, "base_salary", "19000").expect("保存失败");
        update_cell(&fx.db, fx.period_id, fx.zhang, "performance", "3300").expect("保存失败");
        update_cell(&fx.db, fx.period_id, fx.zhang, "meal_allowance", "500").expect("保存失败");
        assert_eq!(audit_count(&fx.db), before, "逐格编辑不该写审计");
    }

    /// 标准 12：单格保存只动那一行。
    #[test]
    fn saving_one_cell_leaves_other_rows_untouched() {
        let fx = Fx::new("isolate");
        fx.seed(
            fx.zhang,
            &[
                ("base_salary", "18000"),
                ("performance", "3200"),
                ("meal_allowance", "400"),
                ("social_insurance", "-2100"),
                ("housing_fund", "-1800"),
            ],
        );
        fx.seed(fx.li, &[("base_salary", "15000")]);
        let li_before = raw_json(&fx.db, fx.li);

        update_cell(&fx.db, fx.period_id, fx.zhang, "base_salary", "20000").expect("保存失败");

        let li_after = raw_json(&fx.db, fx.li);
        assert_eq!(li_before, li_after, "李四的行不该被动过");
    }

    /// 标准 2 的另一半：空月份没有记录行。
    #[test]
    fn an_empty_period_lists_no_rows() {
        let fx = Fx::new("empty");
        let rows = list_records(&fx.db, fx.period_id).expect("读记录失败");
        assert!(rows.is_empty());
        let t = totals(&fx.db, fx.period_id).expect("算合计失败");
        assert_eq!(t.earnings, 0.0);
        assert_eq!(t.deductions, 0.0);
        assert_eq!(t.net, 0.0);
    }

    // ---- 小工具 ----

    fn audit_count(db: &DbState) -> i64 {
        db.read(|conn| {
            conn.query_row("SELECT count(*) FROM audit_log", [], |row| row.get(0))
                .map_err(|e| AppError::Database(e.to_string()))
        })
        .expect("数审计失败")
    }

    fn raw_json(db: &DbState, member_id: i64) -> String {
        db.read(|conn| {
            conn.query_row(
                "SELECT values_json || '|' || checksum FROM salary_records \
                 WHERE period_id = (SELECT id FROM salary_periods LIMIT 1) AND member_id = ?1",
                params![member_id],
                |row| row.get::<_, String>(0),
            )
            .map_err(|e| AppError::Database(e.to_string()))
        })
        .expect("读原始行失败")
    }
}
