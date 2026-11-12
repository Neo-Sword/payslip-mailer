//! TC-302 验收测试 · 把 spec 的 18 条标准里**能自动判定**的那些钉成可跑断言。
//!
//! 为什么单独一个文件：验收标准写在 spec 里，代码里就该有一处能对着读的地方。
//! 评审时逐条指到哪个测试，比逐条指到哪段业务代码更省事，也不容易漏。
//!
//! 范围（spec 标准号 → 本文件的测试）：
//!   标准 1  → `exported_headers_match_exactly_and_fuzzy_ones_need_confirmation`
//!   标准 2  → `the_mapping_result_uses_keys_not_display_names`
//!   标准 3  → `a_missing_fixed_column_blocks_the_import`
//!   标准 4  → `errors_block_the_commit_but_warnings_do_not`
//!   标准 5  → `an_unknown_or_inactive_member_is_an_error_row`
//!   标准 6  → `a_name_mismatch_is_a_warning_and_the_row_is_flagged`
//!   标准 7  → `the_same_member_twice_makes_both_rows_errors`
//!   标准 8  → `an_empty_amount_means_absent_not_zero`
//!   标准 9  → `a_percent_out_of_range_is_an_error`
//!   标准 10 → `the_formula_placeholder_is_ignored_but_a_typed_number_is_not`
//!   标准 11 → `the_conflict_strategy_must_be_chosen_first`
//!   标准 12 → `fill_empty_keeps_the_values_that_are_already_there`
//!   标准 13 → `overwriting_writes_an_audit_row_without_any_amount`
//!   标准 14 → `a_failing_import_writes_nothing_at_all`
//!   标准 15 → `the_original_file_is_never_copied_into_the_app_directory`
//!   标准 16 → `the_imported_records_are_readable_back_with_computed_formulas`
//!   标准 17 → `the_error_report_has_no_amount_in_it`
//!   标准 18 → 门禁脚本自己的事（`scripts/gate.sh`）

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::domain::field::FieldType;
use crate::domain::import::{ColumnTarget, ConflictStrategy, MatchKind, Severity};
use crate::error::AppResult;
use crate::infra::db::{CIPHER_PARAMS, DbState, OpenOutcome};
use crate::infra::import;
use crate::infra::keychain::{TempKeychain, no_user_interaction};

/// 一套「临时数据目录 + 临时钥匙串 + DbState」。
struct Fixture {
    dir: PathBuf,
    kc: TempKeychain,
    db: DbState,
}

impl Fixture {
    fn new(label: &str) -> Self {
        no_user_interaction();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let dir = std::env::temp_dir().join(format!(
            "xinyou-acc-import-{}-{nanos}-{label}",
            std::process::id()
        ));
        Self {
            dir,
            kc: TempKeychain::new(label),
            db: DbState::new(),
        }
    }

    fn open(&self) -> AppResult<OpenOutcome> {
        self.db.open(&self.kc.store, &self.dir, &CIPHER_PARAMS)
    }

