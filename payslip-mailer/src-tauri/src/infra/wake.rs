//! 空闲计时 + 唤醒观察：`LockWatch`（TC-102 · ADR-0011 §7–§9）
//!
//! 这一层只回答一个问题：**什么时候该把库关掉**。它不碰库、不碰口令、不碰界面 ——
//! 「关掉」这个动作由调用方给一个闭包（生产里在 `lib.rs` 组装，内容是 `db.close()`）。
//! 这样分是因为空闲锁定与唤醒通知都是**系统事件**，把它们和 SQLCipher 绑在一起
//! 就没法在测试里把阈值设成 1 秒（spec 标准 10 要的就是这个）。
//!
//! 两条触发路径，缺一不可：
//!
//! 1. **空闲**：`idle` 时间内没有任何 `touch()`。计时用 `Instant`（单调），不用墙钟 ——
//!    改系统时间不该让应用提前锁或永不锁（ADR-0011 §9，与 §5 的墙钟取舍相反，
//!    因为代价不对称：空闲锁错了使用者当场看得见）。
//! 2. **唤醒**：收到 `NSWorkspaceDidWakeNotification`。**必须单独有这一条**，
//!    因为 macOS 的单调时钟在睡眠期间不前进 —— 合盖睡三小时打开，路径 1 算出的
//!    空闲仍然是 0，只有路径 2 会锁。这也是挂观察者的全部理由。
//!
//! 唤醒通道选的是**分布式**通知中心 + `Hold`，不是 Darwin 中心：ADR-0011 §7 的
//! 三组对照实验证明「在 Darwin 中心观察这个名字」收不到 `loginwindow` 发的唤醒通知，
//! 而选错的后果不是报错而是**永远不锁**（静默失效）。`Hold` 而不是 `Drop`，
//! 是因为休眠期间发的通知要在唤醒后补投 —— 那正是我们要的场景。
//!
//! 唤醒回调**只在主线程的 runloop 上**被派发（ADR-0011 §10 的四组对照实验）：注册在哪个
//! 线程都无所谓，但必须是主线程在泵 runloop —— 后台线程泵自己的 runloop 一条都收不到。
//! 真应用里主线程由 Tauri/Cocoa 的主事件循环泵着，所以本模块的观察线程**不跑 runloop**，
//! 只做两件事：数唤醒计数、判空闲。CF 的回调从主线程打进来，只改一个原子整数，
//! 不阻塞主线程，也不引 `objc2-app-kit`。
//!
//! 红线 5：活动信号只有 `touch()` 这一个入口，且只由 **Command 层**调用 ——
//! 前端不能自己声明「我还活跃」，否则「空闲自动锁定」变成前端说了算。

use core_foundation_sys::base::kCFAllocatorDefault;
use core_foundation_sys::dictionary::CFDictionaryRef;
use core_foundation_sys::notification_center::*;
use core_foundation_sys::string::{CFStringCreateWithCString, CFStringRef, kCFStringEncodingUTF8};
use std::ffi::CString;
use std::os::raw::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

/// `loginwindow` 唤醒时往分布式中心发的那个名字。拼错不会报错，只会收不到。
const WAKE_NOTIFICATION: &str = "NSWorkspaceDidWakeNotification";

/// 观察线程两次检查之间的间隔（秒）。
///
/// 这个值决定三件事：唤醒后锁定的最大延迟、空闲锁定的最大误差、`stop()` 的最大等待。
/// 0.25 秒对「使用者打开盖子 → 看到锁屏」完全无感，又不至于让线程常驻烧 CPU。
/// 它**不影响**唤醒通知能不能收到 —— 那是主线程 runloop 的事（ADR-0011 §10）。
const POLL_INTERVAL_SECONDS: f64 = 0.25;

/// `start()` 等观察线程把观察者挂好的上限（秒）。见 [`LockWatch::start`]。
const REGISTER_HANDSHAKE_LIMIT: Duration = Duration::from_millis(1000);

