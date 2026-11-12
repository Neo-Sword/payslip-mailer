//! Excel 导入的落地（TC-302 · ADR-0021）
//!
//! 三步：**读文件**（只读，不复制）→ **校验**（`domain::import`）→ **写库**（一个事务）。
//!
//! 两条硬约束：
//!
//! 1. **原文件只读打开、不复制**（设计文档 ch.9：「导入文件只读打开，不复制；
//!    解析完成后内存中数值立即写入加密库」）。
//!    这里用 `calamine` 直接读用户选的那个路径，**不把它拷进应用目录**。
//! 2. **入库是一个事务，失败整批回滚**（设计文档 S08：「入库为事务，失败整批回滚」）。
//!    覆盖策略的审计行也**在同一个事务里** —— 事务回滚则审计一起回滚，
//!    不会出现「审计说覆盖了、其实没覆盖」。

use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::params;

use crate::domain::field::FieldType;
use crate::domain::import::{
    self, ColumnMapping, ConflictStrategy, ImportSummary, MappingTarget, MemberRef, RawRow,
    ValidatedRows,
};
use crate::domain::record::{self, FieldSpec};
use crate::error::{AppError, AppResult};
use crate::infra::db::DbState;
use crate::infra::periods;

/// 一份读进来的表（表头 + 数据行）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSheet {
    /// 表头（第一行）。
    pub headers: Vec<String>,
    /// 数据行（不含表头）。每行是「列序 → 单元格文字」。
    pub rows: Vec<RawRow>,
    /// 表头行在 Excel 里的行号（1 基）。第一张表通常是 1。
    pub header_row: usize,
}

/// 读一个 Excel 文件（xlsx / xls / xlsb / ods）。
///
/// **只读打开**：`calamine` 只读不写，本函数**不拷贝**原文件（ch.9）。
///
/// 返回的每一行都带上它在 Excel 里的行号（**1 基、含表头行**），
/// 这样错误提示里的「第 12 行」与用户在 Excel 里看到的一致。
pub fn read_sheet(path: &Path) -> AppResult<ParsedSheet> {
    use calamine::Reader;

    let mut reader = calamine::open_workbook_auto(path).map_err(|e| {
        AppError::Io(format!(
            "读不了这个文件：{e}。请确认它是 .xlsx / .xls / .xlsb / .ods 格式，且没有损坏。"
        ))
    })?;

    // 取第一张表：工作簿可能有多个 Sheet，工资表总是第一张。
    // `worksheet_range_at` 返回 `Option<Result<Range, Error>>`：
    // 外层是「有没有这张表」，内层才是「读得对不对」。
    let range = reader
        .worksheet_range_at(0)
        .ok_or_else(|| AppError::Io("这个文件里没有工作表。".into()))?
        .map_err(|e| AppError::Io(format!("读第一张表失败：{e}")))?;

    // `start`/`end` 是**绝对坐标**（1 基的 Excel 行号 - 1），`get` 用**相对坐标**。
    let (start_row, start_col) = range
        .start()
        .ok_or_else(|| AppError::Io("这个工作表是空的。".into()))?;
    let (end_row, end_col) = range
        .end()
        .ok_or_else(|| AppError::Io("这个工作表是空的。".into()))?;
    let header_row = start_row as usize + 1;
    let width = range.width();

    let mut headers: Vec<String> = Vec::new();
    for c in 0..width {
        let cell = range.get((0, c));
        headers.push(cell.map(cell_text).unwrap_or_default());
    }

    // 去掉表头行末尾的空列（Excel 里多出来的空列不该变成「未匹配列」）。
    while headers.last().map(|h| h.trim().is_empty()).unwrap_or(false) {
        headers.pop();
    }
    let used_cols = headers.len();

    let mut rows: Vec<RawRow> = Vec::new();
    for r in 1..range.height() {
        let mut cells: BTreeMap<usize, String> = BTreeMap::new();
        let mut any = false;
        for c in 0..used_cols {
            let text = range.get((r, c)).map(cell_text).unwrap_or_default();
            if !text.trim().is_empty() {
                any = true;
            }
            cells.insert(c, text);
        }
        // 整行都是空的 → 跳过（Excel 结尾常有几百个空行）
        if !any {
            continue;
        }
        // 行号用**绝对坐标**（与 Excel 里显示的一致，1 基）。
        rows.push(RawRow {
            row: start_row as usize + r + 1,
            cells,
        });
    }
    let _ = (end_row, end_col, start_col);

    Ok(ParsedSheet {
        headers,
        rows,
        header_row,
    })
}

