//! 邮件模板的 Command（TC-202 · ADR-0017）
//!
//! **「试发给自己」不在这里** —— 那要真的发一封邮件（要 SMTP → TC-500）。
//! 本卡只做模板的编辑、变量白名单与渲染预览。

use std::sync::Arc;

use tauri::State;

use crate::error::AppResult;
use crate::infra::db::DbState;
use crate::infra::fields;
use crate::infra::templates::{self, Rendered, Template};

/// 列出全部模板。
#[tauri::command]
pub fn list_templates(db: State<'_, Arc<DbState>>) -> AppResult<Vec<Template>> {
    templates::list(&db)
}

/// 变量面板（内置变量 + 未停用的字段）。
#[tauri::command]
pub fn template_variables(db: State<'_, Arc<DbState>>) -> AppResult<Vec<templates::VariableInfo>> {
    let mut out = Vec::new();
    for f in fields::list(&db)? {
        out.push(templates::VariableInfo {
            name: format!("字段.{}", f.key),
            label: f.label,
            required: f.required,
            kind: templates::VariableKind::Field,
        });
    }
    for v in templates::builtin_variables() {
        out.push(v);
    }
    Ok(out)
}

/// 新建一个模板。
#[tauri::command]
pub fn create_template(
    db: State<'_, Arc<DbState>>,
    name: String,
    subject: String,
    body: String,
) -> AppResult<Template> {
    templates::create(&db, &name, &subject, &body)
}

/// 编辑一个模板。
#[tauri::command]
pub fn update_template(
    db: State<'_, Arc<DbState>>,
    id: i64,
    name: String,
    subject: String,
    body: String,
) -> AppResult<()> {
    templates::update(&db, id, &name, &subject, &body)
}

/// 存为副本。
#[tauri::command]
pub fn duplicate_template(db: State<'_, Arc<DbState>>, id: i64) -> AppResult<Template> {
    templates::duplicate(&db, id)
}

/// 设为默认模板。**模板里有未解析变量时拒绝**（设计文档 S10）。
#[tauri::command]
pub fn set_default_template(db: State<'_, Arc<DbState>>, id: i64) -> AppResult<()> {
    let (allowed, known) = templates::whitelists(&db)?;
    templates::set_default(&db, id, &allowed, &known)
}

/// 删除一个模板（默认模板不能删）。
#[tauri::command]
pub fn delete_template(db: State<'_, Arc<DbState>>, id: i64) -> AppResult<()> {
    templates::delete(&db, id)
}

/// 用示例数据渲染一份模板（预览用）。
#[tauri::command]
pub fn preview_template(subject: String, body: String) -> AppResult<Rendered> {
    templates::preview(&subject, &body)
}

/// 校验一份模板里的变量（界面高亮未解析的变量）。
#[tauri::command]
pub fn check_template_variables(
    db: State<'_, Arc<DbState>>,
    subject: String,
    body: String,
) -> AppResult<templates::VariableCheck> {
    let (allowed, known) = templates::whitelists(&db)?;
    let (unknown, empty) = templates::problems_of(&subject, &body, &allowed, &known);
    Ok(templates::VariableCheck { unknown, empty })
}
