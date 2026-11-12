//! Argon2id 口令哈希（TC-102 · ADR-0011 §1–§4）
//!
//! 这一层只管「一串口令 ↔ 一串 PHC 文本」，不碰库、不碰锁定计数。
//! 分成这么薄一层是为了验收：参数对不对、盐随不随机、旧参数能不能验，
//! 都在这里用不依赖 Keychain 的单元测试钉住；`infra::admins` 只负责把它们接到一行数据上。
//!
//! 设计依据：设计文档 ch.4 安全模型 179-183「Argon2id + 随机盐」「绝不明文或 MD5 保存」。

use crate::error::{AppError, AppResult};
use argon2::password_hash::Error as PhcError;
use argon2::{
    Algorithm, Argon2, Params, ParamsBuilder, PasswordHash, PasswordHasher, PasswordVerifier,
    Version,
};
use security_framework::random::SecRandom;

/// 内存开销，单位 KiB。19456 KiB ≈ 19 MiB。
///
/// 这个数**不是**随手挑的：它是 argon2 crate 的 `Params::DEFAULT_M_COST`（源码里写死
/// `19 * 1024`），也是 OWASP 口令存储建议的下限组（ADR-0011 §2）。
/// 实测稳态 hash 10.50 ms、verify 10.24 ms（`/tmp/argon2-run.log`），
/// 对着设计文档 2160「主界面交互响应 < 100ms」留了十倍余量。
/// 同一次实测里 OWASP 推荐的另一组 `m=65536,t=3,p=4` 要 65 ms——不是不能用，
/// 是把预算吃掉三分之二，而解锁是每次唤醒都要做的事，所以取下限。
pub const M_KIB: u32 = 19_456;

/// 迭代次数（时间开销）。
pub const T_COST: u32 = 2;

/// 并行度。1 是因为解锁发生在 UI 线程上，多开线程只是把同一段时间切成几段。
pub const P_COST: u32 = 1;

/// Argon2 版本号（0x13 = 19），写进 PHC 串的 `v=` 段。
pub const VERSION: u32 = 19;

/// PHC 串里的算法标识。spec 标准 3 就是拿这个前缀断言「库里存的确实是 Argon2id」。
pub const ALG_ID: &str = "argon2id";

/// 盐长度（字节）。16 字节 = 128 位随机，够到「不可能撞」的量级；
/// argon2 要求至少 8 字节（实测 7 字节直接 `SaltInvalid`），16 是它的推荐值。
const SALT_BYTES: usize = 16;

/// 一次校验的两个结论，必须一起给：
///
/// - `password_ok`：口令对不对。
/// - `needs_rehash`：这串哈希的参数比当前策略旧，验过之后应当用新参数重写一遍
///   （spec 标准 9）。之所以在**验口令的同时**判这件事，是因为参数写在串里，
///   解析串是验口令的必经步骤，单独判一次反而要多解析一遍。
#[derive(Debug)] // 两个 bool，不含口令与盐，可以安全进断言失败信息
pub struct VerifyOutcome {
    pub password_ok: bool,
    pub needs_rehash: bool,
}

/// 当前策略的参数。`ParamsBuilder::build()` 只在参数越界时才失败，
/// 而这里的三个常量都是编译期写死的合法值 —— 失败即说明有人改了常量，
/// 报 `Internal` 而不是假装能用默认值跑下去。
fn params() -> AppResult<Params> {
    ParamsBuilder::new()
        .m_cost(M_KIB)
        .t_cost(T_COST)
        .p_cost(P_COST)
        .build()
        .map_err(|e| AppError::Internal(format!("口令哈希参数无效：{e}")))
}

/// 按当前策略配好的 Argon2id 实例。构造本身不分配内存（真正的内存块在每次哈希里申请），
/// 所以每次调用现建一个，不藏全局。
fn hasher() -> AppResult<Argon2<'static>> {
    Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params()?))
}

/// 取盐。走 Apple 的 `SecRandomCopyBytes`（security-framework），与 DB 密钥同一个 CSPRNG 源。
///
/// 为什么不用 crate 自带的 `generate_salt()`：它挂在 `getrandom` 特征上，而 ADR-0011 §1
/// 明确关掉那个特征——全项目只留一个随机源，审计时只需要问「SecRandom 用对了吗」，
/// 不用同时追两个实现。
fn random_salt() -> AppResult<[u8; SALT_BYTES]> {
    let mut salt = [0u8; SALT_BYTES];
    SecRandom::default()
        .copy_bytes(&mut salt)
        .map_err(|e| AppError::Internal(format!("生成口令盐失败：{e}")))?;
    Ok(salt)
}