/// 单调时钟的原点。`LazyLock` 而不是 `OnceLock`：没有「还没初始化」这个状态要处理。
static CLOCK: LazyLock<Instant> = LazyLock::new(Instant::now);

/// 相对原点的单调毫秒。所有空闲判定都用它，所以库里那两列（epoch 秒）与这里
/// 是两个时钟，互不换算 —— 空闲锁定不需要知道「现在是几点」。
fn monotonic_millis() -> u64 {
    let elapsed = Instant::now().saturating_duration_since(*CLOCK);
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}

/// 观察者令牌背后那点状态：只有一个唤醒计数器。
///
/// CF 的观察者令牌必须是**稳定地址**，而回调可能在任意时刻从 CF 的投递线程打进来，
/// 所以这个 `Arc` 的裸指针由 `Arc::into_raw` 造出来后**故意不回收**（从不 `from_raw`）：
/// `CFNotificationCenterRemoveObserver` 返回与「另一个线程正在回调」之间有竞态，
/// 这时释放内存就是 use-after-free。一个进程里 `LockWatch` 是个位数，
/// 泄漏一个原子整数换来这条竞态不可能发生。
extern "C" fn on_wake(
    _center: CFNotificationCenterRef,
    observer: *mut c_void,
    _name: CFNotificationName,
    _object: *const c_void,
    _user_info: CFDictionaryRef,
) {
    // 签名按 core-foundation-sys 0.8.7 的源码读准（`notification_center.rs:22`）：
    // 五个参数，**没有** SuspensionBehavior 那第六个。
    if observer.is_null() {
        return;
    }
    let counter = unsafe { &*(observer as *const AtomicU64) };
    counter.fetch_add(1, Ordering::AcqRel);
}

/// 挂上唤醒观察者。返回（观察者令牌, 通知名），两者都要留着才能精确 `Remove`。
///
/// **注册在哪个线程都行**，但进程的主线程必须在泵 runloop —— ADR-0011 §10 的四组实验：
/// 后台线程注册 + 主线程泵 → 收到；主线程注册 + 后台线程泵 → 收不到。本函数由观察线程调，
/// 主线程的 runloop 由 Tauri/Cocoa 的主事件循环泵着（测试里没有那个事件循环，所以这条
/// 路径的投递部分只能在 `tests/wake_delivery.rs` 里测，那里自己泵主线程）。
///
/// `CFStringRef` 同样故意不 `CFRelease`：它要活到线程退出时 `Remove` 那一刻。
fn register_wake_observer(
    wake_count: &Arc<AtomicU64>,
) -> Result<(*mut c_void, CFStringRef), &'static str> {
    let center = unsafe { CFNotificationCenterGetDistributedCenter() };
    if center.is_null() {
        // 无窗口会话（CI、纯 SSH）会走到这里。不是故障：空闲锁定照常工作。
        return Err("CFNotificationCenterGetDistributedCenter 返回 null（没有窗口会话？）");
    }
    let Ok(c_name) = CString::new(WAKE_NOTIFICATION) else {
        return Err("通知名里含内嵌 NUL");
    };
    let name = unsafe {
        CFStringCreateWithCString(kCFAllocatorDefault, c_name.as_ptr(), kCFStringEncodingUTF8)
    };
    if name.is_null() {
        return Err("CFStringCreateWithCString 返回 null");
    }
    let token = Arc::into_raw(Arc::clone(wake_count)) as *mut c_void;
    unsafe {
        CFNotificationCenterAddObserver(
            center,
            token as *const c_void,
            on_wake,
            name,
            std::ptr::null(),
            // Hold（=3）：休眠期间的通知要在唤醒后补投；Drop（=1）会把它们丢掉。
            CFNotificationSuspensionBehaviorHold,
        );
    }
    Ok((token, name))
}

