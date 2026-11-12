//! 从账套与字段配置拼出工资表的结构（TC-301）
//!
//! 桥：`salary_periods`（哪个月、哪套列）+ `salary_fields`（列的定义）
//! + `members`（预填名单要用）→ [`crate::domain::sheet::SheetSchema`]。
//!
//! **表头取账套的快照**（ADR-0020 决策 2）：复用 `infra::periods::display_fields`
//! —— draft 用当前配置、ready+ 用快照。不另立一套口径。

use crate::domain::sheet::{SheetSchema, build_schema};
use crate::error::AppResult;
use crate::infra::db::DbState;
use crate::infra::export::Row;
use crate::infra::periods;

/// 这个账套的表结构。
pub fn schema_for(db: &DbState, period_id: i64) -> AppResult<SheetSchema> {
    let period = periods::get(db, period_id)?;
    let fields = periods::display_fields(db, &period)?;
    let tuples: Vec<(String, String, String, bool, bool, bool)> = fields
        .into_iter()
        .map(|f| {
            (
                f.key,
                f.label,
                f.data_type,
                f.required,
                f.formula.is_some(),
                f.visible,
            )
        })
        .collect();
    Ok(build_schema(&tuples))
}

/// 两行示例数据（设计文档 S09 的「包含 2 行示例数据」选项）。
///
/// 数字是**示例**，不是真的工资；公式列留空（导出时会写占位文字）。
pub fn sample_row(schema: &SheetSchema) -> Row {
    let mut cells = Vec::with_capacity(schema.columns.len());
    for col in &schema.columns {
        let cell = match col.key.as_str() {
            "name" => "张三",
            "email" => "zhangsan@example.com",
            "employee_no" => "E001",
            _ => match col.data_type_hint() {
                Some("money") => "18000.00",
                Some("number") => "12",
                Some("percent") => "12.50",
                _ => "",
            },
        };
        cells.push(cell.to_string());
    }
    Row { cells }
}

/// 预填成员名单（设计文档 S09 的「预填成员名单（42 人）」选项）。
///
/// **这是把名单带出文件的操作**，所以默认关（ADR-0020 决策 5）。
pub fn member_rows(db: &DbState, schema: &SheetSchema) -> AppResult<Vec<Row>> {
    let members = crate::infra::members::list_members(db)?;
    let mut out = Vec::with_capacity(members.len());
    for m in members {
        if !m.active {
            // 停用的成员不进模板（设计文档：停用成员不进入发送范围）
            continue;
        }
        let mut cells = Vec::with_capacity(schema.columns.len());
        for col in &schema.columns {
            let cell = match col.key.as_str() {
                "name" => m.name.clone(),
                "email" => m.email.clone(),
                "employee_no" => m.employee_no.clone().unwrap_or_default(),
                _ => String::new(),
            };
            cells.push(cell);
        }
        out.push(Row { cells });
    }
    Ok(out)
}