/// 哈希一个新口令，返回 PHC 串（`$argon2id$v=19$m=19456,t=2,p=1$<盐>$<哈希>`）。
///
/// 盐和参数都在串里，所以 `admins` 表只需要 `pwd_hash` 一列（ADR-0011 §3）。
/// 入参是 `&str`：我们哈希的是它的 UTF-8 字节。口令策略按**字符**数（spec 标准 2），
/// 两者在中文口令上差三倍，所以策略那边数的是字符、这里喂的是字节，各自写清。
pub fn hash(password: &str) -> AppResult<String> {
    let salt = random_salt()?;
    let parsed = hasher()?
        .hash_password_with_salt(password.as_bytes(), &salt)
        .map_err(|e| AppError::Internal(format!("口令哈希失败：{e}")))?;
    // `PasswordHash: Display` 就是 PHC 串格式（phc 0.6.1 的 lib.rs 里有 impl）。
    Ok(parsed.to_string())
}

/// 校验口令。返回 `Ok(outcome)` 表示「这次判定做成了」，口令对不对在 `outcome.password_ok` 里；
/// 只有**判定本身做不成**（串读不出、参数不认识、内存分配失败）才返回 `Err`。
///
/// 这个区分是 spec 标准 5 的前提：`BAD_CREDENTIALS` 必须只对应「口令确实错了」。
/// 库里那串哈希如果被写坏或来自我们没见过的算法，那是数据问题，
/// 不能报成「你口令错了」——那会让人一直重敲一个本来正确的口令。
///
/// 跨参数这件事（用 `m=19456` 的实例去验一串 `m=2048` 的哈希）在源码里是定死的：
/// `password-hash` 的 `PasswordVerifier::verify_password` 用
/// `T::Params::try_from(hash)` 从**串里**取参数再算，不用 `self.params`
/// （password-hash 0.6.1 的 lib.rs:197-224）。spec 标准 9 的自动升级重哈希就靠这一点，
/// 下面的测试把它从「读过源码」变成「跑过一遍」。
pub fn verify(phc: &str, password: &str) -> AppResult<VerifyOutcome> {
    let parsed = PasswordHash::new(phc)
        .map_err(|e| AppError::Internal(format!("库里的口令哈希串读不出来：{e}")))?;
    let needs_rehash = !params_match(&parsed);

    let ok = match hasher()?.verify_password(password.as_bytes(), &parsed) {
        Ok(()) => true,
        // 口令错的唯一返回码（同上那段源码：比较不过就走 `Err(Error::PasswordInvalid)`）。
        // 已知的一处混叠：口令超过 4 GiB 时 argon2 的 `PwdTooLong` 也映射成同一个码
        // （argon2 0.6.0 的 error.rs）。现实中不会有这种口令，真撞上了也只是
        // 「把内部错误显示成口令错误」，不会放宽任何判定。
        Err(PhcError::PasswordInvalid) => false,
        Err(e) => return Err(AppError::Internal(format!("口令校验失败：{e}"))),
    };

    Ok(VerifyOutcome {
        password_ok: ok,
        needs_rehash,
    })
}

