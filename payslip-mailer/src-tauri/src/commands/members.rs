//! 成员与邮件组的 Command（TC-201 · ADR-0016）
//!
//! **没有删除成员的 Command** —— 要让某人不再收到信 = 停用他（ADR-0016 决策 4）。
//! 邮件组有删除：删组只解除归属，成员本身不动。

use std::sync::Arc;

use tauri::State;

use crate::domain::member::{MemberDraft, PastePreview};
use crate::error::AppResult;
use crate::infra::db::DbState;
use crate::infra::members::{self, Group, Member};

/// 列出全部成员（含停用的 —— 界面要能看见它们是灰的）。
#[tauri::command]
pub fn list_members(db: State<'_, Arc<DbState>>) -> AppResult<Vec<Member>> {
    members::list_members(&db)
}

/// 列出**可发送**的成员（停用的不进入发送范围）。
#[tauri::command]
pub fn list_sendable_members(db: State<'_, Arc<DbState>>) -> AppResult<Vec<Member>> {
    members::list_sendable_members(&db)
}

/// 列出全部邮件组。
#[tauri::command]
pub fn list_groups(db: State<'_, Arc<DbState>>) -> AppResult<Vec<Group>> {
    members::list_groups(&db)
}

/// 名单里的人数。
#[tauri::command]
pub fn member_count(db: State<'_, Arc<DbState>>) -> AppResult<i64> {
    members::member_count(&db)
}

/// 新建一个成员。
#[tauri::command]
pub fn create_member(
    db: State<'_, Arc<DbState>>,
    name: String,
    email: String,
    employee_no: Option<String>,
    group_ids: Vec<i64>,
) -> AppResult<Member> {
    members::create_member(&db, &name, &email, employee_no.as_deref(), &group_ids)
}

/// 编辑一个成员（邮箱可改；历史发送日志仍保留原收件地址）。
#[tauri::command]
pub fn update_member(
    db: State<'_, Arc<DbState>>,
    id: i64,
    name: String,
    email: String,
    employee_no: Option<String>,
    group_ids: Vec<i64>,
) -> AppResult<()> {
    members::update_member(&db, id, &name, &email, employee_no.as_deref(), &group_ids)
}

/// 停用 / 启用一个成员。
#[tauri::command]
pub fn set_member_active(db: State<'_, Arc<DbState>>, id: i64, active: bool) -> AppResult<()> {
    members::set_member_active(&db, id, active)
}

/// 新建一个邮件组。
#[tauri::command]
pub fn create_group(
    db: State<'_, Arc<DbState>>,
    name: String,
    note: Option<String>,
) -> AppResult<Group> {
    members::create_group(&db, &name, note.as_deref())
}

/// 删除一个邮件组（只解除归属，成员不动）。
#[tauri::command]
pub fn delete_group(db: State<'_, Arc<DbState>>, id: i64) -> AppResult<()> {
    members::delete_group(&db, id)
}

/// 预览一段批量粘贴（**不写库**）。
///
/// 设计文档 S05：「并预览去重结果」—— 用户看着预览确认了才真的写进去。
#[tauri::command]
pub fn preview_member_paste(db: State<'_, Arc<DbState>>, text: String) -> AppResult<PastePreview> {
    members::preview_paste_text(&db, &text)
}

/// 把预览通过的成员写进库。
#[tauri::command]
pub fn paste_members(db: State<'_, Arc<DbState>>, drafts: Vec<MemberDraft>) -> AppResult<i64> {
    Ok(members::paste_members(&db, &drafts)? as i64)
}
