//! 解锁与锁定相关的 Command（TC-102 · ADR-0011 §6 与 §9）
//!
//! 这一层是**红线 5 的执行点**：`LockWatch::touch()` 在前端没有对应的 API，
//! 「HR 动了一下」这个信号只能由他调用的某个 Command 顺带发出。所以每个成功的
//! Command 末尾都要 `touch()`，而失败的 Command 不 touch —— 否则对着锁屏连点
//! 「解锁」就等于给自己续命，空闲锁定形同虚设。
//!
//! Command 清单与 spec 的对应：`admin_status` / `unlock` / `lock_now` /
//! `change_password` / `set_idle_lock_minutes` 来自 TC-102 spec 范围第 7 条；
//! `create_admin`（首启建档）与 `get_settings`（S14 的读取口）来自 TC-103 spec
//! 范围第 1/2 条 —— TC-102 时代注释里那句「没有第六个」到此兑现。
//!
//! 口令的方向性：口令只**进**不出。五个 Command 的返回值里没有一处带出口令，
//! 错误也只带 `BAD_CREDENTIALS` 这个码（spec 标准 5）。剩余次数与倒计时是
//! 状态不是错误信息，走 `admin_status` 的字段。

use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;

use crate::error::AppResult;
use crate::infra::admins::{self, AdminState, UnlockEnv};
use crate::infra::db::{CIPHER_PARAMS, DbState};
use crate::infra::keychain::KeychainStore;
use crate::infra::wake::LockWatch;

/// 解锁成功的返回体。口令对不对不在这里——不对就直接返回 `Err` 了。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnlockResult {
    /// 本次顺手把哈希升到了当前参数（spec 标准 9 要求「从返回体里能读到」，
    /// 而不是只能靠读库反推）。界面可以忽略它。
    pub rehashed: bool,
    /// 库处于只读态（迁移失败，TC-100）。解锁照样成功，但界面必须显示
    /// 「当前只能查看」，否则 HR 会以为改动已经存下了。
    pub read_only: bool,
    /// 解锁成功 = 库开着（ADR-0012 第 7 条）。成功路径上恒为 `true`；
    /// 字段在这里是为了让前端不必从「没有错误」反推「能进工作台」，
    /// 与 `admin_status` 的 `unlocked`（`probe` 口径）同名同义。
    pub unlocked: bool,
}

/// 本机数据目录。与 `app_info` 同一个口径。
///
/// 真相源在 [`crate::paths::data_dir`]：发布构建是 `app_data_dir()` 按 bundle id
/// 推出的那个目录（ADR-0012 第 1 条），开发构建加 `-dev` 后缀（两边各用各的钥匙串，
/// 共用一个库必然撞「密钥与数据不匹配」—— 口径见 `paths` 模块的说明）。
fn data_dir(app: &tauri::AppHandle) -> AppResult<PathBuf> {
    crate::paths::data_dir(app)
}

/// 钥匙串句柄：每次 Command 现构造。
///
/// 它只是 `SecKeychain::default()`，没有连接要养；存进 Tauri 状态反而会让一个
/// 钥匙串句柄跨升级与唤醒长期存活，而 ADR-0002 写明了未签名分发升级会重置授权——
/// 一个陈旧句柄比每次现取难查得多。
fn keychain() -> AppResult<KeychainStore> {
    KeychainStore::for_runtime()
}

/// 锁屏**进入时**要显示的一切：有没有建档、要不要显示倒计时、还剩几次。
///
/// 它不判定口令，所以锁屏上每次刷新都能调。`probe` 会自己开库读完再关回去
/// （ADR-0011 §6 的取舍：`pwd_hash` 与计数都在加密库里，不开库读不到，
/// 所以设计文档 1026 的「成功后才取 key」被换成了「先开库、验完立刻关」）。
///
/// 钥匙串条目不存在时这里会返回 `KEYCHAIN_MISSING`，前端据此引导重新初始化；
/// 引导界面归 TC-103。
#[tauri::command]
pub fn admin_status(
    app: tauri::AppHandle,
    db: tauri::State<'_, Arc<DbState>>,
) -> AppResult<AdminState> {
    let store = keychain()?;
    let dir = data_dir(&app)?;
    let env = UnlockEnv {
        db: &db,
        store: &store,
        data_dir: &dir,
        params: &CIPHER_PARAMS,
    };
    // 墙钟只在 Command 层取一次，往下当参数传：这样 domain 层的判定可以在测试里注入时刻。
    admins::probe(&env, admins::now_secs())
}

