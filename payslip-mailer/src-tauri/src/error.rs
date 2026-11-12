//! 统一错误码（设计文档 ch.12 S15：错误码统一由 Rust 映射为中文文案）
//!
//! 约定：错误必须能回答三要素 —— 发生了什么 / 影响范围 / 下一步。
//!
//! 词表先行：错误码是前端与使用者依赖的**对外契约**，一次性按设计文档定齐，
//! 之后各任务卡逐步接线。
//!
//! 还没人构造的码**逐个**标 `#[allow(dead_code)]`，注释里写明哪张卡会构造它，
//! 接线时删掉那一行。为什么不给整个 enum 标一次：整块标注会把「已接线的码后来
//! 没人用了」也一起静默掉，而那种正是我们想让编译器报警的情况
//! （收窄的来由记在 ADR-0009 的遗留项里）。
//!
//! 已接线：`Keychain` / `KeychainMissing`（TC-101）、`Io`（`commands/app.rs` 定位数据目录）、
//! `BadCredentials` / `Locked` / `ValidationError` / `NotInitialized`
//! （TC-102 口令校验、失败锁定、口令策略、首启未建档）、
//! `MigrationDrift` / `ReadOnly` / `Duplicate`
//! （TC-103 迁移漂移专属码、只读态写拒绝、S01 拦重复建档）。
//! 其中 `KeychainMissing` 是**后加的码**，不在设计文档原词表里 ——
//! 原词表把「钥匙串读不到」当成一种情况，但未签名分发下「条目不存在」是升级后的
//! **正常路径**（ADR-0002），前端要据此引导重新初始化，而不是显示一句「无法访问钥匙串」
//! 让人分不清该点「允许」还是该重填。加码的取舍见 ADR-0009 第 6 条。
//!
//! TC-103 加了两个码，同样不在设计文档原词表里，理由分别是：
//!
//! - `MigrationDrift`：原词表把加密库这一层当成一种情况，但**只有驱动报错才是歧义的**
//!   （ADR-0010 第 4 条：密钥不对 / 参数漂移 / 真损坏三者同一句 `file is not a database`），
//!   而我们自己的迁移 checksum 比对**知道**原因是「某一版 SQL 与库里记录的不一致」。
//!   知道原因却不点名，就是把 HR 引去查错的东西。见 ADR-0012 第 4 条。
//! - `ReadOnly`：只读态下写被拒原先复用 `Database`，而 `Database` 的文案是
//!   「本机加密数据打不开」——与真实状态「已解锁、能读、只是不能写」正好相反。
//!   文案与状态不符是 TC-100 结案遗留里点名要治的，见 ADR-0012 第 5 条。

