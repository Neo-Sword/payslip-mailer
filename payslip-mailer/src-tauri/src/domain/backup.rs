//! 备份的口令策略与清单结构（TC-104 · ADR-0013）
//!
//! 纯函数 + 纯数据：不碰库、不碰文件、不碰钥匙串。
//! KDF 派生放在这里，因为它决定「备份的密钥从哪来」——
//! **来自用户口令，不来自钥匙串里的 DB key**（ADR-0013 第 1 条）。
//!
//! 关键性质：**参数随备份走**。清单里记着 argon2 的版本与 m/t/p，
//! 将来应用调强参数时，旧备份仍按它自己记的参数解开。

use argon2::{Algorithm, Argon2, Params, Version};
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};

/// 备份口令的最少位数。与管理员口令同一条规则（`check_password_policy` 的口径）——
/// 全项目两处口令，一套强度标准。
pub const MIN_BACKUP_PASSWORD_CHARS: usize = 10;

/// 派生出来的对称密钥长度。SQLCipher 的 raw key 就是这个长度。
pub const BACKUP_KEY_BYTES: usize = 32;

/// Argon2id 参数。与管理员口令同一套（ADR-0011 §1 实测过的）。
///
/// 备份口令保护的是**会被拷到 U 盘 / 网盘里**的文件，攻击者可以离线爆破，
/// 所以这里的参数至少不能比管理员口令更弱。
const ARGON2_M_COST: u32 = 19_456; // 19 MiB
const ARGON2_T_COST: u32 = 2;
const ARGON2_P_COST: u32 = 1;

/// 清单文件在备份包里的名字。
pub const MANIFEST_ENTRY: &str = "manifest.json";
/// 库副本在备份包里的名字。
pub const DATA_ENTRY: &str = "data.db";
/// 校验和文件在备份包里的名字。
pub const CHECKSUM_ENTRY: &str = "checksum.sha256";

/// 备份的种类。清单里要写清，否则使用者分不清「这是手动导出的」还是
/// 「这是恢复前自动留的保险」。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BackupKind {
    /// 使用者主动导出。
    Manual,
    /// 恢复前自动留的当前库副本（设计文档 ch.6「先自动备份当前库」）。
    PreRestoreSafety,
}

/// KDF 参数。写进清单 → 旧备份不会因为应用调强参数而作废。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KdfParams {
    /// 参数组名，固定 `argon2id`（将来换算法时靠它区分）。
    pub algorithm: String,
    /// argon2 版本号（19）。
    pub version: u32,
    /// 内存代价（KiB）。
    pub m_cost: u32,
    /// 迭代次数。
    pub t_cost: u32,
    /// 并行度。
    pub p_cost: u32,
    /// 盐的十六进制。**盐不保密**，它本来就要跟着备份走。
    pub salt: String,
}

impl KdfParams {
    /// 当前应用的参数。`salt` 由调用方给（每次派生随机生成）。
    pub fn current(salt: [u8; 16]) -> Self {
        Self {
            algorithm: "argon2id".into(),
            version: Version::V0x13 as u32,
            m_cost: ARGON2_M_COST,
            t_cost: ARGON2_T_COST,
            p_cost: ARGON2_P_COST,
            salt: crate::util::to_hex(&salt),
        }
    }
}

/// 备份清单。备份包里三样东西之一，另两样是 `data.db` 与 `checksum.sha256`。
///
/// **不放任何敏感信息**（ADR-0013 第 2 条）：zip 容器本身不加密，
/// `unzip -l` 就能看见这个文件的内容，所以这里只放版本号、时刻、KDF 参数与条目名。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupManifest {
    /// 清单格式版本。将来改结构时靠它区分。
    pub manifest_version: u32,
    /// 导出时应用的版本（如 `0.1.0`）。
    pub app_version: String,
    /// 导出时数据库的 schema 版本（`schema_migrations` 的最大版本号）。
    pub schema_version: u32,
    /// 导出时刻（epoch 秒）。
    pub created_at: i64,
    /// 这份备份是哪来的。
    pub kind: BackupKind,
    /// 库副本的 SHA-256（十六进制）。恢复时用它核对文件没被改过。
    pub data_sha256: String,
    /// 派生密钥用的 KDF 参数。
    pub kdf: KdfParams,
}

/// 校验备份口令的强度。
///
/// 与管理员口令同一条规则：至少 10 位。**不回显口令本身**（红线 2 的同族）。
pub fn check_backup_password_policy(password: &str) -> AppResult<()> {
    let chars = password.chars().count();
    if chars < MIN_BACKUP_PASSWORD_CHARS {
        return Err(AppError::ValidationError(format!(
            "备份口令至少 {MIN_BACKUP_PASSWORD_CHARS} 位，当前 {chars} 位"
        )));
    }
    Ok(())
}

