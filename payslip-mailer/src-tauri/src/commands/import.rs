//! Excel 导入的 Command（TC-302 · ADR-0021）
//!
//! 三步向导的三个入口：
//!
//! 1. [`preview_import`]：读文件 → 表头映射 → 行级校验 → 出预览（**不写库**）。
//! 2. [`commit_import`]：按确认的映射与冲突策略写库（**一个事务**）。
//! 3. [`export_import_errors`]：把错误行导成明细 xlsx（**不含工资数值**）。

use std::path::PathBuf;
use std::sync::Arc;

use tauri::{AppHandle, Manager, State};

use crate::domain::import::{
    ColumnMapping, ColumnTarget, ConflictStrategy, ImportSummary, RowProblem, Severity,
};
use crate::error::{AppError, AppResult};
use crate::infra::db::DbState;
use crate::infra::import;

/// 预览的结果（界面第 2、3 步看到的）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportPreview {
    /// 文件名（只有名字，不含路径 —— 路径不进 DTO）。
    pub file_name: String,
    /// 表头映射（每一列一行）。
    pub mappings: Vec<ColumnMapping>,
    /// 固定列（姓名 / 邮箱 / 工号）缺了哪些（空 = 都对上了）。
    pub missing_fixed: Vec<String>,
    /// 有没有模糊命中需要用户确认。
    pub needs_confirmation: bool,
    /// 统计。
    pub summary: ImportSummary,
    /// 每一行的问题。
    pub problems: Vec<RowProblem>,
    /// 数据行数。
    pub row_count: usize,
}

/// 用户在界面上确认过的映射。
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MappingChoice {
    /// Excel 里的列序（0 基）。
    pub index: usize,
    /// 映射到哪儿。
    pub target: ColumnTarget,
}

/// 读文件、映射、校验 —— **不写库**。
///
/// `path` 是用户在文件对话框里选的那个文件（前端用 `plugin-dialog` 拿到的路径）。
/// 后端**只读**它，不复制（ch.9）。
#[tauri::command]
pub fn preview_import(
    db: State<'_, Arc<DbState>>,
    period_id: i64,
    path: String,
) -> AppResult<ImportPreview> {
    let file = PathBuf::from(&path);
    let sheet = import::read_sheet(&file)?;
    let (targets, _specs) = import::targets_for(&db, period_id)?;
    let mappings = crate::domain::import::map_headers(&sheet.headers, &targets);

    // 用「自动映射」先校验一遍，给界面一个立刻能看的预览。
    let validated = import::validate(&db, period_id, &sheet, &mappings)?;
    let missing_fixed = crate::domain::import::missing_fixed_columns(&mappings);

    let needs_confirmation = crate::domain::import::has_unconfirmed(&mappings);
    Ok(ImportPreview {
        file_name: file
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default(),
        mappings,
        missing_fixed,
        needs_confirmation,
        summary: validated.summary,
        problems: validated.problems,
        row_count: sheet.rows.len(),
    })
}

