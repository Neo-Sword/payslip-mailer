//! macOS 钥匙串：DB key、SMTP 授权码、OAuth token 的唯一容身处。
//! （设计文档 ch.5「凭据只进 Keychain，禁止落库」；选型与 API 取舍见 docs/adr/0009）
//!
//! 三条不可让的约束决定了下面每个 API 的形状：
//!
//! 1. **秘密不进日志**（红线 2）：`Secret` 的 `Debug` 只报字节数，不报内容。
//! 2. **「条目不存在」是正常路径**（ADR-0002：未签名应用覆盖安装会重置钥匙串 ACL）：
//!    它映射成 `KEYCHAIN_MISSING`，与「钥匙串拒绝了我」分开，前端才给得出对的下一步。
//! 3. **无 GUI 会话不能挂**（ADR-0008）：测试用临时钥匙串 + `disable_user_interaction`，
//!    把「可能弹框」变成「立刻报错」。
//!
//! 注意第 3 条只适用于测试：**生产路径绝不能禁用用户交互** ——
//! 升级后第一次读旧条目本来就该弹一次授权框，那是 ADR-0002 里 HR 要点「允许」的地方。

// TC-100 已把 `put` / `get` / `SecretKind::DbKey` / `DEFAULT_ACCOUNT` 接进
// `infra::db::load_or_create_key`，所以这里**不再有整模块的 `allow(dead_code)`**
// （原来那条是 TC-101 留的 TODO，见 TASKBOARD「结案遗留」）。
// 少数还没接线的成员各自带定点 allow，并注明哪张卡接 —— 整模块豁免会把
// 「接了线却又没人用」也一起静默掉，定点 allow 不会。

use std::fmt;
// PathBuf 不在这里 import：它在下面只被 #[cfg(debug_assertions)] 的 dev_keychain_path()
// 用到，而 release 构建关掉 debug_assertions —— 那时这条 import 会变成未使用告警。
// 开发构建与测试构建都过，门禁（clippy 跑 dev profile）看不到它，
// 只有 release 构建才报（TC-600 实测）。开发钥匙串那处直接写全路径 std::path::PathBuf。
use std::path::Path;

use core_foundation::data::CFData;
use security_framework::base::Error as SfError;
use security_framework::item::{
    ItemAddOptions, ItemAddValue, ItemClass, ItemSearchOptions, ItemUpdateOptions, ItemUpdateValue,
    Limit, Location, SearchResult, update_item,
};
use security_framework::os::macos::keychain::{CreateOptions, SecKeychain};

use crate::error::{AppError, AppResult};

/// `errSecItemNotFound`。取自 security-framework-sys 2.17 `base.rs`，不是记忆里的数。
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

/// 所有条目 service 名的前缀，必须与 `tauri.conf.json` 的 `identifier` 同段
/// （`cn.payslipmailer.desktop`）。命名来自设计文档 ch.5；一致性由
/// `service_prefix_matches_bundle_identifier` 单测守，不靠人记（ADR-0009 第 3 条）。
pub const SERVICE_PREFIX: &str = "cn.payslipmailer.";

/// 一段秘密。用新类型而不是 `Vec<u8>`，是为了让「顺手 `{err:?}` 打出来」这条路
/// 在编译期就断掉：`Debug` 只报长度。
///
/// 不做 `Serialize`：一旦允许，它就有可能顺着 IPC 流到前端，而红线 5 要求
/// 前端拿不到工资数值与凭据。要传给谁，由调用方显式 `as_bytes()`。
pub struct Secret(Vec<u8>);

impl Secret {
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(bytes.into())
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    // 目前只有测试调用（TC-101 标准 2 的往返断言、TC-100 的「密钥必须 32 字节」断言）。
    // 生产路径要等 TC-103 首启向导校验密钥长度。
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    // 同上：`len() == 0` 的写法在测试里更直白，但 clippy 要求有 len 就得有 is_empty。
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // 只报字节数：DB key 是 32 字节、授权码是几十~几百字节，长度足够定位问题，
        // 内容进日志就是违反红线 2。
        write!(f, "Secret({} 字节·内容不打印)", self.0.len())
    }
}