/// 摘掉观察者。只在观察线程退出时调，所以不需要处理「正在回调」的情况。
fn unregister_wake_observer(token: *mut c_void, name: CFStringRef) {
    let center = unsafe { CFNotificationCenterGetDistributedCenter() };
    if center.is_null() {
        return;
    }
    unsafe {
        CFNotificationCenterRemoveObserver(center, token as *const c_void, name, std::ptr::null())
    };
}

struct Inner {
    /// 空闲阈值（毫秒）。`set_idle_secs` 会改它，改完立刻生效，不需要重启观察线程。
    idle_millis: AtomicU64,
    /// 上一次活动时刻（单调毫秒）。`touch()` 写它。
    last_activity: AtomicU64,
    /// 本轮活动里是否已经锁过。见 [`fire`]。
    fired: AtomicBool,
    /// 观察线程的存活标志；`stop()` 置 false。
    running: AtomicBool,
    /// 观察线程是否已经跑过一轮（`start()` 的握手位）。
    registered: AtomicBool,
    /// 唤醒观察者是否挂上了。挂不上只影响唤醒路径，不影响空闲路径。
    wake_registered: AtomicBool,
    /// 收到过几条唤醒通知。CF 回调写它（也是观察者令牌指向的对象），观察线程读它。
    wake_count: Arc<AtomicU64>,
    /// 观察线程已经处理到哪一条唤醒通知，用来「每条只锁一次」。
    wakes_seen: AtomicU64,
    /// 触发过几次锁定。只有测试与人工排查读。
    lock_count: AtomicU64,
    /// 「该锁了」的动作。生产里是 `db.close()`。
    on_lock: Box<dyn Fn() + Send + Sync>,
}

/// 触发一次锁定。同一轮活动里只触发一次。
///
/// 不这么做的话，锁上之后观察线程每 0.25 秒就会再叫一次：`db.close()` 幂等所以数据没事，
/// 但「已锁定」的提示会一直刷，而且 `lock_count` 会变成几千，测试没法断言「触发了一次」。
/// 复位只由 `touch()` 做 —— 也就是「使用者真的动了一下」，不是「时间又过了 250 毫秒」。
fn fire(inner: &Inner) {
    if inner.fired.swap(true, Ordering::AcqRel) {
        return;
    }
    inner.lock_count.fetch_add(1, Ordering::Relaxed);
    (inner.on_lock)();
}

/// 观察线程的主体：挂观察者 → 每片检查两条触发路径 → 摘观察者。
fn watch_thread(inner: Arc<Inner>) {
    let observer = match register_wake_observer(&inner.wake_count) {
        Ok(pair) => {
            inner.wake_registered.store(true, Ordering::Release);
            Some(pair)
        }
        Err(why) => {
            // 挂不上不当错误往外抛：唤醒锁定降级，空闲锁定必须照常。
            // 「静默失效」由这条 warn 与 `wake_registered()` 暴露（ADR-0011 §7 的代价）。
            tracing::warn!(
                reason = why,
                "挂不上唤醒通知观察者：合盖唤醒后不会自动锁定，空闲自动锁定不受影响"
            );
            None
        }
    };
    // 握手：告诉 `start()` 「我跑到能干活了」，见 [`LockWatch::start`]。
    inner.registered.store(true, Ordering::Release);

    let slice = Duration::from_secs_f64(POLL_INTERVAL_SECONDS);
    while inner.running.load(Ordering::Relaxed) {
        // 这里**不跑 runloop**。分布式通知不在本线程的 runloop 上派发（ADR-0011 §10 实验 E：
        // 主线程注册 + 后台线程泵 = 一条收不到），在这里泵只是白占一次唤醒。
        // CF 的回调从主线程打进来把 `wake_count` 加一，这个线程只负责发现「计数变了」。
        std::thread::sleep(slice);

        // 路径 2：唤醒。先更新 `wakes_seen` 再 fire，保证同一条通知只锁一次。
        let wakes = inner.wake_count.load(Ordering::Acquire);
        if wakes > inner.wakes_seen.load(Ordering::Relaxed) {
            inner.wakes_seen.store(wakes, Ordering::Relaxed);
            fire(&inner);
        }

        // 路径 1：空闲。
        if !inner.fired.load(Ordering::Relaxed) {
            let idle =
                monotonic_millis().saturating_sub(inner.last_activity.load(Ordering::Acquire));
            if idle >= inner.idle_millis.load(Ordering::Relaxed) {
                fire(&inner);
            }
        }
    }

    if let Some((token, name)) = observer {
        unregister_wake_observer(token, name);
    }
}

