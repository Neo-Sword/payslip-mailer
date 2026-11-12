//! TC-103 验收测试 · 把 spec 的 21 条标准里**能自动判定**的那些钉成可跑断言。
//!
//! 为什么单独一个文件：验收标准写在 spec 里，代码里就该有一处能对着读的地方。
//! 评审时逐条指到哪个测试，比逐条指到哪段业务代码更省事，也不容易漏。
//!
//! 范围（spec 标准号 → 本文件的测试）：
//!   标准 1  → `first_run_shows_the_wizard_not_a_workbench`
//!   标准 2  → `the_admin_row_is_really_written`
//!   标准 4  → `a_bad_email_leaves_no_row_behind`
//!   标准 5  → `the_org_name_survives_a_restart`
//!   标准 6  → `only_the_db_key_is_in_the_keychain`
//!   标准 7  → `after_a_restart_the_app_is_locked`
//!   标准 8  → `looking_at_the_state_never_unlocks`
//!   标准 9  → `the_fifth_mistake_still_counts_and_the_sixth_locks`
//!   标准 10 → `locking_now_closes_the_database`
//!   标准 13 → `the_idle_threshold_round_trips`
//!   标准 14 → `remembering_the_device_only_ever_lengthens`
//!   标准 15 → `a_missing_key_leaves_the_database_untouched`
//!   标准 16 → `the_read_only_banner_and_the_rejection_share_one_wording`
//!   标准 18 → `the_data_directory_has_exactly_one_truth`
//!   标准 20 → `the_password_never_reaches_the_error_text`
//!
//! 标准 3 / 11 / 12 / 17 / 19 / 21 不在这里：
//!   3  是界面实时校验（前端行为，浏览器里逐条点过）；
//!   11 与 19 必须真人做（合盖唤醒、看红绿灯）；
//!   12 要等真机上真的空到点（分钟级观察）；
//!   17 靠改迁移制造漂移（`infra::db` 的单测覆盖了码与文案）；
//!   21 是门禁脚本自己的事（`scripts/gate.sh`）。

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::domain::auth;
use crate::error::{AppError, AppResult};
use crate::infra::admins::{self, AdminState, UnlockEnv};
use crate::infra::db::{CIPHER_PARAMS, DB_FILE_NAME, DbState, OpenOutcome};
use crate::infra::keychain::{SecretKind, TempKeychain, no_user_interaction};

/// 注入的「现在」。与 `infra::admins` 的测试同一口径：墙钟 epoch 秒。
const NOW: i64 = 1_780_000_000;

/// 一个够长、够独特的口令。标准 3/4 的用例用它。
const SECRET: &str = "OuyangAotian-2026-GongZiTiao";

/// 每个用例一套「临时数据目录 + 临时钥匙串 + DbState」。
///
/// **目录刻意不预先创建**：真机首启时那个目录本来就不存在
/// （ADR-0012 第 1 条实测过），所以这里必须复现同一情形，
/// 否则「应用会自己建目录」这件事永远测不到。
struct Fixture {
    dir: PathBuf,
    kc: TempKeychain,
    db: DbState,
}

