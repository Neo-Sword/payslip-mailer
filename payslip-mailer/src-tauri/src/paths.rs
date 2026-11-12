//! 本机数据目录：**只有一个真相源**（ADR-0012 §1）
//!
//! ## 发布构建
//!
//! `~/Library/Application Support/cn.payslipmailer.desktop/`
//! —— 由 Tauri 的 `app_data_dir()` 按 bundle id 推出。设计文档里写的旧目录名
//! （`PayslipMailer/`）作废，代码里不重复它的字面。
//!
//! ## 开发构建：`-dev` 后缀，另一个目录
//!
//! 为什么必须分开（TC-600 真机验收撞到）：
//!
//! - 开发构建用的是**专用钥匙串**（`scripts/setup-dev-keychain.sh` 建的钥匙串文件），
//!   发布构建用的是**登录钥匙串** —— 两边的库密钥天然不同。
//! - 两者却共用同一个 `data.db`：于是**谁后建档谁说了算**，
//!   另一个构建必然撞上「钥匙串里的密钥与这份数据不匹配」。
//! - 症状是发布版（或者开发版）一启动就卡在「没能读到本机状态」，
//!   而使用者根本想不到是自己昨天在另一个构建里初始化过。
//!
//! 所以开发构建用 `cn.payslipmailer.desktop-dev/`：两边各存各的数据，
//! 互不干扰，也互不污染 —— **开发时随便删，发布的数据一点不受影响**。
//!
//! 这个分流是**编译期**的（`debug_assertions`）：发布二进制里根本没有 dev 路径，
//! 不存在「发布版误写进 dev 目录」的可能。

use std::path::PathBuf;

use tauri::Manager;

use crate::error::{AppError, AppResult};

/// 开发构建用的目录后缀。
const DEV_SUFFIX: &str = "-dev";

/// 本机数据目录。解析不出来时不返回兜底路径：
/// 加密库、备份、日志全都无处可放，与其静默降级不如让使用者立刻知道。
pub fn data_dir(app: &tauri::AppHandle) -> AppResult<PathBuf> {
    let base = app
        .path()
        .app_data_dir()
        .map_err(|e| AppError::Io(format!("无法定位本机数据目录：{e}")))?;
    Ok(dev_or_prod(base))
}

/// 目录名分流：开发构建加 `-dev` 后缀，发布构建原样。
///
/// 单独抽成一个纯函数，是为了让「哪些构建用哪个目录」有一处可测的地方 ——
/// 这个分流一旦散在多个调用点，迟早会出现「一半命令写 dev 目录、一半写生产目录」。
pub fn dev_or_prod(base: PathBuf) -> PathBuf {
    if !cfg!(debug_assertions) {
        return base;
    }
    let Some(name) = base.file_name().and_then(|s| s.to_str()) else {
        return base;
    };
    if name.ends_with(DEV_SUFFIX) {
        return base;
    }
    base.with_file_name(format!("{name}{DEV_SUFFIX}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 开发构建与发布构建必须落在**两个不同的目录**里。
    #[test]
    fn the_dev_build_uses_a_different_directory() {
        let base = PathBuf::from("/Users/x/Library/Application Support/cn.payslipmailer.desktop");
        let dev = dev_or_prod(base.clone());
        if cfg!(debug_assertions) {
            assert_ne!(dev, base, "开发构建不该和发布构建共用一个目录");
            assert!(
                dev.to_string_lossy().ends_with("-dev"),
                "开发目录应当带 -dev 后缀：{}",
                dev.display()
            );
        } else {
            assert_eq!(dev, base, "发布构建必须用生产目录");
        }
    }

    /// 加两次后缀不会变成 `-dev-dev`（幂等）。
    #[test]
    fn the_suffix_is_not_applied_twice() {
        let base = PathBuf::from("/Users/x/Library/Application Support/cn.payslipmailer.desktop");
        let once = dev_or_prod(base);
        let twice = dev_or_prod(once.clone());
        assert_eq!(once, twice, "重复调用不能改变结果");
    }
}