/// 空闲 + 唤醒的锁定看门狗。
///
/// 挂在 Tauri 的应用状态里（`Arc<LockWatch>` 或直接 manage），进程内长期存活；
/// `Drop` 会停线程，所以退出路径不显式 `stop()` 也不会漏线程。
pub struct LockWatch {
    inner: Arc<Inner>,
    joiner: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl LockWatch {
    /// 起观察线程。`idle_secs` 是空闲阈值（**秒**）：
    /// 库里那一列的口径是分钟（`app_meta` 的 `idle_lock_minutes`，默认 10），
    /// 换算与钳制在 Command 层做，这里收秒是为了测试能把阈值设成 1（spec 标准 10）。
    ///
    /// **不返回 `Result`**：挂不上唤醒观察者只是降级（无窗口会话、CI），
    /// 空闲锁定照样要工作，不值得让应用启动失败。降级情况进日志，
    /// 并且可以用 [`LockWatch::wake_registered`] 查到。
    ///
    /// 返回前会等观察线程把观察者挂好（上限 1 秒），这样 `wake_registered()`
    /// 一返回就是准的 —— 否则启动日志与唤醒测试都会读到「还没挂上」的假话。
    pub fn start(idle_secs: u64, on_lock: impl Fn() + Send + Sync + 'static) -> Self {
        let inner = Arc::new(Inner {
            idle_millis: AtomicU64::new(idle_secs.saturating_mul(1000)),
            last_activity: AtomicU64::new(monotonic_millis()),
            fired: AtomicBool::new(false),
            running: AtomicBool::new(true),
            registered: AtomicBool::new(false),
            wake_registered: AtomicBool::new(false),
            wake_count: Arc::new(AtomicU64::new(0)),
            wakes_seen: AtomicU64::new(0),
            lock_count: AtomicU64::new(0),
            on_lock: Box::new(on_lock),
        });

        let spawned = std::thread::Builder::new()
            .name("payslip-lock-watch".into())
            .spawn({
                let inner = Arc::clone(&inner);
                move || watch_thread(inner)
            });
        let joiner = match spawned {
            Ok(handle) => {
                let deadline = Instant::now() + REGISTER_HANDSHAKE_LIMIT;
                while !inner.registered.load(Ordering::Acquire) && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Mutex::new(Some(handle))
            }
            Err(e) => {
                // 起不了线程是「操作系统不答应」级别的问题：两条触发路径都没了，
                // 但应用还能用（只是不会自动锁），所以同样不 fail 启动。
                tracing::warn!(error = %e, "起不了锁定观察线程：不会自动锁定");
                Mutex::new(None)
            }
        };

        Self { inner, joiner }
    }

    /// 「使用者动了一下」：复位空闲计时，并允许下一轮再触发一次锁定。
    ///
    /// **只由 Command 层调用**（红线 5）。解锁成功、每次成功的操作之后都该调，
    /// 否则使用者在工作台上连着点半小时，第 10 分钟会被踢回锁屏。
    pub fn touch(&self) {
        self.inner
            .last_activity
            .store(monotonic_millis(), Ordering::Release);
        self.inner.fired.store(false, Ordering::Release);
    }

    /// 改空闲阈值（秒），立刻生效。TC-103 设置页那个 5/10/30 下拉框走这里，
    /// 不然改了设置要重启应用才管用。
    pub fn set_idle_secs(&self, secs: u64) {
        self.inner
            .idle_millis
            .store(secs.saturating_mul(1000), Ordering::Relaxed);
        // 改小阈值时不该让使用者因为「上次活动是 20 分钟前」当场被锁：
        // 改设置本身就是一次活动。
        self.touch();
    }

    /// 唤醒观察是否挂上了。没挂上只意味着**唤醒**不会自动锁，空闲锁定不受影响。
    /// 启动时读一次进日志，是为了让 ADR-0011 §7 那种「静默不锁」有个能查的地方。
    pub fn wake_registered(&self) -> bool {
        self.inner.wake_registered.load(Ordering::Acquire)
    }

    /// 观测点：触发过几次锁定。生产路径不读（没人需要「锁了几次」），
    /// 标 `cfg(test)` 而不是让它在生产里当死代码。
    #[cfg(test)]
    pub fn lock_count(&self) -> u64 {
        self.inner.lock_count.load(Ordering::Relaxed)
    }

    /// 观测点：收到过几条唤醒通知。同上，只有测试读。
    #[cfg(test)]
    pub fn wake_count(&self) -> u64 {
        self.inner.wake_count.load(Ordering::Acquire)
    }

    /// 测试钩子：模拟「CF 的唤醒回调已经打进来过一次」。
    ///
    /// 它只加那个计数，所以测的是**本模块自己写的那段**「计数变了 → 关库」；
    /// 测不到「分布式通知能不能投递到本进程」——那一段由 `tests/wake_delivery.rs`
    /// 用真跨进程 + 真主线程 runloop 覆盖（ADR-0011 §10：`#[test]` 跑在工作线程上，
    /// 永远泵不了主线程的 runloop，所以那半必须在有 `main()` 的测试里做）。
    #[cfg(test)]
    pub fn inject_wake(&self) {
        self.inner.wake_count.fetch_add(1, Ordering::AcqRel);
    }

    /// 停观察线程。可重复调。
    ///
    /// 上界是一片（0.25 秒）：线程每片结束都会回头看 `running`，
    /// 所以退出路径不会被这里卡住。
    pub fn stop(&self) {
        self.inner.running.store(false, Ordering::Release);
        let handle = self
            .joiner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(handle) = handle {
            // release 构建 `panic = "abort"`，线程 panic 时进程已经没了，走不到这里；
            // debug 下 join 失败说明观察线程死了，那也只是「不再自动锁」，记一行。
            if handle.join().is_err() {
                tracing::warn!("锁定观察线程异常退出：不会自动锁定");
            }
        }
    }
}

impl Drop for LockWatch {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::db::{CIPHER_PARAMS, DbState};
    use crate::infra::keychain::{TempKeychain, no_user_interaction};
    use std::path::PathBuf;
    use std::sync::atomic::AtomicUsize;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// 唤醒路径在这里测的是「计数变了 → 关库」这段（见 `inject_wake`）。
    ///
    /// **跨进程投递那半不在这里**，在 `tests/wake_delivery.rs`：libtest 把测试体放在工作
    /// 线程上（实测连 `--test-threads=1` 也一样，`pthread_main_np()` 返回 0），测试进程的
    /// 主线程从不泵 runloop，而分布式通知只由主线程 runloop 派发（ADR-0011 §10）。
    /// 留在这里只会得到一个永远走 SKIP 的「绿」。
    ///
    /// 每个用例一套「临时数据目录 + 临时钥匙串 + DbState」，与 `infra::db` /
    /// `infra::admins` 的测试基座同构：绝不碰登录钥匙串，也绝不碰真实数据目录。
    ///
    /// `DbState` 用 `Arc` 包着，是因为锁定的闭包要持有它 —— 生产里挂在 Tauri
    /// 状态里的也是 `Arc<DbState>`，两边形状必须一致，否则测的不是那条路。
    struct Fixture {
        dir: PathBuf,
        kc: TempKeychain,
        db: Arc<DbState>,
    }