/// 串里的参数是否等于当前策略。任何一项对不上（含读不到该项）都算「偏旧」。
///
/// 用 `get_decimal` 而不是 `Params::try_from`：`Params` 的字段是私有的，
/// 而且 `try_from` 会把缺失的参数**补成默认值**，那样一来
/// 「串里根本没写 m」会被当成「m 等于默认值」，正是我们要能区分的情况。
fn params_match(parsed: &PasswordHash) -> bool {
    parsed.algorithm.as_str() == ALG_ID
        && parsed.version == Some(VERSION)
        && parsed.params.get_decimal("m") == Some(M_KIB)
        && parsed.params.get_decimal("t") == Some(T_COST)
        && parsed.params.get_decimal("p") == Some(P_COST)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验收标准 3（哈希侧）：库里存的确实是 Argon2id 的 PHC 串，参数段等于代码常量。
    #[test]
    fn password_hash_is_argon2id_phc_with_the_pinned_params() {
        let phc = hash("欧阳傲天1234").expect("哈希失败");
        assert!(
            phc.starts_with(&format!("${ALG_ID}$v={VERSION}$")),
            "PHC 串前缀不对：{phc}"
        );
        assert!(
            phc.contains(&format!("m={M_KIB},t={T_COST},p={P_COST}")),
            "参数段不等于钉死的策略常量：{phc}"
        );
        let parsed = PasswordHash::new(&phc).expect("产出的串必须自己可解析");
        assert!(params_match(&parsed), "自产的串就该判定为「参数不旧」");
    }

    /// 验收标准 4：同一口令两次哈希必须不同（盐真的在起作用），但都能验过。
    #[test]
    fn same_password_yields_different_hashes_that_both_verify() {
        let a = hash("同一个口令123456").expect("哈希 a 失败");
        let b = hash("同一个口令123456").expect("哈希 b 失败");
        assert_ne!(a, b, "两次哈希一模一样说明盐没生效（或盐是写死的）");

        for phc in [&a, &b] {
            let out = verify(phc, "同一个口令123456").expect("校验应成功");
            assert!(out.password_ok, "自己刚产的哈希必须验得过");
            assert!(!out.needs_rehash, "参数没理由被判偏旧");
        }
    }

    /// 验收标准 5（底层）：口令错是 `Ok(false)` 而不是 `Err`，且错误/串里不含口令明文。
    #[test]
    fn wrong_password_is_a_negative_not_an_error() {
        let secret = "欧阳傲天1234";
        let phc = hash(secret).expect("哈希失败");
        let out = verify(&phc, "欧阳傲天1235").expect("口令错不该返回 Err");
        assert!(!out.password_ok);

        // 红线 2 的同族：口令不进哈希串本身（Base64 里也不会恰好撞出来，
        // 因为存的是 Argon2 摘要而不是可逆变换的口令）。
        assert!(!phc.contains(secret), "PHC 串里出现了口令明文：{phc}");
    }

    /// 验收标准 9 的前提：校验用的是**串里**的参数，不是实例里的参数。
    ///
    /// 这条测试就是 spec 标准 9 的机制证明：先产一串 `m=2048` 的旧参数哈希，
    /// 用当前策略（`m=19456`）的实例去验 → 口令判定为对、且标记 `needs_rehash`；
    /// 重新哈希后参数段等于当前常量，旧口令仍然验得过。
    #[test]
    fn older_params_hash_verifies_and_is_flagged_for_rehash() {
        let secret = "旧参数口令12345";
        let legacy = Argon2::new(
            Algorithm::Argon2id,
            Version::V0x13,
            ParamsBuilder::new()
                .m_cost(2048)
                .t_cost(2)
                .p_cost(1)
                .build()
                .expect("旧参数应合法"),
        )
        .hash_password_with_salt(secret.as_bytes(), b"0123456789abcdef")
        .expect("旧参数哈希失败")
        .to_string();
        assert!(
            legacy.contains("m=2048"),
            "对照组必须是偏松的参数 · {legacy}"
        );

        let out = verify(&legacy, secret).expect("旧参数哈希应能完成校验");
        assert!(
            out.password_ok,
            "verify 没有用串里的 m=2048 去算（那样会算出不同的摘要）"
        );
        assert!(out.needs_rehash, "参数偏旧必须被标出来");

        // 解锁成功后 `infra::admins` 要做的事，在这一层先验一遍：重写 + 旧口令仍可验。
        let upgraded = hash(secret).expect("重哈希失败");
        assert!(
            upgraded.contains(&format!("m={M_KIB}")),
            "重哈希后参数应升到当前策略 · {upgraded}"
        );
        let recheck = verify(&upgraded, secret).expect("重哈希后校验失败");
        assert!(recheck.password_ok);
        assert!(!recheck.needs_rehash, "升级后不该再被标记");

        // 旧串本身仍然验得过（升级失败/断电时不会把人锁死在门外）。
        assert!(verify(&legacy, secret).expect("旧串重验失败").password_ok);
    }

    /// 「哈希串读不出」必须是 `Err`，不能报成口令错误（spec 标准 5 的边界）。
    #[test]
    fn unreadable_hash_string_is_an_internal_error_not_a_wrong_password() {
        for garbage in ["", "not-a-phc-string", "$argon2id$v=19$m=x$abc$def"] {
            let err = verify(garbage, "随便什么口令123").expect_err("读不出的串必须返回 Err");
            assert!(
                matches!(err, AppError::Internal(_)),
                "必须是 INTERNAL 而不是 BAD_CREDENTIALS · {err}"
            );
        }
    }

    /// 参数缺失也算「偏旧」：`Params::try_from` 会补默认值，`get_decimal` 不会。
    #[test]
    fn missing_params_are_treated_as_outdated() {
        let parsed = PasswordHash::new("$argon2id$v=19$c=1$AAAAAAAAAAAAAAAA$AAAAAAAAAAAAAAAA")
            .expect("无 m/t/p 的串应能解析（参数是可选段）");
        assert!(!params_match(&parsed), "缺参数不能当成「等于默认值」");
    }
}
