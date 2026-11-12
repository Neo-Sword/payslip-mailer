//! 发送历史与凭证的 Command（TC-403 · ADR-0026）

use std::sync::Arc;

use tauri::{AppHandle, Manager, State};

use crate::error::{AppError, AppResult};
use crate::infra::db::DbState;
use crate::infra::sendlog::{self, HistoryEntry, JobDetail};

/// 发送历史（S13 的列表 —— 批次按时间倒序，带进度）。
#[tauri::command]
pub fn list_send_history(db: State<'_, Arc<DbState>>) -> AppResult<Vec<HistoryEntry>> {
    sendlog::list_history(&db)
}

/// 一个批次的逐封明细（出问题时定位 SMTP 错误码）。
#[tauri::command]
pub fn get_batch_jobs(db: State<'_, Arc<DbState>>, batch_id: i64) -> AppResult<Vec<JobDetail>> {
    sendlog::job_details(&db, batch_id)
}

/// 导出 CSV 凭证（**纯读操作** —— 不写库、不改批次状态）。
///
/// 凭证里没有工资数值（红线 2）：姓名、收件地址、状态、SMTP 回码、
/// 发送时间、内容校验和 —— 证据链要的几样，一样不多。
#[tauri::command]
pub fn export_batch_voucher(
    app: AppHandle,
    db: State<'_, Arc<DbState>>,
    batch_id: i64,
) -> AppResult<String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| AppError::Io(format!("无法定位本机数据目录：{e}")))?;
    let path = sendlog::export_voucher(&db, batch_id, &dir)?;
    Ok(path.to_string_lossy().to_string())
}
