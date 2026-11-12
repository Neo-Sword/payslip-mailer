//! 应用级 Command：版本、隐私承诺与运行状态自检。
//! 后续 P1 起在这里接入「是否已初始化」「是否已解锁」等全局状态。

use crate::error::AppResult;
use serde::Serialize;

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
    // 数据目录的真相源在 `crate::paths`（发布构建 = app_data_dir()，
    // 开发构建 = 同名加 -dev 后缀，两边各用各的库）。
    let data_dir = crate::paths::data_dir(&app)?.to_string_lossy().to_string();

    Ok(AppInfo {
        name: "薪邮 Payslip Mailer",
        version: env!("CARGO_PKG_VERSION"),
        privacy_note: "数据仅保存在本机 · 无遥测 · 无崩溃上报 · 无自动更新检查",
        data_dir,
    })
}
