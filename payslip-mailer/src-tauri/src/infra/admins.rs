//! `admins` 表与 `app_meta` 的管理员侧键：建档（含单位名称）、解锁、改口令、
//! 空闲阈值、单位名称读写（TC-102 · TC-103 · ADR-0011 §5–§6 · ADR-0012 §3）
//!
//! 这个文件只负责两件事：**顺序**与**什么时候关库**。判定规则在 `domain::auth`
//! （纯函数、时钟注入），哈希在 `infra::password`（不依赖 Keychain），所以
//! 「错 5 次锁 60 秒」这类规则在一毫秒内就能测完；这里剩下的组合流程只需要回答
//! 「库里那一行后来变成了什么」和「连接还在不在」。
//!
//! 顺序是**先开库、后验口令**，与设计文档 1026 的字面顺序相反 —— 因为 `pwd_hash`、
//! `failed_attempts`、`locked_until` 全在加密库里，不开库读不到。取舍与否掉的方案
//! 记在 ADR-0011 §6；失败与锁定的分支**立刻 `close()`**，回到 ch.5 要求的
//! 「释放连接、内存里不留明文密钥」。

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{OptionalExtension, params};
use serde::Serialize;

use crate::domain::admin::{check_email_format, check_password_policy};
use crate::domain::auth::{self, Attempt, AttemptRecord};
use crate::error::{AppError, AppResult};
use crate::infra::db::{CipherParams, DbState};
use crate::infra::keychain::KeychainStore;
use crate::infra::password;

/// 空闲自动锁定的默认分钟数（设计文档 795 / 2013）。
pub const DEFAULT_IDLE_LOCK_MINUTES: u32 = 10;

/// 阈值下限。0 没有意义：那等于「永不空闲锁定」，而空闲锁定是设计文档安全模型的一部分。
/// 设置页的下拉框（TC-103）只给 5/10/30，这里放宽到 1 是给排障留余地。
pub const MIN_IDLE_LOCK_MINUTES: u32 = 1;

/// 阈值上限：一天。再大就只是「忘了设」而不是策略。
pub const MAX_IDLE_LOCK_MINUTES: u32 = 24 * 60;

/// `app_meta` 里存阈值的键。TC-103 的设置页写同一个键（ADR-0011 §9）。
const IDLE_LOCK_KEY: &str = "idle_lock_minutes";

/// `app_meta` 里存单位名称的键（TC-103 · ADR-0012 §3）。
/// 设计文档说它是「邮件模板默认值」——TC-202 的模板引擎读这个键，不是 `admins` 的列。
const ORG_NAME_KEY: &str = "org_name";

/// 解锁要用的外部资源：库状态 + 钥匙串 + 数据目录 + 加密参数。
///
/// 打包成一个参数是因为 `create_admin` / `unlock` / `probe` 要的是同一套，
/// Command 层构造一次就能给三个函数用，不会三处各拼一遍、各拼错一次。
pub struct UnlockEnv<'a> {
    pub db: &'a DbState,
    pub store: &'a KeychainStore,
    pub data_dir: &'a Path,
    pub params: &'a CipherParams,
}

/// 一次成功解锁的附加结论。口令对不对不在这里——不对就直接返回 `Err` 了。
#[derive(Debug)]
pub struct UnlockOutcome {
    /// 本次顺手把哈希升到了当前参数（spec 标准 9）。前端可以不管它，
    /// 但验收时要能从返回值里读到，而不是只能靠读库反推。
    pub rehashed: bool,
    /// 库处于只读态（迁移失败，TC-100）。解锁照样成功，但界面必须显示
    /// 「当前只能查看」，否则 HR 会以为改动已经存下了。
    pub read_only: bool,
    /// 解锁成功 = 库开着（ADR-0012 第 7 条）。成功路径上恒为 `true`，
    /// 但把「取法」钉在这里是有意的：`unlock` 这条路上库是我们主动开的，
    /// 取 `db.is_open()` 才对——与 `probe` 的 `!opened_by_us` 是两种口径，
    /// 以后改任何一个入口时，编译不过或测试红都会把这件事拽回视线。
    pub unlocked: bool,
}

/// 锁屏**进入时**要显示的状态（spec 标准 6/8 的「剩余次数与倒计时」）。
///
/// 与 `Attempt` 分开，是因为锁定计数跨进程存活：应用重启后锁屏要显示
/// 「已锁定，还剩 37 秒」，那时没人尝试过口令，手上没有任何错误码可依据。
/// `initialized` 是给 TC-103 分流用的：没建档走 S01 向导，已建档走 S02 解锁。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminState {
    pub initialized: bool,
    /// 管理员邮箱，锁屏上显示「解锁 xxx」。没建档时是 `None`。
    pub email: Option<String>,
    /// 库是开着的吗（能不能直接进工作台）。**两个入口两种取法**（ADR-0012 第 7 条）：
    /// `probe` 取 `!opened_by_us`——它为了读邮箱会自己临时开库，照 `is_open()` 取
    /// 会恒为 `true`，锁屏永远进不去；`unlock` 那条路上库是我们主动开的，取 `is_open()`。
    ///
    /// 与 `locked` 是两个「锁」：`locked` = 口令失败锁定（5 次输错），
    /// `unlocked` = 库开着。口令锁定期间库一定是关的，但库关着不等于口令锁定
    /// （空闲锁定、⌘L、唤醒锁定都只关库）。前端判 `unlocked` 选界面，
    /// 显示倒计时看 `locked`。
    pub unlocked: bool,
    pub locked: bool,
    pub seconds_left: u64,
    pub remaining_attempts: u32,
    pub read_only: bool,
    /// 只读横幅的整句文案（TC-103 标准 16 · ADR-0012 第 5 条），与
    /// [`crate::infra::db::DbState::read_only_message`] 同源：横幅显示它，
    /// 写被拒的错误文案也是同一句——「同一件事」在逐字层面成立，前端不用自己拼。
    /// 文案里点名漂移版本（如有）；可写态是 `None`。
    pub read_only_message: Option<String>,
}

/// 墙钟 epoch 秒。锁定判定用它（ADR-0011 §5 明写了墙钟的取舍：能改系统时钟的人
/// 本来也能直接读 `data.db`，这条锁定是给诚实使用者的摩擦控制）。
///
/// **只在 Command 层调一次**，往下全部当参数传：这样 spec 标准 6/7 那两条
/// 「推进 30 秒」「推到 60 秒整」能在测试里注入，不用 sleep 60 秒。
pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        // 时钟早于 epoch（几乎不可能）时退回 0：宁可判定为「很久以前」，
        // 也不要在这里 panic。
        .unwrap_or(0)
}

/// rusqlite 的错误映射成 `DATABASE`：细节（SQL、驱动报错）进 `Display` 与日志，
/// 不进前端 payload（`user_message()` 会抹掉）。
///
/// 保持「绑定值不进错误串」这一点：这里的绑定值是口令哈希与口令本身，
/// rusqlite 的 `Error` 只带 SQL 文本、不带 `params![]` 的值。别改成手工拼串。
fn db_err(e: rusqlite::Error) -> AppError {
    AppError::Database(e.to_string())
}

