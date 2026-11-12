//! 导出空模板的 Command（TC-301 · ADR-0020）

use std::path::PathBuf;
use std::sync::Arc;

use tauri::{AppHandle, Manager, State};

use crate::domain::sheet::{SheetColumn, SheetSchema};
use crate::error::{AppError, AppResult};
use crate::infra::db::DbState;
use crate::infra::export::{self, ExportOptions, Row};
use crate::infra::periods;
use crate::infra::sheet_schema;

/// 表头预览（不落文件）。
///
/// 界面上的「表头预览」与导出文件里的表头**必须一字不差**（标准 11）——
/// 所以两者调的是**同一个函数**。
#[tauri::command]
pub fn preview_template_header(
    db: State<'_, Arc<DbState>>,
    period_id: i64,
) -> AppResult<SheetSchema> {
    sheet_schema::schema_for(&db, period_id)
}

/// 导出一份空模板。
///
/// 导出到数据目录的 `exports/` 下（设计文档 ch.6 的磁盘布局），文件名含年月。
#[tauri::command]
pub fn export_template(
    app: AppHandle,
    db: State<'_, Arc<DbState>>,
    period_id: i64,
    include_instructions: Option<bool>,
    include_sample_rows: Option<bool>,
    include_member_list: Option<bool>,
) -> AppResult<ExportTemplateResult> {
    let schema = sheet_schema::schema_for(&db, period_id)?;
    let period = periods::get(&db, period_id)?;

    let defaults = ExportOptions::defaults();
    let options = ExportOptions {
        include_instructions: include_instructions.unwrap_or(defaults.include_instructions),
        include_sample_rows: include_sample_rows.unwrap_or(defaults.include_sample_rows),
        include_member_list: include_member_list.unwrap_or(defaults.include_member_list),
    };

    // 数据行：示例数据或预填名单（默认都空 —— 这是「空模板」）
    let mut rows: Vec<Row> = Vec::new();
    if options.include_sample_rows {
        rows.push(sheet_schema::sample_row(&schema));
    }
    if options.include_member_list {
        rows.extend(sheet_schema::member_rows(&db, &schema)?);
    }

    // 落盘位置由 `export::target_path` 独家决定（标准 9）。
    let path = export::target_path(
        &data_dir(&app)?,
        &period.year_month,
        options.include_member_list,
    );
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AppError::BackupFailed(format!("建导出目录失败：{e}")))?;
    }
    export::write_xlsx(&path, &schema, options, &rows)?;

    // 导出是一次「可留痕」的动作（ch.9 的证据链）。
    // 只记「导出了哪个账套的模板、几行」——**不记工资数值**。
    periods::write_audit(
        &db,
        "export",
        &period.year_month,
        Some(format!(
            "{{\"rows\":{},\"file\":\"{}\"}}",
            rows.len(),
            path.file_name().unwrap_or_default().to_string_lossy()
        )),
    )?;

    Ok(ExportTemplateResult {
        path: path.to_string_lossy().to_string(),
        columns: schema.columns,
        row_count: rows.len(),
    })
}

/// 导出结果。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportTemplateResult {
    /// 导出文件的路径。
    pub path: String,
    /// 表头（与文件里的表头一字不差）。
    pub columns: Vec<SheetColumn>,
    /// 写了多少行数据（默认 0 —— 空模板）。
    pub row_count: usize,
}

/// 数据目录。与 `commands::auth` 同一个口径（`app_data_dir()`）。
fn data_dir(app: &AppHandle) -> AppResult<PathBuf> {
    app.path()
        .app_data_dir()
        .map_err(|e| AppError::Io(format!("无法定位本机数据目录：{e}")))
}