    impl Fixture {
        fn new(label: &str) -> Self {
            no_user_interaction();
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default();
            let dir = std::env::temp_dir().join(format!(
                "xinyou-wake-test-{}-{nanos}-{label}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).expect("建临时数据目录失败");
            Self {
                dir,
                kc: TempKeychain::new(label),
                db: Arc::new(DbState::new()),
            }
        }

        /// 解锁态。空闲/唤醒锁定的动作就是把这条连接关掉（设计文档 ch.5）。
        fn open_db(&self) {
            self.db
                .open(&self.kc.store, &self.dir, &CIPHER_PARAMS)
                .expect("开库失败");
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// 轮询到条件成立为止，返回有没有等到。
    ///
    /// 返回 bool 而不是直接断言，是为了把「环境不支持」与「代码错了」分开：
    /// 前者打印 `SKIP` 走人（spec 标准 12），后者才是断言失败。
    fn wait_until(limit: Duration, f: impl Fn() -> bool) -> bool {
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        f()
    }

    /// 验收标准 10 前半：阈值 1 秒、不动它 → 回调触发，且库真的被关掉。
    ///
    /// 阈值能设成 1 秒是因为 `start` 收的是**秒**；库里那一列是分钟（默认 10），
    /// 换算与钳制在 Command 层。断言 `is_open() == false` 而不是只断言「回调调过」，
    /// 是因为 spec 标准 10 要的是「连接真的没了、密钥真的离开内存」。
    #[test]
    fn idle_timeout_locks_the_database() {
        let fx = Fixture::new("idle-fire");
        fx.open_db();
        assert!(fx.db.is_open(), "前提：一开始是解锁态");

        let locks = Arc::new(AtomicUsize::new(0));
        let watch = LockWatch::start(1, {
            let (db, locks) = (Arc::clone(&fx.db), Arc::clone(&locks));
            move || {
                db.close();
                locks.fetch_add(1, Ordering::SeqCst);
            }
        });

        let fired = wait_until(Duration::from_secs(5), || locks.load(Ordering::SeqCst) > 0);
        assert!(fired, "阈值 1 秒、5 秒没有任何活动，必须触发一次锁定");
        assert_eq!(
            locks.load(Ordering::SeqCst),
            1,
            "同一轮活动里只该触发一次，否则锁屏提示每 250 毫秒刷一次"
        );
        assert!(
            !fx.db.is_open(),
            "锁定必须真的把连接关掉（spec 标准 10 的 is_open()==false）"
        );
        watch.stop();
    }

    /// 验收标准 10 后半：每 0.5 秒 `touch()` 一次，阈值 1 秒就不该触发。
    #[test]
    fn touching_defers_the_idle_lock() {
        let fx = Fixture::new("idle-touch");
        fx.open_db();

        let locks = Arc::new(AtomicUsize::new(0));
        let watch = LockWatch::start(1, {
            let (db, locks) = (Arc::clone(&fx.db), Arc::clone(&locks));
            move || {
                db.close();
                locks.fetch_add(1, Ordering::SeqCst);
            }
        });

        // 3 秒 > 3 倍阈值，每次间隔 0.5 秒 < 阈值：一直在活动就不该锁。
        for _ in 0..6 {
            std::thread::sleep(Duration::from_millis(500));
            watch.touch();
        }
        assert_eq!(
            locks.load(Ordering::SeqCst),
            0,
            "每 0.5 秒 touch 一次，阈值 1 秒不该触发"
        );
        assert!(fx.db.is_open(), "没锁定就不该关连接");

        // touch 之后计时器必须还活着：停掉 touch，它要能再锁上。
        // 少了这一条，「不触发」可能只是「计时器死了」。
        let fired = wait_until(Duration::from_secs(5), || locks.load(Ordering::SeqCst) > 0);
        assert!(
            fired,
            "停止 touch 之后必须能重新触发，否则测不出是计时器停了"
        );
        watch.stop();
    }

    /// 阈值可以改，改完不用重启观察线程（TC-103 设置页那个下拉框依赖这条）。
    #[test]
    fn changing_the_threshold_takes_effect_immediately() {
        let fx = Fixture::new("idle-retune");
        fx.open_db();

        let locks = Arc::new(AtomicUsize::new(0));
        let watch = LockWatch::start(600, {
            let (db, locks) = (Arc::clone(&fx.db), Arc::clone(&locks));
            move || {
                db.close();
                locks.fetch_add(1, Ordering::SeqCst);
            }
        });
        // 前提：10 分钟阈值下 1 秒内不会锁
        std::thread::sleep(Duration::from_millis(1200));
        assert_eq!(locks.load(Ordering::SeqCst), 0, "阈值 600 秒时不该触发");

        watch.set_idle_secs(1);
        let fired = wait_until(Duration::from_secs(5), || locks.load(Ordering::SeqCst) > 0);
        assert!(fired, "改成 1 秒之后必须立刻按新阈值计时");
        watch.stop();
    }

    /// 验收标准 11（本模块那半）：收到唤醒信号 → 立刻锁定并关库。
    ///
    /// 阈值设 600 秒，这样「触发」只可能来自唤醒计数，不可能来自空闲计时 ——
    /// 两条路径分得开，测试才有指向性。
    ///
    /// 这里用 [`LockWatch::inject_wake`] 而不是真通知：`#[test]` 跑在工作线程上，
    /// 测试进程的主线程从不泵 runloop，真通知在这个进程里根本投递不到（ADR-0011 §10）。
    /// 「另一个进程发、本进程收到」那半在 `tests/wake_delivery.rs`，那里有真的 `main()`。
    #[test]
    fn a_wake_signal_locks_the_database() {
        let fx = Fixture::new("wake");
        fx.open_db();
        assert!(fx.db.is_open(), "前提：一开始是解锁态");

        let locks = Arc::new(AtomicUsize::new(0));
        let watch = LockWatch::start(600, {
            let (db, locks) = (Arc::clone(&fx.db), Arc::clone(&locks));
            move || {
                db.close();
                locks.fetch_add(1, Ordering::SeqCst);
            }
        });

        watch.inject_wake();
        let fired = wait_until(Duration::from_secs(5), || locks.load(Ordering::SeqCst) > 0);
        assert!(
            fired,
            "收到一条唤醒信号必须触发锁定（阈值 600 秒，不可能是空闲触发）"
        );
        assert_eq!(
            locks.load(Ordering::SeqCst),
            1,
            "同一条唤醒通知只该锁一次，否则锁屏提示会一直刷"
        );
        // 两个观测点必须和闭包一致：`wake_count` 是 CF 回调与本线程之间的唯一通道，
        // `lock_count` 是 `fire()` 里的计数。对不上就说明「锁了一次」是巧合而不是这条通道。
        assert_eq!(watch.wake_count(), 1, "唤醒计数应等于注入次数");
        assert_eq!(watch.lock_count(), 1, "模块自己的锁定计数应与闭包一致");
        assert!(
            !fx.db.is_open(),
            "唤醒锁定必须真的把连接关掉（ch.5：唤醒即释放连接）"
        );

        // 锁过一次之后使用者动了一下 → 下一条唤醒信号必须还能锁。
        // 少了这一步，「合盖 → 打开 → 合盖 → 打开」的第二次就不会回锁屏。
        watch.touch();
        watch.inject_wake();
        let again = wait_until(Duration::from_secs(5), || locks.load(Ordering::SeqCst) > 1);
        assert!(again, "touch 之后下一条唤醒信号必须还能触发锁定");
        watch.stop();
    }
}
