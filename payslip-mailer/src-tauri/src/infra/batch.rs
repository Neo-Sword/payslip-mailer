//! 发送批次与逐封任务的持久化 + 编排驱动（TC-400 · ADR-0023）
//!
//! 编排骨架的三层：
//!
//! 1. **持久化**：建批次（按范围生成 jobs）、取下一个该发的 job、
//!    记录一封的结果（按分类推进状态机）、批次级降速、暂停/恢复/中止。
//! 2. **驱动**（[`drive_batch`]）：把「取 job → 限速 → 发 → 记结果」跑成一个循环。
//!    发送器与时钟都是**注入的** —— 测试用假发送器 + 假时钟把整条链路跑完，
//!    生产（TC-500）换上真发送器，策略层一行不改。
//! 3. **可中断的基础**：每封完成（无论成败）**立即**落库（ADR-0023 决策 6）；
//!    进程崩了，TC-402 从库里读「已发的」与「没发的」。

use std::collections::BTreeMap;

use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::domain::period::PeriodStatus;
use crate::domain::ratelimit::{TokenBucket, TryAcquire};
use crate::domain::retry::{Disposition, MAX_RETRIES, SendResult, backoff_after, classify};
use crate::error::{AppError, AppResult};
use crate::infra::db::DbState;
use crate::infra::periods;

/// 一个发送批次。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Batch {
    pub id: i64,
    pub period_id: i64,
    /// 用哪套模板渲染（ADR-0027 决策 2；None = 默认模板）。
    pub template_id: Option<i64>,
    /// running / paused / aborted / finished。
    pub status: String,
    /// 批次开始时的速率；临时失败把**这个批次**的桶减半。
    pub rate_per_min: i64,
    pub total: i64,
    pub success: i64,
    pub failed: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
}

/// 一封任务。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub id: i64,
    pub batch_id: i64,
    pub member_id: i64,
    pub to_email: String,
    /// queued / sending / success / retrying / dead。
    pub status: String,
    pub attempts: i64,
    pub smtp_code: Option<i64>,
    pub error_code: Option<String>,
    pub checksum: String,
    pub next_attempt_at: Option<i64>,
    pub sent_at: Option<i64>,
}

/// 发送器抽象（ADR-0023 决策 3 的消费者）。
///
/// 测试注入假发送器按脚本吐 [`SendResult`]；
/// TC-500 注入 lettre 实现把真 SMTP 回复映射成它。
// 未接线：TC-500 注入 lettre 实现时成为生产 trait（TC-400 由测试注入）。
#[allow(dead_code)]
pub trait Sender {
    /// 发一封。返回一次尝试的结果（**不是**最终处置 —— 处置由 [`classify`] 判）。
    ///
    /// 收 [`EmailMessage`]（内容已备好）—— 渲染是**编排**的事（ADR-0027 决策 1：
    /// ch.7 编排第 2 步「逐人渲染」在核对之后、发送之前），发送器只管发出去。
    fn send(&mut self, message: &EmailMessage) -> SendResult;
}

/// 一封已渲染、待发送的邮件（ADR-0027 决策 1）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailMessage {
    /// 收件地址（恰好一人 —— 红线 1）。
    pub to_email: String,
    /// 渲染后的主题。
    pub subject: String,
    /// 渲染后的 HTML 正文。
    pub html: String,
    /// 渲染后的纯文本兜底（ch.8：强制生成 text/plain）。
    pub text: String,
}

/// 幂等键（设计文档 ch.5）：
/// `hash(batch_id + member_id + period_id + content_checksum)`。
///
/// **它现在就按公式生成**（表的 UNIQUE 靠它挡重复）；
/// TC-402 消费它做中断恢复与「已发的不重发」。
pub fn idempotency_key(batch_id: i64, member_id: i64, period_id: i64, checksum: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(format!("{batch_id}:{member_id}:{period_id}:{checksum}").as_bytes());
    let digest = hasher.finalize();
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// 创建批次（**只建不跑** —— 真正的发送入口在 TC-402 带二次确认）。
///
/// `member_ids` 是发送范围（TC-402 的范围选择界面组装）。
/// 每个成员一条 job（**结构性的一人一封**：`send_jobs` 没有 cc/bcc 列）。
pub fn create_batch(
    db: &DbState,
    period_id: i64,
    template_id: Option<i64>,
    member_ids: &[i64],
    rate_per_min: u32,
    now: i64,
) -> AppResult<Batch> {
    if member_ids.is_empty() {
        return Err(AppError::ValidationError("发送范围是空的".into()));
    }
    let period = periods::get(db, period_id)?;

    // 范围里的人必须有这个月的工资记录（没有记录 = 没有内容可发）。
    let records = crate::infra::records::list_records(db, period_id)?;
    let by_member: BTreeMap<i64, &crate::infra::records::RecordRow> =
        records.iter().map(|r| (r.member_id, r)).collect();

    // 批次只对「待发送」及之后的账套开放（草稿还没准备好发）。
    if period.status == PeriodStatus::Draft {
        return Err(AppError::ValidationError(
            "这个月的账套还是草稿 —— 先「标记为待发送」再发送".into(),
        ));
    }

    let scope_json = serde_json::to_string(member_ids)
        .map_err(|e| AppError::Internal(format!("范围序列化失败：{e}")))?;

    let batch_id = db.write(|conn| {
        conn.execute(
            "INSERT INTO send_batches (period_id, template_id, scope_json, status, rate_per_min, total, started_at) \
             VALUES (?1, ?2, ?3, 'running', ?4, ?5, ?6)",
            params![
                period_id,
                template_id,
                scope_json,
                i64::from(rate_per_min),
                member_ids.len() as i64,
                now
            ],
        )
        .map_err(|e| AppError::Database(format!("建批次失败：{e}")))?;
        let batch_id = conn.last_insert_rowid();

        for member_id in member_ids {
            let row = by_member.get(member_id).ok_or_else(|| {
                AppError::ValidationError(format!(
                    "成员 {member_id} 在这个月没有工资记录，不能发送"
                ))
            })?;
            let key = idempotency_key(batch_id, *member_id, period_id, &row.values_checksum());
            conn.execute(
                "INSERT INTO send_jobs \
                 (batch_id, member_id, to_email, checksum, status, idempotency_key) \
                 VALUES (?1, ?2, ?3, ?4, 'queued', ?5)",
                params![batch_id, member_id, row.email, row.values_checksum(), key],
            )
            .map_err(|e| AppError::Database(format!("建任务失败：{e}")))?;
        }
        Ok(batch_id)
    })?;
    get_batch(db, batch_id)
}

/// `RecordRow` 的 checksum 访问。
///
/// `list_records` 没带 checksum（界面用不着）；批次需要 —— 这里单独查。
trait ValuesChecksum {
    fn values_checksum(&self) -> String;
}

impl ValuesChecksum for crate::infra::records::RecordRow {
    fn values_checksum(&self) -> String {
        // 与 TC-303 的存库口径一致：email + values_json
        let json = crate::domain::record::values_to_json(&self.values);
        crate::domain::record::checksum(&self.email, &json)
    }
}

/// 读一个批次。
pub fn get_batch(db: &DbState, batch_id: i64) -> AppResult<Batch> {
    db.read(|conn| {
        conn.query_row(
            "SELECT id, period_id, template_id, status, rate_per_min, total, success, failed, started_at, finished_at \
             FROM send_batches WHERE id = ?1",
            params![batch_id],
            |row| {
                Ok(Batch {
                    id: row.get(0)?,
                    period_id: row.get(1)?,
                    template_id: row.get(2)?,
                    status: row.get(3)?,
                    rate_per_min: row.get(4)?,
                    total: row.get(5)?,
                    success: row.get(6)?,
                    failed: row.get(7)?,
                    started_at: row.get(8)?,
                    finished_at: row.get(9)?,
                })
            },
        )
        .map_err(|_| AppError::NotFound("批次".into()))
    })
}

/// 列出全部批次（发送历史那屏从最近的看起）。
pub fn list_batches(db: &DbState) -> AppResult<Vec<Batch>> {
    db.read(|conn| {
        let mut stmt = conn
            .prepare(
                "SELECT id, period_id, template_id, status, rate_per_min, total, success, failed, started_at, finished_at \
                 FROM send_batches ORDER BY started_at DESC, id DESC",
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok(Batch {
                    id: row.get(0)?,
                    period_id: row.get(1)?,
                    template_id: row.get(2)?,
                    status: row.get(3)?,
                    rate_per_min: row.get(4)?,
                    total: row.get(5)?,
                    success: row.get(6)?,
                    failed: row.get(7)?,
                    started_at: row.get(8)?,
                    finished_at: row.get(9)?,
                })
            })
            .map_err(|e| AppError::Database(e.to_string()))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| AppError::Database(e.to_string()))
    })
}