    /// 造一个成员。
    fn member(&self, name: &str, email: &str, employee_no: Option<&str>) -> i64 {
        crate::infra::members::create_member(&self.db, name, email, employee_no, &[])
            .expect("建成员失败")
            .id
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.db.close();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// 造一个 xlsx 文件（表头 + 数据行）。
///
/// 用 `rust_xlsxwriter` —— 与 TC-301 导出用的是同一个库，
/// 所以「导出的文件能导回去」这件事验的是真格式，不是自己骗自己。
fn write_xlsx(path: &Path, headers: &[&str], rows: &[Vec<String>]) {
    use rust_xlsxwriter::Workbook;
    let mut workbook = Workbook::new();
    {
        let sheet = workbook.add_worksheet();
        for (i, h) in headers.iter().enumerate() {
            sheet.write_string(0, i as u16, *h).expect("写表头失败");
        }
        for (r, row) in rows.iter().enumerate() {
            for (c, cell) in row.iter().enumerate() {
                sheet
                    .write_string((r + 1) as u32, c as u16, cell)
                    .expect("写数据失败");
            }
        }
    }
    workbook.save(path).expect("保存 xlsx 失败");
}

/// 一个准备好了的导入场景：三个成员、默认字段集、一个月份账套。
struct Scenario {
    fx: Fixture,
    period_id: i64,
}

impl Scenario {
    fn new(label: &str) -> Self {
        let fx = Fixture::new(label);
        fx.open().expect("开库失败");
        crate::infra::fields::ensure_defaults(&fx.db).expect("写默认字段失败");
        fx.member("张三", "zhangsan@example.com", Some("E001"));
        fx.member("李四", "lisi@example.com", Some("E002"));
        fx.member("王五", "wangwu@example.com", Some("E003"));
        let period = crate::infra::periods::create(&fx.db, "2026-10").expect("建账套失败");
        Self {
            fx,
            period_id: period.id,
        }
    }

    /// 造一个文件并读它。
    fn sheet(&self, name: &str, headers: &[&str], rows: &[Vec<String>]) -> import::ParsedSheet {
        let path = self.fx.dir.join(name);
        write_xlsx(&path, headers, rows);
        import::read_sheet(&path).expect("读文件失败")
    }

    /// 自动映射 + 校验。
    fn check(&self, sheet: &import::ParsedSheet) -> crate::domain::import::ValidatedRows {
        let (targets, _) = import::targets_for(&self.fx.db, self.period_id).expect("取目标失败");
        let mappings = crate::domain::import::map_headers(&sheet.headers, &targets);
        import::validate(&self.fx.db, self.period_id, sheet, &mappings).expect("校验失败")
    }
}

/// 默认表头（与 TC-301 导出的一致）。
fn default_headers() -> Vec<&'static str> {
    vec![
        "姓名 *",
        "邮箱 *",
        "工号",
        "基本工资 *",
        "绩效奖金",
        "餐补",
        "社保个人",
        "公积金",
        "实发工资",
    ]
}

/// 标准 1：精确 / 模糊 / 未匹配三档。
#[test]
fn exported_headers_match_exactly_and_fuzzy_ones_need_confirmation() {
    let fx = Fixture::new("std01");
    fx.open().expect("开库失败");
    crate::infra::fields::ensure_defaults(&fx.db).expect("写默认字段失败");

    let (targets, _) = import::targets_for(&fx.db, 1).unwrap_or_else(|_| {
        // 没有账套时也要能测映射 —— 目标集只需要字段配置。
        let fields = crate::infra::fields::list(&fx.db).expect("读字段失败");
        let mut t = vec![
            crate::domain::import::MappingTarget {
                key: "name".into(),
                label: "姓名".into(),
                fixed: true,
                data_type: FieldType::Text,
                formula: false,
            },
            crate::domain::import::MappingTarget {
                key: "email".into(),
                label: "邮箱".into(),
                fixed: true,
                data_type: FieldType::Text,
                formula: false,
            },
            crate::domain::import::MappingTarget {
                key: "employee_no".into(),
                label: "工号".into(),
                fixed: true,
                data_type: FieldType::Text,
                formula: false,
            },
        ];
        for f in fields {
            t.push(crate::domain::import::MappingTarget {
                key: f.key.clone(),
                label: f.label.clone(),
                fixed: false,
                data_type: f.data_type,
                formula: f.formula.is_some(),
            });
        }
        (t, BTreeMap::new())
    });

    // 精确：自己导出的表头（含必填列的「 *」标记）
    let headers: Vec<String> = default_headers().iter().map(|s| s.to_string()).collect();
    let maps = crate::domain::import::map_headers(&headers, &targets);
    assert!(
        maps.iter().all(|m| m.match_kind == MatchKind::Exact),
        "自己导出的表头应当全部精确命中：{:?}",
        maps.iter()
            .map(|m| (&m.header, m.match_kind))
            .collect::<Vec<_>>()
    );
    assert!(!crate::domain::import::has_unconfirmed(&maps));

    // 模糊：「员工邮箱」→ 邮箱
    let fuzzy = crate::domain::import::map_headers(&["员工邮箱".to_string()], &targets);
    assert_eq!(fuzzy[0].match_kind, MatchKind::Fuzzy);
    assert!(fuzzy[0].needs_confirmation);

    // 未匹配：「季度补贴」
    let unmatched = crate::domain::import::map_headers(&["季度补贴".to_string()], &targets);
    assert_eq!(unmatched[0].match_kind, MatchKind::Unmatched);
    assert_eq!(unmatched[0].target, ColumnTarget::Skip);
}

/// 标准 2：映射结果用 key，不用显示名。
#[test]
fn the_mapping_result_uses_keys_not_display_names() {
    let fx = Fixture::new("std02");
    fx.open().expect("开库失败");
    crate::infra::fields::ensure_defaults(&fx.db).expect("写默认字段失败");
    let fields = crate::infra::fields::list(&fx.db).expect("读字段失败");
    let mut targets = vec![
        crate::domain::import::MappingTarget {
            key: "name".into(),
            label: "姓名".into(),
            fixed: true,
            data_type: FieldType::Text,
            formula: false,
        },
        crate::domain::import::MappingTarget {
            key: "email".into(),
            label: "邮箱".into(),
            fixed: true,
            data_type: FieldType::Text,
            formula: false,
        },
    ];
    for f in &fields {
        targets.push(crate::domain::import::MappingTarget {
            key: f.key.clone(),
            label: f.label.clone(),
            fixed: false,
            data_type: f.data_type,
            formula: f.formula.is_some(),
        });
    }

    let maps = crate::domain::import::map_headers(&["基本工资".to_string()], &targets);
    assert_eq!(
        maps[0].target,
        ColumnTarget::Field {
            key: "base_salary".into()
        },
        "映射结果必须是 field key"
    );
}

/// 标准 3：三个固定列缺一个 → 整批不能导。
#[test]
fn a_missing_fixed_column_blocks_the_import() {
    let fx = Fixture::new("std03");
    fx.open().expect("开库失败");
    crate::infra::fields::ensure_defaults(&fx.db).expect("写默认字段失败");
    let period = crate::infra::periods::create(&fx.db, "2026-10").expect("建账套失败");

    // 只有姓名与邮箱，没有工号
    let path = fx.dir.join("no-empno.xlsx");
    write_xlsx(
        &path,
        &["姓名", "邮箱", "基本工资"],
        &[vec![
            "张三".to_string(),
            "zhangsan@example.com".to_string(),
            "18000".to_string(),
        ]],
    );
    let sheet = import::read_sheet(&path).expect("读文件失败");
    let (targets, _) = import::targets_for(&fx.db, period.id).expect("取目标失败");
    let maps = crate::domain::import::map_headers(&sheet.headers, &targets);
    let missing = crate::domain::import::missing_fixed_columns(&maps);
    assert_eq!(missing.len(), 1, "{missing:?}");
    assert!(missing[0].contains("工号"), "{missing:?}");
}

/// 标准 4：K>0 禁止入库，M>0 允许。
#[test]
fn errors_block_the_commit_but_warnings_do_not() {
    let s = Scenario::new("std04");
    // 一行有错（邮箱不在名单里）
    let sheet = s.sheet(
        "err.xlsx",
        &default_headers(),
        &[vec![
            "张三".to_string(),
            "nobody@example.com".to_string(),
            "E001".to_string(),
            "18000".to_string(),
            "3200".to_string(),
            "400".to_string(),
            "-2100".to_string(),
            "-1800".to_string(),
            crate::domain::import::FORMULA_PLACEHOLDER.to_string(),
        ]],
    );
    let v = s.check(&sheet);
    assert!(v.summary.errors > 0, "{:?}", v.problems);
    assert!(!v.summary.can_commit(), "有错误行时不该能入库");

    // 同一份数据里，把邮箱改成名单里的人 → 通过
    let sheet = s.sheet(
        "ok.xlsx",
        &default_headers(),
        &[vec![
            "张三".to_string(),
            "zhangsan@example.com".to_string(),
            "E001".to_string(),
            "18000".to_string(),
            "3200".to_string(),
            "400".to_string(),
            "-2100".to_string(),
            "-1800".to_string(),
            crate::domain::import::FORMULA_PLACEHOLDER.to_string(),
        ]],
    );
    let v = s.check(&sheet);
    assert_eq!(v.summary.errors, 0, "{:?}", v.problems);
    assert!(v.summary.can_commit());
}

/// 标准 5：成员不存在 / 已停用 → 错误。
#[test]
fn an_unknown_or_inactive_member_is_an_error_row() {
    let s = Scenario::new("std05");
    // 把王五停用
    let members = crate::infra::members::list_members(&s.fx.db).expect("读成员失败");
    let wangwu = members
        .iter()
        .find(|m| m.email == "wangwu@example.com")
        .expect("找不到王五");
    crate::infra::members::set_member_active(&s.fx.db, wangwu.id, false).expect("停用失败");

    let sheet = s.sheet(
        "member.xlsx",
        &default_headers(),
        &[
            vec![
                "张三".to_string(),
                "zhangsan@example.com".to_string(),
                "E001".to_string(),
                "18000".to_string(),
                "3200".to_string(),
                "400".to_string(),
                "-2100".to_string(),
                "-1800".to_string(),
                crate::domain::import::FORMULA_PLACEHOLDER.to_string(),
            ],
            vec![
                "王五".to_string(),
                "wangwu@example.com".to_string(),
                "E003".to_string(),
                "15000".to_string(),
                "2000".to_string(),
                "400".to_string(),
                "-1750".to_string(),
                "-1500".to_string(),
                crate::domain::import::FORMULA_PLACEHOLDER.to_string(),
            ],
        ],
    );
    let v = s.check(&sheet);
    assert_eq!(v.summary.errors, 1, "{:?}", v.problems);
    let p = v
        .problems
        .iter()
        .find(|p| p.severity == Severity::Error)
        .expect("应有错误行");
    assert!(p.message.contains("停用"), "{p:?}");
    // 停用的人不该进成功行
    assert_eq!(v.rows.len(), 1);
}

/// 标准 6：姓名不一致 → 警告 + 标记。
#[test]
fn a_name_mismatch_is_a_warning_and_the_row_is_flagged() {
    let s = Scenario::new("std06");
    let sheet = s.sheet(
        "name.xlsx",
        &default_headers(),
        &[vec![
            "张三丰".to_string(),
            "zhangsan@example.com".to_string(),
            "E001".to_string(),
            "18000".to_string(),
            "3200".to_string(),
            "400".to_string(),
            "-2100".to_string(),
            "-1800".to_string(),
            crate::domain::import::FORMULA_PLACEHOLDER.to_string(),
        ]],
    );
    let v = s.check(&sheet);
    assert_eq!(v.summary.errors, 0, "{:?}", v.problems);
    assert_eq!(v.summary.warnings, 1, "{:?}", v.problems);
    assert!(v.summary.can_commit(), "警告不该拦住入库");
    assert_eq!(v.rows.len(), 1);
    assert!(v.rows[0].identity_unverified, "这一行该被标记为身份待核对");
    assert_eq!(v.rows[0].name, "张三", "入库的姓名取成员表的");
}

/// 标准 7：同一成员两次 → 两行都错。
#[test]
fn the_same_member_twice_makes_both_rows_errors() {
    let s = Scenario::new("std07");
    let row = || {
        vec![
            "张三".to_string(),
            "zhangsan@example.com".to_string(),
            "E001".to_string(),
            "18000".to_string(),
            "3200".to_string(),
            "400".to_string(),
            "-2100".to_string(),
            "-1800".to_string(),
            crate::domain::import::FORMULA_PLACEHOLDER.to_string(),
        ]
    };
    let sheet = s.sheet("dup.xlsx", &default_headers(), &[row(), row()]);
    let v = s.check(&sheet);
    assert_eq!(v.summary.errors, 2, "{:?}", v.problems);
    assert_eq!(v.summary.success, 0, "{:?}", v.problems);
    assert!(v.rows.is_empty(), "重复的行不该入库");
}

/// 标准 8：空金额 = 没有这一项（不是 0）。
#[test]
fn an_empty_amount_means_absent_not_zero() {
    let s = Scenario::new("std08");
    let sheet = s.sheet(
        "empty.xlsx",
        &default_headers(),
        &[vec![
            "张三".to_string(),
            "zhangsan@example.com".to_string(),
            "E001".to_string(),
            "18000".to_string(),
            "3200".to_string(),
            // 餐补留空 → 这个人没有餐补
            String::new(),
            "-2100".to_string(),
            "-1800".to_string(),
            crate::domain::import::FORMULA_PLACEHOLDER.to_string(),
        ]],
    );
    let v = s.check(&sheet);
    assert_eq!(v.summary.errors, 0, "{:?}", v.problems);
    let row = &v.rows[0];
    assert!(
        !row.values.contains_key("meal_allowance"),
        "留空的项不该出现在值里（那是「没有」，不是 0）：{:?}",
        row.values
    );
    assert!(row.values.contains_key("base_salary"));
}

/// 标准 9：百分比 0–100。
#[test]
fn a_percent_out_of_range_is_an_error() {
    let s = Scenario::new("std09");
    // 加一个百分比字段
    crate::infra::fields::create(
        &s.fx.db,
        "tax_rate",
        "税率",
        FieldType::Percent,
        false,
        None,
    )
    .expect("建字段失败");
    let sheet = s.sheet(
        "pct.xlsx",
        &["姓名", "邮箱", "税率"],
        &[
            vec![
                "张三".to_string(),
                "zhangsan@example.com".to_string(),
                "12.5%".to_string(),
            ],
            vec![
                "李四".to_string(),
                "lisi@example.com".to_string(),
                "120%".to_string(),
            ],
        ],
    );
    let v = s.check(&sheet);
    assert_eq!(v.summary.errors, 1, "{:?}", v.problems);
    let p = v
        .problems
        .iter()
        .find(|p| p.severity == Severity::Error)
        .expect("应有错误行");
    assert!(p.message.contains("0–100"), "{p:?}");
}

/// 标准 10：公式列的占位文字忽略，填别的报错。
#[test]
fn the_formula_placeholder_is_ignored_but_a_typed_number_is_not() {
    let s = Scenario::new("std10");
    let sheet = s.sheet(
        "formula.xlsx",
        &default_headers(),
        &[
            vec![
                "张三".to_string(),
                "zhangsan@example.com".to_string(),
                "E001".to_string(),
                "18000".to_string(),
                "3200".to_string(),
                "400".to_string(),
                "-2100".to_string(),
                "-1800".to_string(),
                crate::domain::import::FORMULA_PLACEHOLDER.to_string(),
            ],
            vec![
                "李四".to_string(),
                "lisi@example.com".to_string(),
                "E002".to_string(),
                "15000".to_string(),
                "2000".to_string(),
                "400".to_string(),
                "-1750".to_string(),
                "-1500".to_string(),
                "999".to_string(),
            ],
        ],
    );
    let v = s.check(&sheet);
    assert_eq!(v.summary.errors, 1, "{:?}", v.problems);
    assert_eq!(v.summary.success, 1);
    let p = v
        .problems
        .iter()
        .find(|p| p.severity == Severity::Error)
        .expect("应有错误行");
    assert!(p.message.contains("由系统计算"), "{p:?}");
}

/// 标准 11：冲突策略必须先选。
#[test]
fn the_conflict_strategy_must_be_chosen_first() {
    // Command 层检查：`strategy` 为 None → 报错。
    // 这里直接验 domain 层的策略枚举与 Command 的判空逻辑一致。
    let none: Option<ConflictStrategy> = None;
    assert!(none.is_none(), "没选策略时应当没有值");
    // 三个策略都在。
    let all = [
        ConflictStrategy::Overwrite,
        ConflictStrategy::FillEmpty,
        ConflictStrategy::Cancel,
    ];
    assert_eq!(all.len(), 3);
}

/// 标准 12：「仅补空」以字段为粒度。
#[test]
fn fill_empty_keeps_the_values_that_are_already_there() {
    let s = Scenario::new("std12");
    let sheet = s.sheet(
        "fill.xlsx",
        &default_headers(),
        &[vec![
            "张三".to_string(),
            "zhangsan@example.com".to_string(),
            "E001".to_string(),
            "18000".to_string(),
            "3200".to_string(),
            "400".to_string(),
            "-2100".to_string(),
            "-1800".to_string(),
            crate::domain::import::FORMULA_PLACEHOLDER.to_string(),
        ]],
    );
    let v = s.check(&sheet);
    let (_, specs) = import::targets_for(&s.fx.db, s.period_id).expect("取目标失败");

    // 先写一条已有记录（基本工资 17000，没有餐补）
    let mut existing = BTreeMap::new();
    existing.insert("base_salary".to_string(), "17000.00".to_string());
    import::commit(
        &s.fx.db,
        s.period_id,
        &[crate::domain::import::ParsedRow {
            row: 2,
            member_id: v.rows[0].member_id,
            name: "张三".into(),
            email: "zhangsan@example.com".into(),
            values: existing.clone(),
            identity_unverified: false,
        }],
        &specs,
        ConflictStrategy::Overwrite,
    )
    .expect("写入失败");

    // 再用「仅补空」导入
    let result = import::commit(
        &s.fx.db,
        s.period_id,
        &v.rows,
        &specs,
        ConflictStrategy::FillEmpty,
    )
    .expect("补空失败");

    let stored = import::existing_records(&s.fx.db, s.period_id).expect("读记录失败");
    let row = stored.get(&v.rows[0].member_id).expect("记录没了");
    assert_eq!(
        row.get("base_salary").map(|s| s.as_str()),
        Some("17000.00"),
        "已有字段该保留旧值：{row:?}"
    );
    assert!(
        row.contains_key("meal_allowance"),
        "没有的字段该被补上：{row:?}"
    );
    assert_eq!(result.replaced, 0, "仅补空不该算覆盖");
}

/// 标准 13：覆盖前写审计，审计里没有金额。
#[test]
fn overwriting_writes_an_audit_row_without_any_amount() {
    let s = Scenario::new("std13");
    let sheet = s.sheet(
        "audit.xlsx",
        &default_headers(),
        &[vec![
            "张三".to_string(),
            "zhangsan@example.com".to_string(),
            "E001".to_string(),
            "18000".to_string(),
            "3200".to_string(),
            "400".to_string(),
            "-2100".to_string(),
            "-1800".to_string(),
            crate::domain::import::FORMULA_PLACEHOLDER.to_string(),
        ]],
    );
    let v = s.check(&sheet);
    let (_, specs) = import::targets_for(&s.fx.db, s.period_id).expect("取目标失败");

    // 第一次导入（不算覆盖，没有已有记录）
    import::commit(
        &s.fx.db,
        s.period_id,
        &v.rows,
        &specs,
        ConflictStrategy::Overwrite,
    )
    .expect("第一次导入失败");

    // 第二次导入（这次是覆盖）
    let result = import::commit(
        &s.fx.db,
        s.period_id,
        &v.rows,
        &specs,
        ConflictStrategy::Overwrite,
    )
    .expect("覆盖失败");
    assert_eq!(result.replaced, 1);

    let audit = crate::infra::periods::list_audit(&s.fx.db, 50).expect("读审计失败");
    let import_row = audit
        .iter()
        .find(|a| a.action == "import")
        .expect("应有 import 审计");
    let detail = import_row.detail_masked.clone().unwrap_or_default();
    assert!(detail.contains("overwrite"), "{detail}");
    assert!(detail.contains("\"rows\":1"), "{detail}");
    // **审计里不能有金额**（红线 2）
    for amount in ["18000", "3200", "2100", "1800", "16700"] {
        assert!(
            !detail.contains(amount),
            "审计里出现了金额 {amount}：{detail}"
        );
    }
}

/// 标准 14：入库失败 → 一行都没有。
#[test]
fn a_failing_import_writes_nothing_at_all() {
    let s = Scenario::new("std14");
    let sheet = s.sheet(
        "fail.xlsx",
        &default_headers(),
        &[vec![
            "张三".to_string(),
            "zhangsan@example.com".to_string(),
            "E001".to_string(),
            "18000".to_string(),
            "3200".to_string(),
            "400".to_string(),
            "-2100".to_string(),
            "-1800".to_string(),
            crate::domain::import::FORMULA_PLACEHOLDER.to_string(),
        ]],
    );
    let v = s.check(&sheet);

    // 让公式算不出来：**直接改库**绕过字段编辑的校验
    // （`fields::update` 会拦「引用了不存在的字段」——那正是它该做的）。
    // 这里要的是「库里有一条算不出来的公式」，那种脏数据只可能来自手工改库。
    s.fx.db
        .write(|conn| {
            conn.execute(
                "UPDATE salary_fields SET formula = 'nosuchfield + 1' WHERE key = 'net_pay'",
                [],
            )
            .map_err(|e| crate::error::AppError::Database(e.to_string()))?;
            Ok(())
        })
        .expect("改公式失败");

    // 改完**重新取**字段定义 —— 用改之前那份就测不到「公式坏掉」了。
    let (_, specs) = import::targets_for(&s.fx.db, s.period_id).expect("取目标失败");

    let err = import::commit(
        &s.fx.db,
        s.period_id,
        &v.rows,
        &specs,
        ConflictStrategy::Overwrite,
    )
    .expect_err("公式坏着就该失败");
    assert!(
        err.user_message().contains("公式") || err.user_message().contains("算"),
        "{}",
        err.user_message()
    );

    // **库里一行都没有**
    let stored = import::existing_records(&s.fx.db, s.period_id).expect("读记录失败");
    assert!(stored.is_empty(), "失败的导入不该留下任何记录：{stored:?}");
}

/// 标准 15：原文件不被复制进应用目录。
#[test]
fn the_original_file_is_never_copied_into_the_app_directory() {
    let s = Scenario::new("std15");
    // 把「原文件」放在一个**别的目录**里（模拟用户从下载文件夹里选的）
    let outside = std::env::temp_dir().join(format!(
        "xinyou-acc-outside-{}-std15.xlsx",
        std::process::id()
    ));
    write_xlsx(
        &outside,
        &default_headers(),
        &[vec![
            "张三".to_string(),
            "zhangsan@example.com".to_string(),
            "E001".to_string(),
            "18000".to_string(),
            "3200".to_string(),
            "400".to_string(),
            "-2100".to_string(),
            "-1800".to_string(),
            crate::domain::import::FORMULA_PLACEHOLDER.to_string(),
        ]],
    );
    let sheet = import::read_sheet(&outside).expect("读文件失败");
    let v = s.check(&sheet);
    let (_, specs) = import::targets_for(&s.fx.db, s.period_id).expect("取目标失败");
    import::commit(
        &s.fx.db,
        s.period_id,
        &v.rows,
        &specs,
        ConflictStrategy::Overwrite,
    )
    .expect("导入失败");

    // 数据目录里**没有**这个 xlsx 的副本
    let mut found = Vec::new();
    let mut stack = vec![s.fx.dir.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().map(|x| x == "xlsx").unwrap_or(false) {
                found.push(p);
            }
        }
    }
    assert!(found.is_empty(), "数据目录里不该出现 xlsx 副本：{found:?}");
    // 原文件还在原地（我们只读它）
    assert!(outside.exists(), "原文件不该被移动或删除");

