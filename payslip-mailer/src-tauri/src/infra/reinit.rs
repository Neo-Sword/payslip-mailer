//! 重新初始化：把**读不出来**的旧库移到一边（不删）
//!
//! ## 为什么需要它
//!
//! 钥匙串里的密钥与本机那份 `data.db` 对不上时（最常见：应用换过构建，
//! 密钥条目被重建过；另一台机器的备份被恢复到这里；或开发构建与发布构建
//! 共用同一个数据目录却各用各的钥匙串），`admin_status` 会以 `DATABASE` 失败，
//! 前端进不了任何界面 —— 而错误文案里写着「请先从备份恢复或重新完成初始化」，
//! **两条路都没有按钮**：这就是使用者被卡住的地方（TC-600 真机验收撞到）。
//!
//! ## 这一步做什么、不做什么
//!
//! - **移走，不删**：旧库被改名为 `data.db.bak-<时间戳>`，留在同一个目录里。
//!   它仍然是加密的（打不开也读不出内容），但**一个字节都没丢** ——
//!   真要找回，把文件名改回去就行。
//! - **不碰钥匙串**：下一次建档会写新的密钥条目（同一条目名，覆盖写）。
//!   这里主动删条目反而制造「库没了、条目还在」的半成品状态 ——
//!   那是 ADR-0014 明确要避免的一侧（代价对称性的口径见 `infra::wipe`）。
//! - **不碰备份**：导出的加密备份在 `exports/` 里，与本步无关；它们本来就是
//!   「从备份恢复」那条路的输入。

use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{AppError, AppResult};
use crate::infra::db::DB_FILE_NAME;

/// 移走旧库之后告诉界面发生了什么。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveReport {
    /// 原来的库文件路径（已经不在那里了）。
    pub previous_path: String,
    /// 移到哪去了；本来就没有库文件时是 `None`（首启那种情形）。
    pub archived_path: Option<String>,
}

/// 把数据目录里的旧库移到一边。
///
/// **幂等**：本来就没有库文件时不算失败，返回 `archived_path: None` ——
/// 首启向导那条路上也会调它（那时目录里本来就什么都没有）。
pub fn archive_local_db(data_dir: &Path) -> AppResult<ArchiveReport> {
    let db_path = data_dir.join(DB_FILE_NAME);
    let previous_path = db_path.to_string_lossy().to_string();

    if !db_path.exists() {
        return Ok(ArchiveReport {
            previous_path,
            archived_path: None,
        });
    }

    // 目录本身可能还不存在（首启那条路）：那就没什么可移的。
    if !data_dir.exists() {
        return Ok(ArchiveReport {
            previous_path,
            archived_path: None,
        });
    }

    let stamp = timestamp_suffix();
    let mut target = data_dir.join(format!("{DB_FILE_NAME}.bak-{stamp}"));
    // 同一秒内两次重试不该互相覆盖（TC-104 的恢复流程踩过这个坑）。
    let mut n = 1u32;
    while target.exists() {
        target = data_dir.join(format!("{DB_FILE_NAME}.bak-{stamp}-{n}"));
        n += 1;
    }

    fs::rename(&db_path, &target).map_err(|e| {
        AppError::Io(format!(
            "把旧库移到一边失败（{} → {}）：{e}",
            db_path.display(),
            target.display()
        ))
    })?;

    // 旁挂文件（WAL / SHM）跟着走：留着它们等于留着一个指向已移走文件的尾巴。
    for suffix in ["-wal", "-shm"] {
        let side = data_dir.join(format!("{DB_FILE_NAME}{suffix}"));
        if side.exists() {
            let _ = fs::rename(&side, target.with_extension(format!("bak{suffix}")));
        }
    }

    tracing::info!(
        previous = %db_path.display(),
        archived = %target.display(),
        "旧库已移到一边（未删除），可改回文件名恢复"
    );

    Ok(ArchiveReport {
        previous_path,
        archived_path: Some(target.to_string_lossy().to_string()),
    })
}