/// 三类条目，对应设计文档 ch.5 的三行。一个 kind 一个 service，
/// 「同一 kind 的多个实例」靠 account 区分（SMTP 配置是 1─N，见 ADR-0009 第 3 条）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretKind {
    /// 打开 SQLCipher `data.db` 的 32 随机字节密钥。
    DbKey,
    /// SMTP 授权码 / App Password，account = `smtp_settings` 的行标识。
    #[allow(dead_code)] // 未接线：TC-402 起发信时写；词表先行，见 ADR-0009 第 3 条
    SmtpCredential,
    /// OAuth2 的 access + refresh token，account = `provider + 邮箱`。
    #[allow(dead_code)] // 未接线：TC-502（Gmail / M365）才用，那张卡还可能整个不做
    OauthToken,
}

impl SecretKind {
    /// 该 kind 的 service 名。审计时按它一把捞：
    /// `security find-generic-password -s cn.payslipmailer.db-key`
    pub fn service(&self) -> String {
        let leaf = match self {
            SecretKind::DbKey => "db-key",
            SecretKind::SmtpCredential => "smtp-credential",
            SecretKind::OauthToken => "oauth-token",
        };
        format!("{SERVICE_PREFIX}{leaf}")
    }

    /// 只有一个实例的 kind 用的固定 account。
    pub const DEFAULT_ACCOUNT: &'static str = "default";
}

/// 一个钥匙串的句柄。生产用 [`KeychainStore::login`]（登录钥匙串），
/// 开发构建用 [`KeychainStore::dev`]（专用钥匙串文件），
/// 测试用 [`KeychainStore::temporary`]（临时钥匙串）。
///
/// **三者共用下面同一套 put/get/delete 实现** —— 这是 ADR-0009 第 5 条的全部意义：
/// 如果测试走一条单独实现的假路径，测绿了也不代表生产能用。
pub struct KeychainStore {
    keychain: SecKeychain,
}

/// 开发钥匙串的口令。**只保护这台开发机上的开发密钥**，不是任何凭据，
/// 也不在 git 里（钥匙串文件本身在 `~/Library/Keychains/`，不在仓库内）。
#[cfg(debug_assertions)]
const DEV_KEYCHAIN_PASSWORD: &str = "xinyou-dev-keychain";

/// 开发钥匙串文件的位置。与 `scripts/setup-dev-keychain.sh` 里建的那个是同一个。
#[cfg(debug_assertions)]
fn dev_keychain_path() -> std::path::PathBuf {
    let home = std::env::var("HOME").expect("没有 HOME");
    std::path::PathBuf::from(home)
        .join("Library/Keychains")
        .join("xinyou-dev.keychain-db")
}

impl KeychainStore {
    /// 当前用户的登录钥匙串。生产路径。
    ///
    /// TC-102 起有生产调用方：`commands::auth` 的 `admin_status` / `unlock` 每次现构造一个
    /// （它只是 `SecKeychain::default()`，不跨调用持有）。
    pub fn login() -> AppResult<Self> {
        Ok(Self {
            keychain: SecKeychain::default().map_err(map_err)?,
        })
    }

    /// **开发构建**用的专用钥匙串；发布构建走 [`KeychainStore::login`]。
    ///
    /// 为什么要分两条路（2026-11-11 实测，TC-103 验收时撞上的）：
    /// macOS 给每个钥匙串条目配一份 ACL，「谁可以读」写在里面。默认 ACL 信任
    /// **创建它的那个程序** —— 对未签名程序来说身份是二进制的 cdhash，
    /// **每重编译一次就变**，于是每次启动都要弹「想要使用你储存在钥匙串中的机密信息」，
    /// 要人输登录钥匙串口令。开发时每张卡都要重编译，等于每张卡都被弹窗打断一次。
    ///
    /// 试过「给开发构建自签一份」：designated requirement 确实稳定了
    /// （按证书 + bundle id，不按哈希），但 macOS 评估自签根证书时仍不放行 ——
    /// Apple 锚定的 Developer ID 才行，那需要付费开发者计划。
    ///
    /// 所以开发构建改用一个**专用钥匙串文件**：口令固定（只保护开发机上这把
    /// 开发密钥），条目由 `scripts/setup-dev-keychain.sh` 以「允许任意程序」建好，
    /// 于是**永远不弹**。
    ///
    /// **注意这不是另写一套实现**：`put` / `get` / `delete` 与生产路径是同一段代码
    /// （ADR-0009 第 5 条），只有「用哪个钥匙串实例」不同。
    /// **发布构建完全不受影响**：仍然走登录钥匙串 + 默认 ACL，
    /// ADR-0002 记的「未签名分发升级会重置授权」照旧成立。
    #[cfg(debug_assertions)]
    pub fn dev() -> AppResult<Self> {
        let path = dev_keychain_path();
        let mut keychain = SecKeychain::open(&path).map_err(|e| {
            AppError::Io(format!(
                "打不开开发钥匙串 {}：{e}（跑一次 scripts/setup-dev-keychain.sh 即可）",
                path.display()
            ))
        })?;
        keychain
            .unlock(Some(DEV_KEYCHAIN_PASSWORD))
            .map_err(map_err)?;
        Ok(Self { keychain })
    }