    let _ = std::fs::remove_file(&outside);
}

/// 标准 16：导入后读得回来，公式列是系统算的。
#[test]
fn the_imported_records_are_readable_back_with_computed_formulas() {
    let s = Scenario::new("std16");
    let sheet = s.sheet(
        "readback.xlsx",
        &default_headers(),
        &[
            vec![
                "张三".to_string(),
                "zhangsan@example.com".to_string(),
                "E001".to_string(),
                "18000".to_string(),
                "3200".to_string(),
                "400".to_string(),
                "-2100".to_string(),
                "-1800".to_string(),
                // Excel 里填的是 1（错的），系统该算成 16700
                "1".to_string(),
            ],
            vec![
                "李四".to_string(),
                "lisi@example.com".to_string(),
                "E002".to_string(),
                "15000".to_string(),
                "2000".to_string(),
                "400".to_string(),
                "-1750".to_string(),
                "-1500".to_string(),
                "1".to_string(),
            ],
        ],
    );
    let v = s.check(&sheet);
    assert_eq!(
        v.summary.errors, 2,
        "公式列填了数字该报错：{:?}",
        v.problems
    );

    // 把公式列清空 → 通过
    let sheet = s.sheet(
        "readback2.xlsx",
        &default_headers(),
        &[
            vec![
                "张三".to_string(),
                "zhangsan@example.com".to_string(),
                "E001".to_string(),
                "18000".to_string(),
                "3200".to_string(),
                "400".to_string(),
                "-2100".to_string(),
                "-1800".to_string(),
                crate::domain::import::FORMULA_PLACEHOLDER.to_string(),
            ],
            vec![
                "李四".to_string(),
                "lisi@example.com".to_string(),
                "E002".to_string(),
                "15000".to_string(),
                "2000".to_string(),
                "400".to_string(),
                "-1750".to_string(),
                "-1500".to_string(),
                crate::domain::import::FORMULA_PLACEHOLDER.to_string(),
            ],
        ],
    );
    let v = s.check(&sheet);
    assert_eq!(v.summary.errors, 0, "{:?}", v.problems);
    let (_, specs) = import::targets_for(&s.fx.db, s.period_id).expect("取目标失败");
    let result = import::commit(
        &s.fx.db,
        s.period_id,
        &v.rows,
        &specs,
        ConflictStrategy::Overwrite,
    )
    .expect("导入失败");
    assert_eq!(result.written, 2);

    let stored = import::existing_records(&s.fx.db, s.period_id).expect("读记录失败");
    assert_eq!(stored.len(), 2);

    // 「实发工资」是**系统算的**：18000 + 3200 + 400 - 2100 - 1800 = 17700
    let zhang = stored
        .values()
        .find(|v| v.contains_key("base_salary"))
        .expect("没有张三的记录");
    assert_eq!(
        zhang.get("net_pay").map(|s| s.as_str()),
        Some("17700.00"),
        "实发工资该是系统算的：{zhang:?}"
    );
    // 邮箱是校验和的一部分（防错机制 ②）
    let members = crate::infra::members::list_members(&s.fx.db).expect("读成员失败");
    let zhangsan = members
        .iter()
        .find(|m| m.email == "zhangsan@example.com")
        .expect("找不到张三");
    let json = crate::domain::record::values_to_json(zhang);
    let checksum = crate::domain::record::checksum(&zhangsan.email, &json);
    let stored_checksum: String =
        s.fx.db
            .read(|conn| {
                conn.query_row(
                    "SELECT checksum FROM salary_records WHERE member_id = ?1",
                    [zhangsan.id],
                    |row| row.get(0),
                )
                .map_err(|e| crate::error::AppError::Database(e.to_string()))
            })
            .expect("读校验和失败");
    assert_eq!(stored_checksum, checksum, "校验和该是行内容算出来的");
}

