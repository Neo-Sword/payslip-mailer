//! 三重核对的 Command（TC-401 · ADR-0024）
//!
//! S11「预览与核对」屏的两个入口：
//! - [`verify_send_scope`]：整个发送范围的核对报告（谁被拦了、为什么）。
//! - [`render_member_payslip`]：单人预览（与核对**同一份渲染**）。

use std::sync::Arc;

use tauri::State;

use crate::error::{AppError, AppResult};
use crate::infra::db::DbState;
use crate::infra::verify::{self, PreviewPayload, ScopeVerification};

/// 读要用的模板（指定 id；没有就用默认的那套 —— S11 的「用默认模板」语义）。
fn template_of(db: &DbState, template_id: Option<i64>) -> AppResult<(String, String)> {
    let list = crate::infra::templates::list(db)?;
    let tpl = match template_id {
        Some(id) => list
            .iter()
            .find(|t| t.id == id)
            .ok_or_else(|| AppError::NotFound("邮件模板".into()))?,
        None => list
            .iter()
            .find(|t| t.is_default)
            .or_else(|| list.first())
            .ok_or_else(|| AppError::NotFound("邮件模板".into()))?,
    };
    Ok((tpl.subject_tpl.clone(), tpl.body_html.clone()))
}

/// 核对整个发送范围（创建批次前的最后一道闸）。
///
/// `member_ids` 是范围选择界面（TC-402）组装的名单。
/// 报告里被拦的人**不会**被 TC-402 的批次创建放行。
#[tauri::command]
pub fn verify_send_scope(
    db: State<'_, Arc<DbState>>,
    period_id: i64,
    member_ids: Vec<i64>,
    template_id: Option<i64>,
) -> AppResult<ScopeVerification> {
    let (subject, body) = template_of(&db, template_id)?;
    verify::verify_scope(&db, period_id, &member_ids, &subject, &body)
}

/// 单人预览（S11 的逐人视图）。
///
/// 与 [`verify_send_scope`] 同一个 `context_for` ——
/// 核对看到的与发出去的是同一份渲染（spec 标准 9）。
#[tauri::command]
pub fn render_member_payslip(
    db: State<'_, Arc<DbState>>,
    period_id: i64,
    member_id: i64,
    template_id: Option<i64>,
) -> AppResult<PreviewPayload> {
    let (subject, body) = template_of(&db, template_id)?;
    let verification = verify::verify_member(&db, period_id, member_id, &subject, &body)?;
    verification
        .preview
        .ok_or_else(|| AppError::TemplateError("这个人核对没通过，没有可预览的渲染结果".into()))
}
