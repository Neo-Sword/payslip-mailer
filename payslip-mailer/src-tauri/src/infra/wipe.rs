//! 清空数据（TC-105 · ADR-0014）
//!
//! 一句话：**先删库、后删钥匙串条目**，连 `exports/` 里的备份一起删，
//! `logs/` 留着（脱敏、不含个人数据，且合规上「保存 2 年以上备查」指的是它）。
//!
//! **删除顺序不能反**（ADR-0014 决策 1）。两个半成品状态的代价完全不对称：
//!
//! | 崩在 | 留下的状态 | 后果 |
//! |---|---|---|
//! | 条目已删、库还在 | 库文件在、密钥没了 | 下次启动 → KEYCHAIN_MISSING → 用户被**锁在门外** |
//! | 库已删、条目还在 | 库文件没了、密钥还在 | 下次启动 → 首启向导（干净重来）✓ |
//!
//! 「安全删除」的真实含义也在这里：**在 APFS + SSD 上覆写擦除并不可靠**
//! （写时复制 + 磨损均衡让物理块不可寻址）。真正的安全性质是
//! **钥匙串里那把 key 被删了 ⇒ 那份密文永远解不开**。所以不做任何覆写尝试
//! ——那只会给人虚假的安全感。

use std::fs;
use std::path::Path;

use crate::error::{AppError, AppResult};
use crate::infra::admins::{self, UnlockEnv};
use crate::infra::backup::exports_dir;
use crate::infra::db::DB_FILE_NAME;
use crate::infra::keychain::{KeychainStore, SecretKind};

/// 销毁后告诉界面「到底删了什么」。
///
/// 逐项列出来不是为了好看 —— 是为了让用户**在点确认之前**知道
/// 备份文件也会没（ADR-0014 决策 2）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WipeReport {
    /// 库文件是否已删。
    pub database_removed: bool,
    /// 删掉了几个备份文件。
    pub backups_removed: usize,
    /// 钥匙串条目是否已删。
    pub keychain_entry_removed: bool,
    /// 日志保留（脱敏、不含个人数据）。
    pub logs_kept: bool,
}

/// 删之前先给人看一眼的清单（确认框里逐项列出来）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WipePreview {
    /// 库文件路径。
    pub database_path: String,
    /// 库文件大小（字节）。
    pub database_size: u64,
    /// 本机备份文件的名字。
    pub backup_names: Vec<String>,
    /// 日志目录（保留）。
    pub logs_path: String,
}

/// 列出「这次会删什么」。
///
/// **只读**：这一步一个字节都不删（销毁是不可撤销的，确认之前必须能反复看）。
pub fn preview(data_dir: &Path) -> AppResult<WipePreview> {
    let db_path = data_dir.join(DB_FILE_NAME);
    let mut backup_names = Vec::new();
    let exports = exports_dir(data_dir);
    if exports.exists()
        && let Ok(entries) = fs::read_dir(&exports)
    {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.ends_with(".zip") {
                backup_names.push(name);
            }
        }
    }
    backup_names.sort();
    Ok(WipePreview {
        database_path: db_path.to_string_lossy().to_string(),
        database_size: fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0),
        backup_names,
        logs_path: data_dir.join("logs").to_string_lossy().to_string(),
    })
}