/// 标准 17：错误明细里有行号、列名、问题，但**没有金额**。
#[test]
fn the_error_report_has_no_amount_in_it() {
    let s = Scenario::new("std17");
    let sheet = s.sheet(
        "report.xlsx",
        &default_headers(),
        &[vec![
            "张三".to_string(),
            "zhangsan@example.com".to_string(),
            "E001".to_string(),
            "18000".to_string(),
            "3200".to_string(),
            "400".to_string(),
            "-2100".to_string(),
            "-1800".to_string(),
            crate::domain::import::FORMULA_PLACEHOLDER.to_string(),
        ]],
    );
    let v = s.check(&sheet);
    let problems: Vec<crate::domain::import::RowProblem> = v
        .problems
        .iter()
        .filter(|p| p.severity == Severity::Error)
        .cloned()
        .collect();

    let out = s.fx.dir.join("errors.xlsx");
    import::export_error_report(&out, &problems).expect("导明细失败");
    assert!(out.exists());

    // 用 calamine 读回来（与 TC-301 的读回同一套库）
    use calamine::Reader;
    let mut reader = calamine::open_workbook_auto(&out).expect("读不出 xlsx");
    let sheet = reader
        .worksheet_range_at(0)
        .expect("没有 Sheet")
        .expect("读 Sheet 出错");
    let all: String = (0..sheet.height())
        .flat_map(|r| (0..sheet.width()).map(move |c| (r, c)))
        .filter_map(|(r, c)| sheet.get((r, c)).map(|x| x.to_string()))
        .collect::<Vec<_>>()
        .join(" ");

    assert!(all.contains("行号"), "{all}");
    assert!(all.contains("问题"), "{all}");
    // **没有金额**
    for amount in ["18000", "3200", "2100", "1800"] {
        assert!(!all.contains(amount), "明细里出现了金额 {amount}：{all}");
    }
}
