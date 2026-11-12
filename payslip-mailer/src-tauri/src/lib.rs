//! 薪邮 Payslip Mailer —— Rust 后端入口
//!
//! 分层约定（设计文档 ch.4）：
//!   commands/  IPC 边界，参数校验 + DTO 组装
//!   domain/    实体、状态机、校验规则、幂等键
//!   infra/     SQLCipher / Keychain / Excel / 模板 / SMTP / PDF
//!
//! 约束：工资数值、SMTP 凭据、附件路径不通过前端长期持有；
//! 所有外部副作用都经 Command 层，便于做权限白名单、审计与单元测试。

mod commands;
mod domain;
mod error;
mod infra;
mod util;

/// TC-103 的验收测试集中在这里（只编进测试构建，不进生产二进制）。
/// 详见该文件头的说明：spec 的 21 条标准里能自动判定的那些，逐条对应一个测试。
#[cfg(test)]
mod acceptance;

/// TC-301 的验收测试（导出空模板）。同样只编进测试构建，不进生产二进制。
#[cfg(test)]
mod acceptance_export;

/// TC-302 的验收测试（Excel 导入向导）。同样只编进测试构建，不进生产二进制。
#[cfg(test)]
mod acceptance_import;

/// TC-401 的验收测试（三重核对）。同样只编进测试构建，不进生产二进制。
#[cfg(test)]
mod acceptance_verify;

/// TC-403 的验收测试（发送历史与凭证）。同样只编进测试构建，不进生产二进制。
#[cfg(test)]
mod acceptance_sendlog;

/// TC-500 的验收测试（SMTP 引擎 · StubTransport，ch.3 纪律：绝不真发）。
#[cfg(test)]
mod acceptance_smtp;

use tauri::{Emitter, Manager};
use tracing_subscriber::EnvFilter;

/// 编译期断言（ADR-0012 §6）：锁定回调在观察线程上跑，要**跨线程持有** `AppHandle`
/// 才能 `emit`。auto-trait 是编译器算的，不是从字段能推死的 —— 这条红了的既定退路
/// 是 `Mutex<AppHandle>`（`Mutex<T>: Sync` 只需 `T: Send`），届时改这一个闭包即可。
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<tauri::AppHandle>();
};