/// 确保库是开着的。返回**是不是这次开的**——只有这次开的才该由我们关回去。
///
/// 已有会话不重开：重开要白跑一遍 key → 参数 → 读回 → 迁移，而 `probe` 在
/// 解锁状态下也会被调（设置页刷新状态、失败后读剩余次数）。
fn ensure_open(env: &UnlockEnv<'_>) -> AppResult<bool> {
    if env.db.is_open() {
        return Ok(false);
    }
    env.db.open(env.store, env.data_dir, env.params)?;
    Ok(true)
}

/// 只在「这次是我们开的」时关库。
///
/// 已经是解锁状态时不能关：那等于替使用者按了 ⌘L。真实锁屏时库本来就是关着的，
/// 所以失败分支的 `close()` 在生产路径上一定会执行（测试里要先手工关一次模拟锁屏态）。
fn close_if_we_opened(env: &UnlockEnv<'_>, opened_by_us: bool) {
    if opened_by_us {
        env.db.close();
    }
}

/// `admins` 那一行里判定要用的列。
struct AdminRow {
    email: String,
    pwd_hash: String,
    record: AttemptRecord,
}

/// 读那一行。没行 = 还没建档（首启向导路径，界面归 TC-103）。
fn read_row(db: &DbState) -> AppResult<Option<AdminRow>> {
    db.read(|conn| {
        conn.query_row(
            "SELECT email, pwd_hash, failed_attempts, locked_until \
             FROM admins WHERE id = 1",
            [],
            |row| {
                Ok(AdminRow {
                    email: row.get::<_, String>(0)?,
                    pwd_hash: row.get::<_, String>(1)?,
                    record: AttemptRecord {
                        failed_attempts: row.get::<_, u32>(2)?,
                        locked_until: row.get::<_, Option<i64>>(3)?,
                    },
                })
            },
        )
        .optional()
        .map_err(db_err)
    })
}

/// 写回锁定两列。两个占位符都给值：`None` 写 NULL，不是「这一列不动」，
/// 否则上一窗口的 `locked_until` 会留着（见 `domain::auth::Writeback` 的注释）。
fn write_lockout(db: &DbState, writeback: &AttemptRecordWriteback) -> AppResult<()> {
    db.write(|conn| {
        conn.execute(
            "UPDATE admins SET failed_attempts = ?1, locked_until = ?2 WHERE id = 1",
            params![writeback.failed_attempts, writeback.locked_until],
        )
        .map_err(db_err)?;
        Ok(())
    })
}