/// 从口令派生备份密钥（32 字节）。
///
/// `salt` 传进来而不是在这里生成：调用方要把盐写进清单，
/// 两处必须用**同一个**盐（否则解不开）。
pub fn derive_key(password: &str, salt: &[u8]) -> AppResult<[u8; BACKUP_KEY_BYTES]> {
    if salt.len() < 8 {
        // Argon2 自己也要求盐至少 8 字节；这里提前拦住，免得报一个看不懂的错。
        return Err(AppError::Internal("备份盐至少 8 字节".into()));
    }
    let params = Params::new(ARGON2_M_COST, ARGON2_T_COST, ARGON2_P_COST, None)
        .map_err(|e| AppError::Internal(format!("Argon2 参数不合法：{e}")))?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut out = [0u8; BACKUP_KEY_BYTES];
    argon2
        .hash_password_into(password.as_bytes(), salt, &mut out)
        .map_err(|e| AppError::Internal(format!("派生备份密钥失败：{e}")))?;
    Ok(out)
}

/// 按清单里记的参数派生密钥（恢复时用）。
///
/// 参数**跟着备份走**而不是跟着应用走：应用将来调强 argon2 参数，
/// 旧备份仍按它自己记的参数解开（ADR-0013 第 1 条）。
pub fn derive_key_with_params(
    password: &str,
    salt: &[u8],
    params: &KdfParams,
) -> AppResult<[u8; BACKUP_KEY_BYTES]> {
    if params.algorithm != "argon2id" {
        return Err(AppError::BackupCorrupted(format!(
            "备份用了不支持的密钥派生方式：{}",
            params.algorithm
        )));
    }
    if params.m_cost < ARGON2_M_COST || params.t_cost < ARGON2_T_COST {
        // 参数被改小 → 这份备份是用更弱的设置加密的，当前的强度保证对它不成立。
        // 仍然按它自己记的参数解（否则这份备份永远打不开），但要在日志里留痕。
        tracing::warn!(
            m_cost = params.m_cost,
            t_cost = params.t_cost,
            "备份的 KDF 参数弱于当前应用的标准，按备份自己记的参数解"
        );
    }
    let p = Params::new(params.m_cost, params.t_cost, params.p_cost, None)
        .map_err(|e| AppError::BackupCorrupted(format!("备份里的 KDF 参数不合法：{e}")))?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, p);
    let mut out = [0u8; BACKUP_KEY_BYTES];
    argon2
        .hash_password_into(password.as_bytes(), salt, &mut out)
        .map_err(|e| AppError::Internal(format!("派生备份密钥失败：{e}")))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 口令策略与管理员口令同一条：至少 10 位，且不回显口令。
    #[test]
    fn backup_password_policy_matches_the_admin_one() {
        assert!(check_backup_password_policy("123456789").is_err());
        check_backup_password_policy("1234567890").expect("10 位应通过");
        let secret = "短口令不许回显";
        let err = check_backup_password_policy(secret).expect_err("太短应被拒");
        assert!(!err.user_message().contains(secret), "错误里回显了口令");
    }

    /// 同一口令 + 同一盐 → 同一把钥匙；换盐 → 不同的钥匙。
    /// 这条是「备份能解开」的数学基础。
    #[test]
    fn derivation_is_deterministic_per_salt_and_differs_across_salts() {
        let salt_a = [7u8; 16];
        let salt_b = [9u8; 16];
        let k1 = derive_key("一个够长的备份口令", &salt_a).expect("派生失败");
        let k2 = derive_key("一个够长的备份口令", &salt_a).expect("派生失败");
        let k3 = derive_key("一个够长的备份口令", &salt_b).expect("派生失败");
        assert_eq!(k1, k2, "同口令同盐必须同钥匙");
        assert_ne!(k1, k3, "换盐必须换钥匙");
        assert_eq!(k1.len(), BACKUP_KEY_BYTES);
    }

    /// 参数随备份走：清单里记的旧参数能派生出与当初一样的钥匙。
    /// 这是「将来调强 argon2 参数不会让旧备份作废」的前提。
    #[test]
    fn manifest_params_reproduce_the_same_key() {
        let salt = [3u8; 16];
        let params = KdfParams::current(salt);
        let direct = derive_key("一个够长的备份口令", &salt).expect("派生失败");
        let via_manifest =
            derive_key_with_params("一个够长的备份口令", &salt, &params).expect("派生失败");
        assert_eq!(direct, via_manifest, "清单参数必须派生出同一把钥匙");
    }

    /// 清单里不许出现敏感信息：它是可以被 `unzip -p` 直接读出来的明文。
    #[test]
    fn manifest_carries_no_sensitive_fields() {
        let json = serde_json::to_string(&BackupManifest {
            manifest_version: 1,
            app_version: "0.1.0".into(),
            schema_version: 2,
            created_at: 1_780_000_000,
            kind: BackupKind::Manual,
            data_sha256: "00".repeat(32),
            kdf: KdfParams::current([1u8; 16]),
        })
        .expect("序列化失败");
        for forbidden in [
            "email", "password", "口令", "姓名", "工资", "amount", "salary",
        ] {
            assert!(
                !json.to_lowercase().contains(&forbidden.to_lowercase()),
                "清单里不该出现 {forbidden}：{json}"
            );
        }
    }
}