/// 单元格 → 文字。
fn cell_text(cell: &calamine::Data) -> String {
    use calamine::Data;
    match cell {
        Data::Empty => String::new(),
        Data::String(s) => s.clone(),
        Data::Float(v) => {
            // 整数别写成 "18000.0" —— 财务看到的是 "18000"
            if v.fract() == 0.0 && v.abs() < 1e15 {
                format!("{}", *v as i64)
            } else {
                v.to_string()
            }
        }
        Data::Int(v) => v.to_string(),
        Data::Bool(b) => b.to_string(),
        Data::DateTime(dt) => dt.to_string(),
        Data::DateTimeIso(s) => s.clone(),
        Data::DurationIso(s) => s.clone(),
        Data::Error(e) => e.to_string(),
    }
}

/// 导入的**目标集**：三个固定列 + 当月的启用字段。
///
/// **当月的列**从账套的快照/当前配置取（与 TC-301 导出同一口径，
/// ADR-0020 决策 2）—— 导出什么就能导回什么。
pub fn targets_for(
    db: &DbState,
    period_id: i64,
) -> AppResult<(Vec<MappingTarget>, BTreeMap<String, FieldSpec>)> {
    let period = periods::get(db, period_id)?;
    let fields = periods::display_fields(db, &period)?;

    let mut targets: Vec<MappingTarget> = vec![
        MappingTarget {
            key: import::FIXED_NAME.into(),
            label: import::FIXED_LABELS[0].1.into(),
            fixed: true,
            data_type: FieldType::Text,
            formula: false,
        },
        MappingTarget {
            key: import::FIXED_EMAIL.into(),
            label: import::FIXED_LABELS[1].1.into(),
            fixed: true,
            data_type: FieldType::Text,
            formula: false,
        },
        MappingTarget {
            key: import::FIXED_EMPLOYEE_NO.into(),
            label: import::FIXED_LABELS[2].1.into(),
            fixed: true,
            data_type: FieldType::Text,
            formula: false,
        },
    ];
    let mut specs: BTreeMap<String, FieldSpec> = BTreeMap::new();
    for f in &fields {
        if !f.visible {
            continue;
        }
        targets.push(MappingTarget {
            key: f.key.clone(),
            label: f.label.clone(),
            fixed: false,
            data_type: FieldType::parse(&f.data_type).unwrap_or(FieldType::Text),
            formula: f.data_type == "formula",
        });
        specs.insert(
            f.key.clone(),
            FieldSpec {
                key: f.key.clone(),
                label: f.label.clone(),
                data_type: FieldType::parse(&f.data_type).unwrap_or(FieldType::Text),
                formula: f.formula.clone(),
                required: f.required,
            },
        );
    }
    Ok((targets, specs))
}

/// 读成员表（导入要用来核对身份）。
pub fn member_refs(db: &DbState) -> AppResult<BTreeMap<i64, MemberRef>> {
    let members = crate::infra::members::list_members(db)?;
    let mut out = BTreeMap::new();
    for m in members {
        out.insert(
            m.id,
            MemberRef {
                id: m.id,
                name: m.name,
                email: m.email,
                employee_no: m.employee_no,
                active: m.active,
            },
        );
    }
    Ok(out)
}

/// 读某个月已有的记录（member_id → 字段值）。
pub fn existing_records(
    db: &DbState,
    period_id: i64,
) -> AppResult<BTreeMap<i64, BTreeMap<String, String>>> {
    db.read(|conn| {
        let mut stmt = conn
            .prepare("SELECT member_id, values_json FROM salary_records WHERE period_id = ?1")
            .map_err(|e| AppError::Database(e.to_string()))?;
        let rows = stmt
            .query_map(params![period_id], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| AppError::Database(e.to_string()))?;
        let mut out = BTreeMap::new();
        for row in rows {
            let (member_id, json) = row.map_err(|e| AppError::Database(e.to_string()))?;
            let values = record::values_from_json(&json)
                .map_err(|e| AppError::Database(format!("已有记录读不懂：{e}")))?;
            out.insert(member_id, values);
        }
        Ok(out)
    })
}