/// 集成测试专用的可见性口子（`tests/wake_delivery.rs`）。
///
/// 上面的四个模块是私有的 —— 生产前端只经 Command 层，本就不该看见 `infra`。
/// 但唤醒通知的投递那半必须在**有 `main()` 的进程**里测：libtest 把 `#[test]`
/// 跑在工作线程上（实测连 `--test-threads=1` 也一样，`pthread_main_np()` 返回 0），
/// 而分布式通知只由**主线程的 runloop** 派发（ADR-0011 §10 四组对照实验），
/// 所以单元测试里那条路永远收不到，只能由 `harness = false` 的测试自己泵主线程。
///
/// 标 `#[doc(hidden)]` 是为了它不出现在文档里：这是测试脚手架，不是给下游的 API。
#[doc(hidden)]
pub mod wake_probe {
    pub use crate::infra::db::{CIPHER_PARAMS, DbState};
    pub use crate::infra::keychain::KeychainStore;
    pub use crate::infra::wake::LockWatch;
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 日志脱敏：默认只记 info 以上，且不记录工资数值与邮件正文
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,payslip_mailer_lib=debug")),
        )
        .with_target(false)
        .init();

    // 不注册任何 Tauri 插件：脚手架自带的 opener 前端从未调用，
    // 摘掉它可缩小权限白名单，也少一处 npm/Rust 版本要对齐的耦合点。

    // `DbState` 是设计文档 ch.5 那条「锁定 / 休眠 / 切后台即释放连接」的载体：
    // 里面是 `Mutex<Option<Session>>`，`None` 就是「没解锁、密钥不在内存里」。
    //
    // 外面包一层 `Arc` 是因为它有**两个持有者**：Tauri 的状态表（Command 层拿它
    // 开库、关库）和 `LockWatch` 的观察线程（锁定回调里要 `close()`）。两边握的是
    // 同一份状态，不是两份连接 —— 这一点必须由类型说清楚，而不是靠「记得用同一个」。
    let db = std::sync::Arc::new(infra::db::DbState::new());

    // `LockWatch` 在 `.setup()` 里起表，因为锁定回调除了关库，还要 `emit` 事件把
    // 「已经锁上」推进界面（ADR-0012 §6）—— 而 `AppHandle` 要等 Builder 造出来才有。
    // 不发事件的话前端停在「已解锁」的样子让人继续点，每个操作都失败：
    // 静默失败是最难查的形态，这条事件通道就是它的解药。
    //
    // 空闲阈值存在加密库的 `app_meta` 里，而开机那一刻库还锁着、读不到，
    // 所以先按默认 10 分钟起表；每次解锁成功后 `commands::auth::unlock` 会读库里的
    // 实际值再校正一次（ADR-0011 §9）。
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            let watch =
                infra::wake::LockWatch::start(u64::from(infra::admins::DEFAULT_IDLE_LOCK_MINUTES) * 60, {
                    let db_for_lock = std::sync::Arc::clone(&db);
                    let handle = app.handle().clone();
                    move || {
                        // 先关库、后发事件：事件到了前端就切锁屏，
                        // 那时密钥已经不在内存里（顺序反了只是视觉先行，不改变安全性，
                        // 但「先关」让两条腿的先后关系在日志里查得到）。
                        db_for_lock.close();
                        if let Err(e) = handle.emit("app-locked", ()) {
                            // 前端此刻没监听也不算错（Tauri 不会因此报错）；
                            // 走到这个 warn 说明是运行时的推送管道出了问题。
                            tracing::warn!(error = %e, "锁定事件发送失败：界面可能停在已解锁的样子");
                        }
                    }
                });
            // 唤醒观察者挂不上是**降级**不是失败（理由见 `infra::wake::LockWatch::start`）：
            // 没有窗口会话、或分布式通知中心返回 null 时，空闲锁定这条腿照常守。
            // 这里在启动时记一次日志，是为了让「这台机器上唤醒会不会自动锁屏」在日志里查得到，
            // 而不是等到某次合盖没锁屏时才回头猜。
            if !watch.wake_registered() {
                tracing::warn!("唤醒观察者未挂上：本进程只靠空闲计时锁屏，休眠唤醒不会自动锁定");
            }
            app.manage(std::sync::Arc::clone(&db));
            app.manage(watch);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::app::app_info,
            commands::auth::admin_status,
            commands::auth::unlock,
            commands::auth::create_admin,
            commands::auth::get_settings,
            commands::auth::lock_now,
            commands::auth::change_password,
            commands::auth::set_idle_lock_minutes,
            commands::backup::export_backup,
            commands::backup::list_backups,
            commands::backup::restore_backup,
            commands::backup::wipe_preview,
            commands::backup::wipe_data,
            commands::fields::list_fields,
            commands::fields::create_field,
            commands::fields::update_field,
            commands::fields::set_field_visible,
            commands::fields::reorder_fields,
            commands::fields::preview_header,
            commands::fields::list_selectable_variables,
            commands::fields::list_all_variable_keys,
            commands::fields::evaluate_formula,
            commands::fields::evaluate_row,
            commands::members::list_members,
            commands::members::list_sendable_members,
            commands::members::list_groups,
            commands::members::member_count,
            commands::members::create_member,
            commands::members::update_member,
            commands::members::set_member_active,
            commands::members::create_group,
            commands::members::delete_group,
            commands::members::preview_member_paste,
            commands::members::paste_members,
            commands::templates::list_templates,
            commands::templates::template_variables,
            commands::templates::create_template,
            commands::templates::update_template,
            commands::templates::duplicate_template,
            commands::templates::set_default_template,
            commands::templates::delete_template,
            commands::templates::preview_template,
            commands::templates::check_template_variables,
            commands::periods::list_periods,
            commands::periods::get_period,
            commands::periods::create_period,
            commands::periods::transition_period,
            commands::periods::unlock_period,
            commands::periods::get_period_fields,
            commands::periods::list_audit_log,
            commands::export::preview_template_header,
            commands::export::export_template,
            commands::import::preview_import,
            commands::import::commit_import,
            commands::import::export_import_errors,
            commands::records::list_records,
            commands::records::update_record_cell,
            commands::records::delete_record,
            commands::records::get_period_totals,
            commands::batch::create_send_batch,
            commands::batch::list_send_batches,
            commands::batch::get_send_batch,
            commands::batch::pause_send_batch,
            commands::batch::resume_send_batch,
            commands::batch::abort_send_batch,
            commands::verify::verify_send_scope,
            commands::verify::render_member_payslip,
            commands::batch::start_batch,
            commands::batch::find_unfinished_batch,
            commands::batch::get_batch_progress,
            commands::batch::recover_and_run_batch,
            commands::batch::retry_failed_jobs,
            commands::sendlog::list_send_history,
            commands::sendlog::get_batch_jobs,
            commands::sendlog::export_batch_voucher,
            commands::settings::get_smtp_settings,
            commands::settings::save_smtp_settings,
            commands::settings::test_smtp_connection
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|app_handle, event| {
        if let tauri::RunEvent::Exit = event {
            // 先停观察线程、再关库：反过来的话观察线程可能在库已关之后再触发一次锁定
            // 回调（内容只是 `close()`，无害），但没必要留这种顺序不确定。
            if let Some(watch) = app_handle.try_state::<infra::wake::LockWatch>() {
                watch.stop();
            }
            // 进程马上要退，连接随内存释放也一样；显式关闭的意义是让 `close()`
            // 处在编译器的死代码监视之下 —— 哪天它没人调了，门禁会告诉我们，
            // 而不是默默留着一个「看着会释放」的状态。
            if let Some(state) = app_handle.try_state::<std::sync::Arc<infra::db::DbState>>() {
                state.close();
            }
        }
    });
}
