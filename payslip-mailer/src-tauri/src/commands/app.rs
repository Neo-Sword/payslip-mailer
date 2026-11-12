//! 应用级 Command：版本、隐私承诺与运行状态自检。
//! 后续 P1 起在这里接入「是否已初始化」「是否已解锁」等全局状态。

use crate::error::{AppError, AppResult};
use serde::Serialize;
use tauri::Manager;

/// 应用自述信息。
///
/// 现状要写清楚：前端**还没有调用**它（P1 的 S01/S02 接线时在「关于」面板接上）。
/// 它留在 P0 的理由不是「以后要用」，而是它是 Command 层唯一跑通全链路的样例 ——
/// Rust 返回 `AppResult` → `AppError` 序列化成 `{code, message}` → 前端可消费，
/// 这条通路一旦被 CI 的 `cargo test` 覆盖，后面每个 Command 就照这个形状写。
#[derive(Serialize)]
pub struct AppInfo {
    pub name: &'static str,
    pub version: &'static str,
    /// 隐私承诺常量：除 SMTP 发信外不外联、无遥测
    pub privacy_note: &'static str,
    pub data_dir: String,
}

#[tauri::command]
pub fn app_info(app: tauri::AppHandle) -> AppResult<AppInfo> {
    // 解析不出数据目录时不返回一个「看着能用」的兜底路径：
    // 加密库、备份、日志全都无处可放，与其静默降级不如让使用者立刻知道。
    let data_dir = app
        .path()
        .app_data_dir()
        .map(|p| p.to_string_lossy().to_string())
        .map_err(|e| AppError::Io(format!("无法定位本机数据目录：{e}")))?;

    Ok(AppInfo {
        name: "薪邮 Payslip Mailer",
        version: env!("CARGO_PKG_VERSION"),
        privacy_note: "数据仅保存在本机 · 无遥测 · 无崩溃上报 · 无自动更新检查",
        data_dir,
    })
}
