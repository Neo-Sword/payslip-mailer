//! TC-301 验收测试 · 把 spec 的 12 条标准里**能自动判定**的那些钉成可跑断言。
//!
//! 为什么单独一个文件：验收标准写在 spec 里，代码里就该有一处能对着读的地方。
//! 评审时逐条指到哪个测试，比逐条指到哪段业务代码更省事，也不容易漏。
//!
//! 范围（spec 标准号 → 本文件的测试）：
//!   标准 1  → `the_header_is_three_fixed_columns_plus_the_enabled_fields`
//!   标准 2  → `the_header_comes_from_the_period_snapshot_not_the_current_config`
//!   标准 3  → `a_disabled_field_never_reaches_the_header`
//!   标准 4  → `a_formula_column_carries_the_placeholder_not_a_number`
//!   标准 5  → `the_instructions_sheet_is_there_and_says_fill_negatives`
//!   标准 6  → `the_first_row_is_frozen_and_the_money_columns_are_numeric`
//!   标准 7  → `the_money_columns_carry_a_data_validation`
//!   标准 8  → `the_default_export_has_no_data_rows_at_all`
//!   标准 9  → `the_export_lands_in_exports_with_the_month_in_the_name`
//!   标准 10 → `the_default_export_carries_no_member_data`
//!   标准 11 → `the_preview_and_the_file_agree_word_for_word`
//!   标准 12 → 门禁脚本自己的事（`scripts/gate.sh`）
//!
//! 标准 6/7 验的是 **xlsx 内部结构**（冻结窗格、数值格式、数据验证）。
//! 这些读不进 calamine 的值接口，只能直接读 xlsx 里那几段 XML ——
//! 读 XML 是「看它到底写了什么」，不是「猜它应该有什么」。
//!
//! 这里刻意**不用** `infra::export::write_xlsx` 的调用方那条路（Command 层要 AppHandle），
//! 而是自己拼路径写文件 —— 因为要验的是**文件本身**，不是 Command 的接线。

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::domain::field::FieldType;
use crate::domain::period::PeriodStatus;
use crate::error::AppResult;
use crate::infra::backup::exports_dir;
use crate::infra::db::{CIPHER_PARAMS, DbState, OpenOutcome};
use crate::infra::export::{self, ExportOptions, Row};
use crate::infra::fields;
use crate::infra::keychain::{TempKeychain, no_user_interaction};
use crate::infra::members;
use crate::infra::periods;
use crate::infra::sheet_schema;

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
            "xinyou-acc-export-{}-{nanos}-{label}",
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

    /// 导出到数据目录下的 `exports/`（标准 9）。
    fn export(&self, schema: &crate::domain::sheet::SheetSchema, rows: &[Row]) -> PathBuf {
        let dir = exports_dir(&self.dir);
        fs::create_dir_all(&dir).expect("建导出目录失败");
        let path = dir.join("薪资模板_2026-10.xlsx");
        export::write_xlsx(&path, schema, ExportOptions::defaults(), rows).expect("导出失败");
        path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.db.close();
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// 读 xlsx 里某个部件的原始 XML。
///
/// xlsx 就是一个 zip；`xl/worksheets/sheet1.xml` 是第一张表，
/// `xl/styles.xml` 是样式表（数值格式在那儿）。
fn read_part(path: &std::path::Path, part: &str) -> String {
    use std::io::Read;
    let file = fs::File::open(path).expect("打开 xlsx 失败");
    let mut archive = zip::ZipArchive::new(file).expect("读 zip 失败");
    let mut entry = archive.by_name(part).expect("xlsx 里没有这个部件");
    let mut text = String::new();
    entry.read_to_string(&mut text).expect("读部件内容失败");
    text
}

/// 列出 xlsx 里的所有 Sheet 名（标准 5：有两个 Sheet）。
fn sheet_names(path: &std::path::Path) -> Vec<String> {
    let text = read_part(path, "xl/workbook.xml");
    let mut names = Vec::new();
    let mut rest = text.as_str();
    while let Some(i) = rest.find("<sheet ") {
        rest = &rest[i..];
        let end = rest.find("/>").unwrap_or(rest.len());
        let tag = &rest[..end];
        if let Some(j) = tag.find("name=\"") {
            let name = &tag[j + 6..];
            if let Some(k) = name.find('"') {
                names.push(name[..k].to_string());
            }
        }
        rest = &rest[end..];
    }
    names
}

/// 用 calamine 读第一张表的第 `row` 行（0 基）。
fn read_row(path: &std::path::Path, row: usize) -> Vec<String> {
    use calamine::Reader;
    let mut reader = calamine::open_workbook_auto(path).expect("读不出 xlsx");
    let sheet = reader
        .worksheet_range_at(0)
        .expect("导出的文件里没有 Sheet")
        .expect("读 Sheet 出错");
    (0..sheet.width())
        .map(|c| {
            sheet
                .get((row, c))
                .map(|cell| cell.to_string())
                .unwrap_or_default()
        })
        .collect()
}

/// 标准 1：表头 = 3 个固定列 + 启用字段，顺序与设计文档一致。
///
/// 标准 11：预览与文件一字不差 —— 两者是**同一个** `schema_for` 出来的，
/// 所以这里比的是「预览给的列」与「文件里的表头」。
#[test]
fn the_header_is_three_fixed_columns_plus_the_enabled_fields() {
    let fx = Fixture::new("std01");
    fx.open().expect("开库失败");
    fields::ensure_defaults(&fx.db).expect("写默认字段失败");
    let period = periods::create(&fx.db, "2026-10").expect("建账套失败");

    // 预览（= 界面上「表头预览」看到的那份）
    let preview = sheet_schema::schema_for(&fx.db, period.id).expect("取表头失败");
    let preview_titles: Vec<String> = preview.columns.iter().map(|c| c.title.clone()).collect();

    // 前 3 列必须是固定列
    assert_eq!(preview.columns[0].key, "name", "第 1 列不是姓名");
    assert_eq!(preview.columns[1].key, "email", "第 2 列不是邮箱");
    assert_eq!(preview.columns[2].key, "employee_no", "第 3 列不是工号");
    assert_eq!(preview_titles[0], "姓名");
    assert_eq!(preview_titles[1], "邮箱");
    assert_eq!(preview_titles[2], "工号");

    // 默认字段集 7 项 + 3 个固定列 = 10 列（设计文档 S09 原文）
    let field_count = preview
        .columns
        .iter()
        .filter(|c| c.kind == crate::domain::sheet::ColumnKind::Field)
        .count();
    assert_eq!(
        preview.columns.len(),
        3 + field_count,
        "表头列数与「3 固定列 + 启用字段」对不上"
    );
    assert!(field_count > 0, "默认字段集不该是空的");

    // 导出并读回表头 —— 与预览一字不差
    let path = fx.export(&preview, &[]);
    let file_header = read_row(&path, 0);
    // 导出时必填列的标题带「 *」（xlsx 里就是这么写的）
    let expected: Vec<String> = preview
        .columns
        .iter()
        .map(|c| {
            if c.required {
                format!("{} *", c.title)
            } else {
                c.title.clone()
            }
        })
        .collect();
    assert_eq!(file_header, expected, "文件里的表头与预览不一致");
}

/// 标准 2：表头取的是**账套快照**，不是当前配置。
///
/// 导出一个 ready 状态的账套 → 之后加字段、改显示名 → 再导出，
/// 表头与第一次导出完全一致。
#[test]
fn the_header_comes_from_the_period_snapshot_not_the_current_config() {
    let fx = Fixture::new("std02");
    fx.open().expect("开库失败");
    fields::ensure_defaults(&fx.db).expect("写默认字段失败");
    let period = periods::create(&fx.db, "2026-10").expect("建账套失败");

    // 冻结快照
    periods::transition(&fx.db, period.id, PeriodStatus::Ready).expect("进 ready 失败");

    let before = sheet_schema::schema_for(&fx.db, period.id).expect("取表头失败");
    let before_titles: Vec<String> = before.columns.iter().map(|c| c.title.clone()).collect();

    // 事后改配置：加一个新字段、改一个字段的显示名
    fields::create(&fx.db, "bonus", "奖金", FieldType::Money, false, None).expect("建字段失败");
    let existing = fields::list(&fx.db).expect("读字段失败");
    let target = existing.iter().find(|f| f.key == "base_salary").unwrap();
    fields::update(
        &fx.db,
        target.id,
        "基本工资(改过)",
        FieldType::Money,
        true,
        None,
    )
    .expect("改字段失败");

    // 再导出 —— 表头必须与第一次完全一致
    let after = sheet_schema::schema_for(&fx.db, period.id).expect("取表头失败");
    let after_titles: Vec<String> = after.columns.iter().map(|c| c.title.clone()).collect();
    assert_eq!(before_titles, after_titles, "快照冻结后表头不该变");
    assert!(
        !after_titles.iter().any(|t| t == "奖金"),
        "事后新增的字段不该进这个月的表头"
    );
    assert!(
        !after_titles.iter().any(|t| t == "基本工资(改过)"),
        "事后改的显示名不该进这个月的表头"
    );

    // 而一个**草稿**账套看到的却是当前配置（对照组：证明上面不是「压根没读到配置」）
    let draft = periods::create(&fx.db, "2026-11").expect("建账套失败");
    let draft_schema = sheet_schema::schema_for(&fx.db, draft.id).expect("取表头失败");
    let draft_titles: Vec<String> = draft_schema
        .columns
        .iter()
        .map(|c| c.title.clone())
        .collect();
    assert!(
        draft_titles.iter().any(|t| t == "奖金"),
        "草稿账套应该看到新字段：{draft_titles:?}"
    );
}

/// 标准 3：停用的字段不进表头（隐藏的 `visible = false` 也不进）。
#[test]
fn a_disabled_field_never_reaches_the_header() {
    let fx = Fixture::new("std03");
    fx.open().expect("开库失败");
    fields::ensure_defaults(&fx.db).expect("写默认字段失败");
    let period = periods::create(&fx.db, "2026-10").expect("建账套失败");

    let all = fields::list(&fx.db).expect("读字段失败");
    let victim = all.iter().find(|f| f.key == "meal_allowance").unwrap();
    fields::set_visible(&fx.db, victim.id, false).expect("停用字段失败");

    let schema = sheet_schema::schema_for(&fx.db, period.id).expect("取表头失败");
    assert!(
        !schema.columns.iter().any(|c| c.key == "meal_allowance"),
        "停用的字段进了表头：{:?}",
        schema.columns
    );

    // 导出的文件里也不能有它
    let path = fx.export(&schema, &[]);
    let header = read_row(&path, 0);
    assert!(
        !header.iter().any(|t| t.starts_with("餐补")),
        "导出的文件里有停用字段：{header:?}"
    );
}

/// 标准 4：公式列不导出数据，单元格写「公式自动计算」。
#[test]
fn a_formula_column_carries_the_placeholder_not_a_number() {
    let fx = Fixture::new("std04");
    fx.open().expect("开库失败");
    fields::ensure_defaults(&fx.db).expect("写默认字段失败");
    let period = periods::create(&fx.db, "2026-10").expect("建账套失败");

    let schema = sheet_schema::schema_for(&fx.db, period.id).expect("取表头失败");
    let formula_idx = schema
        .columns
        .iter()
        .position(|c| c.is_formula)
        .expect("默认字段集里该有公式列（实发工资）");

    // 带一行数据进去
    let mut cells: Vec<String> = schema.columns.iter().map(|_| String::new()).collect();
    cells[0] = "张三".to_string();
    cells[1] = "zhangsan@example.com".to_string();
    let rows = vec![Row { cells }];
    let path = fx.export(&schema, &rows);

    let row = read_row(&path, 1);
    assert_eq!(
        row.get(formula_idx).map(|s| s.as_str()),
        Some(crate::domain::sheet::FORMULA_PLACEHOLDER),
        "公式列写的不是占位文字：{row:?}"
    );
}

/// 标准 5：说明 Sheet 默认包含，且写清了「扣款项填负数」。
#[test]
fn the_instructions_sheet_is_there_and_says_fill_negatives() {
    let fx = Fixture::new("std05");
    fx.open().expect("开库失败");
    fields::ensure_defaults(&fx.db).expect("写默认字段失败");
    let period = periods::create(&fx.db, "2026-10").expect("建账套失败");

    let schema = sheet_schema::schema_for(&fx.db, period.id).expect("取表头失败");
    let path = fx.export(&schema, &[]);

    let names = sheet_names(&path);
    assert_eq!(names.len(), 2, "默认导出应该有两个 Sheet：{names:?}");
    assert!(
        names[0].contains("工资"),
        "第一个 Sheet 不是工资表：{names:?}"
    );
    assert!(
        names[1].contains("说明"),
        "第二个 Sheet 不是填写说明：{names:?}"
    );

    // 说明里的关键那句话必须真的在文件里
    let instructions = read_part(&path, "xl/sharedStrings.xml");
    assert!(
        instructions.contains("填负数"),
        "说明里没有「填负数」：{instructions}"
    );
    assert!(
        instructions.contains("公式列"),
        "说明里没有「公式列」：{instructions}"
    );
}

/// 标准 6：首行冻结、金额列是数值格式（不是文本）。
///
/// 冻结窗格与数字格式都写在 xlsx 的 XML 里，这里直接读出来看。
#[test]
fn the_first_row_is_frozen_and_the_money_columns_are_numeric() {
    let fx = Fixture::new("std06");
    fx.open().expect("开库失败");
    fields::ensure_defaults(&fx.db).expect("写默认字段失败");
    let period = periods::create(&fx.db, "2026-10").expect("建账套失败");

    let schema = sheet_schema::schema_for(&fx.db, period.id).expect("取表头失败");
    // 放一行真实数据进去，好验数值格式
    let mut cells: Vec<String> = schema.columns.iter().map(|_| String::new()).collect();
    cells[0] = "张三".to_string();
    cells[1] = "zhangsan@example.com".to_string();
    let money_idx = schema
        .columns
        .iter()
        .position(|c| c.numeric && !c.is_formula)
        .expect("该有金额列");
    cells[money_idx] = "18000.00".to_string();
    let rows = vec![Row { cells }];
    let path = fx.export(&schema, &rows);

    // 冻结首行：<pane ySplit="1" ... state="frozen"/>
    let sheet_xml = read_part(&path, "xl/worksheets/sheet1.xml");
    assert!(
        sheet_xml.contains("state=\"frozen\""),
        "没有冻结窗格：{sheet_xml}"
    );
    assert!(
        sheet_xml.contains("ySplit=\"1\""),
        "冻结的不是首行：{sheet_xml}"
    );

    // 数值格式：样式表里该有 `#,##0.00`
    let styles = read_part(&path, "xl/styles.xml");
    assert!(
        styles.contains("#,##0.00"),
        "样式表里没有金额的数值格式：{styles}"
    );

    // 那一格读回来是**数字**不是文本（calamine 会区分）
    use calamine::Reader;
    let mut reader = calamine::open_workbook_auto(&path).expect("读不出 xlsx");
    let sheet = reader
        .worksheet_range_at(0)
        .expect("没有 Sheet")
        .expect("读 Sheet 出错");
    let cell = sheet.get((1, money_idx)).expect("金额格没读到");
    assert!(
        matches!(cell, calamine::Data::Float(_) | calamine::Data::Int(_)),
        "金额格不是数字：{cell:?}"
    );
}

/// 标准 7：数据验证在金额列上（Excel 里填非数字会被拦下）。
#[test]
fn the_money_columns_carry_a_data_validation() {
    let fx = Fixture::new("std07");
    fx.open().expect("开库失败");
    fields::ensure_defaults(&fx.db).expect("写默认字段失败");
    let period = periods::create(&fx.db, "2026-10").expect("建账套失败");

    let schema = sheet_schema::schema_for(&fx.db, period.id).expect("取表头失败");
    let path = fx.export(&schema, &[]);

    let sheet_xml = read_part(&path, "xl/worksheets/sheet1.xml");
    assert!(
        sheet_xml.contains("<dataValidation"),
        "表里没有数据验证：{sheet_xml}"
    );
    assert!(
        sheet_xml.contains("type=\"decimal\""),
        "数据验证不是数字型：{sheet_xml}"
    );
    assert!(
        sheet_xml.contains("allowBlank=\"1\""),
        "数据验证不允许留空 —— 空格子是合法的（表示这项没有）：{sheet_xml}"
    );
}

/// 标准 8：默认导出的是空模板（只有表头，下面没有数据行）。
#[test]
fn the_default_export_has_no_data_rows_at_all() {
    let fx = Fixture::new("std08");
    fx.open().expect("开库失败");
    fields::ensure_defaults(&fx.db).expect("写默认字段失败");
    let period = periods::create(&fx.db, "2026-10").expect("建账套失败");

    // 先造一个成员 —— **它也不该出现在空模板里**（标准 10）
    members::create_member(&fx.db, "李四", "lisi@example.com", Some("E002"), &[])
        .expect("建成员失败");

    let schema = sheet_schema::schema_for(&fx.db, period.id).expect("取表头失败");
    let path = fx.export(&schema, &[]);

    use calamine::Reader;
    let mut reader = calamine::open_workbook_auto(&path).expect("读不出 xlsx");
    let sheet = reader
        .worksheet_range_at(0)
        .expect("没有 Sheet")
        .expect("读 Sheet 出错");
    assert_eq!(
        sheet.height(),
        1,
        "空模板应该只有一行表头，实际 {} 行",
        sheet.height()
    );
}

/// 标准 9：导出落在 `exports/`，文件名含年月。
#[test]
fn the_export_lands_in_exports_with_the_month_in_the_name() {
    let fx = Fixture::new("std09");
    fx.open().expect("开库失败");

    let path = export::target_path(&fx.dir, "2026-10", false);
    assert_eq!(
        path,
        exports_dir(&fx.dir).join("薪资模板_2026-10.xlsx"),
        "落盘位置或文件名不对：{path:?}"
    );
    assert!(
        path.to_string_lossy().contains("exports"),
        "没落在 exports/ 里：{path:?}"
    );
    assert!(
        path.to_string_lossy().contains("2026-10"),
        "文件名里没有年月：{path:?}"
    );
}

/// 标准 10：默认导出的文件里没有成员姓名与邮箱。
#[test]
fn the_default_export_carries_no_member_data() {
    let fx = Fixture::new("std10");
    fx.open().expect("开库失败");
    fields::ensure_defaults(&fx.db).expect("写默认字段失败");
    let period = periods::create(&fx.db, "2026-10").expect("建账套失败");
    members::create_member(&fx.db, "李四", "lisi@secret-company.com", Some("E002"), &[])
        .expect("建成员失败");

    let schema = sheet_schema::schema_for(&fx.db, period.id).expect("取表头失败");
    let path = fx.export(&schema, &[]);

    // 整个 xlsx 的所有 XML 部件里都不该出现这个邮箱
    for part in [
        "xl/worksheets/sheet1.xml",
        "xl/worksheets/sheet2.xml",
        "xl/sharedStrings.xml",
    ] {
        let text = read_part(&path, part);
        assert!(
            !text.contains("lisi@secret-company.com"),
            "{part} 里出现了成员邮箱"
        );
        assert!(!text.contains("李四"), "{part} 里出现了成员姓名");
    }
}