/// 校验一批行（不写库）。
pub fn validate(
    db: &DbState,
    period_id: i64,
    sheet: &ParsedSheet,
    mappings: &[ColumnMapping],
) -> AppResult<ValidatedRows> {
    let (_targets, specs) = targets_for(db, period_id)?;
    let members = member_refs(db)?;

    // 目标表：key → MappingTarget（校验时要用类型与「是不是公式列」）
    let mut target_map: BTreeMap<String, MappingTarget> = BTreeMap::new();
    for (key, spec) in &specs {
        target_map.insert(
            key.clone(),
            MappingTarget {
                key: key.clone(),
                label: spec.label.clone(),
                fixed: false,
                data_type: spec.data_type,
                formula: spec.is_formula(),
            },
        );
    }

    Ok(import::validate_rows(
        &sheet.rows,
        mappings,
        &target_map,
        &members,
    ))
}

/// 入库的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitResult {
    pub summary: ImportSummary,
    /// 写进去多少行。
    pub written: usize,
    /// 覆盖了多少行（覆盖策略下）。
    pub replaced: usize,
    /// 因为冲突被取消（`cancel` 策略）。
    pub cancelled: bool,
}

/// 把校验通过的行写进库（**一个事务**）。
///
/// `strategy` 决定同月已有数据怎么处理（ADR-0021 决策 8）。
pub fn commit(
    db: &DbState,
    period_id: i64,
    rows: &[import::ParsedRow],
    specs: &BTreeMap<String, FieldSpec>,
    strategy: ConflictStrategy,
) -> AppResult<CommitResult> {
    let existing = existing_records(db, period_id)?;

    // 冲突检测：有记录且策略是「取消」→ 整批不做。
    let conflicting: Vec<i64> = rows
        .iter()
        .filter(|r| existing.contains_key(&r.member_id))
        .map(|r| r.member_id)
        .collect();
    if !conflicting.is_empty() && strategy == ConflictStrategy::Cancel {
        return Ok(CommitResult {
            summary: ImportSummary::new(),
            written: 0,
            replaced: 0,
            cancelled: true,
        });
    }

    let now = now_secs();
    let mut replaced = 0usize;
    let mut written = 0usize;

    // 审计的细节：**只有行数与动作**，没有任何工资数值（红线 2）。
    let audit_detail = serde_json::json!({
        "strategy": match strategy {
            ConflictStrategy::Overwrite => "overwrite",
            ConflictStrategy::FillEmpty => "fillEmpty",
            ConflictStrategy::Cancel => "cancel",
        },
        "rows": rows.len(),
        "replaced": conflicting.len(),
    })
    .to_string();

    db.write_tx(|tx| {
        for row in rows {
            // 这一行最终的值（合并后的）
            let incoming = row.values.clone();
            let final_values = match existing.get(&row.member_id) {
                Some(old) => match strategy {
                    ConflictStrategy::Overwrite => {
                        replaced += 1;
                        import::merge_overwrite(old, &incoming)
                    }
                    ConflictStrategy::FillEmpty => import::merge_fill_empty(old, &incoming),
                    ConflictStrategy::Cancel => old.clone(),
                },
                None => incoming,
            };

            // 公式列由系统算（用户填的值在校验阶段已被拒/忽略）。
            let final_values = compute_formulas(&final_values, specs)?;

            let json = record::values_to_json(&final_values);
            let checksum = record::checksum(&row.email, &json);

            // 身份核对标记跟着入库（ADR-0024 决策 2）：Excel 姓名与成员表
            // 不一致的那行 → 0，发送范围核对（TC-401）拿它拦。
            let verified = i64::from(!row.identity_unverified);
            let affected = tx
                .execute(
                    "INSERT INTO salary_records \
                     (period_id, member_id, values_json, checksum, source, created_at, updated_at, identity_verified) \
                     VALUES (?1, ?2, ?3, ?4, 'import', ?5, ?5, ?6) \
                     ON CONFLICT(period_id, member_id) DO UPDATE SET \
                       values_json = excluded.values_json, \
                       checksum = excluded.checksum, \
                       source = excluded.source, \
                       updated_at = excluded.updated_at, \
                       identity_verified = excluded.identity_verified",
                    params![period_id, row.member_id, json, checksum, now, verified],
                )
                .map_err(|e| AppError::Database(format!("写工资记录失败：{e}")))?;
            if affected > 0 {
                written += 1;
            }
        }

        // 覆盖的审计：**在同一事务里**写 —— 事务回滚则审计一起回滚。
        if !conflicting.is_empty() && strategy == ConflictStrategy::Overwrite {
            tx.execute(
                "INSERT INTO audit_log (action, target, detail_masked, at) VALUES ('import', ?1, ?2, ?3)",
                params![period_id.to_string(), audit_detail, now],
            )
            .map_err(|e| AppError::Database(format!("写导入审计失败：{e}")))?;
        }

        Ok(())
    })?;

    Ok(CommitResult {
        summary: ImportSummary::new(),
        written,
        replaced,
        cancelled: false,
    })
}