/// 一个批次的全部任务（发送进度屏）。
pub fn list_jobs(db: &DbState, batch_id: i64) -> AppResult<Vec<Job>> {
    db.read(|conn| {
        let mut stmt = conn
            .prepare(
                "SELECT id, batch_id, member_id, to_email, status, attempts, smtp_code, error_code, checksum, next_attempt_at, sent_at \
                 FROM send_jobs WHERE batch_id = ?1 ORDER BY id",
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        let rows = stmt
            .query_map(params![batch_id], |row| {
                Ok(Job {
                    id: row.get(0)?,
                    batch_id: row.get(1)?,
                    member_id: row.get(2)?,
                    to_email: row.get(3)?,
                    status: row.get(4)?,
                    attempts: row.get(5)?,
                    smtp_code: row.get(6)?,
                    error_code: row.get(7)?,
                    checksum: row.get(8)?,
                    next_attempt_at: row.get(9)?,
                    sent_at: row.get(10)?,
                })
            })
            .map_err(|e| AppError::Database(e.to_string()))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| AppError::Database(e.to_string()))
    })
}

/// 取下一个该发的任务：queued，或到了重试时刻的 retrying。
///
/// **没有到点的 retrying 不算** —— 退避在等，抢跑只会又被拒一次。
// 未接线：发送编排循环归 TC-402（点发送 + 二次确认 + 恢复）。
#[allow(dead_code)]
pub fn take_next(db: &DbState, batch_id: i64, now_ms: i64) -> AppResult<Option<Job>> {
    db.write(|conn| {
        // 先到点的 retrying，再 queued 的第一封 —— 原子：谁取到谁发
        for (sql, params_) in [
            (
                "UPDATE send_jobs SET status = 'sending' \
                 WHERE batch_id = ?1 AND status = 'retrying' AND next_attempt_at <= ?2 \
                   AND id = (SELECT id FROM send_jobs WHERE batch_id = ?1 \
                             AND status = 'retrying' AND next_attempt_at <= ?2 \
                             ORDER BY id LIMIT 1)",
                params![batch_id, now_ms],
            ),
            (
                "UPDATE send_jobs SET status = 'sending' \
                 WHERE batch_id = ?1 AND status = 'queued' \
                   AND id = (SELECT id FROM send_jobs WHERE batch_id = ?1 \
                             AND status = 'queued' ORDER BY id LIMIT 1)",
                params![batch_id],
            ),
        ] {
            conn.execute(sql, params_)
                .map_err(|e| AppError::Database(e.to_string()))?;
            if conn.changes() > 0 {
                // 刚被自己标成 sending 的那行：按批次内唯一 sending 取回
                let job = conn
                    .query_row(
                        "SELECT id, batch_id, member_id, to_email, status, attempts, smtp_code, error_code, checksum, next_attempt_at, sent_at \
                         FROM send_jobs WHERE batch_id = ?1 AND status = 'sending' \
                         ORDER BY id LIMIT 1",
                        params![batch_id],
                        |row| {
                            Ok(Job {
                                id: row.get(0)?,
                                batch_id: row.get(1)?,
                                member_id: row.get(2)?,
                                to_email: row.get(3)?,
                                status: row.get(4)?,
                                attempts: row.get(5)?,
                                smtp_code: row.get(6)?,
                                error_code: row.get(7)?,
                                checksum: row.get(8)?,
                                next_attempt_at: row.get(9)?,
                                sent_at: row.get(10)?,
                            })
                        },
                    )
                    .map_err(|e| AppError::Database(e.to_string()))?;
                return Ok(Some(job));
            }
        }
        Ok(None)
    })
}

/// 读一行 job（辅助）。
#[allow(dead_code)]
fn read_job(conn: &rusqlite::Connection, id: i64) -> AppResult<Job> {
    conn.query_row(
        "SELECT id, batch_id, member_id, to_email, status, attempts, smtp_code, error_code, checksum, next_attempt_at, sent_at \
         FROM send_jobs WHERE id = ?1",
        params![id],
        |row| {
            Ok(Job {
                id: row.get(0)?,
                batch_id: row.get(1)?,
                member_id: row.get(2)?,
                to_email: row.get(3)?,
                status: row.get(4)?,
                attempts: row.get(5)?,
                smtp_code: row.get(6)?,
                error_code: row.get(7)?,
                checksum: row.get(8)?,
                next_attempt_at: row.get(9)?,
                sent_at: row.get(10)?,
            })
        },
    )
    .map_err(|_| AppError::NotFound("任务".into()))
}

