//! 工资记录的 Command（TC-303 · ADR-0022）
//!
//! S07 表格的四个入口：列表 / 单格保存 / 删行 / 合计。
//! 编辑权、公式列只读、校验列的判定都在 `infra::records` —— 这里只组装 DTO。

use std::sync::Arc;

use tauri::State;

use crate::error::AppResult;
use crate::infra::db::DbState;
use crate::infra::records::{self, PeriodTotals, RecordRow};

/// 列出某个月的全部工资记录（S07 表格的数据源）。
#[tauri::command]
pub fn list_records(db: State<'_, Arc<DbState>>, period_id: i64) -> AppResult<Vec<RecordRow>> {
    records::list_records(&db, period_id)
}

/// 单格保存（失焦/回车触发）。
///
/// 改一格 → 整行重算公式列 → 重算 checksum → 返回这一行的新状态
/// （前端拿它原地替换那一行，不整表刷新）。
#[tauri::command]
pub fn update_record_cell(
    db: State<'_, Arc<DbState>>,
    period_id: i64,
    member_id: i64,
    key: String,
    value: String,
) -> AppResult<RecordRow> {
    records::update_cell(&db, period_id, member_id, &key, &value)
}

/// 删除整行（给错了人要撤掉的场景）。需二次确认（界面上做）。
#[tauri::command]
pub fn delete_record(db: State<'_, Arc<DbState>>, period_id: i64, member_id: i64) -> AppResult<()> {
    records::delete_record(&db, period_id, member_id)
}

/// 整月合计（S07 表底的「本页合计」；v1 无分页，即整月）。
#[tauri::command]
pub fn get_period_totals(db: State<'_, Arc<DbState>>, period_id: i64) -> AppResult<PeriodTotals> {
    records::totals(&db, period_id)
}