/// 清空数据。
///
/// `password` 是**管理员口令**（不是备份口令）。口令不对 → **一个字节都不删**。
///
/// 顺序（ADR-0014 决策 1）：关库 → 删库文件 → 删暂存文件 → 删备份 → **最后**删钥匙串条目。
/// 每一步都记日志；**日志里不出现任何工资数值或口令**（红线 2）。
pub fn wipe(
    env: &UnlockEnv<'_>,
    store: &KeychainStore,
    data_dir: &Path,
    password: &str,
) -> AppResult<WipeReport> {
    // ① 验证口令。没有副作用：不对就到此为止。
    if !admins::verify_password(env, password)? {
        // 专属码：前端要能区分「解锁口令错」与「销毁口令错」——
        // 后者的下一步是「重新输一次」，不是「等 60 秒」。
        return Err(AppError::WipePasswordWrong);
    }

    // ② 关库：不关着就删文件，连接还开着会留下 journal/WAL 的尾巴。
    env.db.close();

    // ③ 删库文件
    let db_path = data_dir.join(DB_FILE_NAME);
    let database_removed = remove_if_exists(&db_path)?;
    // ④ 删可能存在的旁挂文件（WAL / SHM / 恢复暂存）
    let mut extra_removed = 0;
    for suffix in ["-wal", "-shm", ".restored"] {
        let p = data_dir.join(format!("{DB_FILE_NAME}{suffix}"));
        if remove_if_exists(&p)? {
            extra_removed += 1;
        }
    }
    // 恢复用的临时目录（TC-104 留下的），里面是解出来的库副本
    let mut restore_tmp_removed = 0;
    if let Ok(entries) = fs::read_dir(data_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(".restore-tmp-") && remove_if_exists(&entry.path())? {
                restore_tmp_removed += 1;
            }
        }
    }

    // ⑤ 删本机的备份文件（「彻底清空」要名副其实 —— ADR-0014 决策 2）
    let mut backups_removed = 0;
    let exports = exports_dir(data_dir);
    if exports.exists()
        && let Ok(entries) = fs::read_dir(&exports)
    {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && remove_if_exists(&path)? {
                backups_removed += 1;
            }
        }
    }
    // 备份目录本身也删掉
    let _ = fs::remove_dir_all(&exports);

    // ⑥ **最后**删钥匙串条目。到这一步为止，就算崩了，
    //    用户下次启动也只会回到首启向导（而不是被锁在门外）。
    let keychain_entry_removed = match store.delete(SecretKind::DbKey, SecretKind::DEFAULT_ACCOUNT)
    {
        Ok(()) => true,
        // 条目本来就不在（从没初始化过 / 之前删过）也算达成目标
        Err(AppError::KeychainMissing) => true,
        Err(e) => return Err(e),
    };

    tracing::info!(
        database_removed,
        extra_removed,
        restore_tmp_removed,
        backups_removed,
        keychain_entry_removed,
        "本机数据已清空"
    );

    Ok(WipeReport {
        database_removed,
        backups_removed,
        keychain_entry_removed,
        logs_kept: true,
    })
}