use serde::{Serialize, Serializer};

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// 库里还没有管理员那一行（TC-102 已接线：`infra::admins::unlock` / `change_password`
    /// 读不到行时）。前端据它走 S01 首启向导而不是弹解锁框 —— 界面归 TC-103。
    #[error("尚未初始化，请先完成首次启动向导")]
    NotInitialized,

    /// 口令不对（TC-102 已接线：`infra::admins::unlock`）。
    ///
    /// **不带剩余次数**：剩余次数是「状态」不是「错误信息」，走 `admin_status` 的独立字段。
    /// 锁屏**进入时**就要显示「已锁定 / 还剩几次」，那时没人尝试过口令，
    /// 拿不到一个错误码（见 spec 标准 6 与 ADR-0011 §6）。
    #[error("管理员口令错误，连续失败 5 次将锁定 60 秒")]
    BadCredentials,

    /// 锁定期内（TC-102 已接线：同上）。`{0}` 是**剩余秒数**，每次查询现算，
    /// 不是存进库里的值 —— 所以锁定期内的重复尝试不会延长锁定（spec 标准 6）。
    #[error("账户已锁定，请在 {0} 秒后重试")]
    Locked(u64),

    #[allow(dead_code)] // 未接线：TC-201 成员邮箱批量粘贴
    #[error("邮箱格式不正确：{0}")]
    InvalidEmail(String),

    /// 已存在，不允许重复建档。`{0}` 是**类别名**（「管理员」「成员」），不是那个值本身 ——
    /// 值可能是口令或邮箱，回显进错误就等于回显敏感值（红线 2 的同族）。
    ///
    /// 构造方在 TC-102 就位（`infra::admins::create_admin` 拦单管理员重复建档），
    /// 入口在 TC-103 接上（S01 首启向导 → `commands::auth::create_admin`）。
    /// TC-200 / TC-201 还要用它拦项目与成员重名。
    #[error("{0} 已存在，不允许重复")]
    Duplicate(String),

    #[allow(dead_code)] // 未接线：TC-200 查已停用项
    #[error("{0} 不存在或已被停用")]
    NotFound(String),

    #[allow(dead_code)] // 未接线：TC-202 模板变量解析
    #[error("模板变量无法解析：{0}。请检查变量名与数据完整性，已拦截发送")]
    TemplateError(String),

    /// 输入不符合规则（TC-102 已接线：口令策略「至少 10 位」，`domain::admin`；
    /// TC-302 起还要承接 Excel 导入的行级校验）。
    ///
    /// `{0}` 必须是**不含敏感值**的规则描述：口令策略的细节只能说「至少 10 位」，
    /// 不能把使用者刚输入的口令回显进错误里（红线 2 的同族，spec 标准 5）。
    #[error("数据校验未通过：{0}")]
    ValidationError(String),

    #[allow(dead_code)] // 未接线：TC-500 飞书 SMTP 实连
    #[error("SMTP 认证失败（535）。请更新专用密码后仅重试失败项，已中止本次发送")]
    SmtpAuthFailed,

    #[allow(dead_code)] // 未接线：P4 发送结果分类（TC-400 退避）
    #[error("SMTP 服务暂时不可用（{0}）。已自动降速并排队重试")]
    SmtpTransient(String),

    #[allow(dead_code)] // 未接线：P4 发送结果分类
    #[error("收件地址无效（{0}）。请核对该成员邮箱后单独补发")]
    SmtpInvalidRecipient(String),

    /// 加密库这一层的错误（TC-100 已接线：`infra::db` 的打开、参数校验、迁移全部走它）。
    ///
    /// `{0}` 是给人排查的细节（驱动报错、SQL、文件路径），只进 `Display`/日志；
    /// 面向使用者的文案见 `user_message()` —— 那边必须同时提「密钥不匹配」与「文件损坏」，
    /// 因为实测**参数漂移、错误密钥、真损坏三者的驱动报错完全相同**（ADR-0010 第 4 条），
    /// 假装知道原因会把 HR 引去查错的东西。
    #[error("数据库错误：{0}")]
    Database(String),

    /// 已应用的迁移 SQL 与程序内置的不一致，或库里的版本比程序还新（TC-103 已接线：
    /// `infra::db::migrate` 的两处漂移分支）。
    ///
    /// 与 `Database` 分开是**因为我们知道原因**：`Database` 那种情况驱动只报一句
    /// `file is not a database`，三种可能原因分不开，只能给通用文案；而这里是自己的
    /// checksum 比对报出来的，确切知道是第几版迁移对不上（ADR-0012 第 4 条）。
    ///
    /// `version` 进使用者文案（要点名是哪一版），`detail`（两个校验和各是什么）只进
    /// `Display`/日志 —— UI 文案不堆十六进制串。
    #[error("迁移漂移：第 {version} 版迁移与本机数据记录的不一致。{detail}")]
    MigrationDrift { version: u32, detail: String },

    /// 加密库处于只读态（迁移没跑到最新），本次写操作被拒（TC-103 已接线：
    /// `infra::db::DbState::write`）。
    ///
    /// `{0}` 是**整句**使用者文案，由 `infra::db::read_only_message` 生成一份、
    /// 横幅与写拒绝共用同一份 —— 这样「横幅说的」与「写被拒时看到的」必然逐字相同
    /// （spec 标准 16）。不复用 `Database`：那句「本机加密数据打不开」与
    /// 「已解锁、能读、只是不能写」是矛盾的（ADR-0012 第 5 条）。
    #[error("{0}")]
    ReadOnly(String),

    // 以下三个已接线（TC-101 / commands::app），不标 allow：
    // 哪天没人构造了，要能让编译器发现。
    #[error("钥匙串错误：{0}")]
    Keychain(String),

    /// 条目不存在 —— 未签名分发下升级会重置 ACL，这是**正常路径**而非故障（ADR-0002）。
    /// 与 `Keychain` 分开，是为了让前端能区分「点允许 / 重新输入」两种下一步。
    #[error(
        "本机钥匙串里找不到需要的凭据。可能是应用刚升级（未签名分发会重置钥匙串授权），或这台机器还没初始化过。请重新完成初始化或更新发件授权码；在此之前无法打开本机加密数据。"
    )]
    KeychainMissing,

    #[error("文件读写错误：{0}")]
    Io(String),

    // TC-100 起有生产构造方：钥匙串里取出的密钥长度不对、CSPRNG 取随机字节失败，
    // 都是「不是库的问题、也不是使用者能修的问题」，所以不归 Database 而归这里。
    #[error("内部错误：{0}")]
    Internal(String),

    // ── TC-104 备份相关（ADR-0013）────────────────────────────────────
    // 这四个码同样不在设计文档的原词表里。原词表只有一句「备份：AES-256 加密，用户设口令」，
    // 但备份恢复要区分四种**要给出不同下一步**的失败，混成一句「恢复失败」会让人乱试：
    //   · 口令错 → 该重输口令
    //   · 文件坏 → 该重新导出
    //   · 版本新 → 该升级应用（重输口令没用）
    //   · 写不出去 → 该看磁盘空间
    // 区分的依据与 `MigrationDrift` 同源：**我们知道原因**。
    /// 备份口令不对（TC-104 接线：`infra::backup`）。
    ///
    /// 与管理员口令错**刻意不共用** `BadCredentials`：那一句带「连续失败 5 次将锁定 60 秒」，
    /// 而备份口令保护的是一个离线文件，在线锁定给不了任何保护（ADR-0013 第 5 条）。
    /// 这里的下一步只有一个：重新输口令。
    #[allow(dead_code)] // 未接线：TC-104 infra::backup 恢复时构造
    #[error("备份口令不对，无法解开这个备份文件")]
    BackupPasswordWrong,

    /// 备份文件本身有问题：不是薪邮的备份、zip 解不开、或校验和对不上（TC-104 接线）。
    ///
    /// `{0}` 是给人排查的细节（哪个条目、期望与实际的校验和），只进 `Display`/日志。
    #[allow(dead_code)] // 未接线：TC-104 infra::backup 校验时构造
    #[error("备份文件有问题：{0}")]
    BackupCorrupted(String),

    /// 备份来自比当前应用更新的版本（TC-104 接线）。
    ///
    /// `version` 是备份里的 schema 版本，`supported` 是当前应用支持到的版本。
    /// 两个都进使用者文案 —— 下一步是「升级应用」，不说版本号等于让人猜。
    #[allow(dead_code)] // 未接线：TC-104 infra::backup 校验版本时构造
    #[error("备份来自更新的版本（第 {version} 版），当前应用只支持到第 {supported} 版")]
    BackupVersionTooNew { version: u32, supported: u32 },

    /// 导出或恢复这一步没做成（磁盘满、目录不可写、库被占用等）。
    ///
    /// `{0}` 是驱动报错与路径，只进 `Display`/日志；使用者文案在 `user_message()`。
    #[allow(dead_code)] // 未接线：TC-104 infra::backup / commands::backup
    #[error("备份操作失败：{0}")]
    BackupFailed(String),

    /// 清空数据时管理员口令不对（TC-105 接线：`infra::wipe`）。
    ///
    /// **刻意不复用 `BadCredentials`**：那一句带「连续失败 5 次将锁定 60 秒」，
    /// 而销毁口令**不锁定**（ADR-0014 决策 5）—— 那个入口本来就在解锁态，
    /// 锁定只会让人不敢再试。给独立码，前端才能说清「口令不对，什么都没删」。
    #[allow(dead_code)] // 未接线：TC-105 infra::wipe
    #[error("管理员口令不对")]
    WipePasswordWrong,
}