/// 时间戳后缀：秒级足够（同一秒的重试用序号兜底）。
fn timestamp_suffix() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // 本地时区的可读时间戳（`date` 那套格式），比 epoch 秒好认。
    let out = std::process::Command::new("date")
        .arg("+%Y%m%d-%H%M%S")
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if s.is_empty() { secs.to_string() } else { s }
        }
        _ => secs.to_string(),
    }
}

/// 旧库移走后，目录里应该只剩下别的文件（导出目录、日志等）。
#[cfg(test)]
fn dir_entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let dir = std::env::temp_dir().join(format!(
            "xinyou-reinit-test-{}-{nanos}-{label}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("建临时目录失败");
        dir
    }

    /// 标准 1：目录里本来没有库 → 不算失败，报告里说「没移走什么」。
    #[test]
    fn an_absent_database_is_not_an_error() {
        let dir = tmp_dir("std01");
        let report = archive_local_db(&dir).expect("应当成功");
        assert_eq!(report.archived_path, None, "没有库就没有可移走的东西");
        assert!(
            report.previous_path.ends_with(DB_FILE_NAME),
            "还是要说清原来的路径：{}",
            report.previous_path
        );
        fs::remove_dir_all(&dir).ok();
    }

    /// 标准 2：库被**移走**而不是被删 —— 内容还在，路径变了。
    #[test]
    fn the_database_is_moved_not_deleted() {
        let dir = tmp_dir("std02");
        let db = dir.join(DB_FILE_NAME);
        fs::write(&db, b"encrypted bytes").expect("写库失败");

        let report = archive_local_db(&dir).expect("应当成功");
        let archived = report.archived_path.expect("应当有归档路径");
        assert!(!db.exists(), "原路径不该再有文件");
        assert!(Path::new(&archived).exists(), "归档文件必须还在");
        assert_eq!(
            fs::read(&archived).expect("读归档失败"),
            b"encrypted bytes".to_vec(),
            "归档文件的内容必须逐字节一样（一个字节都没丢）"
        );
        fs::remove_dir_all(&dir).ok();
    }

    /// 标准 3：两次重试不互相覆盖（同一秒内）。
    #[test]
    fn two_archives_do_not_overwrite_each_other() {
        let dir = tmp_dir("std03");
        let db = dir.join(DB_FILE_NAME);
        fs::write(&db, b"first").expect("写库失败");
        let first = archive_local_db(&dir).expect("第一次应当成功");

        fs::write(&db, b"second").expect("写库失败");
        let second = archive_local_db(&dir).expect("第二次应当成功");

        assert_ne!(
            first.archived_path, second.archived_path,
            "两次归档不该落在同一个文件上"
        );
        // 两份都在，内容各自不变 —— 第一次归档没有被第二次覆盖。
        assert_eq!(
            fs::read(&first.archived_path.expect("第一次的归档路径")).unwrap(),
            b"first".to_vec(),
            "第一次归档的内容不该被动过"
        );
        assert_eq!(
            fs::read(&second.archived_path.expect("第二次的归档路径")).unwrap(),
            b"second".to_vec()
        );
        fs::remove_dir_all(&dir).ok();
    }

    /// 标准 4：移走之后，目录里**没有** data.db 了 —— 下一步建档才建得出新库。
    #[test]
    fn the_database_name_is_free_after_archiving() {
        let dir = tmp_dir("std04");
        fs::write(dir.join(DB_FILE_NAME), b"x").expect("写库失败");
        archive_local_db(&dir).expect("应当成功");
        let names = dir_entries(&dir);
        assert!(
            !names.iter().any(|n| n == DB_FILE_NAME),
            "data.db 这个名字要让出来：{names:?}"
        );
        fs::remove_dir_all(&dir).ok();
    }
}
