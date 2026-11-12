//! 备份的 Command（TC-104 · ADR-0013）
//!
//! 三个：`export_backup` / `list_backups` / `restore_backup`。
//!
//! **口令只进不出**（红线 2 的同族）：返回值里没有口令，日志里也不出现。
//! 备份口令与管理员口令是两回事 —— 前者派生出备份密钥，后者只用来登录。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tauri::Manager;

use crate::domain::backup::{BackupKind, check_backup_password_policy};
use crate::error::{AppError, AppResult};
use crate::infra::admins::{self, UnlockEnv};
use crate::infra::backup::{self, BackupFileInfo, RestoreOutcome};
use crate::infra::db::{CIPHER_PARAMS, DbState};
use crate::infra::keychain::KeychainStore;
use crate::infra::wipe::{WipePreview, WipeReport};

/// 导出一份加密备份。
///
/// 口令不足 10 位直接拒（`domain::backup::check_backup_password_policy`）。
/// 备份口令**不进管理员口令的那套失败锁定**（ADR-0013 第 5 条）：
/// 它保护的是一个离线文件，在线锁定给不了任何保护。
#[tauri::command]
pub fn export_backup(
    app: tauri::AppHandle,
    db: tauri::State<'_, Arc<DbState>>,
    password: String,
) -> AppResult<BackupFileInfo> {
    check_backup_password_policy(&password)?;
    let dir = data_dir(&app)?;
    let exports = backup::exports_dir(&dir);
    std::fs::create_dir_all(&exports)
        .map_err(|e| AppError::BackupFailed(format!("建备份目录失败：{e}")))?;

    let stamp = timestamp();
    let path = next_free_path(&exports, &stamp);
    let manifest = backup::export(&db, &password, BackupKind::Manual, &path)?;
    // 记「上次导出备份时刻」：设置页的常驻备份提醒靠它说话（TC-602 · ch.14）。
    // 写在导出**成功之后** —— 失败的那次不算备份过。
    backup::set_last_exported_at(&db, manifest.created_at)?;
    Ok(BackupFileInfo {
        path: path.to_string_lossy().to_string(),
        name: file_name(&path),
        size: std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
        created_at: manifest.created_at,
        kind: manifest.kind,
        app_version: manifest.app_version,
        schema_version: manifest.schema_version,
    })
}

/// 列出本机 `exports/` 里的备份。
#[tauri::command]
pub fn list_backups(app: tauri::AppHandle) -> AppResult<Vec<BackupFileInfo>> {
    let dir = data_dir(&app)?;
    backup::list_backups(&dir)
}

/// 从一个备份文件恢复。
///
/// `path` 由前端的原生文件选择器给出（`dialog:allow-open`）。
/// 恢复的六步顺序在 `infra::backup::restore` 里，这里只做前置检查与收尾。
#[tauri::command]
pub fn restore_backup(
    app: tauri::AppHandle,
    db: tauri::State<'_, Arc<DbState>>,
    password: String,
    path: String,
) -> AppResult<RestoreOutcome> {
    let dir = data_dir(&app)?;
    let backup_path = PathBuf::from(&path);
    if !backup_path.exists() {
        return Err(AppError::BackupCorrupted("找不到这个备份文件".into()));
    }

    // 恢复要用 DB key 把备份内容重新加密回 live 库 —— **备份不接管主库的密钥**
    // （ADR-0013 §4 的第 ⑤ 步）。DB key 从钥匙串取。
    let store = KeychainStore::for_runtime()?;
    let db_key = store
        .get(
            crate::infra::keychain::SecretKind::DbKey,
            crate::infra::keychain::SecretKind::DEFAULT_ACCOUNT,
        )
        .map_err(|e| AppError::BackupFailed(format!("取不到本机库密钥：{e}")))?
        .as_bytes()
        .to_vec();

    let stamp = timestamp();
    let outcome = backup::restore(&db, &dir, &db_key, &backup_path, &password, &stamp)?;

    // 恢复后把 live 库重开：这一步会跑迁移（备份比应用旧时正好在这里补齐）。
    let env = UnlockEnv {
        db: &db,
        store: &store,
        data_dir: &dir,
        params: &CIPHER_PARAMS,
    };
    db.open(&store, &dir, &CIPHER_PARAMS)
        .map_err(|e| AppError::BackupFailed(format!("恢复后重开本机库失败：{e}")))?;
    let _ = &env;

    // 记「上次恢复时刻」（设计文档 ch.6 点名过的那个键）。
    backup::set_last_restored_at(&db, backup::now_secs())
        .map_err(|e| AppError::BackupFailed(format!("记录恢复时刻失败：{e}")))?;

    Ok(outcome)
}