/// 解锁（S02 的「输入口令」）。库保持打开才算成功；返回 `Err` 的三条路
/// （未建档 / 锁定期内 / 口令错）都已经把连接释放干净了。
///
/// `rememberDevice` 是 S02 那个「记住本机 10 分钟」开关（默认 ON）：
/// 勾选时本次解锁的空闲阈值取 `max(库里阈值, 10 分钟)`，**不写库、重启失效**，
/// 唤醒锁定与 ⌘L 不受影响（ADR-0012 §8 · spec 标准 14）。
#[tauri::command]
pub fn unlock(
    app: tauri::AppHandle,
    db: tauri::State<'_, Arc<DbState>>,
    watch: tauri::State<'_, LockWatch>,
    password: String,
    remember_device: bool,
) -> AppResult<UnlockResult> {
    let store = keychain()?;
    let dir = data_dir(&app)?;
    let env = UnlockEnv {
        db: &db,
        store: &store,
        data_dir: &dir,
        params: &CIPHER_PARAMS,
    };

    let outcome = admins::unlock(&env, &password, admins::now_secs())?;

    // 阈值存在加密库里，所以只有解锁之后才读得到：启动时按默认值起表，
    // 解锁成功后按库里的值校正（ADR-0011 §9）。读失败退回默认值而不是报错——
    // 解锁本身已经成功，不该被一个配置项读不出来拖累。
    let stored = admins::get_idle_lock_minutes(&db).unwrap_or(admins::DEFAULT_IDLE_LOCK_MINUTES);
    // 「记住本机 10 分钟」的取值规则在 `unlock_idle_secs`（纯函数、有单测）；
    // `set_idle_secs` 内部顺带 `touch()`：解锁成功本身就是一次活动，不必再 touch 一次。
    watch.set_idle_secs(admins::unlock_idle_secs(stored, remember_device));

    Ok(UnlockResult {
        rehashed: outcome.rehashed,
        read_only: outcome.read_only,
        unlocked: outcome.unlocked,
    })
}

/// 首启建档（S01 向导的最终提交 · spec 范围 1/2 · ADR-0012 §3）。
///
/// 成功 = 管理员行 + `app_meta.org_name` 同一事务落库，库保持打开——
/// 前端收到 `Ok` 就直接切工作台，不必再问一次状态。`org_name` 不在这里校验：
/// S01 表单里它是可空项，空串就是「没填」，TC-202 的模板引擎读它时自行兜底。
/// 重复建档 → `Duplicate`（S01 只在 `initialized == false` 时可达；
/// 绕过界面直接喂 Command 才会撞上它）。
#[tauri::command]
pub fn create_admin(
    app: tauri::AppHandle,
    db: tauri::State<'_, Arc<DbState>>,
    watch: tauri::State<'_, LockWatch>,
    email: String,
    password: String,
    org_name: String,
) -> AppResult<()> {
    let store = keychain()?;
    let dir = data_dir(&app)?;
    let env = UnlockEnv {
        db: &db,
        store: &store,
        data_dir: &dir,
        params: &CIPHER_PARAMS,
    };
    admins::create_admin(&env, &email, &password, &org_name, admins::now_secs())?;
    // 设计文档 S01：「初始化成功后写入 admins、Keychain 与**默认字段模板**」。
    // 这里写的是**工资条项目**那组默认字段（设计文档 S04 原型的示例集，ADR-0015 决策 3）——
    // 它是用户数据、在 S04 里可随时增删停用，此刻改它零成本（还没有任何月份引用过字段）。
    crate::infra::fields::ensure_defaults(&db)?;
    // 默认邮件模板（设计文档 S10 的「标准中文模板」，正文取 ch.10 原文）
    crate::infra::templates::ensure_defaults(&db)?;
    watch.touch();
    Ok(())
}