/// 入库。
///
/// `mappings` 是用户确认过的映射（前端改过的也传上来）。
/// `strategy` 是冲突策略 —— **必须先选**（ADR-0021 决策 8）。
#[tauri::command]
pub fn commit_import(
    db: State<'_, Arc<DbState>>,
    period_id: i64,
    path: String,
    mappings: Vec<MappingChoice>,
    strategy: Option<ConflictStrategy>,
) -> AppResult<ImportOutcome> {
    // 冲突策略必须先选（spec 标准 11）。
    let Some(strategy) = strategy else {
        return Err(AppError::ValidationError(
            "请先选择导入策略：覆盖同月已有数据 / 仅补充空白单元格 / 取消导入".into(),
        ));
    };

    let file = PathBuf::from(&path);
    let sheet = import::read_sheet(&file)?;
    let (_targets, specs) = import::targets_for(&db, period_id)?;

    // 用用户确认的映射（没给的那几列沿用自动映射的结果）。
    let (targets, _) = import::targets_for(&db, period_id)?;
    let mut auto = crate::domain::import::map_headers(&sheet.headers, &targets);
    for choice in &mappings {
        if let Some(m) = auto.iter_mut().find(|m| m.index == choice.index) {
            m.target = choice.target.clone();
            // 用户自己选的目标就算确认过了。
            m.needs_confirmation = false;
        }
    }

    // 三个固定列必须都对上（ADR-0021 决策 3）。
    let missing = crate::domain::import::missing_fixed_columns(&auto);
    if !missing.is_empty() {
        return Err(AppError::ValidationError(format!(
            "表头映射还差：{}。请在「表头映射」里把 Excel 的对应列选上。",
            missing.join("、")
        )));
    }

    let validated = import::validate(&db, period_id, &sheet, &auto)?;

    // K>0 禁止入库（ADR-0021 决策 9 / spec 标准 4）。
    if !validated.summary.can_commit() {
        return Err(AppError::ValidationError(format!(
            "有 {} 行数据有问题，没有导入。请按「校验结果」里的说明改好 Excel 后再导。",
            validated.summary.errors
        )));
    }

    let result = import::commit(&db, period_id, &validated.rows, &specs, strategy)?;
    if result.cancelled {
        return Ok(ImportOutcome {
            written: 0,
            replaced: 0,
            cancelled: true,
            summary: validated.summary,
            warnings: validated
                .problems
                .iter()
                .filter(|p| p.severity == Severity::Warning)
                .count(),
            identity_unverified: validated
                .rows
                .iter()
                .filter(|r| r.identity_unverified)
                .count(),
        });
    }

    Ok(ImportOutcome {
        written: result.written,
        replaced: result.replaced,
        cancelled: false,
        summary: validated.summary,
        warnings: validated
            .problems
            .iter()
            .filter(|p| p.severity == Severity::Warning)
            .count(),
        identity_unverified: validated
            .rows
            .iter()
            .filter(|r| r.identity_unverified)
            .count(),
    })
}

/// 入库的结果。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportOutcome {
    /// 写进去多少行。
    pub written: usize,
    /// 覆盖了多少行。
    pub replaced: usize,
    /// 因为选了「取消导入」而什么都没做。
    pub cancelled: bool,
    /// 统计。
    pub summary: ImportSummary,
    /// 警告行数。
    pub warnings: usize,
    /// 「身份待核对」的行数（这些行**不会进入发送范围**）。
    pub identity_unverified: usize,
}

/// 导出一份错误明细 xlsx。
///
/// **明细里没有工资数值**（红线 2）：只有行号、列名与问题描述。
#[tauri::command]
pub fn export_import_errors(
    app: AppHandle,
    db: State<'_, Arc<DbState>>,
    period_id: i64,
    path: String,
    mappings: Vec<MappingChoice>,
) -> AppResult<String> {
    let file = PathBuf::from(&path);
    let sheet = import::read_sheet(&file)?;
    let (targets, _) = import::targets_for(&db, period_id)?;
    let mut auto = crate::domain::import::map_headers(&sheet.headers, &targets);
    for choice in &mappings {
        if let Some(m) = auto.iter_mut().find(|m| m.index == choice.index) {
            m.target = choice.target.clone();
        }
    }
    let validated = import::validate(&db, period_id, &sheet, &auto)?;
    let problems: Vec<RowProblem> = validated
        .problems
        .iter()
        .filter(|p| p.severity == Severity::Error)
        .cloned()
        .collect();

    if problems.is_empty() {
        return Err(AppError::ValidationError(
            "没有错误行，不用导出明细。".into(),
        ));
    }

    let period = crate::infra::periods::get(&db, period_id)?;
    let dir = crate::infra::backup::exports_dir(&data_dir(&app)?);
    std::fs::create_dir_all(&dir)
        .map_err(|e| AppError::BackupFailed(format!("建导出目录失败：{e}")))?;
    // 文件名含年月，不含任何数值（红线 2）。
    let out = dir.join(format!("导入错误明细_{}.xlsx", period.year_month));
    import::export_error_report(&out, &problems)?;
    Ok(out.to_string_lossy().to_string())
}

/// 数据目录。与 `commands::export` 同一个口径。
fn data_dir(app: &AppHandle) -> AppResult<PathBuf> {
    app.path()
        .app_data_dir()
        .map_err(|e| AppError::Io(format!("无法定位本机数据目录：{e}")))
}