    /// 运行时该用哪个钥匙串：开发构建用专用的，发布构建用登录钥匙串。
    pub fn for_runtime() -> AppResult<Self> {
        #[cfg(debug_assertions)]
        {
            Self::dev()
        }
        #[cfg(not(debug_assertions))]
        {
            Self::login()
        }
    }

    /// 在 `path` 新建一个独立钥匙串，用 `password` 保护。测试专用（ADR-0009 第 5 条）。
    ///
    /// `prompt_user(false)`：建钥匙串时不要弹「设置口令」的框，否则无 GUI 会话会挂。
    ///
    /// 非测试构建里必然无人调用（只有 `TempKeychain` 用它），但**不加 `#[cfg(test)]`**：
    /// 它是 ADR-0009 第 5 条「测试与生产共用同一套实现」的另一半，留在公开 API 上
    /// 才看得出这条设计还在不在。
    #[allow(dead_code)]
    pub fn temporary(path: &Path, password: &str) -> AppResult<Self> {
        let keychain = CreateOptions::new()
            .password(password)
            .prompt_user(false)
            .create(path)
            .map_err(map_err)?;
        Ok(Self { keychain })
    }

    /// 定位「本钥匙串里、这个 service+account、且**不同步 iCloud**」的那一条。
    ///
    /// 两个限定都不是可有可无：
    /// - `keychains` 限定到句柄所指的那一个，测试才不会读到登录钥匙串（标准 6）；
    /// - `cloud_sync(Some(false))` 是因为 iCloud 同步与非同步是**两个独立 store**，
    ///   同一个 service/account 能在两边各存一份；不写死等于把「读到哪一份」交给系统
    ///   （ADR-0009 第 4 条）。
    fn locate(&self, service: &str, account: &str) -> ItemSearchOptions {
        let mut opts = ItemSearchOptions::new();
        opts.class(ItemClass::generic_password())
            .service(service)
            .account(account)
            .keychains(std::slice::from_ref(&self.keychain))
            .cloud_sync(Some(false));
        opts
    }

    /// 写入或覆盖。
    ///
    /// 顺序是 **先 update，只有「找不到条目」才 add**。反过来（先 find 再决定）
    /// 或者「任何错误都退化成 add」都会踩 ADR-0009 第 2 条点名的坑：
    /// ACL 拒绝时（正是升级后的场景）写出**重复条目**，之后每次读命中哪一份都不确定。
    pub fn put(&self, kind: SecretKind, account: &str, secret: &Secret) -> AppResult<()> {
        let service = kind.service();
        let search = self.locate(&service, account);

        let mut update = ItemUpdateOptions::new();
        update.set_value(ItemUpdateValue::Data(CFData::from_buffer(
            secret.as_bytes(),
        )));

        match update_item(&search, &update) {
            Ok(()) => Ok(()),
            Err(err) if err.code() == ERR_SEC_ITEM_NOT_FOUND => {
                let mut add = ItemAddOptions::new(ItemAddValue::Data {
                    class: ItemClass::generic_password(),
                    data: CFData::from_buffer(secret.as_bytes()),
                });
                add.set_service(&service)
                    .set_account_name(account)
                    .set_location(Location::FileKeychain(self.keychain.clone()));
                add.add().map_err(map_err)
            }
            // ACL 拒绝、钥匙串被锁……一律原样上报，绝不 add 兜底。
            Err(err) => Err(map_err(err)),
        }
    }