/// 算出行里的公式列（用户填的值不算数）。
///
/// 用的是 `domain::record::evaluate_row` —— 与 TC-203 的试算、TC-303 的逐格编辑
/// **同一个**求值口径（ADR-0018：只有一份权威）。
fn compute_formulas(
    values: &BTreeMap<String, String>,
    specs: &BTreeMap<String, FieldSpec>,
) -> AppResult<BTreeMap<String, String>> {
    let spec_list: Vec<FieldSpec> = specs.values().cloned().collect();
    if !spec_list.iter().any(|s| s.is_formula()) {
        return Ok(values.clone());
    }
    let evaluated = record::evaluate_row(&spec_list, values)
        .map_err(|e| AppError::ValidationError(format!("公式算不出来：{e}")))?;
    let mut out = values.clone();
    for (k, v) in evaluated.computed {
        out.insert(k, v);
    }
    Ok(out)
}

/// 墙钟 epoch 秒。
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 导出一份错误明细 xlsx（**不含工资数值** —— 红线 2）。
///
/// 明细里有：行号、列名、问题。
/// **没有**金额 —— 一个错误行里的金额也是工资数据，
/// 而错误明细会被发到微信/邮件里到处传。
pub fn export_error_report(path: &Path, problems: &[import::RowProblem]) -> AppResult<()> {
    use rust_xlsxwriter::{Format, FormatBorder, Workbook};

    let mut workbook = Workbook::new();
    let header_fmt = Format::new()
        .set_bold()
        .set_border_bottom(FormatBorder::Thin);
    {
        let sheet = workbook.add_worksheet();
        sheet
            .set_name("错误明细")
            .map_err(|e| AppError::Internal(format!("写错误明细失败：{e}")))?;
        for (i, title) in ["Excel 行号", "列", "问题"].iter().enumerate() {
            sheet
                .write_string_with_format(0, i as u16, *title, &header_fmt)
                .map_err(|e| AppError::Internal(format!("写错误明细失败：{e}")))?;
        }
        for (r, p) in problems.iter().enumerate() {
            let row = (r + 1) as u32;
            sheet
                .write_string_with_format(row, 0, p.row.to_string(), &Format::new())
                .map_err(|e| AppError::Internal(format!("写错误明细失败：{e}")))?;
            sheet
                .write_string_with_format(row, 1, &p.column, &Format::new())
                .map_err(|e| AppError::Internal(format!("写错误明细失败：{e}")))?;
            sheet
                .write_string_with_format(row, 2, &p.message, &Format::new())
                .map_err(|e| AppError::Internal(format!("写错误明细失败：{e}")))?;
        }
        sheet
            .set_column_width(0, 12.0)
            .map_err(|e| AppError::Internal(format!("写错误明细失败：{e}")))?;
        sheet
            .set_column_width(1, 16.0)
            .map_err(|e| AppError::Internal(format!("写错误明细失败：{e}")))?;
        sheet
            .set_column_width(2, 60.0)
            .map_err(|e| AppError::Internal(format!("写错误明细失败：{e}")))?;
    }
    workbook
        .save(path)
        .map_err(|e| AppError::Internal(format!("保存错误明细失败：{e}")))?;
    Ok(())
}