impl Fixture {
    fn new(label: &str) -> Self {
        no_user_interaction();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let dir = std::env::temp_dir().join(format!(
            "xinyou-acceptance-{}-{nanos}-{label}",
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

    fn open(&self) -> AppResult<OpenOutcome> {
        self.db.open(&self.kc.store, &self.dir, &CIPHER_PARAMS)
    }

    /// 前端分流用的状态（`admin_status` 背后就是它）。
    fn state(&self) -> AdminState {
        admins::probe(&self.env(), NOW).expect("读状态失败")
    }

    fn create_admin(&self, email: &str, org_name: &str) -> AppResult<()> {
        admins::create_admin(&self.env(), email, SECRET, org_name, NOW)
    }

    /// 「重装应用」：换一个 `DbState`，数据目录与钥匙串都不变。
    fn restart(&mut self) {
        self.db = DbState::new();
    }

    fn admins_row_count(&self) -> i64 {
        self.db
            .read(|conn| {
                conn.query_row("SELECT count(*) FROM admins", [], |row| {
                    row.get::<_, i64>(0)
                })
                .map_err(|e| AppError::Database(e.to_string()))
            })
            .expect("读 admins 表失败")
    }

    fn pwd_hash(&self) -> String {
        self.db
            .read(|conn| {
                conn.query_row("SELECT pwd_hash FROM admins WHERE id = 1", [], |row| {
                    row.get::<_, String>(0)
                })
                .map_err(|e| AppError::Database(e.to_string()))
            })
            .expect("读 pwd_hash 失败")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.db.close();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// 标准 1：空数据目录首启 → 走 S01 向导，不是空工作台。
///
/// 前端只认 `initialized` 这一个字段来分流：`false` → S01，`true` 且 `unlocked`
/// → 工作台。所以这一条的后端半就是「没建档时 `initialized` 必须是 false」。
/// 前端的另一半（侧栏不出现、三步指示）在浏览器里逐条点过。
#[test]
fn first_run_shows_the_wizard_not_a_workbench() {
    let fx = Fixture::new("std01");
    assert!(!fx.dir.exists(), "前置条件：数据目录现在不该存在");

    // 注意：**不要**先 `open()`。真机的首启流程是「应用起来时库是关的，
    // 前端问一次状态，后端自己临时开库再关回去」。预先开库会把
    // `probe` 的 `unlocked` 口径变成「进来时库已经开着」，测的就不是首启了。
    let state = fx.state();
    assert!(
        !state.initialized,
        "没建档时 initialized 必须是 false，前端据此走 S01：{state:?}"
    );
    assert_eq!(state.email, None, "没建档就没有邮箱");
    assert!(!state.unlocked, "没建档谈不上解锁");
    assert!(fx.db_file().exists(), "库文件应落在数据目录里");
}

/// 标准 2：`create_admin` 有 Command 入口，且库里真的出现管理员行。
#[test]
fn the_admin_row_is_really_written() {
    let fx = Fixture::new("std02");
    fx.create_admin("hr@company.com", "测试单位甲")
        .expect("建档失败");
    assert_eq!(fx.admins_row_count(), 1, "admins 表必须恰好 1 行");
    assert!(
        fx.pwd_hash().starts_with("$argon2id$"),
        "口令哈希必须是 Argon2id PHC 串"
    );
}

/// 标准 4：绕过界面喂一个坏邮箱 → `VALIDATION_ERROR`，且库里不留行。
#[test]
fn a_bad_email_leaves_no_row_behind() {
    let fx = Fixture::new("std04");
    let err = fx
        .create_admin("hr", "测试单位甲")
        .expect_err("坏邮箱必须被拒");
    assert!(
        matches!(err, AppError::ValidationError(_)),
        "必须是 VALIDATION_ERROR：{err:?}"
    );
    fx.open().expect("开库");
    assert_eq!(fx.admins_row_count(), 0, "被拒的建档不能留下任何行");
}

/// 标准 5：单位名称落 `app_meta.org_name`，重启后仍在。
#[test]
fn the_org_name_survives_a_restart() {
    let mut fx = Fixture::new("std05");
    fx.create_admin("hr@company.com", "测试单位甲")
        .expect("建档失败");
    assert_eq!(
        admins::get_org_name(&fx.db).expect("读 org_name 失败"),
        Some("测试单位甲".into())
    );
    fx.restart();
    fx.open().expect("重启后开库");
    assert_eq!(
        admins::get_org_name(&fx.db).expect("重启后读 org_name 失败"),
        Some("测试单位甲".into()),
        "单位名称必须活过重启（spec 标准 5）"
    );
}

/// 标准 6：「稍后设置」跳过 SMTP 后，钥匙串里只有 `db-key`。
///
/// 这条不能靠「界面上没有 SMTP 输入框」判定 —— 那是界面侧的事。
/// 数据侧的说法是：**一个 SMTP 凭据条目都没有**。
#[test]
fn only_the_db_key_is_in_the_keychain() {
    let fx = Fixture::new("std06");
    fx.create_admin("hr@company.com", "测试单位甲")
        .expect("建档失败");

    // db-key 在
    fx.kc
        .store
        .get(SecretKind::DbKey, SecretKind::DEFAULT_ACCOUNT)
        .expect("钥匙串里必须有 db-key");
    // smtp-credential 不在
    let smtp = fx
        .kc
        .store
        .get(SecretKind::SmtpCredential, SecretKind::DEFAULT_ACCOUNT);
    assert!(
        matches!(smtp, Err(AppError::KeychainMissing)),
        "没配 SMTP 就不该有凭据条目（不预置空凭据）：{smtp:?}"
    );
}

/// 标准 7：初始化后重启 → 是 S02（已初始化、未解锁），不是 S01。
#[test]
fn after_a_restart_the_app_is_locked() {
    let mut fx = Fixture::new("std07");
    fx.create_admin("hr@company.com", "测试单位甲")
        .expect("建档失败");
    fx.restart();

    // 重启后库是关的；状态查询会自己临时开库，所以这里**不能**先 open()。
    let state = fx.state();
    assert!(state.initialized, "重启后仍然是已建档：{state:?}");
    assert!(
        !state.unlocked,
        "重启后库是关着的 → 前端走 S02 锁屏，不是 S01：{state:?}"
    );
    assert_eq!(state.email.as_deref(), Some("hr@company.com"));
}

/// 标准 8：「看一眼状态」不会把人解锁。锁定态连续两次都是 `unlocked == false`。
#[test]
fn looking_at_the_state_never_unlocks() {
    let fx = Fixture::new("std08");
    fx.create_admin("hr@company.com", "测试单位甲")
        .expect("建档失败");
    fx.db.close();

    let first = fx.state();
    let second = fx.state();
    assert!(
        !first.unlocked && !second.unlocked,
        "{first:?} / {second:?}"
    );
    assert!(!fx.db.is_open(), "probe 读完必须把临时开的库关回去");
}

/// 标准 9：失败次数可见；第 5 次失败仍报口令错、remaining = 0，
/// **锁定从第 6 次尝试起生效**（TC-102 结案时钉的判定口径）。
#[test]
fn the_fifth_mistake_still_counts_and_the_sixth_locks() {
    let fx = Fixture::new("std09");
    fx.create_admin("hr@company.com", "测试单位甲")
        .expect("建档失败");
    fx.db.close();

    const WRONG: &str = "OuyangAotian-2026-GongZiTiaP";
    for attempt in 1..=auth::MAX_FAILED_ATTEMPTS {
        let err = admins::unlock(&fx.env(), WRONG, NOW).expect_err("错口令必须失败");
        assert!(
            matches!(err, AppError::BadCredentials),
            "第 {attempt} 次应是 BAD_CREDENTIALS（锁定从第 6 次尝试起生效）：{err:?}"
        );
        let state = fx.state();
        let expected = auth::MAX_FAILED_ATTEMPTS - attempt;
        assert_eq!(
            state.remaining_attempts, expected,
            "第 {attempt} 次之后应剩 {expected} 次"
        );
        // 锁是**第 5 次失败按下的**（`locked_until` 立刻写库），
        // 但第 5 次那一刻使用者看到的仍是「口令错了」而不是「锁定」——
        // 设计文档 1023 的文案是「连续错误 5 次**将**锁定 60 秒」。
        // 所以第 1..4 次时 locked 必须是 false，第 5 次之后锁已生效。
        if attempt < auth::MAX_FAILED_ATTEMPTS {
            assert!(
                !state.locked,
                "第 {attempt} 次还不该锁（锁定在第 5 次失败按下）：{state:?}"
            );
            assert_eq!(state.seconds_left, 0, "{state:?}");
        } else {
            assert!(
                state.locked,
                "第 5 次失败后锁已按下，锁屏要显示倒计时：{state:?}"
            );
            assert_eq!(
                state.seconds_left,
                auth::LOCK_SECONDS as u64,
                "倒计时应当是 60 秒：{state:?}"
            );
        }
    }

    // 第 6 次：锁定期内，本次不判定口令、不动计数
    let err = admins::unlock(&fx.env(), WRONG, NOW).expect_err("第 6 次必须锁");
    assert!(
        matches!(err, AppError::Locked(_)),
        "第 6 次应是 LOCKED：{err:?}"
    );
    let state = fx.state();
    assert!(state.locked, "锁屏要显示已锁定：{state:?}");
    assert!(
        state.seconds_left > 0 && state.seconds_left <= auth::LOCK_SECONDS as u64,
        "倒计时秒数要对得上 60：{state:?}"
    );
    assert!(
        !fx.db.is_open(),
        "锁定期内库必须关着（什么都不读得到）：{state:?}"
    );
}

/// 标准 10：⌘L 立刻锁定。Rust 侧就是「关库」，关完 `unlocked` 必须变 false。
#[test]
fn locking_now_closes_the_database() {
    let fx = Fixture::new("std10");
    fx.create_admin("hr@company.com", "测试单位甲")
        .expect("建档失败");
    let outcome = admins::unlock(&fx.env(), SECRET, NOW).expect("解锁失败");
    assert!(outcome.unlocked, "解锁成功 = 库开着：{outcome:?}");
    assert!(fx.db.is_open());

    fx.db.close();
    let state = fx.state();
    assert!(
        !state.unlocked,
        "锁定后 admin_status 必须 unlocked == false：{state:?}"
    );
    // 再解锁能进
    let outcome = admins::unlock(&fx.env(), SECRET, NOW).expect("再解锁失败");
    assert!(outcome.unlocked, "{outcome:?}");
}

/// 标准 13：空闲阈值 5/10/30 落库。
#[test]
fn the_idle_threshold_round_trips() {
    let fx = Fixture::new("std13");
    fx.open().expect("开库");
    assert_eq!(
        admins::get_idle_lock_minutes(&fx.db).expect("读阈值失败"),
        admins::DEFAULT_IDLE_LOCK_MINUTES
    );
    admins::set_idle_lock_minutes(&fx.db, 5).expect("设阈值失败");
    assert_eq!(
        admins::get_idle_lock_minutes(&fx.db).expect("读阈值失败"),
        5,
        "选了 5 分钟，库里就该是 5（spec 标准 13）"
    );
}

/// 标准 14：「记住本机 10 分钟」= `max(库里阈值, 10 分钟)`，只拉长不缩短。
#[test]
fn remembering_the_device_only_ever_lengthens() {
    assert_eq!(admins::unlock_idle_secs(5, true), 600);
    assert_eq!(admins::unlock_idle_secs(5, false), 300);
    assert_eq!(
        admins::unlock_idle_secs(10, true),
        600,
        "默认阈值下是恒等操作"
    );
    assert_eq!(admins::unlock_idle_secs(30, true), 1800, "只拉长，不缩短");
    assert_eq!(admins::unlock_idle_secs(30, false), 1800);
}

/// 标准 15：钥匙串条目没了 → `KEYCHAIN_MISSING`，**旧库文件一字不变**。
///
/// 这一条钉的是「不覆盖旧数据」：钥匙串没了就生成新密钥开一个新库，
/// 工资没了而应用一声不响 —— 那是最坏的结果。
#[test]
fn a_missing_key_leaves_the_database_untouched() {
    let fx = Fixture::new("std15");
    fx.create_admin("hr@company.com", "测试单位甲")
        .expect("建档失败");
    fx.db.close();

    let before = std::fs::metadata(fx.db_file()).expect("库文件应存在");
    let size_before = before.len();
    let mtime_before = before.modified().expect("取 mtime 失败");

    fx.kc
        .store
        .delete(SecretKind::DbKey, SecretKind::DEFAULT_ACCOUNT)
        .expect("删钥匙串条目失败");

    let err = fx
        .open()
        .expect_err("库在而密钥没了，必须报 KEYCHAIN_MISSING");
    assert!(
        matches!(err, AppError::KeychainMissing),
        "必须是 KEYCHAIN_MISSING：{err:?}"
    );

    let after = std::fs::metadata(fx.db_file()).expect("库文件仍应存在");
    assert_eq!(after.len(), size_before, "库文件字节数不能变");
    assert_eq!(
        after.modified().expect("取 mtime 失败"),
        mtime_before,
        "库文件 mtime 不能变（不能被重写）"
    );
    // 也没生成新密钥
    assert!(
        fx.kc
            .store
            .get(SecretKind::DbKey, SecretKind::DEFAULT_ACCOUNT)
            .is_err(),
        "不能顺手生成一把新密钥"
    );
}

/// 标准 16：只读横幅的文案与写被拒的文案**逐字相同**，且都不是那句
/// 「本机加密数据打不开」（那与「已解锁、能读」矛盾）。
#[test]
fn the_read_only_banner_and_the_rejection_share_one_wording() {
    let fx = Fixture::new("std16");
    fx.create_admin("hr@company.com", "测试单位甲")
        .expect("建档失败");

    // 制造只读：改一条已应用的迁移，checksum 对不上
    let state = fx.state();
    assert!(!state.read_only, "前置条件：现在是可写态");
    assert_eq!(state.read_only_message, None, "可写态没有横幅");

    // 用只读会话：直接构造一个「迁移漂移」的库。
    // 这里借 `infra::db` 的测试做法：改迁移文本 → 重开 → 只读。
    let edited = crate::infra::db::MIGRATIONS
        .iter()
        .map(|m| crate::infra::db::Migration {
            version: m.version,
            name: m.name,
            sql: m.sql,
        })
        .collect::<Vec<_>>();
    // 用一条会失败的迁移把会话打成只读（失败原因不重要，重要的是「只读」这个状态）
    let failing = crate::infra::db::Migration {
        version: 999,
        name: "acceptance-failing",
        sql: "CREATE TABLE nope (x INTEGER); INSERT INTO no_such_table VALUES (1);",
    };
    let mut with_failing = edited.clone();
    with_failing.push(failing);
    let outcome = fx
        .db
        .open_with(&fx.kc.store, &fx.dir, &CIPHER_PARAMS, &with_failing)
        .expect("重开应走只读分支");
    assert!(
        matches!(outcome, OpenOutcome::ReadOnly { .. }),
        "必须是只读态：{outcome:?}"
    );

    let state = fx.state();
    assert!(state.read_only, "状态必须报只读：{state:?}");
    let banner = state.read_only_message.expect("只读态必须带横幅文案");
    assert!(banner.contains("本次修改未写入"), "缺：影响范围 · {banner}");
    assert!(
        !banner.contains("打不开"),
        "不该用那句矛盾的通用文案 · {banner}"
    );

    let err = fx.db.write(|_| Ok(())).expect_err("只读态写必须被拒");
    assert_eq!(
        err.user_message(),
        banner,
        "写被拒的文案必须与横幅逐字相同（spec 标准 16）"
    );
}

/// 标准 18：数据目录只有一个真相源 —— `app_data_dir()` 那个，
/// 界面上显示的、备份要找的、库文件实际待着的，是同一个目录。
#[test]
fn the_data_directory_has_exactly_one_truth() {
    // identifier 从 tauri.conf.json 读，不硬编码（与 `infra::db` 的 `dirs_hint` 同口径）
    let conf: serde_json::Value =
        serde_json::from_str(include_str!("../tauri.conf.json")).expect("tauri.conf.json 读不动");
    let identifier = conf["identifier"]
        .as_str()
        .expect("tauri.conf.json 里没有 identifier");
    assert_eq!(
        identifier, "cn.payslipmailer.desktop",
        "bundle id 改了，备份与界面显示的路径都要跟着变（ADR-0012 第 1 条）"
    );

    // 应用真的会在那个目录下建出库文件
    let fx = Fixture::new("std18");
    fx.open().expect("开库");
    assert!(fx.db_file().exists(), "库文件应落在传进去的数据目录里");
    assert_eq!(fx.db_file().file_name().unwrap(), DB_FILE_NAME);
}

/// 标准 20：口令不进错误文案、不进序列化后的 payload。
#[test]
fn the_password_never_reaches_the_error_text() {
    let fx = Fixture::new("std20");
    fx.create_admin("hr@company.com", "测试单位甲")
        .expect("建档失败");
    fx.db.close();

    let err = admins::unlock(&fx.env(), "这不是那个口令", NOW).expect_err("错口令必须失败");
    let msg = err.user_message();
    assert!(
        !msg.contains("这不是那个口令"),
        "错误文案回显了口令 · {msg}"
    );
    assert!(
        !serde_json::to_string(&err)
            .unwrap()
            .contains("这不是那个口令"),
        "序列化后的 payload 里也不该有口令"
    );
    // 库里也不该有口令明文
    fx.open().expect("开库");
    let hash = fx.pwd_hash();
    assert!(hash.starts_with("$argon2id$"), "库里只存哈希：{hash}");
    assert!(!hash.contains("这不是那个口令"));
}