    /// 读取。未命中返回 `KEYCHAIN_MISSING`（不是 panic、也不是空 `Secret`）。
    pub fn get(&self, kind: SecretKind, account: &str) -> AppResult<Secret> {
        let service = kind.service();
        let mut search = self.locate(&service, account);
        search.load_data(true).limit(Limit::Max(1));

        let results = search.search().map_err(map_err)?;
        // 已请求 load_data + Limit::Max(1)，正常最多一条。
        // 找不到时不能返回空 `Secret`：那会把「密钥丢了」这种真故障伪装成成功，
        // 一路到 SQLCipher 打开时才炸，报出来的还是「密码错误」。
        for result in results {
            if let SearchResult::Data(bytes) = result {
                return Ok(Secret::new(bytes));
            }
        }
        Err(AppError::KeychainMissing)
    }

    /// 删除。条目本来就不存在算成功（幂等）：调用方的意图是「这条不该再有」，
    /// 已经没有了就别报错。
    #[allow(dead_code)] // 未接线：TC-103「重新初始化」与 TC-104 恢复时删旧密钥；现在只有测试在用
    pub fn delete(&self, kind: SecretKind, account: &str) -> AppResult<()> {
        let service = kind.service();
        match self.locate(&service, account).delete() {
            Ok(()) => Ok(()),
            Err(err) if err.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(()),
            Err(err) => Err(map_err(err)),
        }
    }

    /// 命中条目的**数量**（不取数据）。专给「覆盖写不产生重复条目」这条验收标准用：
    /// 只读回值看不出重复，读回第一份还是第二份都可能是对的。
    #[cfg(test)]
    fn match_count(&self, kind: SecretKind, account: &str) -> AppResult<usize> {
        let service = kind.service();
        let mut search = self.locate(&service, account);
        search.load_attributes(true).limit(Limit::All);
        Ok(search.search().map_err(map_err)?.len())
    }
}

/// OSStatus → `AppError`。唯一一处区分「没有」与「不给」的地方（ADR-0009 第 6 条）。
fn map_err(err: SfError) -> AppError {
    if err.code() == ERR_SEC_ITEM_NOT_FOUND {
        AppError::KeychainMissing
    } else {
        // `Display` 是 Apple 的 `SecCopyErrorMessageString` 文案 + 码，进日志够排查；
        // 它不含秘密内容，所以放进 `Keychain(detail)` 不违反红线 2。
        AppError::Keychain(err.to_string())
    }
}

/// 整个测试进程共用一把「禁止用户交互」锁。
///
/// 为什么不在每个测试里各取各的：这把锁是**进程级**的，先跑完的测试 Drop 时会把
/// 开关恢复成「允许」，此刻正在跑的另一个测试就暴露在「可能弹框」下 ——
/// 典型的全局态并行竞态，表现为偶发挂死。取一次、永不释放，就没这个问题；
/// 测试进程本来也不需要弹框。
///
/// 提到模块级是因为 TC-100 的 `infra::db` 测试也要用它：加密库测试同样会读钥匙串，
/// 在 CI 上没有 GUI 会话，弹框就是挂到超时（ADR-0008）。
#[cfg(test)]
pub(crate) fn no_user_interaction() {
    use std::sync::OnceLock;
    static LOCK: OnceLock<security_framework::os::macos::keychain::KeychainUserInteractionLock> =
        OnceLock::new();
    LOCK.get_or_init(|| SecKeychain::disable_user_interaction().expect("关闭钥匙串用户交互失败"));
    // 不能写成 `assert_eq!(..., Ok(false))`：`base::Error` 没实现 `PartialEq`，
    // 整个 `Result` 就没法比较。查不到状态本身就是问题，直接 expect。
    assert!(
        !SecKeychain::user_interaction_allowed().expect("查询「是否允许用户交互」失败"),
        "锁没生效：测试可能在等一个没人点的授权框"
    );
}

/// 每个测试一个独立的临时钥匙串：并行跑互不干扰，也绝不碰登录钥匙串。
///
/// 与 `no_user_interaction` 一样提到模块级，供 `infra::db` 的测试复用 ——
/// 「测试不碰登录钥匙串」这件事（TC-101 标准 6）只有在**所有**钥匙串使用者都用它时
/// 才成立，各模块各造一套就会各漏一处。
#[cfg(test)]
pub(crate) struct TempKeychain {
    pub(crate) store: KeychainStore,
    path: std::path::PathBuf,
}

