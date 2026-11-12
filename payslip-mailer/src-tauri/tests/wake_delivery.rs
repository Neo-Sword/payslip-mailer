//! TC-102 验收标准 11：唤醒通知**跨进程**投递到本进程后，必须真的触发锁定（连接关闭）。
//!
//! 为什么这个文件不能是一个普通的 `#[test]`：
//!
//! - 分布式通知中心的回调**只在主线程的 runloop 上**被派发。ADR-0011 §10 的四组对照实验：
//!   后台线程注册 + 主线程泵 → 收到；主线程注册 + 后台线程泵 → 一条都收不到。
//!   注册在哪个线程无所谓，「谁在泵主线程的 runloop」才是约束。
//! - libtest 把测试体跑在**工作线程**上（实测连 `--test-threads=1` 也一样，
//!   `pthread_main_np()` 返回 0），所以 `#[test]` 里没有任何人泵主线程的 runloop，
//!   这条断言在单元测试里永远只能 SKIP。
//!
//! 所以 `Cargo.toml` 里给它 `harness = false`，自己出 `main()`：主线程注册（由
//! `LockWatch::start` 完成）+ 主线程泵 runloop —— 正是实验 C 的形状，只不过观察者是
//! 生产的 `LockWatch` 而不是探针里的手写回调。
//!
//! 另一个进程发通知（`--post`）而不是本进程自己发：ADR-0011 §8 实测分布式中心
//! **不向自己投递**，自发会得到一个永远走不到的「绿」。真实的唤醒通知由 loginwindow
//! 发出，跨进程才是同一条路。
//!
//! 环境不满足时（没有窗口会话、拿不到分布式中心、起不了子进程）打印 SKIP 并退出 0，
//! 不挂住也不假绿 —— spec 标准 12。这条分支在有窗口会话的开发机上撞不到，
//! 所以 `PAYSLIP_WAKE_TEST_SKIP=1` 可以把「观察者没挂上」这个条件手动置真来观察它。

use core_foundation_sys::base::kCFAllocatorDefault;
use core_foundation_sys::notification_center::*;
use core_foundation_sys::runloop::{CFRunLoopRunInMode, kCFRunLoopDefaultMode};
use core_foundation_sys::string::{CFStringCreateWithCString, kCFStringEncodingUTF8};
use payslip_mailer_lib::wake_probe::{CIPHER_PARAMS, DbState, KeychainStore, LockWatch};
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// 与 `infra::wake.rs` 里注册的名字必须一致：真唤醒通知用的就是这个字符串。
const WAKE_NOTIFICATION: &str = "NSWorkspaceDidWakeNotification";

/// 空闲阈值取 1 小时。测试最多等 5 秒，所以「锁了」只可能是唤醒那条腿触发的，
/// 不可能是空闲计时到点 —— 这是本测试能归因的前提。
const IDLE_SECS: u64 = 3600;

/// 等投递的上限。spike 里通知是毫秒级到达的，5 秒是留给调度抖动的余量；
/// 超过它就判失败，而不是继续等（spec 标准 12：不能挂住）。
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(5);

/// 子进程角色：往分布式通知中心发一条唤醒通知，模拟 loginwindow。
///
/// `PostNotification` 没有返回值（core-foundation-sys 0.8.7 `notification_center.rs`），
/// 所以这里只能报「发出去了」，不能报「有人收到了」——收到与否由父进程那边判。
fn post_wake() -> ExitCode {
    let c_name = std::ffi::CString::new(WAKE_NOTIFICATION).unwrap();
    let center = unsafe { CFNotificationCenterGetDistributedCenter() };
    if center.is_null() {
        println!("SKIP: 拿不到分布式通知中心，无法发通知");
        return ExitCode::SUCCESS;
    }
    let name = unsafe {
        CFStringCreateWithCString(kCFAllocatorDefault, c_name.as_ptr(), kCFStringEncodingUTF8)
    };
    if name.is_null() {
        println!("SKIP: CFStringCreateWithCString 返回 null");
        return ExitCode::SUCCESS;
    }
    unsafe {
        CFNotificationCenterPostNotification(
            center,
            name,
            std::ptr::null(),
            std::ptr::null(),
            1, // Boolean = u8，不是 bool
        );
    }
    println!("[子进程] 已向分布式通知中心发出 {WAKE_NOTIFICATION}");
    ExitCode::SUCCESS
}

