//! 月份账套的 Command（TC-300 · ADR-0019）
//!
//! 状态迁移与解锁都只在这里暴露「能力」，
//! **不驱动发送**（ready → sending → sent 的推进是 TC-402 的事）。

use std::sync::Arc;

use tauri::State;

use crate::domain::period::PeriodStatus;
use crate::error::{AppError, AppResult};
use crate::infra::db::DbState;
use crate::infra::periods::{self, FieldSnapshot, Period};

/// 列出全部账套。
#[tauri::command]
pub fn list_periods(db: State<'_, Arc<DbState>>) -> AppResult<Vec<Period>> {
    periods::list(&db)
}

/// 按 id 取一个账套。
#[tauri::command]
pub fn get_period(db: State<'_, Arc<DbState>>, id: i64) -> AppResult<Period> {
    periods::get(&db, id)
}

/// 新建一个账套（草稿）。
#[tauri::command]
pub fn create_period(db: State<'_, Arc<DbState>>, year_month: String) -> AppResult<Period> {
    periods::create(&db, &year_month)
}

/// 迁移状态。非法迁移被拒，且**不留任何痕迹**。
#[tauri::command]
pub fn transition_period(
    db: State<'_, Arc<DbState>>,
    id: i64,
    target: String,
) -> AppResult<Period> {
    let to = PeriodStatus::parse(&target).ok_or_else(|| {
        AppError::ValidationError(format!(
            "状态不对：{target}（只能是 draft / ready / sending / sent / archived）"
        ))
    })?;
    periods::transition(&db, id, to)
}

/// 解锁一个已发送的账套（回到草稿），并留下一条修订记录。
#[tauri::command]
pub fn unlock_period(db: State<'_, Arc<DbState>>, id: i64) -> AppResult<Period> {
    periods::unlock(&db, id)
}

/// 这个月显示的列：**draft 用当前配置，其余用快照**。
#[tauri::command]
pub fn get_period_fields(db: State<'_, Arc<DbState>>, id: i64) -> AppResult<Vec<FieldSnapshot>> {
    let period = periods::get(&db, id)?;
    periods::display_fields(&db, &period)
}

/// 读审计日志。
#[tauri::command]
pub fn list_audit_log(
    db: State<'_, Arc<DbState>>,
    limit: Option<i64>,
) -> AppResult<Vec<periods::AuditEntry>> {
    periods::list_audit(&db, limit.unwrap_or(50))
}