/// 数据目录。与 `commands::auth` 同一个口径（`app_data_dir()`）。
fn data_dir(app: &tauri::AppHandle) -> AppResult<PathBuf> {
    app.path()
        .app_data_dir()
        .map_err(|e| AppError::Io(format!("无法定位本机数据目录：{e}")))
}

/// 备份文件名的时间戳：`YYYYMMDD`。
///
/// **只有日期**（红线 2：不把工资数值写进备份文件名）。
/// 同一天导出多次时靠 `next_free_path` 加序号，不覆盖已有的。
fn timestamp() -> String {
    let secs = backup::now_secs();
    let days = secs.div_euclid(86_400);
    // 从 1970-01-01 起的天数 → 年月日。避开引一个时间 crate：
    // 算术而已，且时区无关（备份文件名用 UTC 的日期，不随时区跳变）。
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}{m:02}{d:02}")
}

/// 同名就加序号：`-2`、`-3`……直到空位。
fn next_free_path(exports: &Path, stamp: &str) -> PathBuf {
    let mut candidate = exports.join(format!("payslip-{stamp}.zip"));
    let mut n = 2;
    while candidate.exists() {
        candidate = exports.join(format!("payslip-{stamp}-{n}.zip"));
        n += 1;
    }
    candidate
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// 天数 → (年, 月, 日)（Howard Hinnant 的 civil_from_days 算法）。
///
/// 为什么要自己算：引一个时间 crate 只为了给文件名加日期，不值。
/// 用 UTC 的日期而不是本地日期：备份文件名不随时区变化，跨时区看同一个备份
/// 不会觉得「日期不对」。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/* ---------------- 清空数据（TC-105 · ADR-0014）---------------- */

/// 销毁前的清单：确认框里逐项列出来。
///
/// 这一步**只读**：销毁是不可撤销的，确认之前必须能反复看。
#[tauri::command]
pub fn wipe_preview(app: tauri::AppHandle) -> AppResult<WipePreview> {
    let dir = data_dir(&app)?;
    crate::infra::wipe::preview(&dir)
}

/// 清空数据：删库、删备份、删钥匙串条目。**不可撤销**。
///
/// `password` 是管理员口令。口令不对 → 一个字节都不删（ADR-0014 决策 5）。
/// 删除顺序固定为「先库后密钥」（决策 1），崩在中间也停在可恢复的那一侧。
#[tauri::command]
pub fn wipe_data(
    app: tauri::AppHandle,
    db: tauri::State<'_, Arc<DbState>>,
    password: String,
) -> AppResult<WipeReport> {
    let dir = data_dir(&app)?;
    let store = KeychainStore::for_runtime()?;
    let env = UnlockEnv {
        db: &db,
        store: &store,
        data_dir: &dir,
        params: &CIPHER_PARAMS,
    };
    let report = crate::infra::wipe::wipe(&env, &store, &dir, &password)?;

    // 销毁后状态读回来：库里没有管理员行了 → initialized == false → 前端切 S01
    let state = admins::probe(&env, admins::now_secs())?;
    debug_assert!(!state.initialized, "销毁后应当没有管理员行了");
    Ok(report)
}