/// 重新初始化：把**读不出来**的旧库移到一边（**不删**），然后回到首启向导。
///
/// 这条命令存在的理由：错误文案里写着「请先从备份恢复或重新完成初始化」，
/// 而**两条路都没有按钮** —— 使用者被卡在「没能读到本机状态」那一屏，
/// 唯一能按的「重试」永远不会成功（TC-600 真机验收撞到）。
///
/// 口径（`infra::reinit` 模块头写全了）：
/// - 旧库被**改名**成 `data.db.bak-<时间戳>`，留在原目录，一个字节都没丢；
/// - **不碰钥匙串**：下一次建档会用同一条目名写入新密钥（覆盖写），
///   主动删条目反而会造出「库没了、条目还在」的半成品状态（ADR-0014 要避免的那一侧）；
/// - **不碰备份**：导出的加密备份还在 `exports/` 里，那是「从备份恢复」那条路的输入。
#[tauri::command]
pub fn reinitialize_local_state(
    app: tauri::AppHandle,
    db: tauri::State<'_, Arc<DbState>>,
) -> AppResult<crate::infra::reinit::ArchiveReport> {
    let dir = data_dir(&app)?;
    // 先关库：文件被移走时连接还开着，SQLite 会在旧路径上留下尾巴。
    db.close();
    crate::infra::reinit::archive_local_db(&dir)
}

/// 设置页（S14）要显示的当前值：空闲阈值（下拉初值，spec 范围 11）、
/// 单位名称（spec 范围 2 的「S14 读」）与上次恢复时刻（TC-104）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsState {
    pub idle_lock_minutes: u32,
    pub org_name: Option<String>,
    /// 上次从备份恢复的时刻（epoch 秒）。没恢复过是 `None`。
    pub last_restored_at: Option<i64>,
}

/// 读设置页初值。只在解锁状态可达（S14 是工作台里的页面），库是开着的；
/// 导航到设置页本身是一次用户活动，所以这里照例 `touch()`。
#[tauri::command]
pub fn get_settings(
    db: tauri::State<'_, Arc<DbState>>,
    watch: tauri::State<'_, LockWatch>,
) -> AppResult<SettingsState> {
    let state = SettingsState {
        idle_lock_minutes: admins::get_idle_lock_minutes(&db)?,
        org_name: admins::get_org_name(&db)?,
        last_restored_at: crate::infra::backup::get_last_restored_at(&db)?,
    };
    watch.touch();
    Ok(state)
}

/// 立即锁定（设计文档 966「⌘L 立即锁定」的 Rust 侧；键绑定归 TC-103）。
///
/// 只关库，不碰 `LockWatch`：刚锁上就把空闲计时拨回去，等于用「锁定」按钮给自己
/// 续命。下一次 `unlock` 会重新计时并按库里的阈值校正。
#[tauri::command]
pub fn lock_now(db: tauri::State<'_, Arc<DbState>>) -> AppResult<()> {
    db.close();
    // 日志里只有事件本身，没有任何口令与工资数值（红线 2 的同族）。
    tracing::info!("已手动锁定：释放数据库连接与内存中的密钥");
    Ok(())
}

/// 改管理员口令（设计文档 313「密码重置需原密码」）。
///
/// 要求当前是解锁状态：这个入口只在设置页可达。库没开时 `db.write` 会报
/// `DATABASE`，那是调用顺序写错了的兜底，不是使用者会看到的常态。
/// 旧口令错 → `BAD_CREDENTIALS` 且哈希原样；两种情况下锁定两列都不动（spec 标准 13）。
#[tauri::command]
pub fn change_password(
    db: tauri::State<'_, Arc<DbState>>,
    watch: tauri::State<'_, LockWatch>,
    old_password: String,
    new_password: String,
) -> AppResult<()> {
    admins::change_password(&db, &old_password, &new_password)?;
    watch.touch();
    Ok(())
}

/// 改空闲自动锁定的分钟数（设计文档 2013「空闲自动锁定 10 分钟 ▾」的 Rust 侧；
/// 下拉框归 TC-103）。
///
/// 顺序是**先落库、后改计时器**：只读态下 `db.write` 会报错，那时阈值存不下去，
/// 就不能只把内存里的计时器改了让人以为生效——重启回到 10 分钟，比当场不生效更难查。
#[tauri::command]
pub fn set_idle_lock_minutes(
    db: tauri::State<'_, Arc<DbState>>,
    watch: tauri::State<'_, LockWatch>,
    minutes: u32,
) -> AppResult<()> {
    admins::set_idle_lock_minutes(&db, minutes)?;
    watch.set_idle_secs(u64::from(minutes) * 60);
    Ok(())
}