pub type AppResult<T> = Result<T, AppError>;

impl AppError {
    /// 面向使用者的文案：回答「发生了什么 / 影响范围 / 下一步」，随 IPC 进前端。
    ///
    /// 与 `Display` 的分工：`Display` 给日志用，保留驱动报错、文件路径、SQL 等内部细节；
    /// 这里把「内部管线」类错误（`Internal` / `Database` / `Keychain` / `Io`）的细节抹掉，
    /// 因为那些字符串是给人排查的，不是给使用者看的 —— 它们可能带绝对路径与库版本。
    /// 业务类错误（邮箱、模板、SMTP 状态码）的细节本身就是用户能理解的信息，原样保留。
    pub fn user_message(&self) -> String {
        match self {
            AppError::Internal(_) => "内部错误，本次操作未完成。请重试；若反复出现，请把日志交给维护人员排查。".into(),
            // 两种可能都要点名：实测「密钥与库不匹配」「加密参数漂移」「文件真损坏」三者的
            // 驱动报错是同一句 `file is not a database`，只说一种会把人引偏（ADR-0010 第 4 条）。
            AppError::Database(_) => "本机加密数据打不开：可能是钥匙串里的密钥与这份数据不匹配（应用升级会重置钥匙串授权），也可能是文件损坏。请先从备份恢复或重新完成初始化，再重试本次操作；在此之前请不要删除数据文件。".into(),
            AppError::Keychain(_) => "无法访问 macOS 钥匙串。若弹出授权窗请点击「允许」；点「拒绝」将无法打开本机加密数据。".into(),
            AppError::Io(_) => "文件读写失败，本次操作未完成。请检查磁盘剩余空间与该文件夹的访问权限后重试。".into(),
            // 点名是哪一版，但不带校验和：`detail` 是十六进制串，给人排查用，不是给 HR 看。
            // 只读是漂移的直接后果，所以「影响范围」写的是「只能查看、改动不会写入」。
            AppError::MigrationDrift { version, .. } => format!(
                "本机数据结构与当前应用不一致（第 {version} 版迁移对不上）：本机数据当前只能查看，本次修改不会写入。请不要手工修改数据文件；可从备份恢复或联系维护人员。"
            ),
            // `ReadOnly` 的载荷本身就是整句使用者文案（`infra::db::read_only_message` 生成），
            // 所以走 `other => other.to_string()` 原样透出，**不要**在这里加一条臂改写它：
            // 横幅与写拒绝必须逐字相同，改一次就多一处会漂移的文案（spec 标准 16）。
            //
            // 备份这四个码各自给一句三要素齐的话（TC-104 · ADR-0013）：
            AppError::BackupPasswordWrong => "备份口令不对：这个备份文件没有解开。\
请重新输入导出时设置的那个备份口令；本机数据没有被改动。".into(),
            AppError::BackupCorrupted(_) => "备份文件有问题，无法用于恢复：\
它可能不是薪邮导出的备份，或者在拷贝过程中损坏了。本机数据没有被改动；\
请重新导出一次备份，或换一份拷贝完整的文件再试。".into(),
            AppError::BackupVersionTooNew { version, supported } => format!(
                "这份备份来自更新的版本（第 {version} 版），当前应用只支持到第 {supported} 版。\
本机数据没有被改动；请先升级应用再恢复，或换一份旧版本的备份。"
            ),
            AppError::BackupFailed(_) => "备份操作没有完成，本次备份或恢复未生效。\
请检查磁盘剩余空间与所选目录的读写权限后重试；若反复失败，请把日志交给维护人员排查。".into(),
            other => other.to_string(),
        }
    }
}

