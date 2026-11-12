//! 工资条项目的 Command（TC-200 · ADR-0015）
//!
//! 这一层只做「参数校验 → 调用 infra → 组装 DTO」。
//! 口令与凭据一律不出现（红线 2 的同族）。

use std::sync::Arc;

use tauri::State;

use crate::domain::field::FieldType;
use crate::error::{AppError, AppResult};
use crate::infra::db::DbState;
use crate::infra::fields::{self, Field};

/// 列出全部字段（含已停用的 —— 界面要能看见它们是灰的）。
#[tauri::command]
pub fn list_fields(db: State<'_, Arc<DbState>>) -> AppResult<Vec<Field>> {
    fields::list(&db)
}

/// 新建一个字段。
#[tauri::command]
pub fn create_field(
    db: State<'_, Arc<DbState>>,
    key: String,
    label: String,
    data_type: String,
    required: bool,
    formula: Option<String>,
) -> AppResult<Field> {
    let ty = FieldType::parse(&data_type)?;
    fields::create(&db, &key, &label, ty, required, formula.as_deref())
}

/// 编辑一个字段（**key 不可改** —— 它已经进过表头与模板变量）。
#[tauri::command]
pub fn update_field(
    db: State<'_, Arc<DbState>>,
    id: i64,
    label: String,
    data_type: String,
    required: bool,
    formula: Option<String>,
) -> AppResult<()> {
    let ty = FieldType::parse(&data_type)?;
    fields::update(&db, id, &label, ty, required, formula.as_deref())
}

/// 停用 / 启用一个字段。
#[tauri::command]
pub fn set_field_visible(db: State<'_, Arc<DbState>>, id: i64, visible: bool) -> AppResult<()> {
    fields::set_visible(&db, id, visible)
}

/// 按给定的 id 顺序重排（拖拽排序的结果）。
#[tauri::command]
pub fn reorder_fields(db: State<'_, Arc<DbState>>, order: Vec<i64>) -> AppResult<()> {
    fields::reorder(&db, &order)
}

/// **下一份空模板会出现的列** —— 导出表头的唯一真相源。
#[tauri::command]
pub fn preview_header(db: State<'_, Arc<DbState>>) -> AppResult<Vec<String>> {
    fields::preview_header(&db)
}

/// 新建模板时可选的变量（TC-202 用）。
///
/// ⚠️ 只含未停用的字段。渲染**历史**模板时要用全部 key（含已停用的）——
/// 那个字段的历史值还在，老模板要能渲染出来。
#[tauri::command]
pub fn list_selectable_variables(db: State<'_, Arc<DbState>>) -> AppResult<Vec<(String, String)>> {
    fields::selectable_variables(&db)
}

/// **全部**字段的 key（**含已停用的**）—— 渲染历史模板时用。
///
/// 与 `list_selectable_variables` 是**两份不同的名单**：
/// 新建模板只能选未停用的；渲染老模板必须认全部（那个字段的历史值还在，
/// 老模板要能渲染出来）。别把两者混用。
#[tauri::command]
pub fn list_all_variable_keys(db: State<'_, Arc<DbState>>) -> AppResult<Vec<String>> {
    fields::all_keys(&db)
}

/// 试算一条公式（界面即时反馈用）。
///
/// `values` 是这一行的数值（字段 key → 数值字符串）。
/// 返回算出来的值（已按分四舍五入）。
#[tauri::command]
pub fn evaluate_formula(
    formula: String,
    values: std::collections::BTreeMap<String, String>,
    total_earnings: Option<f64>,
    total_deductions: Option<f64>,
) -> AppResult<String> {
    let mut field_values = std::collections::BTreeMap::new();
    for (k, v) in values {
        match v.trim().parse::<f64>() {
            Ok(n) => {
                field_values.insert(k, n);
            }
            Err(_) => {
                return Err(AppError::ValidationError(format!(
                    "{k} 的值不是一个能算的数字"
                )));
            }
        }
    }
    let ctx = crate::domain::formula::EvalContext {
        field_values,
        total_earnings: total_earnings.unwrap_or(0.0),
        total_deductions: total_deductions.unwrap_or(0.0),
        text_fields: std::collections::BTreeSet::new(),
    };
    let value = crate::domain::formula::eval_formula(&formula, &ctx)
        .map_err(|e| AppError::ValidationError(e.to_string()))?;
    Ok(crate::domain::formula::round_to_cents(value).to_string())
}

/// 按**一行**求值全部公式列（TC-203）。
///
/// 这是公式求值器的**生产调用方**：TC-303 逐格编辑时自动重算公式列、
/// TC-302 导入时算公式列，都走这一条路。
/// 顺带在这里算两个汇总变量（应发合计 / 扣款合计）——
/// 它们是「该行」的合计，不是整张表的（S07 表格底部的「本页合计」是另一回事）。
///
/// `values` 是这一行的原始数值（字段 key → 数值字符串）。
/// 返回公式字段的 key → 值（已按分四舍五入）。
#[tauri::command]
pub fn evaluate_row(
    db: State<'_, Arc<DbState>>,
    values: std::collections::BTreeMap<String, String>,
) -> AppResult<std::collections::BTreeMap<String, String>> {
    let fields = crate::infra::fields::list(&db)?;

    // **求值口径只有一份**：`domain::record::evaluate_row`。
    // 本 Command、TC-302 的导入、TC-303 的逐格编辑都走它 ——
    // 三处各写一份，三处的「应发合计」迟早会算出不同的数，
    // 而那个数是要发出去的（ADR-0018 的同一条道理）。
    let specs: Vec<crate::domain::record::FieldSpec> = fields
        .iter()
        .map(|f| crate::domain::record::FieldSpec {
            key: f.key.clone(),
            label: f.label.clone(),
            data_type: f.data_type,
            formula: f.formula.clone(),
            required: f.required,
        })
        .collect();

    let evaluated =
        crate::domain::record::evaluate_row(&specs, &values).map_err(AppError::ValidationError)?;

    Ok(evaluated.computed)
}