/// 删一个文件；本来不存在也算成功。
fn remove_if_exists(path: &Path) -> AppResult<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(AppError::Io(format!("删除 {} 失败：{e}", path.display()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::admins::{self, UnlockEnv};
    use crate::infra::backup::exports_dir;
    use crate::infra::db::{CIPHER_PARAMS, DB_FILE_NAME, DbState};
    use crate::infra::keychain::{Secret, SecretKind, TempKeychain, no_user_interaction};
    use std::path::PathBuf;

    const ADMIN_PW: &str = "OuyangAotian-2026-GongZiTiao";

    /// 一台「机器」：临时数据目录 + 临时钥匙串 + 一个 `DbState`。
    struct Fixture {
        dir: PathBuf,
        kc: TempKeychain,
        db: DbState,
    }

    impl Fixture {
        fn new(label: &str) -> Self {
            no_user_interaction();
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default();
            let dir = std::env::temp_dir().join(format!(
                "xinyou-wipe-test-{}-{nanos}-{label}",
                std::process::id()
            ));
            Self {
                dir,
                kc: TempKeychain::new(label),
                db: DbState::new(),
            }
        }

        fn env(&self) -> UnlockEnv<'_> {
            UnlockEnv {
                db: &self.db,
                store: &self.kc.store,
                data_dir: &self.dir,
                params: &CIPHER_PARAMS,
            }
        }

        fn db_file(&self) -> PathBuf {
            self.dir.join(DB_FILE_NAME)
        }

        /// 建档 + 造两个备份文件（模拟「本机有备份」的情形）。
        fn seed(&self) {
            self.db
                .open(&self.kc.store, &self.dir, &CIPHER_PARAMS)
                .expect("开库失败");
            admins::create_admin(
                &self.env(),
                "hr@example.com",
                ADMIN_PW,
                "测试单位甲",
                1_780_000_000,
            )
            .expect("建档失败");
            let exports = exports_dir(&self.dir);
            fs::create_dir_all(&exports).expect("建 exports 失败");
            fs::write(exports.join("payslip-20261111.zip"), "备份".as_bytes()).expect("写备份失败");
            fs::write(exports.join("payslip-20261110.zip"), "备份".as_bytes()).expect("写备份失败");
        }

        fn keychain_has_db_key(&self) -> bool {
            self.kc
                .store
                .get(SecretKind::DbKey, SecretKind::DEFAULT_ACCOUNT)
                .is_ok()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            self.db.close();
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    /// 标准 1：口令不对 → 一个字节都不删。
    #[test]
    fn a_wrong_password_deletes_nothing() {
        let fx = Fixture::new("std01");
        fx.seed();
        let before = fs::read(fx.db_file()).expect("读库失败");

        let err =
            wipe(&fx.env(), &fx.kc.store, &fx.dir, "这不是管理员口令").expect_err("口令错必须失败");
        assert!(
            matches!(err, AppError::WipePasswordWrong),
            "必须是 WIPE_PASSWORD_WRONG：{err:?}"
        );

        assert!(fx.db_file().exists(), "库不该被删");
        assert_eq!(before, fs::read(fx.db_file()).expect("读库失败"));
        assert!(fx.keychain_has_db_key(), "钥匙串条目不该被删");
        let backups: Vec<_> = fs::read_dir(exports_dir(&fx.dir))
            .expect("读 exports 失败")
            .flatten()
            .collect();
        assert_eq!(backups.len(), 2, "备份不该被删");
    }

    /// 标准 2：口令正确 → 库、备份、密钥三样都不留。
    #[test]
    fn a_correct_password_removes_database_backups_and_key() {
        let fx = Fixture::new("std02");
        fx.seed();
        assert!(fx.keychain_has_db_key(), "前置条件：钥匙串里要有条目");

        let report = wipe(&fx.env(), &fx.kc.store, &fx.dir, ADMIN_PW).expect("销毁失败");

        assert!(report.database_removed, "库没删掉");
        assert_eq!(report.backups_removed, 2, "两个备份都要删");
        assert!(report.keychain_entry_removed, "钥匙串条目没删掉");
        assert!(report.logs_kept, "日志应当保留（脱敏）");

        assert!(!fx.db_file().exists(), "库文件还在");
        assert!(!exports_dir(&fx.dir).exists(), "备份目录还在");
        assert!(!fx.keychain_has_db_key(), "钥匙串条目还在");
    }

    /// 标准 9：库文件是被**删掉**的，不是被清空内容的。
    #[test]
    fn the_database_file_is_deleted_not_emptied() {
        let fx = Fixture::new("std09");
        fx.seed();
        wipe(&fx.env(), &fx.kc.store, &fx.dir, ADMIN_PW).expect("销毁失败");
        assert!(
            !fx.db_file().exists(),
            "库文件必须整个消失（清空内容会在磁盘上留下曾经有数据的痕迹）"
        );
    }

    /// 标准 6：销毁后应用回到 S01 首启向导（`initialized == false`）。
    #[test]
    fn after_wiping_the_app_is_back_to_the_first_run_wizard() {
        let fx = Fixture::new("std06");
        fx.seed();
        wipe(&fx.env(), &fx.kc.store, &fx.dir, ADMIN_PW).expect("销毁失败");

        let state = admins::probe(&fx.env(), 1_780_000_000).expect("读状态失败");
        assert!(
            !state.initialized,
            "销毁后应当没有管理员行 → 前端切 S01：{state:?}"
        );
        assert!(state.email.is_none(), "销毁后不该还有邮箱：{state:?}");
    }

    /// 标准 8：口令验证**不计入**那 5 次失败锁定。
    #[test]
    fn the_wipe_password_check_does_not_touch_the_lockout_counters() {
        let fx = Fixture::new("std08");
        fx.seed();
        let before = fx
            .env()
            .db
            .read(|conn| {
                conn.query_row(
                    "SELECT failed_attempts FROM admins WHERE id = 1",
                    [],
                    |row| row.get::<_, u32>(0),
                )
                .map_err(|e| AppError::Database(e.to_string()))
            })
            .expect("读计数失败");

        for _ in 0..3 {
            let err =
                wipe(&fx.env(), &fx.kc.store, &fx.dir, "还是不对").expect_err("口令错必须失败");
            assert!(matches!(err, AppError::WipePasswordWrong));
        }

        let after = fx
            .env()
            .db
            .read(|conn| {
                conn.query_row(
                    "SELECT failed_attempts FROM admins WHERE id = 1",
                    [],
                    |row| row.get::<_, u32>(0),
                )
                .map_err(|e| AppError::Database(e.to_string()))
            })
            .expect("读计数失败");
        assert_eq!(before, after, "销毁的口令校验不该动锁定计数");
    }

    /// 标准 3：**先删库、后删钥匙串条目**。
    ///
    /// 用一把**里面没有条目**的钥匙串去删：条目那一步会返回「本来就不在」，
    /// 于是库被删了、条目还在 —— 这正是「崩在中间」的那一侧。
    /// 断言此时的库状态是「能重新初始化」而不是「钥匙串缺失循环」。
    #[test]
    fn the_database_goes_before_the_keychain_entry() {
        let fx = Fixture::new("std03");
        fx.seed();
        assert!(fx.keychain_has_db_key(), "前置条件：钥匙串里要有条目");

        let other = TempKeychain::new("std03-other");
        wipe(&fx.env(), &other.store, &fx.dir, ADMIN_PW).expect("销毁失败");

        assert!(!fx.db_file().exists(), "库必须先删");
        assert!(
            fx.keychain_has_db_key(),
            "条目应当还在（这次删的是另一把钥匙串里的）"
        );
        // 条目还在 + 库没了 → 下次启动是「首启向导」，不是「钥匙串缺失」循环
        let state = admins::probe(&fx.env(), 1_780_000_000).expect("读状态失败");
        assert!(!state.initialized, "应当回到首启向导：{state:?}");
    }

    /// 标准 7：钥匙串里**别的条目不受影响**。
    #[test]
    fn other_keychain_entries_are_untouched() {
        let fx = Fixture::new("std07");
        fx.seed();
        fx.kc
            .store
            .put(
                SecretKind::SmtpCredential,
                "someone-else",
                &Secret::new(b"other".to_vec()),
            )
            .expect("放条目失败");

        wipe(&fx.env(), &fx.kc.store, &fx.dir, ADMIN_PW).expect("销毁失败");

        assert!(
            fx.kc
                .store
                .get(SecretKind::SmtpCredential, "someone-else")
                .is_ok(),
            "销毁只清薪邮自己的条目，别人的条目不能动"
        );
    }

    /// 标准 5 的后端半：清单里逐项列得出要删什么。
    #[test]
    fn the_preview_lists_exactly_what_will_be_removed() {
        let fx = Fixture::new("std05");
        fx.seed();
        let p = preview(&fx.dir).expect("取清单失败");
        assert!(p.database_size > 0, "清单要报出库的大小");
        assert_eq!(p.backup_names.len(), 2, "清单要列出两个备份");
        assert!(p.backup_names.iter().all(|n| n.ends_with(".zip")));
        assert!(
            p.logs_path.ends_with("logs"),
            "清单要说明日志目录（保留）：{}",
            p.logs_path
        );
    }
}
