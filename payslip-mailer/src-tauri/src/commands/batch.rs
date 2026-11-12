//! 发送批次的 Command（TC-400 · ADR-0023）
//!
//! 编排骨架的入口。**真正的「点发送」在 TC-402**（带二次确认 + 幂等恢复）；
//! 本卡的 Command 是批次的生命周期管理：建（只建不跑）、查、暂停、恢复、中止。

use std::sync::Arc;

use rusqlite::OptionalExtension;
use tauri::State;

use crate::error::{AppError, AppResult};
use crate::infra::batch::{self, Batch, BatchProgress, Job};
use crate::infra::db::DbState;
use crate::infra::keychain::KeychainStore;

/// 从 `app_meta` 读发送速率（`rate_per_min`，默认 30 —— ch.7）。
///
/// TC-500 的 SMTP 设置页写这个键；本卡只保证「读出来的速率真的被桶用了」。
fn rate_from_settings(db: &DbState) -> u32 {
    let raw: Option<String> = db
        .read(|conn| {
            conn.query_row(
                "SELECT value FROM app_meta WHERE key = 'rate_per_min'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|e| crate::error::AppError::Database(e.to_string()))
        })
        .ok()
        .flatten();
    raw.and_then(|s| s.trim().parse::<u32>().ok())
        .filter(|r| (1..=600).contains(r))
        .unwrap_or(crate::domain::ratelimit::DEFAULT_RATE_PER_MIN)
}

/// SMTP 凭据配置好了吗？（TC-500 建 `smtp_settings` 表 + 写 Keychain 之前恒为 false。）
///
/// 探针走「查表」而不是查 Keychain：表都不存在 = 设置页还没交付过 ——
/// 两种「没配置」对使用者是同一句话：「发信设置还没配置」。
fn smtp_configured(db: &DbState) -> bool {
    // TC-500 起：表有行 **且** Keychain 有凭据（infra::mailer::smtp_configured）
    match KeychainStore::login() {
        Ok(store) => crate::infra::mailer::smtp_configured(db, &store),
        Err(_) => false,
    }
}

/// 一个批次及其任务（发送进度屏）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchDetail {
    pub batch: Batch,
    pub jobs: Vec<Job>,
}

/// 批次 + 进度（「已发 27/60」）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchWithProgress {
    pub batch: Batch,
    pub progress: crate::infra::batch::BatchProgress,
}

/// 创建批次（**只建不跑**）。
///
/// 真正的「点发送」在 TC-402 —— 那里有二次确认（红线 3）与中断恢复。
/// 本卡先给范围选择的编排骨架一个能落地的入口。
#[tauri::command]
pub fn create_send_batch(
    db: State<'_, Arc<DbState>>,
    period_id: i64,
    template_id: Option<i64>,
    member_ids: Vec<i64>,
) -> AppResult<Batch> {
    let rate = rate_from_settings(&db);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    batch::create_batch(&db, period_id, template_id, &member_ids, rate, now)
}

/// 列出全部批次（发送历史）。
#[tauri::command]
pub fn list_send_batches(db: State<'_, Arc<DbState>>) -> AppResult<Vec<Batch>> {
    batch::list_batches(&db)
}

/// 一个批次及其全部任务。
#[tauri::command]
pub fn get_send_batch(db: State<'_, Arc<DbState>>, batch_id: i64) -> AppResult<BatchDetail> {
    Ok(BatchDetail {
        jobs: batch::list_jobs(&db, batch_id)?,
        batch: batch::get_batch(&db, batch_id)?,
    })
}

#[tauri::command]
pub fn pause_send_batch(db: State<'_, Arc<DbState>>, batch_id: i64) -> AppResult<()> {
    batch::pause_batch(&db, batch_id)
}

#[tauri::command]
pub fn resume_send_batch(db: State<'_, Arc<DbState>>, batch_id: i64) -> AppResult<()> {
    batch::resume_batch(&db, batch_id)
}

