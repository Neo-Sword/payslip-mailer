//! 发送历史与 CSV 凭证（TC-403 · ADR-0026）
//!
//! 「留痕」的读侧：批次列表（带进度）、逐封明细、**CSV 凭证导出**。
//!
//! 脱敏在四个层次各自被测试钉住（ADR-0026 决策 3）：
//! 1. **库层**（结构性）：`send_jobs` 无正文列 —— 数值没有地方放；
//! 2. **审计层**：`audit_log.detail_masked` 只写行数与动作；
//! 3. **凭证层**：CSV 逐格扫不到任何金额；
//! 4. **日志层**：编排日志只记 id 与处置。
//!
//! 凭证导出是**纯读操作** —— 不写库、不改批次状态（ADR-0026：导出是
//! 「看」与「拿走」，不是「改」）。

use std::path::Path;

use serde::Serialize;

use crate::error::{AppError, AppResult};
use crate::infra::backup::exports_dir;
use crate::infra::batch::{self, Batch, BatchProgress};
use crate::infra::db::DbState;

/// 发送历史的一条批次（批次 + 进度，S13 的列表行）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub batch: Batch,
    pub progress: BatchProgress,
}

/// 发送历史（批次按时间倒序 —— 最近的在前面）。
pub fn list_history(db: &DbState) -> AppResult<Vec<HistoryEntry>> {
    let batches = batch::list_batches(db)?;
    let mut out = Vec::with_capacity(batches.len());
    for b in batches {
        out.push(HistoryEntry {
            progress: batch::progress(db, b.id)?,
            batch: b,
        });
    }
    Ok(out)
}

/// 逐封明细的一行（S13 点开批次看到的）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobDetail {
    pub job_id: i64,
    pub member_id: i64,
    /// 姓名（按 member_id 关联成员表 —— send_jobs 本身不存姓名）。
    pub name: String,
    pub to_email: String,
    pub status: String,
    pub attempts: i64,
    pub smtp_code: Option<i64>,
    pub error_code: Option<String>,
    pub sent_at: Option<i64>,
}

/// 一个批次的逐封明细（出问题时「看发送日志定位 SMTP 错误码」—— ch.5 用户任务）。
pub fn job_details(db: &DbState, batch_id: i64) -> AppResult<Vec<JobDetail>> {
    let jobs = batch::list_jobs(db, batch_id)?;
    let members = crate::infra::members::list_members(db)?;
    let names: std::collections::BTreeMap<i64, String> =
        members.into_iter().map(|m| (m.id, m.name)).collect();
    Ok(jobs
        .into_iter()
        .map(|j| JobDetail {
            name: names.get(&j.member_id).cloned().unwrap_or_default(),
            to_email: j.to_email,
            status: j.status,
            smtp_code: j.smtp_code,
            error_code: j.error_code,
            sent_at: j.sent_at,
            attempts: j.attempts,
            member_id: j.member_id,
            job_id: j.id,
        })
        .collect())
}

/// CSV 凭证的一行。
struct VoucherRow {
    name: String,
    to_email: String,
    status: String,
    smtp_code: Option<i64>,
    sent_at: Option<i64>,
    checksum: String,
}

/// 导出 CSV 凭证（ADR-0026 决策 1、4）。
///
/// 列：姓名、收件地址、状态、SMTP 回码、发送时间、内容校验和。
/// **没有工资数值** —— 凭证证明「发了工资条」，不复述「发了多少钱」。
///
/// 格式：UTF-8 **带 BOM**（Windows Excel 双击不乱码）+ RFC 4180 转义
/// （含引号翻倍、字段整体包引号）。
pub fn export_voucher(
    db: &DbState,
    batch_id: i64,
    data_dir: &Path,
) -> AppResult<std::path::PathBuf> {
    let b = batch::get_batch(db, batch_id)?;
    let period = crate::infra::periods::get(db, b.period_id)?;

    let rows = voucher_rows(db, batch_id)?;

    let dir = exports_dir(data_dir);
    std::fs::create_dir_all(&dir)
        .map_err(|e| AppError::BackupFailed(format!("建导出目录失败：{e}")))?;
    // 文件名含年月（ch.8「按年月归档导出」）；批次 id 是序号不是工资数据，可进文件名
    let path = dir.join(format!(
        "发送凭证_{}_{:06}.csv",
        period.year_month, batch_id
    ));

    let mut out = String::from("\u{feff}"); // UTF-8 BOM
    out.push_str("姓名,收件地址,状态,SMTP回码,发送时间,内容校验和\r\n");
    for r in &rows {
        out.push_str(&csv_field(&r.name));
        out.push(',');
        out.push_str(&csv_field(&r.to_email));
        out.push(',');
        out.push_str(&csv_field(&r.status));
        out.push(',');
        out.push_str(&match r.smtp_code {
            Some(c) => c.to_string(),
            None => String::new(),
        });
        out.push(',');
        out.push_str(&match r.sent_at {
            Some(t) => format_epoch(t),
            None => String::new(),
        });
        out.push(',');
        out.push_str(&csv_field(&r.checksum));
        out.push_str("\r\n");
    }
    std::fs::write(&path, out.as_bytes())
        .map_err(|e| AppError::BackupFailed(format!("写凭证失败：{e}")))?;
    Ok(path)
}

