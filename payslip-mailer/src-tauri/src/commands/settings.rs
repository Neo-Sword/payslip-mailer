//! SMTP 设置的 Command（TC-500 · ADR-0027）

use std::sync::Arc;

use tauri::State;

use crate::error::{AppError, AppResult};
use crate::infra::db::DbState;
use crate::infra::keychain::KeychainStore;
use crate::infra::mailer::{self, SmtpSettings, TestResult};

/// 读发信配置（**不含密码** —— 密码只在 Keychain，前端永远拿不到，红线 5）。
#[tauri::command]
pub fn get_smtp_settings(db: State<'_, Arc<DbState>>) -> AppResult<Option<SmtpSettings>> {
    mailer::get_settings(&db)
}

/// 保存发信配置（**密码只进 Keychain** —— 表里存的是条目名）。
#[tauri::command]
pub fn save_smtp_settings(
    db: State<'_, Arc<DbState>>,
    settings: SmtpSettings,
    password: String,
) -> AppResult<SmtpSettings> {
    let store = KeychainStore::login()?;
    mailer::save_settings(&db, &store, &settings, &password)
}

/// 测试连接：给管理员自己发一封自检信（ch.7 —— 不是空连接探测）。
///
/// 用**表里的**配置测（保存过的才是真正会用的那份）；
/// `password` 非空时用它（用户改了密码还没保存也想先测 —— 不落库）。
#[tauri::command]
pub fn test_smtp_connection(
    db: State<'_, Arc<DbState>>,
    _settings: SmtpSettings,
    password: String,
) -> AppResult<TestResult> {
    let saved = mailer::get_settings(&db)?
        .ok_or_else(|| AppError::ValidationError("发信配置还没有保存，先保存再测试".into()))?;
    let store = KeychainStore::login()?;
    let pw = if password.trim().is_empty() {
        None
    } else {
        Some(password.as_str())
    };
    mailer::test_connection(&store, &saved, pw)
}