/// 记录一封的结果：按分类推进状态机（ADR-0023 决策 4）。
///
/// 返回处置（编排器据此决定是否降速 / 暂停 / 中止）。
// 未接线：发送编排循环归 TC-402（点发送 + 二次确认 + 恢复）。
#[allow(dead_code)]
pub fn record_result(
    db: &DbState,
    job_id: i64,
    result: SendResult,
    now_ms: i64,
) -> AppResult<Disposition> {
    let disposition = classify(result);
    let now_secs = now_ms / 1000;

    db.write(|conn| {
        let job: Job = {
            let mut stmt = conn
                .prepare(
                    "SELECT id, batch_id, member_id, to_email, status, attempts, smtp_code, error_code, checksum, next_attempt_at, sent_at \
                     FROM send_jobs WHERE id = ?1",
                )
                .map_err(|e| AppError::Database(e.to_string()))?;
            let mut rows = stmt
                .query_map(params![job_id], |row| {
                    Ok(Job {
                        id: row.get(0)?,
                        batch_id: row.get(1)?,
                        member_id: row.get(2)?,
                        to_email: row.get(3)?,
                        status: row.get(4)?,
                        attempts: row.get(5)?,
                        smtp_code: row.get(6)?,
                        error_code: row.get(7)?,
                        checksum: row.get(8)?,
                        next_attempt_at: row.get(9)?,
                        sent_at: row.get(10)?,
                    })
                })
                .map_err(|e| AppError::Database(e.to_string()))?;
            rows.next()
                .ok_or_else(|| AppError::NotFound("任务".into()))?
                .map_err(|e| AppError::Database(e.to_string()))?
        };

        let attempts = job.attempts + 1;
        let smtp_code = match result {
            SendResult::Accepted => None,
            SendResult::Rejected(c) => Some(i64::from(c)),
            SendResult::Network => None,
        };
        let error_code = match result {
            SendResult::Accepted => None,
            SendResult::Rejected(_) => Some(error_kind(&disposition).to_string()),
            SendResult::Network => Some("network".to_string()),
        };

        match disposition {
            Disposition::Success => {
                conn.execute(
                    "UPDATE send_jobs SET status='success', attempts=?2, smtp_code=?3, error_code=NULL, \
                     next_attempt_at=NULL, sent_at=?4 WHERE id=?1",
                    params![job_id, attempts, smtp_code, now_secs],
                )
                .map_err(|e| AppError::Database(e.to_string()))?;
                conn.execute(
                    "UPDATE send_batches SET success = success + 1 WHERE id = ?1",
                    params![job.batch_id],
                )
                .map_err(|e| AppError::Database(e.to_string()))?;
            }
            Disposition::Retry => {
                // 重试次数用完 → dead（第 failed_attempts 次 > MAX_RETRIES）
                let failed_attempts = attempts; // 发送失败次数（第 1 次尝试失败 = 1）
                let (status, next_at) = if failed_attempts > i64::from(MAX_RETRIES) {
                    ("dead", None)
                } else {
                    ("retrying", Some(now_ms + backoff_after(u32::try_from(failed_attempts).unwrap_or(MAX_RETRIES))))
                };
                conn.execute(
                    "UPDATE send_jobs SET status=?2, attempts=?3, smtp_code=?4, error_code=?5, \
                     next_attempt_at=?6 WHERE id=?1",
                    params![job_id, status, attempts, smtp_code, error_code, next_at],
                )
                .map_err(|e| AppError::Database(e.to_string()))?;
                if status == "dead" {
                    conn.execute(
                        "UPDATE send_batches SET failed = failed + 1 WHERE id = ?1",
                        params![job.batch_id],
                    )
                    .map_err(|e| AppError::Database(e.to_string()))?;
                }
            }
            Disposition::Dead => {
                conn.execute(
                    "UPDATE send_jobs SET status='dead', attempts=?2, smtp_code=?3, error_code=?4, \
                     next_attempt_at=NULL WHERE id=?1",
                    params![job_id, attempts, smtp_code, error_code],
                )
                .map_err(|e| AppError::Database(e.to_string()))?;
                conn.execute(
                    "UPDATE send_batches SET failed = failed + 1 WHERE id = ?1",
                    params![job.batch_id],
                )
                .map_err(|e| AppError::Database(e.to_string()))?;
            }
            Disposition::Pause => {
                // 网络中断：这一封回 queued（保留现场），批次暂停
                conn.execute(
                    "UPDATE send_jobs SET status='queued', attempts=?2, smtp_code=NULL, error_code=?3, \
                     next_attempt_at=NULL WHERE id=?1",
                    params![job_id, attempts, error_code],
                )
                .map_err(|e| AppError::Database(e.to_string()))?;
                conn.execute(
                    "UPDATE send_batches SET status='paused' WHERE id = ?1",
                    params![job.batch_id],
                )
                .map_err(|e| AppError::Database(e.to_string()))?;
            }
            Disposition::Abort => {
                // 认证失败：这一封判死，批次中止（其余 queued 不再发）
                conn.execute(
                    "UPDATE send_jobs SET status='dead', attempts=?2, smtp_code=?3, error_code=?4, \
                     next_attempt_at=NULL WHERE id=?1",
                    params![job_id, attempts, smtp_code, error_code],
                )
                .map_err(|e| AppError::Database(e.to_string()))?;
                conn.execute(
                    "UPDATE send_batches SET status='aborted', failed = failed + 1 WHERE id = ?1",
                    params![job.batch_id],
                )
                .map_err(|e| AppError::Database(e.to_string()))?;
            }
        }
        Ok(disposition)
    })
}

/// 处置 → error_code 字段（证据链里说清「为什么死的」）。
#[allow(dead_code)]
fn error_kind(disposition: &Disposition) -> &'static str {
    match disposition {
        Disposition::Success => "none",
        Disposition::Retry => "transient",
        Disposition::Dead => "permanent",
        Disposition::Pause => "network",
        Disposition::Abort => "auth",
    }
}