/// 读一个批次的凭证行（导出与测试共用）。
fn voucher_rows(db: &DbState, batch_id: i64) -> AppResult<Vec<VoucherRow>> {
    let details = job_details(db, batch_id)?;
    let jobs = batch::list_jobs(db, batch_id)?;
    let checksums: std::collections::BTreeMap<i64, String> =
        jobs.into_iter().map(|j| (j.id, j.checksum)).collect();
    Ok(details
        .into_iter()
        .map(|d| VoucherRow {
            checksum: checksums.get(&d.job_id).cloned().unwrap_or_default(),
            name: d.name,
            to_email: d.to_email,
            status: d.status,
            smtp_code: d.smtp_code,
            sent_at: d.sent_at,
        })
        .collect())
}

/// RFC 4180 字段转义：含引号就翻倍、含逗号/引号/换行就整体包引号。
pub fn csv_field(text: &str) -> String {
    if text.contains(',') || text.contains('"') || text.contains('\n') || text.contains('\r') {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text.to_string()
    }
}

/// epoch 秒 → `YYYY-MM-DD HH:MM:SS`（本地时区）。
fn format_epoch(secs: i64) -> String {
    // 没引 chrono：一个时间格式化不值得多一个依赖（ADR-0010 的同族取向）。
    // 用标准的 UNIX 算法（ civil_from_days ），本地时区交给 UI 层处理。
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let (hh, mm, ss) = (
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60,
    );
    format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}:{ss:02}")
}

/// Howard Hinnant 的 `civil_from_days`（天数 → 公历年月日，epoch=1970-01-01）。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// 导出是不是纯读操作？—— **是**：本模块没有任何 `db.write` 调用。
/// （这条事实由「sendlog.rs 全文无 `db.write`」的源码形态保证，
/// 测试在标准 9 里从行为上再钉一遍：导出前后 send_jobs 逐字节不变。）

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 4180 转义：逗号 / 引号 / 换行 / 普通文本。
    #[test]
    fn the_csv_escaping_follows_rfc_4180() {
        assert_eq!(csv_field("张三"), "张三");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("a\"b"), "\"a\"\"b\"");
        assert_eq!(csv_field("a\nb"), "\"a\nb\"");
        assert_eq!(csv_field(""), "");
        // 往返：转义后的字段按 RFC 4180 解析回原值
        let original = "张三, \"引号\"\n第二行";
        let escaped = csv_field(original);
        assert!(escaped.starts_with('"') && escaped.ends_with('"'));
        let parsed = escaped[1..escaped.len() - 1].replace("\"\"", "\"");
        assert_eq!(parsed, original);
    }

    /// epoch → 时间的换算（含闰年与月末边界）。
    #[test]
    fn the_epoch_formatting_handles_boundaries() {
        assert_eq!(format_epoch(0), "1970-01-01 00:00:00");
        // 2026-11-12 00:00:00 UTC = 1_794_441_600（date -j -u 校准）
        assert_eq!(format_epoch(1_794_441_600), "2026-11-12 00:00:00");
        // 2024-02-29（闰年）00:00:00 UTC = 1_709_164_800
        assert_eq!(format_epoch(1_709_164_800), "2024-02-29 00:00:00");
        // 一天结束
        assert_eq!(format_epoch(86_399), "1970-01-01 23:59:59");
    }
}