/// 临时钥匙串 + 临时库目录。开真库是为了让「锁定 = 连接关闭」这半句是真的，
/// 而不是拿一个 `AtomicBool` 冒充。`KeychainStore::temporary` 不是 `cfg(test)` 专用
/// （ADR-0009 第 5 条：测试与生产共用同一套实现），所以集成测试也能用。
fn open_temp_db(root: &Path) -> Result<Arc<DbState>, String> {
    std::fs::create_dir_all(root).map_err(|e| format!("建临时目录失败：{e}"))?;
    let store = KeychainStore::temporary(&root.join("wake-delivery.keychain-db"), "test-only")
        .map_err(|e| format!("建临时钥匙串失败：{e}"))?;
    let db = Arc::new(DbState::new());
    db.open(&store, root, &CIPHER_PARAMS)
        .map_err(|e| format!("开加密库失败：{e}"))?;
    Ok(db)
}

/// 父进程角色：起生产的 `LockWatch` → 起子进程发通知 → **主线程**泵 runloop 等锁定。
fn watch_and_wait() -> ExitCode {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let root = std::env::temp_dir().join(format!(
        "xinyou-wake-delivery-{}-{nanos}",
        std::process::id()
    ));

    let db = match open_temp_db(&root) {
        Ok(db) => db,
        Err(why) => {
            println!("SKIP: {why}（本测试需要 Security 框架可用，不做假绿）");
            return ExitCode::SUCCESS;
        }
    };
    assert!(
        db.is_open(),
        "前提：一开始必须是解锁态，否则「锁定」无从谈起"
    );

    let locked = Arc::new(AtomicBool::new(false));
    let watch = LockWatch::start(IDLE_SECS, {
        let (db_for_lock, locked_for_lock) = (Arc::clone(&db), Arc::clone(&locked));
        move || {
            // 与 `lib.rs` 里生产接线完全同一件事：锁定回调只做 `close()`。
            db_for_lock.close();
            locked_for_lock.store(true, Ordering::SeqCst);
        }
    });

    // 观察者没挂上 = 这台机器/这个会话测不了唤醒这条腿（无窗口会话、分布式中心为 null）。
    // 跳过而不是失败：CI 的 headless 会话本来就没有 loginwindow 会话可唤醒。
    //
    // `PAYSLIP_WAKE_TEST_SKIP=1` 只是把**判定条件**换成真：开发机上有窗口会话，
    // 真实条件永远为假，标准 12 那条「跳过而不是挂住」就没法当场观察到。
    // 换成真之后走的仍是下面同一段代码（停表 → 清临时目录 → 退出 0），
    // 所以观察到的是跳过行为本身，不是另写的一份模拟。
    let no_observer =
        !watch.wake_registered() || std::env::var_os("PAYSLIP_WAKE_TEST_SKIP").is_some();
    if no_observer {
        println!("SKIP: 唤醒观察者未挂上（无窗口会话或分布式中心不可用），跳过投递断言");
        watch.stop();
        let _ = std::fs::remove_dir_all(&root);
        return ExitCode::SUCCESS;
    }

    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            println!("SKIP: 取不到 current_exe：{e}");
            watch.stop();
            let _ = std::fs::remove_dir_all(&root);
            return ExitCode::SUCCESS;
        }
    };
    let spawned = std::process::Command::new(&exe).arg("--post").status();
    if let Err(e) = spawned {
        println!("SKIP: 起不了发通知的子进程：{e}");
        watch.stop();
        let _ = std::fs::remove_dir_all(&root);
        return ExitCode::SUCCESS;
    }
    let status = spawned.expect("刚判过不是 Err");
    if !status.success() {
        println!("FAIL: 发通知的子进程退出码 {status}");
        watch.stop();
        let _ = std::fs::remove_dir_all(&root);
        return ExitCode::FAILURE;
    }

    // 关键的一段：**主线程**泵 runloop。
    // 观察线程（`payslip-lock-watch`）只数计数、判空闲，它不泵 runloop（ADR-0011 §10）。
    let deadline = Instant::now() + DELIVERY_TIMEOUT;
    while Instant::now() < deadline && !locked.load(Ordering::SeqCst) {
        unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.25, 1) };
    }

    let fired = locked.load(Ordering::SeqCst);
    let closed = !db.is_open();
    watch.stop();
    let _ = std::fs::remove_dir_all(&root);

    if fired && closed {
        println!("PASS: 另一个进程发的唤醒通知触发了锁定，加密库连接已关闭");
        ExitCode::SUCCESS
    } else {
        println!(
            "FAIL: 锁定回调触发 = {fired}，连接已关闭 = {closed}（阈值 {IDLE_SECS}s，不可能是空闲触发）"
        );
        ExitCode::FAILURE
    }
}

fn main() -> ExitCode {
    // 只认 `--post` 这一个标志：`cargo test -- --nocapture` 之类的参数会原样传进
    // `harness = false` 的二进制，不能被当成角色。
    if std::env::args().any(|a| a == "--post") {
        post_wake()
    } else {
        watch_and_wait()
    }
}