/// 前端只拿到一个稳定的 code + 中文文案，绝不暴露内部堆栈或凭据。
impl Serialize for AppError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let code = match self {
            AppError::NotInitialized => "NOT_INITIALIZED",
            AppError::BadCredentials => "BAD_CREDENTIALS",
            AppError::Locked(_) => "LOCKED",
            AppError::InvalidEmail(_) => "INVALID_EMAIL",
            AppError::Duplicate(_) => "DUPLICATE",
            AppError::NotFound(_) => "NOT_FOUND",
            AppError::TemplateError(_) => "TEMPLATE_ERROR",
            AppError::ValidationError(_) => "VALIDATION_ERROR",
            AppError::SmtpAuthFailed => "SMTP_AUTH_FAILED",
            AppError::SmtpTransient(_) => "SMTP_TRANSIENT",
            AppError::SmtpInvalidRecipient(_) => "SMTP_INVALID_RECIPIENT",
            AppError::Database(_) => "DATABASE",
            AppError::MigrationDrift { .. } => "MIGRATION_DRIFT",
            AppError::ReadOnly(_) => "READ_ONLY",
            AppError::Keychain(_) => "KEYCHAIN",
            AppError::KeychainMissing => "KEYCHAIN_MISSING",
            AppError::Io(_) => "IO",
            AppError::Internal(_) => "INTERNAL",
            AppError::BackupPasswordWrong => "BACKUP_PASSWORD_WRONG",
            AppError::BackupCorrupted(_) => "BACKUP_CORRUPTED",
            AppError::BackupVersionTooNew { .. } => "BACKUP_VERSION_TOO_NEW",
            AppError::BackupFailed(_) => "BACKUP_FAILED",
            AppError::WipePasswordWrong => "WIPE_PASSWORD_WRONG",
        };
        let mut st = serializer.serialize_struct("AppError", 2)?;
        st.serialize_field("code", code)?;
        st.serialize_field("message", &self.user_message())?;
        st.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_serializes_to_stable_code_and_chinese_message() {
        let err = AppError::SmtpAuthFailed;
        let json = serde_json::to_string(&err).unwrap();
        assert!(json.contains("\"code\":\"SMTP_AUTH_FAILED\""));
        assert!(json.contains("专用密码"));
    }

    #[test]
    fn message_states_what_happened_impact_and_next_step() {
        // 「发生了什么 + 影响范围 + 下一步」三要素缺一不可
        let msg = AppError::SmtpAuthFailed.to_string();
        assert!(msg.contains("认证失败"), "缺：发生了什么");
        assert!(msg.contains("中止"), "缺：影响范围");
        assert!(msg.contains("重试"), "缺：下一步");
    }

    #[test]
    fn invalid_email_error_carries_the_offending_value() {
        let err = AppError::InvalidEmail("abc".into());
        assert!(err.to_string().contains("abc"));
        let json = serde_json::to_string(&err).unwrap();
        assert!(json.contains("INVALID_EMAIL"));
    }

    /// 内部管线类错误：细节只进日志，不进前端 payload。
    #[test]
    fn plumbing_errors_never_leak_internal_details_to_the_frontend() {
        let cases = [
            (
                AppError::Internal("panic at src/foo.rs:42".into()),
                "panic at",
            ),
            (
                AppError::Database("no such column: net_pay in SELECT * FROM rows".into()),
                "no such column",
            ),
            (
                AppError::Keychain("SecItemCopyMatching failed: -25308".into()),
                "SecItemCopyMatching",
            ),
            (
                AppError::Io(
                    "Permission denied (os error 13) at /Users/hr/.payslip/data.db".into(),
                ),
                "/Users/hr",
            ),
        ];
        for (err, detail) in cases {
            let json = serde_json::to_string(&err).unwrap();
            assert!(
                !json.contains(detail),
                "{:?} 把内部细节泄漏进了 payload",
                err
            );
            // 抹掉细节不能顺手抹掉三要素
            let msg = err.user_message();
            assert!(!msg.is_empty(), "缺：发生了什么");
            assert!(
                msg.contains("重试") || msg.contains("允许") || msg.contains("维护人员"),
                "缺：下一步 · {msg}"
            );
        }
    }

    /// 同一份细节必须还能在日志里找到，否则排查时无从下手。
    #[test]
    fn internal_detail_still_reaches_the_log_via_display() {
        let err = AppError::Database("no such column: net_pay".into());
        assert!(err.to_string().contains("no such column"));
        assert!(!err.user_message().contains("no such column"));
    }

    /// 业务类错误的细节本身就是用户能理解的信息，必须保留。
    #[test]
    fn business_errors_keep_their_user_readable_detail() {
        assert!(
            AppError::InvalidEmail("zhang@".into())
                .user_message()
                .contains("zhang@")
        );
        assert!(
            AppError::TemplateError("{{社保个人}} 缺失".into())
                .user_message()
                .contains("社保个人")
        );
    }

    /// 迁移漂移必须与通用 `DATABASE` 分开：**知道原因就必须点名是哪一版**，
    /// 同时不把校验和十六进制串塞进使用者文案。见 ADR-0012 第 4 条、spec 标准 17。
    #[test]
    fn migration_drift_names_the_version_without_leaking_checksums() {
        let err = AppError::MigrationDrift {
            version: 2,
            detail: "库里记录 a1b2c3d4e5f60718，程序内置 9f8e7d6c5b4a3210".into(),
        };
        let json = serde_json::to_string(&err).unwrap();
        assert!(
            json.starts_with("{\"code\":\"MIGRATION_DRIFT\""),
            "码必须是 MIGRATION_DRIFT，实际 {json}"
        );

        let msg = err.user_message();
        assert!(msg.contains("迁移"), "缺：发生了什么");
        assert!(
            msg.contains("第 2 版"),
            "必须点出是哪一版，否则 HR 无从查起"
        );
        assert!(!msg.contains("a1b2c3d4"), "校验和不得进使用者文案");
        assert!(msg.contains("只能查看"), "缺：影响范围");
        assert!(
            msg.contains("备份") || msg.contains("维护人员"),
            "缺：下一步 · {msg}"
        );

        // 抹掉校验和不能顺手抹掉排查线索：两个校验和必须还能从 Display 进日志。
        let display = err.to_string();
        assert!(display.contains("a1b2c3d4"), "库里记录的校验和没进日志");
        assert!(display.contains("9f8e7d6c"), "程序内置的校验和没进日志");
    }

    /// 只读态的写拒绝必须**原样**透出整句文案：横幅与写失败提示共用同一份字符串，
    /// 才谈得上「说的是同一件事」（spec 标准 16 · ADR-0012 第 5 条）。
    #[test]
    fn read_only_message_passes_through_verbatim() {
        let msg = "数据库处于只读状态（迁移未完成），本次修改未写入。".to_string();
        let err = AppError::ReadOnly(msg.clone());
        assert_eq!(err.user_message(), msg, "文案必须逐字透出，不得被改写");

        let json = serde_json::to_string(&err).unwrap();
        assert!(json.starts_with("{\"code\":\"READ_ONLY\""), "实际 {json}");
        assert!(
            !json.contains("本机加密数据打不开"),
            "不得复用 DATABASE 那句与「已解锁、能读」矛盾的文案"
        );
    }

    /// 「条目不存在」是未签名分发升级后的**正常路径**（ADR-0002），
    /// 必须与「钥匙串拒绝了我」用不同的 code 分开，前端才分得清
    /// 该点「允许」还是该重新输入授权码。见 ADR-0009 第 6 条。
    #[test]
    fn keychain_missing_is_a_distinct_code_with_its_own_next_step() {
        let json = serde_json::to_string(&AppError::KeychainMissing).unwrap();
        assert!(json.contains("\"code\":\"KEYCHAIN_MISSING\""));
        assert!(
            !json.contains("\"code\":\"KEYCHAIN\""),
            "两个码必须不同，否则前端无法分支"
        );

        let msg = AppError::KeychainMissing.user_message();
        assert!(msg.contains("找不到"), "缺：发生了什么");
        assert!(msg.contains("无法打开"), "缺：影响范围");
        assert!(msg.contains("重新"), "缺：下一步");
    }
}