#[cfg(test)]
impl TempKeychain {
    pub(crate) fn new(label: &str) -> Self {
        // 一次性口令：临时钥匙串只活一个测试，不需要强度
        // 失败重试：SecKeychainCreate 偶发报「same name already exists」——
        // 名字里有纳秒，撞名不该发生，但实测在并行测试高负载时出现过一次
        // （门禁 2026-11-12 的 `looking_at_the_state_never_unlocks`）。
        // 换个名字再试，三次都失败才算真问题。
        let mut last_err = None;
        for attempt in 0..3 {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default();
            // 唯一文件名：进程号 + 纳秒 + 用例名（+ 重试序号），避免并行用例撞车
            let path = std::env::temp_dir().join(format!(
                "xinyou-keychain-test-{}-{}-{attempt}-{label}.keychain-db",
                std::process::id(),
                nanos
            ));
            // create() 遇到已存在的同名文件会报错，先清掉上次异常退出的残留
            let _ = std::fs::remove_file(&path);
            match KeychainStore::temporary(&path, "test-only-口令") {
                Ok(store) => return Self { store, path },
                Err(e) => {
                    last_err = Some(e);
                    // 稍等再试：让上一次 SecKeychainCreate 的内部注册先落定
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            }
        }
        panic!(
            "建临时钥匙串失败（重试 3 次）: {:?}",
            last_err.expect("重试过至少一次")
        );
    }

    /// 本用例专属的 account 名，保证即使在登录钥匙串里也不会和真实数据撞名。
    pub(crate) fn unique_account(&self, label: &str) -> String {
        format!(
            "xinyou-test-{label}-{}-{}",
            std::process::id(),
            self.path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
        )
    }
}

#[cfg(test)]
impl Drop for TempKeychain {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验收标准 2：二进制字节往返。
    ///
    /// DB key 是随机字节，必然含 `0x00` 与非 UTF-8 序列。任何「顺手 String 化」的
    /// 实现会写入成功、下次开机打开库时失败 —— 那是 P1 最难查的一种失败。
    #[test]
    fn binary_bytes_roundtrip_identically() {
        no_user_interaction();
        let kc = TempKeychain::new("roundtrip");
        let account = kc.unique_account("roundtrip");

        // 32 字节，故意含 0x00、0xFF 与一段非法 UTF-8（0xC3 单独出现）
        let key: Vec<u8> = vec![
            0x00, 0x01, 0x02, 0x00, 0x0a, 0xff, 0xfe, 0x00, 0x41, 0x00, 0xc3, 0x28, 0xa0, 0xa1,
            0xbf, 0xbd, 0x00, 0x7f, 0x80, 0x81, 0xfe, 0xff, 0x00, 0x2f, 0x5c, 0x22, 0x00, 0x00,
            0x10, 0x20, 0x7f, 0x00,
        ];
        assert_eq!(key.len(), 32, "DB key 必须是 32 字节（设计文档 ch.5）");

        kc.store
            .put(SecretKind::DbKey, &account, &Secret::new(key.clone()))
            .expect("写入失败");
        let back = kc.store.get(SecretKind::DbKey, &account).expect("读回失败");

        assert_eq!(back.as_bytes(), key.as_slice(), "逐字节必须相等");
        assert_eq!(back.len(), 32);
        assert!(!back.is_empty());
    }

    /// 验收标准 3：覆盖写不产生重复条目。
    ///
    /// 这是 ADR-0009 第 2 条那个坑的反证：`SecKeychain::set_generic_password` 之类
    /// 「find 失败就 add」的实现会在这里留下两条。
    #[test]
    fn overwrite_updates_in_place_without_duplicating() {
        no_user_interaction();
        let kc = TempKeychain::new("overwrite");
        let account = kc.unique_account("overwrite");

        kc.store
            .put(
                SecretKind::SmtpCredential,
                &account,
                &Secret::new("first-授权码".as_bytes()),
            )
            .expect("首次写入失败");
        kc.store
            .put(
                SecretKind::SmtpCredential,
                &account,
                &Secret::new("second-授权码".as_bytes()),
            )
            .expect("覆盖写失败");

        let back = kc
            .store
            .get(SecretKind::SmtpCredential, &account)
            .expect("读回失败");
        assert_eq!(
            back.as_bytes(),
            "second-授权码".as_bytes(),
            "必须读到后写入的那份"
        );

        // 关键断言：命中数 == 1。只比对读回值抓不出重复（读到哪一份都可能碰巧对）。
        assert_eq!(
            kc.store
                .match_count(SecretKind::SmtpCredential, &account)
                .expect("统计条目数失败"),
            1,
            "覆盖写写出了重复条目 —— 正是 ADR-0009 第 2 条要避免的"
        );
    }