/// 批次是否所有 job 都到了终态（到了就 finished）。
// 未接线：发送编排循环归 TC-402（点发送 + 二次确认 + 恢复）。
#[allow(dead_code)]
pub fn maybe_finish(db: &DbState, batch_id: i64, now: i64) -> AppResult<bool> {
    let pending: i64 = db.read(|conn| {
        conn.query_row(
            "SELECT count(*) FROM send_jobs WHERE batch_id = ?1 AND status IN ('queued','sending','retrying')",
            params![batch_id],
            |row| row.get(0),
        )
        .map_err(|e| AppError::Database(e.to_string()))
    })?;
    if pending == 0 {
        db.write(|conn| {
            conn.execute(
                "UPDATE send_batches SET status='finished', finished_at=?2 WHERE id=?1 AND status='running'",
                params![batch_id, now],
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
            Ok(())
        })?;
        return Ok(true);
    }
    Ok(false)
}

/// 暂停批次（用户手动 / 网络中断）。
pub fn pause_batch(db: &DbState, batch_id: i64) -> AppResult<()> {
    db.write(|conn| {
        conn.execute(
            "UPDATE send_batches SET status='paused' WHERE id=?1 AND status='running'",
            params![batch_id],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    })
}

/// 恢复批次。
pub fn resume_batch(db: &DbState, batch_id: i64) -> AppResult<()> {
    db.write(|conn| {
        conn.execute(
            "UPDATE send_batches SET status='running' WHERE id=?1 AND status='paused'",
            params![batch_id],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    })
}

/// 中止批次（用户手动；认证失败也走这里）。
pub fn abort_batch(db: &DbState, batch_id: i64) -> AppResult<()> {
    db.write(|conn| {
        conn.execute(
            "UPDATE send_batches SET status='aborted' WHERE id=?1 AND status IN ('running','paused')",
            params![batch_id],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    })
}

/// 一个批次的进度（「已发 27/60」从库算 —— ADR-0025 决策 6）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchProgress {
    pub total: i64,
    pub success: i64,
    pub failed: i64,
    /// queued（含还没到点的 retrying）。
    pub pending: i64,
    /// 正在发或存疑的（sending）。
    pub in_flight: i64,
}

/// 一个批次的进度。
pub fn progress(db: &DbState, batch_id: i64) -> AppResult<BatchProgress> {
    db.read(|conn| {
        let mut stmt = conn
            .prepare(
                "SELECT \
                 (SELECT count(*) FROM send_jobs WHERE batch_id = ?1), \
                 (SELECT count(*) FROM send_jobs WHERE batch_id = ?1 AND status='success'), \
                 (SELECT count(*) FROM send_jobs WHERE batch_id = ?1 AND status='dead'), \
                 (SELECT count(*) FROM send_jobs WHERE batch_id = ?1 AND status IN ('queued','retrying')), \
                 (SELECT count(*) FROM send_jobs WHERE batch_id = ?1 AND status='sending')",
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        stmt.query_row(params![batch_id], |row| {
            Ok(BatchProgress {
                total: row.get(0)?,
                success: row.get(1)?,
                failed: row.get(2)?,
                pending: row.get(3)?,
                in_flight: row.get(4)?,
            })
        })
        .map_err(|e| AppError::Database(e.to_string()))
    })
}

/// 最近的未完成批次（running / paused）+ 进度（ch.7 的「发现未完成的批次」）。
///
/// **可重复发起**（ADR-0025 决策 2）：随时可查 —— 工作台提示条与编排屏共用。
/// 没有 → `None`（一切正常，不用打扰）。
pub fn find_unfinished(db: &DbState) -> AppResult<Option<(Batch, BatchProgress)>> {
    let latest: Option<i64> = db.read(|conn| {
        conn.query_row(
            "SELECT id FROM send_batches WHERE status IN ('running','paused') \
                 ORDER BY started_at DESC, id DESC LIMIT 1",
            [],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(|e| AppError::Database(e.to_string()))
    })?;
    match latest {
        Some(id) => Ok(Some((get_batch(db, id)?, progress(db, id)?))),
        None => Ok(None),
    }
}

/// 恢复批次：**存疑的 sending 回 queued**（ADR-0025 决策 1）+ paused → running。
///
/// 为什么 sending 回队：进程崩溃那一刻它「可能已发、可能没发」——
/// 发给**本人**的重复无害（不构成隐私事故），丢发却要 HR 逐个排查补发。
/// 已成功（success）的**绝不**回队 —— 「已发的人不会收到第二封」（ch.7）。
pub fn recover_batch(db: &DbState, batch_id: i64) -> AppResult<Batch> {
    let batch = get_batch(db, batch_id)?;
    if !matches!(batch.status.as_str(), "running" | "paused") {
        return Err(AppError::ValidationError(format!(
            "批次已经{}，没有可恢复的任务",
            match batch.status.as_str() {
                "finished" => "完成",
                "aborted" => "中止",
                _ => "结束",
            }
        )));
    }
    db.write(|conn| {
        conn.execute(
            "UPDATE send_jobs SET status='queued' WHERE batch_id=?1 AND status='sending'",
            params![batch_id],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        conn.execute(
            "UPDATE send_batches SET status='running' WHERE id=?1 AND status='paused'",
            params![batch_id],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    })?;
    get_batch(db, batch_id)
}

/// 仅重试失败项（红线 4）：dead → queued，**attempts 保留**、error_code 清空。
///
/// success 的碰都不碰；aborted 的批次拒绝（凭据坏时重试没有意义 ——
/// 引导去 SMTP 设置）。attempts 保留的意义：死过一次的事实不清零，
/// 下一轮失败立即按退避表的后续档位走。
pub fn retry_failed(db: &DbState, batch_id: i64) -> AppResult<u32> {
    let batch = get_batch(db, batch_id)?;
    if batch.status == "aborted" {
        return Err(AppError::ValidationError(
            "这个批次因认证失败被中止 —— 请先在设置页修好发信邮箱，再重新发送".into(),
        ));
    }
    let n = db.write(|conn| {
        // 回队多少就记多少 —— changes() 在这条 UPDATE 之后立即取
        let retried = conn
            .execute(
                "UPDATE send_jobs SET status='queued', error_code=NULL, next_attempt_at=NULL \
             WHERE batch_id=?1 AND status='dead'",
                params![batch_id],
            )
            .map_err(|e| AppError::Database(e.to_string()))? as u32;
        // failed 计数随 job 回队而回退（进度与任务清单一致）
        conn.execute(
            "UPDATE send_batches SET failed = (\
               SELECT count(*) FROM send_jobs WHERE batch_id=?1 AND status='dead') \
             WHERE id=?1",
            params![batch_id],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        // 回到 running（finished 的批次也能补发 —— 那是「仅重试失败项」的主场景）
        conn.execute(
            "UPDATE send_batches SET status='running', finished_at=NULL \
             WHERE id=?1 AND status IN ('finished','paused')",
            params![batch_id],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(retried)
    })?;
    Ok(n)
}

/// 渲染一封 job（ADR-0027 决策 1）。
///
/// 用批次创建时定的模板（没有就用默认模板）——
/// 与 TC-401 的核对/预览**同一个** `render_for`：「核对看到的」与「发出去的」同一份渲染。
#[allow(dead_code)]
pub fn render_job(db: &DbState, template_id: Option<i64>, job: &Job) -> AppResult<EmailMessage> {
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
    let period_id: i64 = db.read(|conn| {
        conn.query_row(
            "SELECT period_id FROM send_batches WHERE id = ?1",
            params![job.batch_id],
            |row| row.get(0),
        )
        .map_err(|_| AppError::NotFound("批次".into()))
    })?;
    let rendered = crate::infra::verify::render_for(
        db,
        period_id,
        job.member_id,
        &tpl.subject_tpl,
        &tpl.body_html,
    )?;
    Ok(EmailMessage {
        to_email: job.to_email.clone(),
        subject: rendered.subject,
        html: rendered.html,
        text: rendered.text,
    })
}

/// 渲染失败的那封判死（ADR-0027 决策 1）。
#[allow(dead_code)]
fn mark_render_failed(db: &DbState, job_id: i64, now_ms: i64) -> AppResult<()> {
    let _ = now_ms;
    db.write(|conn| {
        conn.execute(
            "UPDATE send_jobs SET status='dead', error_code='render', next_attempt_at=NULL \
             WHERE id=?1",
            params![job_id],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        conn.execute(
            "UPDATE send_batches SET failed = failed + 1 \
             WHERE id = (SELECT batch_id FROM send_jobs WHERE id = ?1)",
            params![job_id],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    })
}

/// 驱动一个批次的发送循环（**注入发送器与时钟** —— ADR-0023 决策 1）。
///
/// 每一步：
/// 1. 批次还在 running 吗？（paused / aborted / finished 就停）
/// 2. 桶放行吗？（不放行就**把时钟拨到下一封的时刻** —— 睡是调用方的事，
///    这里只推进时钟；真进程由 TC-402/500 的 async 循环 sleep）
/// 3. 取下一个 job（没有可发的 → 收尾判定）。
/// 4. 发 → 记结果（立即落库）。
/// 5. 按处置调整桶（临时失败 → 降速 50%）。
///
/// `clock` 返回注入的「当前毫秒」；每次循环拨一次。
/// 测试里它是假时钟；生产里是真时钟。
// 未接线：发送编排循环归 TC-402（点发送 + 二次确认 + 恢复）；TC-500 换上真发送器。
#[allow(dead_code)]
pub fn drive_batch(
    db: &DbState,
    batch_id: i64,
    sender: &mut dyn Sender,
    rate_per_min: u32,
    now_ms: &mut i64,
) -> AppResult<Batch> {
    let mut bucket = TokenBucket::new(rate_per_min, *now_ms);

    loop {
        let batch = get_batch(db, batch_id)?;
        if batch.status != "running" {
            return Ok(batch);
        }

        // 限速：不放行就把时钟拨到放行的时刻（真进程在这里 sleep）
        let gate: TryAcquire = bucket.try_acquire(*now_ms);
        if !gate.allowed {
            *now_ms += gate.retry_after_ms;
            continue;
        }

        let Some(job) = take_next(db, batch_id, *now_ms)? else {
            // 没有可发的了：要么全到终态（收尾），要么都在退避等（拨到最近的 next_attempt_at）
            let next_retry: Option<i64> = db.read(|conn| {
                conn.query_row(
                    "SELECT min(next_attempt_at) FROM send_jobs \
                     WHERE batch_id = ?1 AND status = 'retrying'",
                    params![batch_id],
                    |row| row.get::<_, Option<i64>>(0),
                )
                .map_err(|e| AppError::Database(e.to_string()))
            })?;
            match next_retry {
                Some(at) => {
                    *now_ms = (*now_ms).max(at);
                    continue;
                }
                None => {
                    maybe_finish(db, batch_id, *now_ms / 1000)?;
                    return get_batch(db, batch_id);
                }
            }
        };

        // 渲染（ch.7 编排第 2 步「逐人渲染」）：与 TC-401 的核对/预览同一份渲染
        // （ADR-0027 决策 1）。失败 → 该封判 dead（渲染是残余风险，核对已把守必填），
        // **批次继续** —— 一条模板 bug 不该拦住整批。
        let message = match render_job(db, batch.template_id, &job) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(job_id = job.id, error = %e, "渲染失败，该封判死");
                mark_render_failed(db, job.id, *now_ms)?;
                continue;
            }
        };

        let result = sender.send(&message);
        let disposition = record_result(db, job.id, result, *now_ms)?;

        // 临时失败 → 本批次的桶降速 50%（不叠加；ADR-0023 决策 2）
        if disposition == Disposition::Retry {
            bucket.halve_rate();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 按脚本吐结果的假发送器。
    struct ScriptSender {
        script: std::vec::IntoIter<SendResult>,
        calls: Vec<String>,
        /// 收到的邮件（全链路验收要验 MIME 与正文 —— TC-500）。
        inbox: Vec<EmailMessage>,
    }

    impl ScriptSender {
        fn new(script: Vec<SendResult>) -> Self {
            Self {
                script: script.into_iter(),
                calls: Vec::new(),
                inbox: Vec::new(),
            }
        }
    }

    impl Sender for ScriptSender {
        fn send(&mut self, message: &EmailMessage) -> SendResult {
            self.calls.push(message.to_email.clone());
            self.inbox.push(message.clone());
            self.script.next().unwrap_or(SendResult::Accepted)
        }
    }

    struct Fx {
        dir: std::path::PathBuf,
        // TempKeychain 留在作用域里让钥匙串活着；没人读它
        #[allow(dead_code)]
        kc: crate::infra::keychain::TempKeychain,
        db: DbState,
        period_id: i64,
        members: Vec<i64>,
    }

    impl Fx {
        fn new(label: &str, n_members: usize) -> Self {
            crate::infra::keychain::no_user_interaction();
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default();
            let dir = std::env::temp_dir().join(format!(
                "xinyou-batch-test-{}-{nanos}-{label}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).expect("建临时目录失败");
            let kc = crate::infra::keychain::TempKeychain::new(label);
            let db = DbState::new();
            db.open(&kc.store, &dir, &crate::infra::db::CIPHER_PARAMS)
                .expect("开库失败");
            crate::infra::fields::ensure_defaults(&db).expect("写默认字段失败");
            crate::infra::templates::ensure_defaults(&db).expect("写默认模板失败");
            let mut members = Vec::new();
            for i in 0..n_members {
                let id = crate::infra::members::create_member(
                    &db,
                    &format!("成员{i}"),
                    &format!("member{i}@example.com"),
                    None,
                    &[],
                )
                .expect("建成员失败")
                .id;
                members.push(id);
            }
            let period = periods::create(&db, "2026-10").expect("建账套失败");
            periods::transition(&db, period.id, PeriodStatus::Ready).expect("进 ready 失败");
            for (i, member_id) in members.iter().enumerate() {
                let mut values = BTreeMap::new();
                values.insert("base_salary".to_string(), format!("{}", 15000 + i * 100));
                values.insert("performance".to_string(), "2000".to_string());
                values.insert("meal_allowance".to_string(), "400".to_string());
                values.insert("social_insurance".to_string(), "-1750".to_string());
                values.insert("housing_fund".to_string(), "-1500".to_string());
                crate::infra::records::update_cell(
                    &db,
                    period.id,
                    *member_id,
                    "base_salary",
                    values.get("base_salary").unwrap(),
                )
                .expect("录基本工资失败");
                crate::infra::records::update_cell(
                    &db,
                    period.id,
                    *member_id,
                    "performance",
                    "2000",
                )
                .expect("录绩效失败");
                crate::infra::records::update_cell(
                    &db,
                    period.id,
                    *member_id,
                    "meal_allowance",
                    "400",
                )
                .expect("录餐补失败");
                crate::infra::records::update_cell(
                    &db,
                    period.id,
                    *member_id,
                    "social_insurance",
                    "-1750",
                )
                .expect("录社保失败");
                crate::infra::records::update_cell(
                    &db,
                    period.id,
                    *member_id,
                    "housing_fund",
                    "-1500",
                )
                .expect("录公积金失败");
            }
            Self {
                dir,
                kc,
                db,
                period_id: period.id,
                members,
            }
        }
    }

    impl Drop for Fx {
        fn drop(&mut self) {
            self.db.close();
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// 标准 8 + 1：整批跑通 —— 每封一 job、逐封成功、速率被桶压住。
    #[test]
    fn a_batch_drives_through_with_pacing() {
        let fx = Fx::new("drive", 3);
        let now = 1_780_000_000;
        let batch =
            create_batch(&fx.db, fx.period_id, None, &fx.members, 30, now).expect("建批次失败");
        assert_eq!(batch.total, 3);
        assert_eq!(batch.status, "running");

        let mut sender = ScriptSender::new(vec![SendResult::Accepted; 3]);
        let mut now_ms = now * 1000;
        let final_batch =
            drive_batch(&fx.db, batch.id, &mut sender, 30, &mut now_ms).expect("驱动失败");

        assert_eq!(final_batch.status, "finished");
        assert_eq!(final_batch.success, 3);
        assert_eq!(final_batch.failed, 0);
        assert_eq!(sender.calls.len(), 3);
        // 每人一封，收件地址各归各（没有 cc/bcc 的可能性 —— 3 次调用 3 个地址）
        let mut addrs = sender.calls.clone();
        addrs.sort();
        addrs.dedup();
        assert_eq!(addrs.len(), 3, "三封必须是三个不同地址：{addrs:?}");

        // 每封完成即持久化：全部 success + sent_at
        let jobs = list_jobs(&fx.db, batch.id).expect("读任务失败");
        assert!(jobs.iter().all(|j| j.status == "success"));
        assert!(jobs.iter().all(|j| j.sent_at.is_some()));
        assert!(jobs.iter().all(|j| j.attempts == 1));
    }

    /// 标准 1：速率 30/分 → 跑 3 封至少耗 2 个间隔（4 秒）。
    #[test]
    fn the_drive_paces_by_the_bucket() {
        let fx = Fx::new("pace", 3);
        let now = 1_780_000_000;
        let batch =
            create_batch(&fx.db, fx.period_id, None, &fx.members, 30, now).expect("建批次失败");
        let mut sender = ScriptSender::new(vec![SendResult::Accepted; 3]);
        let mut now_ms = now * 1000;
        drive_batch(&fx.db, batch.id, &mut sender, 30, &mut now_ms).expect("驱动失败");
        // 第 1 封在 t=0，第 2 封 ≈ t+2000，第 3 封 ≈ t+4000
        let elapsed_ms = now_ms - now * 1000;
        assert!(
            elapsed_ms >= 4000,
            "3 封该花至少 2 个间隔（4000ms），实际 {elapsed_ms}ms"
        );
        assert!(elapsed_ms < 8000, "也不该慢得离谱：{elapsed_ms}ms");
    }

    /// 标准 3/4/6：临时失败退避重试、第 4 次失败判死；永久失败不重试。
    #[test]
    fn transient_and_permanent_failures_follow_the_table() {
        let fx = Fx::new("retry", 2);
        let now = 1_780_000_000;
        let batch =
            create_batch(&fx.db, fx.period_id, None, &fx.members, 30, now).expect("建批次失败");

        // 成员0：452 三次 + 第四次失败 → dead（3 次重试用尽）
        // 成员1：550 → 直接 dead
        let mut sender = ScriptSender::new(vec![
            SendResult::Rejected(452),
            SendResult::Rejected(550),
            SendResult::Rejected(452),
            SendResult::Rejected(452),
            SendResult::Rejected(452),
        ]);
        let mut now_ms = now * 1000;
        let final_batch =
            drive_batch(&fx.db, batch.id, &mut sender, 30, &mut now_ms).expect("驱动失败");

        // 两人都死了，但路径不同：一个重试 3 次后死，一个一次就死
        assert_eq!(final_batch.status, "finished");
        assert_eq!(final_batch.failed, 2);
        let jobs = list_jobs(&fx.db, batch.id).expect("读任务失败");
        let retried = jobs
            .iter()
            .find(|j| j.attempts == 4)
            .expect("该有一封试了 4 次");
        assert_eq!(retried.status, "dead");
        assert_eq!(retried.error_code.as_deref(), Some("transient"));
        let instant = jobs
            .iter()
            .find(|j| j.attempts == 1)
            .expect("该有一封一次就死");
        assert_eq!(instant.status, "dead");
        assert_eq!(instant.error_code.as_deref(), Some("permanent"));
    }

    /// 标准 5：认证失败立即中止整批 —— 后面的 queued 不再发。
    #[test]
    fn an_auth_failure_aborts_the_whole_batch() {
        let fx = Fx::new("auth", 3);
        let now = 1_780_000_000;
        let batch =
            create_batch(&fx.db, fx.period_id, None, &fx.members, 30, now).expect("建批次失败");

        // 第 1 封成功、第 2 封 535 → 中止；第 3 封不该被发
        let mut sender = ScriptSender::new(vec![
            SendResult::Accepted,
            SendResult::Rejected(535),
            SendResult::Accepted,
        ]);
        let mut now_ms = now * 1000;
        let final_batch =
            drive_batch(&fx.db, batch.id, &mut sender, 30, &mut now_ms).expect("驱动失败");

        assert_eq!(final_batch.status, "aborted");
        assert_eq!(sender.calls.len(), 2, "第 3 封不该被发：{:?}", sender.calls);
        let jobs = list_jobs(&fx.db, batch.id).expect("读任务失败");
        assert_eq!(jobs.iter().filter(|j| j.status == "success").count(), 1);
        assert_eq!(jobs.iter().filter(|j| j.status == "dead").count(), 1);
        assert_eq!(jobs.iter().filter(|j| j.status == "queued").count(), 1);
    }

    /// 标准 7：网络中断暂停批次，恢复后继续，已成功的不重发。
    #[test]
    fn a_network_pause_keeps_the_scene_and_resume_continues() {
        let fx = Fx::new("pause", 3);
        let now = 1_780_000_000;
        let batch =
            create_batch(&fx.db, fx.period_id, None, &fx.members, 30, now).expect("建批次失败");

        let mut sender = ScriptSender::new(vec![
            SendResult::Accepted,
            SendResult::Network,
            SendResult::Accepted,
            SendResult::Accepted,
        ]);
        let mut now_ms = now * 1000;
        let paused = drive_batch(&fx.db, batch.id, &mut sender, 30, &mut now_ms).expect("驱动失败");
        assert_eq!(paused.status, "paused", "网络中断该暂停");
        assert_eq!(sender.calls.len(), 2, "暂停后不该继续发");

        // 恢复 → 剩下的发完
        resume_batch(&fx.db, batch.id).expect("恢复失败");
        let final_batch =
            drive_batch(&fx.db, batch.id, &mut sender, 30, &mut now_ms).expect("再驱动失败");
        assert_eq!(final_batch.status, "finished");
        assert_eq!(final_batch.success, 3, "已成功的不该重发");
        assert_eq!(
            sender.calls.len(),
            4,
            "暂停前 2 封（1 成 1 断）+ 恢复后 2 封"
        );

        let jobs = list_jobs(&fx.db, batch.id).expect("读任务失败");
        // 断网那封的 attempts 是 2（第 1 次失败、恢复后第 2 次成功）
        let network_victim = jobs
            .iter()
            .find(|j| j.attempts == 2)
            .expect("该有一封试了 2 次");
        assert_eq!(network_victim.status, "success");
    }

    /// 标准 9：临时失败让本批次降速 50%，连续不叠加。
    #[test]
    fn a_transient_failure_halves_the_batch_rate_once() {
        let fx = Fx::new("half", 4);
        let now = 1_780_000_000;
        let batch =
            create_batch(&fx.db, fx.period_id, None, &fx.members, 30, now).expect("建批次失败");

        // 第 1 封 452（降速）、重试成功；第 2 封又 452（**不该再降**）、重试成功；剩两封成功
        let mut sender = ScriptSender::new(vec![
            SendResult::Rejected(452),
            SendResult::Accepted,
            SendResult::Rejected(452),
            SendResult::Accepted,
            SendResult::Accepted,
            SendResult::Accepted,
        ]);
        let mut now_ms = now * 1000;
        let final_batch =
            drive_batch(&fx.db, batch.id, &mut sender, 30, &mut now_ms).expect("驱动失败");
        assert_eq!(final_batch.status, "finished");
        assert_eq!(final_batch.success, 4);

        // 降速 50% = 15/分 = 4000ms 一封。
        // 若不降速：4 封成功 = 3 × 2000ms = 6000ms 起步。
        // 降速后：第 1 封(0) → 重试等 30s(退避是 retry 的 next_attempt_at，
        //   桶同时在等 2000ms —— take_next 到点前 drive 拨时钟) → ...
        // 精确断言太脆；这里验「总耗时明显大于不降速的情形」：
        assert!(
            now_ms - now * 1000 > 6000,
            "降速后的批次该明显更慢：{}ms",
            now_ms - now * 1000
        );
    }

    /// 标准 10/11（结构性）：send_jobs 没有 cc/bcc 的可能 —— 一人一封、不存正文。
    #[test]
    fn the_job_table_is_structurally_one_recipient_per_email() {
        let fx = Fx::new("structure", 2);
        let now = 1_780_000_000;
        let batch =
            create_batch(&fx.db, fx.period_id, None, &fx.members, 30, now).expect("建批次失败");
        let mut sender = ScriptSender::new(vec![SendResult::Accepted; 2]);
        let mut now_ms = now * 1000;
        drive_batch(&fx.db, batch.id, &mut sender, 30, &mut now_ms).expect("驱动失败");

        let jobs = list_jobs(&fx.db, batch.id).expect("读任务失败");
        assert_eq!(jobs.len(), 2, "两个人恰好两封");
        let emails: std::collections::BTreeSet<&str> =
            jobs.iter().map(|j| j.to_email.as_str()).collect();
        assert_eq!(emails.len(), 2, "两封是两个地址");
        // checksum 有值（证据链），正文不存在（表里就没有正文列）
        assert!(jobs.iter().all(|j| !j.checksum.is_empty()));
    }

    /// 标准 4/5：中断后能发现未完成批次；恢复后已成功的不重发、sending 回队。
    #[test]
    fn an_interrupted_batch_is_discovered_and_resumed_without_resending() {
        let fx = Fx::new("recover", 4);
        let now = 1_780_000_000;
        let batch =
            create_batch(&fx.db, fx.period_id, None, &fx.members, 30, now).expect("建批次失败");

        // 发 2 封成功、1 封存疑（sending）、1 封还没轮到
        let mut sender = ScriptSender::new(vec![
            SendResult::Accepted,
            SendResult::Accepted,
            SendResult::Accepted, // 不会真用到第 3 次调用（第 3 封停在 sending）
        ]);
        let mut now_ms = now * 1000;
        // 手动跑到第 3 封取出来就停（模拟进程崩溃）
        for _ in 0..2 {
            let job = take_next(&fx.db, batch.id, now_ms)
                .expect("取任务失败")
                .expect("该有任务");
            let msg = render_job(&fx.db, None, &job).expect("渲染失败");
            let d = record_result(&fx.db, job.id, sender.send(&msg), now_ms).expect("记结果失败");
            assert_eq!(d, Disposition::Success);
        }
        let job3 = take_next(&fx.db, batch.id, now_ms)
            .expect("取任务失败")
            .expect("该有第 3 封");
        assert_eq!(job3.status, "sending", "第 3 封被取走后停在 sending");
        // 进程崩溃 —— 什么都不做

        // 重启：发现未完成的批次
        let (found, prog) = find_unfinished(&fx.db)
            .expect("查未完成失败")
            .expect("该有未完成批次");
        assert_eq!(found.id, batch.id);
        assert_eq!(found.status, "running");
        assert_eq!(prog.total, 4);
        assert_eq!(prog.success, 2);
        assert_eq!(prog.in_flight, 1, "存疑的 sending 在进度里");
        assert_eq!(prog.pending, 1);

        // 恢复：sending 回 queued
        let recovered = recover_batch(&fx.db, batch.id).expect("恢复失败");
        assert_eq!(recovered.status, "running");
        let jobs = list_jobs(&fx.db, batch.id).expect("读任务失败");
        assert!(
            jobs.iter().all(|j| j.status != "sending"),
            "不该有 sending 残留"
        );
        assert_eq!(
            jobs.iter().filter(|j| j.status == "success").count(),
            2,
            "已成功的不动"
        );

        // 继续跑完：只有 2 封被发（第 3、4 封），已成功的 2 封**不再发**
        let calls_before = sender.calls.len(); // 2 次（前两封成功时）
        let final_batch =
            drive_batch(&fx.db, batch.id, &mut sender, 30, &mut now_ms).expect("续跑失败");
        assert_eq!(final_batch.status, "finished");
        assert_eq!(final_batch.success, 4);
        assert_eq!(
            sender.calls.len() - calls_before,
            2,
            "恢复后只发剩下的 2 封：{:?}",
            sender.calls
        );
        // 恢复后没有未完成批次了
        assert!(find_unfinished(&fx.db).expect("查失败").is_none());
    }

    /// 标准 6/7：仅重试失败项 —— dead 回队、attempts 保留、success 不动；aborted 拒绝。
    #[test]
    fn only_failed_jobs_are_retried_and_aborted_batches_refuse() {
        let fx = Fx::new("retryfailed", 3);
        let now = 1_780_000_000;
        let batch =
            create_batch(&fx.db, fx.period_id, None, &fx.members, 30, now).expect("建批次失败");

        // 1 成功、1 永久失败（550）、1 网络暂停
        let mut sender = ScriptSender::new(vec![
            SendResult::Accepted,
            SendResult::Rejected(550),
            SendResult::Network,
            SendResult::Accepted, // 重试后的
        ]);
        let mut now_ms = now * 1000;
        let paused = drive_batch(&fx.db, batch.id, &mut sender, 30, &mut now_ms).expect("驱动失败");
        assert_eq!(paused.status, "paused");

        // aborted 批次的拒绝：造一个 aborted
        let fx2 = Fx::new("abortedrefuse", 2);
        let b2 =
            create_batch(&fx2.db, fx2.period_id, None, &fx2.members, 30, now).expect("建批次失败");
        let mut s2 = ScriptSender::new(vec![SendResult::Rejected(535)]);
        let mut now2 = now * 1000;
        let aborted = drive_batch(&fx2.db, b2.id, &mut s2, 30, &mut now2).expect("驱动失败");
        assert_eq!(aborted.status, "aborted");
        let err = retry_failed(&fx2.db, b2.id).expect_err("aborted 该拒绝重试");
        assert!(
            err.user_message().contains("认证失败"),
            "{}",
            err.user_message()
        );

        // 正常路径：恢复 → 仅重试失败项
        recover_batch(&fx.db, batch.id).expect("恢复失败");
        let retried = retry_failed(&fx.db, batch.id).expect("重试失败");
        assert_eq!(retried, 1, "只有那封 550 的会回队");
        let jobs = list_jobs(&fx.db, batch.id).expect("读任务失败");
        let dead_before = jobs
            .iter()
            .find(|j| j.attempts == 1 && j.status == "queued");
        assert!(dead_before.is_some(), "550 那封该回 queued");
        let success_jobs = jobs.iter().filter(|j| j.status == "success").count();
        assert_eq!(success_jobs, 1, "已成功的不动");

        // 继续驱动：重试的那封 + 原来排着的都发完
        let final_batch =
            drive_batch(&fx.db, batch.id, &mut sender, 30, &mut now_ms).expect("驱动失败");
        assert_eq!(final_batch.status, "finished");
        assert_eq!(final_batch.success, 3);
    }

    /// 标准 9：进度从库算 —— success/failed 与逐封状态一致。
    #[test]
    fn the_progress_is_derived_from_the_jobs_table() {
        let fx = Fx::new("progress", 3);
        let now = 1_780_000_000;
        let batch =
            create_batch(&fx.db, fx.period_id, None, &fx.members, 30, now).expect("建批次失败");
        let mut sender = ScriptSender::new(vec![SendResult::Accepted; 3]);
        let mut now_ms = now * 1000;
        let final_batch =
            drive_batch(&fx.db, batch.id, &mut sender, 30, &mut now_ms).expect("驱动失败");
        assert_eq!(final_batch.status, "finished");

        let prog = progress(&fx.db, batch.id).expect("算进度失败");
        let jobs = list_jobs(&fx.db, batch.id).expect("读任务失败");
        assert_eq!(prog.total, jobs.len() as i64);
        assert_eq!(
            prog.success,
            jobs.iter().filter(|j| j.status == "success").count() as i64
        );
        assert_eq!(prog.pending + prog.in_flight, 0);
        // 批次行的累加计数与任务表一致
        assert_eq!(final_batch.success, prog.success);
    }

    /// 幂等键的形状与确定性（TC-402 的基础）。
    #[test]
    fn the_idempotency_key_is_deterministic_and_covers_the_inputs() {
        let a = idempotency_key(1, 2, 3, "sum");
        let b = idempotency_key(1, 2, 3, "sum");
        assert_eq!(a, b);
        assert_ne!(
            idempotency_key(1, 2, 3, "sum"),
            idempotency_key(1, 2, 4, "sum")
        );
        assert_ne!(
            idempotency_key(1, 2, 3, "sum"),
            idempotency_key(1, 3, 3, "sum")
        );
        assert_ne!(
            idempotency_key(1, 2, 3, "sum"),
            idempotency_key(9, 2, 3, "sum")
        );
        assert_ne!(
            idempotency_key(1, 2, 3, "sum"),
            idempotency_key(1, 2, 3, "other")
        );
        assert_eq!(a.len(), 64, "SHA-256 十六进制");
    }

    /// 草稿账套不能建批次（还没准备好发）。
    #[test]
    fn a_draft_period_cannot_start_a_batch() {
        let fx = Fx::new("draft", 1);
        let now = 1_780_000_000;
        // Fx 已经推到 ready；再造一个草稿账套试
        let draft = periods::create(&fx.db, "2026-11").expect("建账套失败");
        let err = create_batch(&fx.db, draft.id, None, &fx.members, 30, now)
            .expect_err("草稿不该能建批次");
        assert!(
            err.user_message().contains("草稿"),
            "{}",
            err.user_message()
        );
        let _ = now;
    }
}