#[tauri::command]
pub fn abort_send_batch(db: State<'_, Arc<DbState>>, batch_id: i64) -> AppResult<()> {
    batch::abort_batch(&db, batch_id)
}

/// **点发送**（TC-402 · 红线 3 的兑现处）。
///
/// 前端已经核对过一次（S11 屏）；这里**再核一遍** —— 数据可能在
/// 确认对话框停留期间被改（ADR-0025 决策 4：Command 层是唯一关卡）。
/// 核对不过 → 拒绝并说清谁被拦。
#[tauri::command]
pub fn start_batch(
    db: State<'_, Arc<DbState>>,
    period_id: i64,
    member_ids: Vec<i64>,
    template_id: Option<i64>,
) -> AppResult<Batch> {
    // Command 层的核对闸
    let (subject, body) = {
        let list = crate::infra::templates::list(&db)?;
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
        (tpl.subject_tpl.clone(), tpl.body_html.clone())
    };
    let report = crate::infra::verify::verify_scope(&db, period_id, &member_ids, &subject, &body)?;
    if !report.ok {
        let names: Vec<String> = report
            .blocked
            .iter()
            .map(|m| {
                let reason = m.blocked_reasons.first().or(m.variable_problems.first());
                format!(
                    "{}（{}）",
                    m.name,
                    reason.map(String::as_str).unwrap_or("核对未通过")
                )
            })
            .collect();
        return Err(AppError::ValidationError(format!(
            "核对没有通过，不能发送：{}。请先处理再发送。",
            names.join("、")
        )));
    }
    let rate = rate_from_settings(&db);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    batch::create_batch(&db, period_id, template_id, &member_ids, rate, now)
}

/// 未完成的批次（工作台提示条 + 编排屏 ——「已发 27/60，是否继续？」）。
#[tauri::command]
pub fn find_unfinished_batch(db: State<'_, Arc<DbState>>) -> AppResult<Option<BatchWithProgress>> {
    Ok(batch::find_unfinished(&db)?.map(|(batch, progress)| BatchWithProgress { batch, progress }))
}

/// 批次进度（编排屏轮询/刷新用 —— TC-500 起换事件推送）。
#[tauri::command]
pub fn get_batch_progress(db: State<'_, Arc<DbState>>, batch_id: i64) -> AppResult<BatchProgress> {
    batch::progress(&db, batch_id)
}

/// 恢复并继续发送。
///
/// **TC-500 之前 SMTP 凭据必然未配置** —— 这里返回明确错误（事实陈述），
/// 绝不假装成功。恢复的持久化语义（sending → queued）已落库；
/// 真发送循环接上后这段代码一行不改（ADR-0025 决策 5）。
#[tauri::command]
pub fn recover_and_run_batch(db: State<'_, Arc<DbState>>, batch_id: i64) -> AppResult<Batch> {
    let recovered = batch::recover_batch(&db, batch_id)?;
    if !smtp_configured(&db) {
        return Err(AppError::ValidationError(
            "发信设置还没配置：批次与任务已恢复到队列，但还不能真正发出邮件。             请先在设置页配置发信邮箱（SMTP 专用密码）。"
                .into(),
        ));
    }
    // **TC-500 接线点已兑现**：真发送器（lettre + rustls）+ 真 drive_batch（ADR-0027 决策 5）。
    let settings = crate::infra::mailer::get_settings(&db)?
        .ok_or_else(|| AppError::ValidationError("发信设置还没配置".into()))?;
    let store = KeychainStore::login()?;
    let mut sender = crate::infra::mailer::SmtpSender::new(settings, store);
    let mut now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    batch::drive_batch(
        &db,
        batch_id,
        &mut sender,
        recovered.rate_per_min as u32,
        &mut now_ms,
    )
}

/// 仅重试失败项（红线 4：只补未成功的，没有整批重发）。
#[tauri::command]
pub fn retry_failed_jobs(db: State<'_, Arc<DbState>>, batch_id: i64) -> AppResult<u32> {
    batch::retry_failed(&db, batch_id)
}