    /// 验收标准 4：未命中是错误码，不是崩溃、不是空值。
    #[test]
    fn missing_entry_is_keychain_missing_not_panic() {
        no_user_interaction();
        let kc = TempKeychain::new("missing");

        let err = kc
            .store
            .get(SecretKind::DbKey, "这个 account 从来没写过")
            .expect_err("未命中必须报错");

        assert!(
            matches!(err, AppError::KeychainMissing),
            "实际是 {err:?}：必须与「钥匙串拒绝了我」区分开（ADR-0002 / ADR-0009 第 6 条）"
        );
        let msg = err.user_message();
        assert!(msg.contains("重新"), "缺：下一步引导 · {msg}");
    }

    /// 验收标准 5：秘密不进日志、不进 payload（红线 2 在秘密这条线上的机器化）。
    #[test]
    fn secret_never_appears_in_debug_or_error_output() {
        no_user_interaction();
        let kc = TempKeychain::new("leak");
        let account = kc.unique_account("leak");
        // 用 `"...".as_bytes()` 而不是 `b"..."`：字节字符串字面量只允许 ASCII，
        // 而真实授权码本来就是任意字节。
        const SENTINEL: &[u8] = "SENTINEL-勿外泄-授权码-9f3a".as_bytes();

        let secret = Secret::new(SENTINEL.to_vec());

        // ① Debug 只报字节数
        let dbg = format!("{secret:?}");
        assert!(!dbg.contains("SENTINEL"), "Debug 泄漏了内容：{dbg}");
        assert!(
            dbg.contains(&SENTINEL.len().to_string()),
            "Debug 应报字节数：{dbg}"
        );

        // ② 写入后，任何错误对象的 Display / JSON payload 都不该带出哨兵
        kc.store
            .put(SecretKind::SmtpCredential, &account, &secret)
            .expect("写入失败");
        let read = kc.store.get(SecretKind::SmtpCredential, &account).unwrap();
        let read_dbg = format!("{read:?}");
        assert!(
            !read_dbg.contains("SENTINEL"),
            "读回的 Debug 泄漏：{read_dbg}"
        );

        // ③ 失败路径同样：错误里既没有秘密也没有 account。
        // 必须换一个**没写过**的 account —— 上面那个已经存进去了，读它必然命中。
        let ghost = kc.unique_account("leak-未写入");
        let miss = kc
            .store
            .get(SecretKind::SmtpCredential, &ghost)
            .expect_err("这个 account 没写过，应未命中");
        let json = serde_json::to_string(&miss).expect("错误必须可序列化");
        assert!(!json.contains("SENTINEL"), "payload 泄漏哨兵：{json}");
        // account 存的是 smtp_settings 行标识 / `provider+邮箱`，设计文档要求邮箱也不进日志。
        // 注意不能断言「payload 不含『授权码』」：KEYCHAIN_MISSING 的引导文案里
        // 本来就有「更新发件授权码」，那是给 HR 看的下一步，不是泄漏。
        assert!(!json.contains(&ghost), "payload 泄漏 account：{json}");
    }

    /// 验收标准 6（进程内那半）：测试确实跑在临时钥匙串上，没写进登录钥匙串。
    ///
    /// 反向验证 —— 如果实现其实忽略了 `keychains(...)` 限定、写进了登录钥匙串，
    /// 上面几条照样全绿。所以必须正面查一次：同一个 service+account
    /// 在登录钥匙串里查不到。account 用进程唯一名，即使开发机真的初始化过也不撞。
    #[test]
    fn tests_run_against_a_temporary_keychain_not_the_login_one() {
        no_user_interaction();
        let kc = TempKeychain::new("isolation");
        let account = kc.unique_account("isolation");

        kc.store
            .put(SecretKind::DbKey, &account, &Secret::new(vec![7u8; 32]))
            .expect("写入临时钥匙串失败");

        // 临时钥匙串里在
        assert!(kc.store.get(SecretKind::DbKey, &account).is_ok());

        // 登录钥匙串里没有
        let login = KeychainStore::login().expect("打不开登录钥匙串");
        let leaked = login.get(SecretKind::DbKey, &account);
        assert!(
            matches!(leaked, Err(AppError::KeychainMissing)),
            "测试写进了登录钥匙串：{leaked:?}"
        );
    }