/// 建管理员（设计文档 ch.6 S01 354）。成功后的库**保持打开**：
/// 首启向导走完就是工作台，不该为了「统一关一下」把人再锁一次。
///
/// 邮箱格式与口令策略都在**开库之前**判：校验没过就不必动库，也不必花那 10ms 哈希。
/// 邮箱走 [`crate::domain::admin::check_email_format`]（`VALIDATION_ERROR`）——
/// `INVALID_EMAIL` 那个码是 TC-201 给成员批量粘贴用的，别混用（ADR-0012 第 3 条）。
///
/// 管理员行与单位名称（`app_meta.org_name`）是**同一个事务**：不存在
/// 「建档成功、单位名称丢了」的中间态——单位名称是 TC-202 模板引擎的默认值，
/// 而建档一旦被拒（Duplicate）就没有第二次写它的入口。
/// `org_name` 不校验内容：S01 表单里它是可空项，空串就是「没填」，
/// trim 与「要不要必填」是界面的职责，TC-202 读到空值时自行兜底。
pub fn create_admin(
    env: &UnlockEnv<'_>,
    email: &str,
    password: &str,
    org_name: &str,
    now: i64,
) -> AppResult<()> {
    check_email_format(email)?;
    check_password_policy(password)?;
    let phc = password::hash(password)?;

    let opened_by_us = ensure_open(env)?;
    let written = env.db.write(|conn| {
        let exists = conn
            .query_row("SELECT count(*) FROM admins WHERE id = 1", [], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(db_err)?;
        if exists > 0 {
            // 单管理员是库层面钉的（`CHECK (id = 1)`），这里先查是为了给一句
            // 人话，而不是把 SQLite 的约束报错丢给界面。
            return Err(AppError::Duplicate("管理员".into()));
        }
        conn.execute(
            "INSERT INTO admins (id, email, pwd_hash, created_at, last_login, failed_attempts, locked_until) \
             VALUES (1, ?1, ?2, ?3, NULL, 0, NULL)",
            params![email, phc, now],
        )
        .map_err(db_err)?;
        conn.execute(
            "INSERT INTO app_meta (key, value) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![ORG_NAME_KEY, org_name],
        )
        .map_err(db_err)?;
        Ok(())
    });
    if let Err(e) = written {
        close_if_we_opened(env, opened_by_us);
        return Err(e);
    }
    Ok(())
}

/// 解锁（设计文档 ch.6 S02）。流程与关库时机逐条对应 ADR-0011 §6 的 ①–⑥。
///
/// 返回 `Ok` 就是「进了工作台」，库保持打开；返回 `Err` 的三条路
/// （读不到行、锁定期内、口令错）都必须已经释放连接。
pub fn unlock(env: &UnlockEnv<'_>, password: &str, now: i64) -> AppResult<UnlockOutcome> {
    let opened_by_us = ensure_open(env)?;
    // 只读态要在关库之前抓：`close()` 之后 `is_read_only()` 恒为 false。
    let read_only = env.db.is_read_only();

    let row = match read_row(env.db) {
        Ok(Some(row)) => row,
        Ok(None) => {
            close_if_we_opened(env, opened_by_us);
            return Err(AppError::NotInitialized);
        }
        Err(e) => {
            close_if_we_opened(env, opened_by_us);
            return Err(e);
        }
    };

    // `verify` 返回 `Err` 意味着「判定做不成」（哈希串读不出、内存分配失败），
    // 不是「口令错」——那种情况报 BAD_CREDENTIALS 会让人一直重敲一个正确的口令。
    let checked = match password::verify(&row.pwd_hash, password) {
        Ok(checked) => checked,
        Err(e) => {
            close_if_we_opened(env, opened_by_us);
            return Err(e);
        }
    };
    let (attempt, writeback) = auth::decide(&row.record, checked.password_ok, now);

    match attempt {
        Attempt::Locked { seconds_left } => {
            // 锁定期内：不写回、不延长（spec 标准 6），立刻关库。
            close_if_we_opened(env, opened_by_us);
            Err(AppError::Locked(seconds_left))
        }
        Attempt::Failed { .. } => {
            if let Some(writeback) = writeback
                && let Err(e) = write_lockout(env.db, &writeback)
            {
                // 只读态（迁移失败）会写不进去。这时计数不落盘 = 下次还剩几次是错的，
                // 但不能因此把「解锁失败」说成别的码，也不能谎称已经落盘。
                tracing::warn!(error = %e, "解锁失败后的锁定计数没有落盘（只读态或未解锁）");
            }
            close_if_we_opened(env, opened_by_us);
            Err(AppError::BadCredentials)
        }
        Attempt::Unlocked => {
            // 参数偏旧就在解锁这次顺手升级（spec 标准 9）。哈希放在写之前算，
            // 免得握着库的互斥锁做 10ms 的 Argon2。
            let upgraded = if checked.needs_rehash {
                match password::hash(password) {
                    Ok(phc) => Some(phc),
                    Err(e) => {
                        close_if_we_opened(env, opened_by_us);
                        return Err(e);
                    }
                }
            } else {
                None
            };

            // 三条 UPDATE 不在一个事务里：中途断电最坏的后果是计数没清（下次少一次机会）
            // 或哈希没升级（旧串照样能验，见 `password.rs` 的对照测试），没有会丢数据的情形。
            let written = env.db.write(|conn| {
                conn.execute(
                    "UPDATE admins SET failed_attempts = 0, locked_until = NULL, last_login = ?1 \
                     WHERE id = 1",
                    params![now],
                )
                .map_err(db_err)?;
                if let Some(phc) = &upgraded {
                    conn.execute("UPDATE admins SET pwd_hash = ?1 WHERE id = 1", params![phc])
                        .map_err(db_err)?;
                }
                Ok(())
            });
            if let Err(e) = written {
                // 只读态下解锁照样算成功：库读得出来，使用者就该进工作台，
                // 只是界面要显示「当前只能查看」（`UnlockOutcome::read_only`）。
                tracing::warn!(error = %e, "解锁成功但计数与哈希未写回（只读态）");
            }
            Ok(UnlockOutcome {
                rehashed: upgraded.is_some(),
                read_only,
                unlocked: env.db.is_open(),
            })
        }
    }
}

/// 解锁后本次会话的空闲阈值，秒（TC-103 标准 14 · ADR-0012 第 8 条）。
///
/// 「记住本机 10 分钟」= 取 `max(库里阈值, 10 分钟)`；不勾就是库里阈值。
/// 规则放这里是纯函数（阈值 → 秒，只做算术），Command 层拿到后喂给
/// `LockWatch::set_idle_secs`。**不写库**——`app_meta.idle_lock_minutes`
/// 在解锁前后一字不变，这是界面上那个开关与设置页那个下拉的本质区别；
/// 重启失效（下次解锁回到库里阈值）；唤醒锁定与 ⌘L 不受影响（它们是事件驱动）。
///
/// 要诚实的一点：库里阈值是默认 10 分钟时勾不勾结果一样（`max(10, 10) = 10`），
/// 这个开关真正起作用的格子是「阈值被设成 5 分钟 + 今天想偷懒」。
pub fn unlock_idle_secs(stored_minutes: u32, remember_device: bool) -> u64 {
    let minutes = if remember_device {
        stored_minutes.max(DEFAULT_IDLE_LOCK_MINUTES)
    } else {
        stored_minutes
    };
    u64::from(minutes) * 60
}

/// 校验管理员口令，**没有任何副作用**（TC-105 · ADR-0014 决策 5）。
///
/// 与 [`unlock`] 的区别是**不写回锁定计数**、不改 `last_login`、不重哈希：
/// 那 5 次失败锁定是给**锁屏上的登录**用的；而销毁入口本来就在解锁态
/// （用户已经拿着全部数据），计数只会让人「手滑输错一次 → 之后不敢再试」。
///
/// 销毁是不可撤销的，所以验证与删除必须分成两步：这里只回答「对不对」，
/// 删不删由调用方决定（验证失败就一个字节都不动）。
///
/// **口令本身不出现在返回值与错误里**（红线 2 的同族）。
pub fn verify_password(env: &UnlockEnv<'_>, password: &str) -> AppResult<bool> {
    let opened_by_us = ensure_open(env)?;
    let result = (|| -> AppResult<bool> {
        let row = match read_row(env.db)? {
            Some(row) => row,
            None => return Ok(false),
        };
        match password::verify(&row.pwd_hash, password) {
            Ok(checked) => Ok(checked.password_ok),
            // 「判定做不成」（哈希串读不出、内存分配失败）不是「口令错」：
            // 报成 false 会让界面说「口令不对」，把人引去反复重输。
            Err(e) => Err(e),
        }
    })();
    if opened_by_us {
        env.db.close();
    }
    result
}

/// 读锁屏要显示的状态，不判定口令，所以任何时候都能调。
///
/// 自己开的库读完就关回去：`probe` 只是「看一眼」，不该顺手把人解锁。
pub fn probe(env: &UnlockEnv<'_>, now: i64) -> AppResult<AdminState> {
    let opened_by_us = ensure_open(env)?;
    let read_only = env.db.is_read_only();
    // 只读横幅的文案与 `read_only` 在同一处抓：都得趁会话还开着。
    let read_only_message = env.db.read_only_message();
    // `unlocked` 的取法（ADR-0012 第 7 条）：库是我们自己临时开的，「看一眼」
    // 不算解锁——进来时就已经开着（`!opened_by_us`）才是真的解锁态。
    let unlocked = !opened_by_us;

    let row = match read_row(env.db) {
        Ok(row) => row,
        Err(e) => {
            close_if_we_opened(env, opened_by_us);
            return Err(e);
        }
    };
    let state = match row {
        Some(row) => {
            let status = auth::status(&row.record, now);
            AdminState {
                initialized: true,
                email: Some(row.email),
                unlocked,
                locked: status.locked,
                seconds_left: status.seconds_left,
                remaining_attempts: status.remaining_attempts,
                read_only,
                read_only_message,
            }
        }
        None => AdminState {
            initialized: false,
            email: None,
            // 没建档谈不上锁定，但剩余次数要给满，否则界面显示「剩 0 次」很奇怪。
            locked: false,
            seconds_left: 0,
            remaining_attempts: auth::MAX_FAILED_ATTEMPTS,
            read_only,
            read_only_message,
            unlocked,
        },
    };
    close_if_we_opened(env, opened_by_us);
    Ok(state)
}

/// 改口令（设计文档 313-314「密码重置需原密码」）。
///
/// 要求库已解锁：这个入口只在设置页（解锁状态）可达。库没开时 `db.write` 会报
/// `DATABASE`，那是调用顺序写错了的兜底，不是使用者会看到的常态。
///
/// **不碰锁定两列**（spec 标准 13）：否则猜对一次旧口令就同时拿到「改口令」和
/// 「重置 5 次预算」两件事。
pub fn change_password(db: &DbState, old: &str, new: &str) -> AppResult<()> {
    check_password_policy(new)?;
    let row = read_row(db)?.ok_or(AppError::NotInitialized)?;
    let checked = password::verify(&row.pwd_hash, old)?;
    if !checked.password_ok {
        return Err(AppError::BadCredentials);
    }
    // 顺手也完成了重哈希：新串一定是当前参数。
    let phc = password::hash(new)?;
    db.write(|conn| {
        conn.execute("UPDATE admins SET pwd_hash = ?1 WHERE id = 1", params![phc])
            .map_err(db_err)?;
        Ok(())
    })
}

/// 读空闲阈值。库里没这个键（从没改过）时给默认值。
///
/// 值读不出或超出范围时**退回默认并记日志**，不报错：这是配置不是数据，
/// 一个被手工改坏的键不该让整个应用起不来。
pub fn get_idle_lock_minutes(db: &DbState) -> AppResult<u32> {
    let raw = db.read(|conn| {
        conn.query_row(
            "SELECT value FROM app_meta WHERE key = ?1",
            params![IDLE_LOCK_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(db_err)
    })?;
    let Some(raw) = raw else {
        return Ok(DEFAULT_IDLE_LOCK_MINUTES);
    };
    match raw.parse::<u32>() {
        Ok(minutes) if (MIN_IDLE_LOCK_MINUTES..=MAX_IDLE_LOCK_MINUTES).contains(&minutes) => {
            Ok(minutes)
        }
        _ => {
            tracing::warn!(
                value = %raw,
                default = DEFAULT_IDLE_LOCK_MINUTES,
                "app_meta 里的空闲阈值不可用，按默认值走"
            );
            Ok(DEFAULT_IDLE_LOCK_MINUTES)
        }
    }
}

/// 写空闲阈值。TC-103 的设置页调它，同时要把新值喂给 `LockWatch::set_idle_secs`。
pub fn set_idle_lock_minutes(db: &DbState, minutes: u32) -> AppResult<()> {
    if !(MIN_IDLE_LOCK_MINUTES..=MAX_IDLE_LOCK_MINUTES).contains(&minutes) {
        return Err(AppError::ValidationError(format!(
            "空闲自动锁定需要在 {MIN_IDLE_LOCK_MINUTES}–{MAX_IDLE_LOCK_MINUTES} 分钟之间，当前 {minutes} 分钟"
        )));
    }
    db.write(|conn| {
        conn.execute(
            "INSERT INTO app_meta (key, value) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![IDLE_LOCK_KEY, minutes.to_string()],
        )
        .map_err(db_err)?;
        Ok(())
    })
}

/// 读单位名称（S14 显示 · spec 范围 2 的「S14 读」）。没写过是 `None`，
/// 空串就是「向导里没填」——两者前端都按「未填」渲染。
pub fn get_org_name(db: &DbState) -> AppResult<Option<String>> {
    db.read(|conn| {
        conn.query_row(
            "SELECT value FROM app_meta WHERE key = ?1",
            params![ORG_NAME_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(db_err)
    })
}

/// `domain::auth::Writeback` 的别名，只为让 `write_lockout` 的签名读得通。
/// 真正的规则（什么时候写、写什么）全在 `domain/auth.rs`。
type AttemptRecordWriteback = crate::domain::auth::Writeback;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::db::{CIPHER_PARAMS, MIGRATIONS, Migration, OpenOutcome};
    use crate::infra::keychain::{TempKeychain, no_user_interaction};
    use argon2::{Algorithm, Argon2, ParamsBuilder, PasswordHasher, Version};
    use std::path::PathBuf;

    /// 注入的「现在」。库里那两列是 epoch 秒，测试里全部用它算，不碰真实时钟。
    const NOW: i64 = 1_780_000_000;

    /// 够长、够独特的口令：标准 3 要在库文件的原始字节里搜它，短了会有撞车的运气问题。
    const SECRET: &str = "OuyangAotian-2026-GongZiTiao";

    /// 错口令。与 SECRET 只差几个字符，才测得出「比较是不是真的在比」。
    const WRONG: &str = "OuyangAotian-2026-GongZiTiaP";

    /// 每个用例一套「临时数据目录 + 临时钥匙串 + DbState」，与 `infra::db` 的测试基座同构：
    /// 绝不碰登录钥匙串，也绝不碰应用真实使用的数据目录（`app_data_dir()` 那个，
    /// 口径见 ADR-0012 第 1 条）。
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
                "xinyou-admins-test-{}-{nanos}-{label}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).expect("建临时数据目录失败");
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
            self.dir.join("data.db")
        }

        /// 回到锁屏态。建档会把库留着，而真实解锁发生在库关着的时候——
        /// 不先关就测不到「失败分支释放连接」，也测不到 `ensure_open` 自己开自己关。
        fn lock(&self) {
            self.db.close();
        }

        /// 「重装应用」：换一个 `DbState`，数据目录与钥匙串都不变（spec 标准 8）。
        fn restart(&mut self) {
            self.db = DbState::new();
        }

        /// 读锁定两列。库没开就临时开、读完关回去，不改变调用方看到的锁屏态。
        fn lock_columns(&self) -> (u32, Option<i64>) {
            let env = self.env();
            let opened = ensure_open(&env).expect("开库失败");
            let out = self
                .db
                .read(|conn| {
                    conn.query_row(
                        "SELECT failed_attempts, locked_until FROM admins WHERE id = 1",
                        [],
                        |row| Ok((row.get::<_, u32>(0)?, row.get::<_, Option<i64>>(1)?)),
                    )
                    .map_err(db_err)
                })
                .expect("读锁定两列失败");
            if opened {
                self.db.close();
            }
            out
        }

        /// 手工把两列写成给定值（用来造「已经错过几次」的前提，不靠真的错几次）。
        fn force_lock_columns(&self, attempts: u32, until: Option<i64>) {
            let env = self.env();
            let opened = ensure_open(&env).expect("开库失败");
            self.db
                .write(|conn| {
                    conn.execute(
                        "UPDATE admins SET failed_attempts = ?1, locked_until = ?2 WHERE id = 1",
                        params![attempts, until],
                    )
                    .map_err(db_err)?;
                    Ok(())
                })
                .expect("写锁定两列失败");
            if opened {
                self.db.close();
            }
        }

        fn pwd_hash(&self) -> String {
            let env = self.env();
            let opened = ensure_open(&env).expect("开库失败");
            let out = self
                .db
                .read(|conn| {
                    conn.query_row("SELECT pwd_hash FROM admins WHERE id = 1", [], |row| {
                        row.get::<_, String>(0)
                    })
                    .map_err(db_err)
                })
                .expect("读 pwd_hash 失败");
            if opened {
                self.db.close();
            }
            out
        }

        /// 把库里的哈希换成任意串（标准 9 要的是「库里存着老参数的哈希」这个前提）。
        fn force_pwd_hash(&self, phc: &str) {
            let env = self.env();
            let opened = ensure_open(&env).expect("开库失败");
            self.db
                .write(|conn| {
                    conn.execute("UPDATE admins SET pwd_hash = ?1 WHERE id = 1", params![phc])
                        .map_err(db_err)?;
                    Ok(())
                })
                .expect("改写 pwd_hash 失败");
            if opened {
                self.db.close();
            }
        }

        /// `app_meta` 里的行数。用来钉住「没改过设置时库里不该预插行」。
        fn app_meta_rows(&self) -> i64 {
            let env = self.env();
            let opened = ensure_open(&env).expect("开库失败");
            let out = self
                .db
                .read(|conn| {
                    conn.query_row("SELECT count(*) FROM app_meta", [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .map_err(db_err)
                })
                .expect("数 app_meta 失败");
            if opened {
                self.db.close();
            }
            out
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// 手工产一串**偏松参数**的哈希（`m=2048,t=2,p=1`，spike 实测过这组能跑）。
    fn legacy_hash(password: &str) -> String {
        Argon2::new(
            Algorithm::Argon2id,
            Version::V0x13,
            ParamsBuilder::new()
                .m_cost(2048)
                .t_cost(2)
                .p_cost(1)
                .build()
                .expect("旧参数应合法"),
        )
        .hash_password_with_salt(password.as_bytes(), b"0123456789abcdef")
        .expect("旧参数哈希失败")
        .to_string()
    }

    /// 原始字节里出现了几次 —— 标准 3 要的「grep 命中 0 次」。
    fn count_occurrences(haystack: &[u8], needle: &[u8]) -> usize {
        if needle.is_empty() || haystack.len() < needle.len() {
            return 0;
        }
        haystack
            .windows(needle.len())
            .filter(|window| *window == needle)
            .count()
    }

    /// 验收标准 3：库里是 Argon2id 的 PHC 串，且 `data.db` 的原始字节里搜不到口令。
    #[test]
    fn admin_row_holds_an_argon2id_hash_and_the_db_file_holds_no_plaintext() {
        let fx = Fixture::new("hash-at-rest");
        create_admin(&fx.env(), "hr@example.com", SECRET, "测试单位甲", NOW).expect("建档失败");

        let phc = fx.pwd_hash();
        assert!(
            phc.starts_with("$argon2id$"),
            "库里存的不是 Argon2id 的 PHC 串 · {phc}"
        );
        assert!(!phc.contains(SECRET), "哈希串里出现了口令明文 · {phc}");
        assert!(
            password::verify(&phc, SECRET)
                .expect("读回的哈希应能校验")
                .password_ok,
            "读回来的那一串验不过原口令，说明存的时候写坏了"
        );

        // 整页加密：关掉连接后再读文件，否则「搜不到」可能只是因为页还在内存里。
        fx.db.close();
        let bytes = std::fs::read(fx.db_file()).expect("读 data.db 失败");
        assert!(
            bytes.len() > 4096,
            "库文件小到不像话，加密页可能根本没写出来 · {} 字节",
            bytes.len()
        );
        assert_eq!(
            count_occurrences(&bytes, SECRET.as_bytes()),
            0,
            "data.db 的原始字节里搜到了口令明文"
        );
        // 反向对照：同一个搜索方法在明文上必须搜得到，否则「0 命中」是方法坏了。
        assert_eq!(count_occurrences(&bytes, b"SQLite format 3"), 0);
        assert_eq!(count_occurrences(SECRET.as_bytes(), b"Ouyang"), 1);
    }

    /// 验收标准 4：两台机器（两套钥匙串 + 数据目录）同一口令 → 两个不同的哈希。
    #[test]
    fn the_same_password_hashes_differently_on_two_machines() {
        let a = Fixture::new("salt-a");
        let b = Fixture::new("salt-b");
        create_admin(&a.env(), "a@example.com", SECRET, "单位甲", NOW).expect("A 建档失败");
        create_admin(&b.env(), "b@example.com", SECRET, "单位乙", NOW).expect("B 建档失败");

        let (hash_a, hash_b) = (a.pwd_hash(), b.pwd_hash());
        assert_ne!(
            hash_a, hash_b,
            "同一口令在两台机器上得到同一个哈希，说明盐没生效或是写死的"
        );
        for phc in [&hash_a, &hash_b] {
            let out = password::verify(phc, SECRET).expect("校验失败");
            assert!(out.password_ok, "另一台机器上的哈希验不过同一口令");
            // 对照：同一串哈希对错口令必须给「否」，否则上面那句「可验」没有信息量
            let wrong = password::verify(phc, WRONG).expect("错口令应返回判定而不是报错");
            assert!(!wrong.password_ok, "错口令在 {phc} 上验得过了");
        }
    }

    /// 验收标准 5：错口令是 `BAD_CREDENTIALS`，且错误里不含口令明文。
    #[test]
    fn wrong_password_is_bad_credentials_and_never_carries_the_password() {
        let fx = Fixture::new("bad-credentials");
        create_admin(&fx.env(), "hr@example.com", SECRET, "测试单位甲", NOW).expect("建档失败");
        fx.lock();

        let err = unlock(&fx.env(), WRONG, NOW).expect_err("错口令必须报错");
        assert!(
            matches!(err, AppError::BadCredentials),
            "实际是 {err:?}：必须是 BAD_CREDENTIALS"
        );

        let json = serde_json::to_string(&err).expect("错误应可序列化");
        assert!(
            json.contains("\"code\":\"BAD_CREDENTIALS\""),
            "前端要按 code 分支 · {json}"
        );
        for text in [json.clone(), err.to_string(), err.user_message()] {
            assert!(!text.contains(SECRET), "错误里出现了口令明文 · {text}");
        }
        // 剩余次数走独立字段，不拼进文案（spec 标准 5 的说明）。
        assert!(
            !err.user_message().contains("剩余"),
            "文案里不该自己数次数 · {}",
            err.user_message()
        );

        // ADR-0011 §6 ⑤：失败分支必须回到 ch.5 的「不持有连接」。
        assert!(!fx.db.is_open(), "解锁失败后连接必须释放");
    }

    /// 验收标准 6：错 1..5 剩 4/3/2/1/0，第 6 次 `LOCKED(60)`；锁定期内不延长、两列不动。
    #[test]
    fn five_failures_lock_and_the_window_freezes_the_counters() {
        let fx = Fixture::new("lockout");
        create_admin(&fx.env(), "hr@example.com", SECRET, "测试单位甲", NOW).expect("建档失败");
        fx.lock();

        let mut remaining_seen = Vec::new();
        for attempt in 1..=5 {
            let err = unlock(&fx.env(), WRONG, NOW).expect_err("错口令必须报错");
            assert!(
                matches!(err, AppError::BadCredentials),
                "第 {attempt} 次失败应是 BAD_CREDENTIALS，实际 {err:?}"
            );
            assert!(!fx.db.is_open(), "第 {attempt} 次失败后连接必须释放");
            let state = probe(&fx.env(), NOW).expect("读状态失败");
            remaining_seen.push(state.remaining_attempts);
            // 第 5 次失败当场就把锁按下了（写回 `locked_until = now + 60`），所以从那一刻起
            // 锁屏就该显示「已锁定 · 还剩 60 秒」；前 4 次只是计数在涨，还能继续试。
            // 本次尝试本身仍然返回 `BAD_CREDENTIALS` 而不是 `LOCKED`——设计文档 1023 写的是
            // 「连续错误 5 次**将**锁定 60 秒」，见 `domain::auth::Attempt::Failed` 的注释。
            if attempt < 5 {
                assert!(
                    !state.locked,
                    "第 {attempt} 次失败不该进入锁定期 · {state:?}"
                );
            } else {
                assert!(
                    state.locked && state.seconds_left == 60,
                    "第 5 次失败当场就该进入锁定期并给出倒计时 · {state:?}"
                );
            }
        }
        assert_eq!(
            remaining_seen,
            vec![4, 3, 2, 1, 0],
            "剩余次数必须逐个对上设计文档 1023 的文案"
        );

        let frozen = fx.lock_columns();
        assert_eq!(
            frozen,
            (5, Some(NOW + 60)),
            "第 5 次失败应同时写下计数与锁定时刻"
        );

        // 第 6 次：锁定期内，剩余 60 秒整
        let err = unlock(&fx.env(), WRONG, NOW).expect_err("锁定期内必须报错");
        assert!(matches!(err, AppError::Locked(60)), "实际是 {err:?}");

        // 推进 30 秒：仍是 LOCKED，且剩余是 30（每次现算，不是存的值）
        let err = unlock(&fx.env(), WRONG, NOW + 30).expect_err("锁定期内必须报错");
        assert!(matches!(err, AppError::Locked(30)), "实际是 {err:?}");
        assert_eq!(
            fx.lock_columns(),
            frozen,
            "锁定期内的尝试不该改动库里那两列"
        );

        // 锁定期内连正确口令都不放行：那 60 秒要挡的就是「不停试」这个动作本身
        let err = unlock(&fx.env(), SECRET, NOW + 40).expect_err("锁定期内正确口令也不该解锁");
        assert!(matches!(err, AppError::Locked(20)), "实际是 {err:?}");
        assert_eq!(fx.lock_columns(), frozen, "连输对口令都不该改动两列");

        let state = probe(&fx.env(), NOW + 50).expect("读状态失败");
        assert!(state.locked && state.seconds_left == 10, "锁屏要显示倒计时");
        assert_eq!(state.remaining_attempts, 0);
    }

    /// 验收标准 7：推到 60 秒整，正确口令解锁成功，两列清空。
    #[test]
    fn the_lockout_expires_and_the_budget_resets() {
        let fx = Fixture::new("expiry");
        create_admin(&fx.env(), "hr@example.com", SECRET, "测试单位甲", NOW).expect("建档失败");
        fx.lock();
        for _ in 1..=5 {
            let _ = unlock(&fx.env(), WRONG, NOW).expect_err("前 5 次都该报错");
        }
        assert_eq!(fx.lock_columns(), (5, Some(NOW + 60)));

        // 60 秒整算已过期（不是还差一毫秒）
        let outcome = unlock(&fx.env(), SECRET, NOW + 60).expect("60 秒整应能解锁");
        assert!(!outcome.rehashed, "参数没变，不该标记重哈希");
        assert!(fx.db.is_open(), "解锁成功必须留着连接（进工作台）");
        assert_eq!(
            fx.lock_columns(),
            (0, None),
            "计数与锁定时刻都必须清空，否则「连续 5 次」永远回不来"
        );

        // 过期后第一次失败是从 0 重新数起，不是接着 5 立刻再锁
        fx.lock();
        let err = unlock(&fx.env(), WRONG, NOW + 61).expect_err("错口令必须报错");
        assert!(matches!(err, AppError::BadCredentials), "实际是 {err:?}");
        let state = probe(&fx.env(), NOW + 61).expect("读状态失败");
        assert_eq!(state.remaining_attempts, 4, "窗口过期后必须给回 5 次预算");
        assert_eq!(fx.lock_columns(), (1, None));
    }

    /// 验收标准 8：计数跨进程存活 —— 设计文档 1023「无法通过重装应用绕过」。
    #[test]
    fn counters_survive_a_process_restart() {
        let mut fx = Fixture::new("restart");
        create_admin(&fx.env(), "hr@example.com", SECRET, "测试单位甲", NOW).expect("建档失败");
        fx.lock();

        for _ in 1..=3 {
            let _ = unlock(&fx.env(), WRONG, NOW).expect_err("错口令必须报错");
        }
        assert_eq!(
            probe(&fx.env(), NOW)
                .expect("读状态失败")
                .remaining_attempts,
            2
        );

        // 「重装应用」= 换一个 DbState，数据目录与钥匙串都不变
        fx.restart();
        assert!(!fx.db.is_open(), "重启后必须是锁定态");
        let state = probe(&fx.env(), NOW).expect("重启后读状态失败");
        assert_eq!(
            state.remaining_attempts, 2,
            "计数在内存里的话这里会变成 4，那句话就成了谎"
        );

        let err = unlock(&fx.env(), WRONG, NOW).expect_err("错口令必须报错");
        assert!(matches!(err, AppError::BadCredentials), "实际是 {err:?}");
        let state = probe(&fx.env(), NOW).expect("读状态失败");
        assert_eq!(
            state.remaining_attempts, 1,
            "重启后再错一次，剩余必须是 1 而不是 3"
        );
    }

    /// 验收标准 9：库里存着偏旧参数的哈希时，解锁顺手升级并标记「已重哈希」。
    #[test]
    fn an_outdated_hash_is_upgraded_during_a_successful_unlock() {
        let fx = Fixture::new("rehash");
        create_admin(&fx.env(), "hr@example.com", SECRET, "测试单位甲", NOW).expect("建档失败");

        let legacy = legacy_hash(SECRET);
        assert!(
            legacy.contains("m=2048"),
            "对照组必须是偏松的参数 · {legacy}"
        );
        fx.force_pwd_hash(&legacy);
        assert!(
            fx.pwd_hash().contains("m=2048"),
            "前提没成立：库里不是旧参数 · {}",
            fx.pwd_hash()
        );
        fx.lock();

        let outcome = unlock(&fx.env(), SECRET, NOW).expect("旧参数的哈希也该解锁成功");
        assert!(
            outcome.rehashed,
            "返回体必须标记「已重哈希」，否则这条只能靠读库反推"
        );

        let upgraded = fx.pwd_hash();
        assert!(
            upgraded.contains(&format!(
                "m={},t={},p={}",
                password::M_KIB,
                password::T_COST,
                password::P_COST
            )),
            "库里的参数段没升到代码常量 · {upgraded}"
        );
        assert_ne!(upgraded, legacy, "重哈希必须换盐，不能只改参数段");

        // 新串可验、且不再被标记为偏旧
        let recheck = password::verify(&upgraded, SECRET).expect("重哈希后校验失败");
        assert!(recheck.password_ok);
        assert!(!recheck.needs_rehash, "升级后不该再被标记");

        // 旧串本身仍可验：升级中途断电不会把人锁死在门外
        let old_still_works = password::verify(&legacy, SECRET).expect("旧串重验失败");
        assert!(old_still_works.password_ok);
        assert!(old_still_works.needs_rehash);

        // 第二次解锁不该再重哈希（否则每次解锁都白写一遍库）
        fx.lock();
        let second = unlock(&fx.env(), SECRET, NOW + 1).expect("第二次解锁失败");
        assert!(!second.rehashed, "已经是当前参数就不该再升级");
    }

    /// 验收标准 13：改口令要原口令，且两种结果都不动锁定两列。
    #[test]
    fn changing_the_password_requires_the_old_one_and_leaves_the_counters_alone() {
        let fx = Fixture::new("change-password");
        create_admin(&fx.env(), "hr@example.com", SECRET, "测试单位甲", NOW).expect("建档失败");
        // 前提：两列是非默认值，才看得出「改口令有没有顺手清零」。
        fx.force_lock_columns(2, Some(NOW + 60));
        let before = fx.lock_columns();
        assert_eq!(before, (2, Some(NOW + 60)));

        let old_hash = fx.pwd_hash();
        let new_password = "XinKouLing-2026-Abc";

        let err = change_password(&fx.db, WRONG, new_password).expect_err("原口令错必须报错");
        assert!(matches!(err, AppError::BadCredentials), "实际是 {err:?}");
        assert_eq!(fx.pwd_hash(), old_hash, "原口令错时不该改写哈希");
        assert_eq!(fx.lock_columns(), before, "改口令失败不该动锁定计数");

        change_password(&fx.db, SECRET, new_password).expect("原口令对应该能改口令");
        let fresh = fx.pwd_hash();
        assert_ne!(fresh, old_hash, "哈希没变，说明没写进去");
        assert!(
            password::verify(&fresh, new_password)
                .expect("新口令校验失败")
                .password_ok,
            "新口令验不过"
        );
        assert!(
            !password::verify(&fresh, SECRET)
                .expect("旧口令校验失败")
                .password_ok,
            "改过之后旧口令必须验不过了"
        );
        assert_eq!(fx.lock_columns(), before, "改口令成功也不该重置锁定预算");

        // 新口令仍要过策略：太短直接被拒，且不改动库里任何东西
        let err = change_password(&fx.db, new_password, "太短").expect_err("太短必须被拒");
        assert!(
            matches!(err, AppError::ValidationError(_)),
            "实际是 {err:?}"
        );
        assert_eq!(fx.pwd_hash(), fresh, "被拒的改口令不该写库");
    }

    /// 空闲阈值：默认 10、可改、越界被拒，且没改过时库里不该预插行。
    #[test]
    fn idle_threshold_defaults_to_ten_and_round_trips() {
        let fx = Fixture::new("idle-minutes");
        ensure_open(&fx.env()).expect("开库失败");

        assert_eq!(
            DEFAULT_IDLE_LOCK_MINUTES, 10,
            "设计文档 795/2013 写的是 10 分钟"
        );
        assert_eq!(
            get_idle_lock_minutes(&fx.db).expect("读默认值失败"),
            DEFAULT_IDLE_LOCK_MINUTES
        );
        assert_eq!(
            fx.app_meta_rows(),
            0,
            "没改过设置时库里不该预插行，否则「没改过」与「改成 10」长得一样"
        );

        for minutes in [5u32, 30, 1, MAX_IDLE_LOCK_MINUTES] {
            set_idle_lock_minutes(&fx.db, minutes).expect("写阈值失败");
            assert_eq!(
                get_idle_lock_minutes(&fx.db).expect("读阈值失败"),
                minutes,
                "{minutes} 分钟没存住"
            );
        }

        for bad in [0u32, MAX_IDLE_LOCK_MINUTES + 1, u32::MAX] {
            let err = set_idle_lock_minutes(&fx.db, bad).expect_err("{bad} 应被拒");
            assert!(
                matches!(err, AppError::ValidationError(_)),
                "{bad} 实际是 {err:?}"
            );
        }
        assert_eq!(
            get_idle_lock_minutes(&fx.db).expect("读阈值失败"),
            MAX_IDLE_LOCK_MINUTES,
            "被拒的写入不该改动已存的值"
        );
    }

    /// 首启的两个岔路：没建档时解锁给 `NOT_INITIALIZED`；重复建档被拒。
    #[test]
    fn unlock_before_init_reports_not_initialized_and_a_second_admin_is_refused() {
        let fx = Fixture::new("not-initialized");

        // 口令策略在开库之前判：策略没过就不必动库
        let err = create_admin(&fx.env(), "hr@example.com", "太短", "测试单位甲", NOW)
            .expect_err("短口令必须被策略拒掉");
        assert!(
            matches!(err, AppError::ValidationError(_)),
            "实际是 {err:?}"
        );

        let err = unlock(&fx.env(), SECRET, NOW).expect_err("还没建档就解锁必须报错");
        assert!(
            matches!(err, AppError::NotInitialized),
            "实际是 {err:?}：前端要据此走 S01 首启向导"
        );
        assert!(!fx.db.is_open(), "报错分支必须释放连接");

        let state = probe(&fx.env(), NOW).expect("读状态失败");
        assert!(!state.initialized, "锁屏要能区分「没建档」与「已锁定」");
        assert_eq!(state.remaining_attempts, auth::MAX_FAILED_ATTEMPTS);
        assert_eq!(state.email, None);

        create_admin(&fx.env(), "hr@example.com", SECRET, "测试单位甲", NOW)
            .expect("首启建档应成功");
        assert!(probe(&fx.env(), NOW).expect("读状态失败").initialized);

        let before = fx.pwd_hash();
        let err = create_admin(&fx.env(), "hr2@example.com", SECRET, "另一个单位", NOW)
            .expect_err("单管理员不该能被覆盖");
        assert!(matches!(err, AppError::Duplicate(_)), "实际是 {err:?}");
        assert_eq!(fx.pwd_hash(), before, "被拒的建档不该改动已存的那一行");
    }

    /// 验收标准 14 的机制面（ADR-0012 第 8 条）：阈值是秒级算术，注入值直接测。
    /// 「不写库」由 unlock 侧的既有测试保证（它们从不触碰 `app_meta`），
    /// 这里钉的是**算出来的阈值**。
    #[test]
    fn remember_device_takes_the_max_of_stored_and_ten_minutes() {
        // 库里 5 分钟：勾选 → 本次 10 分钟；不勾 → 5 分钟
        assert_eq!(unlock_idle_secs(5, true), 600);
        assert_eq!(unlock_idle_secs(5, false), 300);
        // 库里已是默认 10 分钟：恒等操作（界面文案必须诚实说明这一点）
        assert_eq!(unlock_idle_secs(10, true), 600);
        // 库里 30 分钟：勾选不降档——「记住本机」只会拉长，不会缩短
        assert_eq!(unlock_idle_secs(30, true), 1800);
        assert_eq!(unlock_idle_secs(30, false), 1800);
    }

    /// 验收标准 8 的后半句（ADR-0012 第 7 条的坑）：锁屏态连续看两次状态，
    /// 两次 `unlocked` 都必须是 `false` —— `probe` 自己临时开库不算解锁，
    /// 照 `is_open()` 取会恒为 `true`，锁屏永远进不去。
    /// 真解锁后（库是 `unlock` 留下的）再看一次，`unlocked` 必须变 `true`，
    /// 且 `probe` 不该把解锁态的库关掉。
    #[test]
    fn probe_never_unlocks_and_reports_the_true_unlocked_flag() {
        let fx = Fixture::new("probe-unlocked");
        create_admin(&fx.env(), "hr@example.com", SECRET, "测试单位甲", NOW).expect("建档失败");
        fx.lock();

        let first = probe(&fx.env(), NOW).expect("第一次 probe 失败");
        let second = probe(&fx.env(), NOW).expect("第二次 probe 失败");
        assert!(
            !first.unlocked && !second.unlocked,
            "锁屏态两次都必须 unlocked == false：{first:?} / {second:?}"
        );
        assert!(first.initialized && second.initialized, "已建档");
        assert!(!fx.db.is_open(), "probe 读完必须把临时开的库关回去");

        unlock(&fx.env(), SECRET, NOW).expect("解锁失败");
        let third = probe(&fx.env(), NOW).expect("解锁后 probe 失败");
        assert!(
            third.unlocked,
            "解锁后的 admin_status 必须 unlocked == true：{third:?}"
        );
        assert!(fx.db.is_open(), "解锁后的库不该被一次 probe 关掉");
    }

    /// 验收标准 4 的库层半句（ADR-0012 第 3 条）：绕过界面直接喂一个没有 @ 的邮箱
    /// → `VALIDATION_ERROR`，且校验发生在开库之前——磁盘上连库文件都不该出现。
    #[test]
    fn create_admin_rejects_a_malformed_email_before_touching_the_db() {
        let fx = Fixture::new("bad-email");
        let err = create_admin(&fx.env(), "hr", SECRET, "任意单位", NOW).expect_err("坏邮箱应被拒");
        assert!(
            matches!(err, AppError::ValidationError(_)),
            "必须是 VALIDATION_ERROR：{err:?}"
        );
        assert!(!fx.db.is_open(), "格式校验不该顺手开库");
        assert!(!fx.db_file().exists(), "校验失败不该留下库文件");
        // 同一个入口、同一个口令，好邮箱应当通过——证明拦的是格式而不是别的
        create_admin(&fx.env(), "hr@example.com", SECRET, "测试单位甲", NOW).expect("好邮箱应通过");
    }

    /// 验收标准 5：单位名称与建档**同一个事务**落 `app_meta.org_name`，
    /// 「重装应用」（`restart`）后仍在；重复建档被拒时不得改写它。
    #[test]
    fn org_name_is_written_with_the_admin_and_survives_a_restart() {
        let mut fx = Fixture::new("org-name");
        create_admin(&fx.env(), "hr@example.com", SECRET, "测试单位甲", NOW).expect("建档失败");
        assert_eq!(
            get_org_name(&fx.db).expect("读 org_name 失败"),
            Some("测试单位甲".into()),
            "建档与单位名称必须一起落库"
        );

        let err = create_admin(&fx.env(), "hr2@example.com", SECRET, "另一个单位", NOW)
            .expect_err("重复建档应被拒");
        assert!(matches!(err, AppError::Duplicate(_)), "实际是 {err:?}");
        assert_eq!(
            get_org_name(&fx.db).expect("读 org_name 失败"),
            Some("测试单位甲".into()),
            "被拒的建档不得改写 org_name"
        );

        fx.restart();
        // 重启后库是关的；S14 能读到它，前提总是先有一句 admin_status / unlock 开过库。
        ensure_open(&fx.env()).expect("重启后开库失败");
        assert_eq!(
            get_org_name(&fx.db).expect("重启后读 org_name 失败"),
            Some("测试单位甲".into()),
            "单位名称必须活过重启（spec 标准 5）"
        );
    }

    /// 验收标准 16 / 17 的后端半链（ADR-0012 第 4/5 条）：迁移 checksum 漂移 →
    /// 只读会话下 `probe` 仍然可用并把横幅整句文案带出来（点名是哪一版），
    /// `unlock` 照样成功且 `UnlockOutcome` 带只读与 unlocked，
    /// 写被拒的错误文案与横幅逐字同源。
    #[test]
    fn probe_and_unlock_stay_usable_in_a_read_only_session() {
        let fx = Fixture::new("read-only");
        create_admin(&fx.env(), "hr@example.com", SECRET, "测试单位甲", NOW).expect("建档失败");
        fx.lock();

        // 用「追加一行注释」的第 2 版重开：checksum 漂移 → 只读会话。
        // 漂移只比对迁移文本，表结构与数据不受影响，admins 表还在。
        let real = MIGRATIONS[1];
        let edited_sql: &'static str =
            Box::leak(format!("{}\n-- 测试用：改文本制造漂移", real.sql).into_boxed_str());
        let edited = Migration {
            version: real.version,
            name: real.name,
            sql: edited_sql,
        };
        let outcome = fx
            .db
            .open_with(
                &fx.kc.store,
                &fx.dir,
                &CIPHER_PARAMS,
                &[MIGRATIONS[0], edited],
            )
            .expect("重开失败");
        assert!(
            matches!(outcome, OpenOutcome::ReadOnly { .. }),
            "必须是只读态：{outcome:?}"
        );

        // probe：只读态下正常工作，横幅文案点名漂移版本
        let state = probe(&fx.env(), NOW).expect("只读态下 probe 必须可用");
        assert!(state.read_only, "probe 必须报告只读：{state:?}");
        assert!(
            state.unlocked,
            "库是上面 open_with 留下的（进来时已开着）：{state:?}"
        );
        let banner = state.read_only_message.expect("只读态必须带横幅文案");
        assert!(banner.contains("第 2 版"), "横幅要点名漂移版本 · {banner}");
        assert!(banner.contains("只读"), "缺：发生了什么 · {banner}");

        // unlock：照样成功（能读出哈希就能验口令），写计数失败只记日志
        let outcome = unlock(&fx.env(), SECRET, NOW).expect("只读态下解锁必须成功");
        assert!(outcome.read_only, "UnlockOutcome 必须带只读：{outcome:?}");
        assert!(outcome.unlocked, "解锁成功 = 库开着：{outcome:?}");
        assert_eq!(
            fx.db.read_only_message().as_deref(),
            Some(banner.as_str()),
            "解锁前后横幅文案必须同源"
        );

        // 写被拒的文案与横幅逐字相同（标准 16 的「同一件事」，admins 视角再钉一次）
        let err = fx.db.write(|_| Ok(())).expect_err("只读态写必须被拒");
        assert_eq!(err.user_message(), banner, "写被拒文案必须与横幅同源");
    }
}