    /// 验收标准 8：命名不漂移。service 前缀必须与 `tauri.conf.json` 的 identifier 同段。
    ///
    /// 设计文档写的 `cn.payslipmailer.*` 与 bundle id 一旦分叉（改名、换域名），
    /// 钥匙串条目会变成「读不到的孤儿」—— 靠人记必然漏，所以读配置文件比对。
    #[test]
    fn service_prefix_matches_bundle_identifier() {
        let conf_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tauri.conf.json");
        let raw = std::fs::read_to_string(&conf_path)
            .unwrap_or_else(|e| panic!("读 {} 失败：{e}", conf_path.display()));
        let conf: serde_json::Value =
            serde_json::from_str(&raw).expect("tauri.conf.json 不是合法 JSON");
        let identifier = conf["identifier"]
            .as_str()
            .expect("tauri.conf.json 缺 identifier");

        // identifier 是 cn.payslipmailer.desktop，我们的前缀是 cn.payslipmailer.
        assert!(
            identifier.starts_with(SERVICE_PREFIX),
            "bundle id {identifier} 与前缀 {SERVICE_PREFIX} 不同段：\
             钥匙串条目会在改名后变成读不到的孤儿"
        );

        // 三类 service 名必须与设计文档 ch.5 一致，且都带前缀
        for (kind, expected) in [
            (SecretKind::DbKey, "cn.payslipmailer.db-key"),
            (
                SecretKind::SmtpCredential,
                "cn.payslipmailer.smtp-credential",
            ),
            (SecretKind::OauthToken, "cn.payslipmailer.oauth-token"),
        ] {
            assert_eq!(kind.service(), expected, "命名以设计文档 ch.5 为准");
        }
    }

    /// 删除的幂等性：真实场景是「删一条已经不存在的凭据」，不该报错。
    #[test]
    fn delete_is_idempotent() {
        no_user_interaction();
        let kc = TempKeychain::new("delete");
        let account = kc.unique_account("delete");

        // 从没写过 → 删 → 成功
        kc.store
            .delete(SecretKind::OauthToken, &account)
            .expect("删不存在的条目应视为成功");

        kc.store
            .put(SecretKind::OauthToken, &account, &Secret::new(b"token"))
            .expect("写入失败");
        kc.store
            .delete(SecretKind::OauthToken, &account)
            .expect("删除失败");
        assert!(
            matches!(
                kc.store.get(SecretKind::OauthToken, &account),
                Err(AppError::KeychainMissing)
            ),
            "删除后必须读不到"
        );
    }

    /// 不同 account 之间互不干扰（SMTP 配置 1─N 的前提，ADR-0009 第 3 条）。
    #[test]
    fn accounts_under_one_service_are_independent() {
        no_user_interaction();
        let kc = TempKeychain::new("multi");

        kc.store
            .put(
                SecretKind::SmtpCredential,
                "smtp-1",
                &Secret::new("码一".as_bytes()),
            )
            .expect("写 smtp-1 失败");
        kc.store
            .put(
                SecretKind::SmtpCredential,
                "smtp-2",
                &Secret::new("码二".as_bytes()),
            )
            .expect("写 smtp-2 失败");

        assert_eq!(
            kc.store
                .get(SecretKind::SmtpCredential, "smtp-1")
                .expect("读 smtp-1 失败")
                .as_bytes(),
            "码一".as_bytes()
        );
        assert_eq!(
            kc.store
                .get(SecretKind::SmtpCredential, "smtp-2")
                .expect("读 smtp-2 失败")
                .as_bytes(),
            "码二".as_bytes()
        );
        assert_eq!(
            kc.store
                .match_count(SecretKind::SmtpCredential, "smtp-1")
                .expect("统计失败"),
            1,
            "按 account 精确匹配，不该把 smtp-2 也算进来"
        );
    }
}
