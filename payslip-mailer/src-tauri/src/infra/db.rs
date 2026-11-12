//! SQLCipher 连接与版本化迁移（TC-100；选型与实测依据见 docs/adr/0010）。
//!
//! 设计文档 ch.5 的三条硬约束决定了下面每个 API 的形状：
//!
//! 1. **库文件里搜不到明文**：密钥只在 Keychain，且**读不到密钥时绝不新建空库** ——
//!    未签名应用覆盖安装会重置钥匙串 ACL（ADR-0002），那是升级后的**正常路径**，
//!    顺手生成新密钥会把老数据原地封存、给 HR 一个空应用还不报错（spec 标准 3）。
//! 2. **加密参数必须在 `PRAGMA key` 之后设**：先设会被 SQLCipher **静默忽略**，
//!    而设计文档指定的 `4096 / 256000 / aes-256-cbc` 正好就是 SQLCipher 4 的默认值，
//!    所以这个顺序写反时「全库加密」「错误密钥打不开」两条测试照样全绿（ADR-0010 第 3 条）。
//!    本模块因此**读回比对**参数，且测试必须用非默认值建库。
//! 3. **锁定即释放连接**：`Mutex<Option<Connection>>`，不是连接池 —— 池里至少常驻一条连接，
//!    密钥就常驻内存，做不到 ch.5 的「锁定 / 休眠 / 切后台即释放连接」（ADR-0010 第 5 条）。

use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags, params};
use security_framework::random::SecRandom;

use crate::error::{AppError, AppResult};
use crate::infra::keychain::{KeychainStore, Secret, SecretKind};
use crate::util::to_hex;

/// 设计文档 ch.5 指定的库文件名。TC-104 的备份就是整文件复制它，
/// 所以这里不能开 WAL：`-wal` / `-shm` 旁文件会让「复制 data.db 即备份」漏掉未合并的页。
// TC-102 起有生产调用方：`infra::admins::unlock` / `probe` 经 `DbState::open` 引用它。
pub const DB_FILE_NAME: &str = "data.db";

/// DB key 字节数（设计文档 ch.5）。raw key 形式，不做口令派生 ——
/// 因此 `kdf_iter` 对本库不参与解密，见 ADR-0010 第 4 条（带两个对照组实测）。
const KEY_BYTES: usize = 32;

/// 加密参数。取值来自设计文档 ch.5。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CipherParams {
    pub page_size: u32,
    pub kdf_iter: u32,
    /// ⚠️ 实测这行 pragma **在 key 前后设都不生效**，且读回恒为 `aes-256-cbc`
    /// （ADR-0010 第 3 条的探针 10：建库时设 128，改用 256 重开**照样读到**，
    /// 说明磁盘上本来就是默认的 256）。所以它**不进读回比对** ——
    /// 比对一个恒等于默认值的读回，只是给自己一个恒真的假安全感。
    /// 留着这行只为声明意图；若将来 SQLCipher 改了默认算法，它拦不住。
    pub cipher: &'static str,
}

/// 生产参数（设计文档 ch.5）。
// TC-102 起有生产调用方：`commands::auth` 每次解锁都把它放进 `UnlockEnv`。
// 测试故意用非默认值（见下），为的是「参数漂移必须被拒」那条实测不被默认值掩盖。
pub const CIPHER_PARAMS: CipherParams = CipherParams {
    page_size: 4096,
    kdf_iter: 256000,
    cipher: "aes-256-cbc",
};

/// 迁移 SQL 的**漂移探测器**，不是安全哈希：只需要「改过就发现」，不需要抗碰撞。
/// 同上，不引 `sha2`（它在 Cargo.lock 里只挂在 tauri-codegen 的编译期侧）。
fn fnv1a_64_hex(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// 迁移记录表的 `applied_at`：存 epoch 秒而不是 ISO 串。
/// TC-102 的失败锁定要做时间差运算，epoch 直接可比；且不必为此引时间 crate。
fn now_epoch_secs() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|_| "0".into())
}

/// 读一个 PRAGMA 的值，一律按字符串返回；读不到行返回 `None`。
///
/// 为什么要双类型兜底：SQLCipher 下 PRAGMA 的返回类型**不统一**（实测 `cipher_page_size`
/// 返回 Text，而 `user_version` 这类返回 Integer），rusqlite 的 `FromSql for String`
/// 就是 `ValueRef::as_str()`，只认 Text（`types/from_sql.rs` L184-189），
/// 按 String 读 Integer 会报 `InvalidType`。
///
/// 调用方**必须把 `None` 当错误**，不能当「没问题」：实测参数在 `PRAGMA key` 之前设时，
/// 这些 pragma 就是返回「无行」。
fn pragma_str(conn: &Connection, name: &str) -> Option<String> {
    let sql = format!("PRAGMA {name}");
    if let Ok(text) = conn.query_row(&sql, [], |row| row.get::<_, String>(0)) {
        return Some(text);
    }
    conn.query_row(&sql, [], |row| row.get::<_, i64>(0))
        .ok()
        .map(|n| n.to_string())
}

/// 设密钥与加密参数。**顺序是这段实现的全部要点**：key → 参数 → 读回。
fn apply_key_and_params(conn: &Connection, key: &[u8], params: &CipherParams) -> AppResult<()> {
    if key.len() != KEY_BYTES {
        // 不是 Database：这不是库的问题，是钥匙串里存了别的东西（或版本串了）。
        return Err(AppError::Internal(format!(
            "DB 密钥应为 {KEY_BYTES} 字节，实际 {} 字节",
            key.len()
        )));
    }
    let hex = to_hex(key);
    // raw key 必须整体当字符串：裸写 `PRAGMA key = x'…'` 实测报
    // `near "x'0001…'": syntax error`（ADR-0010 第 3 条）。
    conn.execute_batch(&format!("PRAGMA key = \"x'{hex}'\";"))
        .map_err(|e| AppError::Database(format!("设置数据库密钥失败：{e}")))?;

    // 参数必须在 key **之后**设。先设会被静默忽略：读回无行、页大小仍是默认 4096，
    // 而设计文档要的值恰好等于默认值，所以没有任何「加密了吗」的测试会因此变红。
    let sql = format!(
        "PRAGMA cipher_page_size = {}; PRAGMA kdf_iter = {}; PRAGMA cipher = '{}';",
        params.page_size, params.kdf_iter, params.cipher
    );
    conn.execute_batch(&sql)
        .map_err(|e| AppError::Database(format!("设置加密参数失败：{e}")))?;

    verify_params(conn, params)
}

/// 读回比对。只覆盖 `cipher_page_size` 与 `kdf_iter`；`cipher` 的读回恒为默认值，
/// 比对它没有信息量（见 [`CipherParams::cipher`]）。
fn verify_params(conn: &Connection, params: &CipherParams) -> AppResult<()> {
    let expected = [
        ("cipher_page_size", params.page_size.to_string()),
        ("kdf_iter", params.kdf_iter.to_string()),
    ];
    for (name, want) in expected {
        match pragma_str(conn, name).as_deref() {
            Some(got) if got == want => {}
            Some(got) => {
                return Err(AppError::Database(format!(
                    "加密参数读回不符：{name} 期望 {want}，实际 {got}"
                )));
            }
            None => {
                return Err(AppError::Database(format!(
                    "加密参数读不到：{name} 无返回行（通常是参数在 PRAGMA key 之前设，被静默忽略）"
                )));
            }
        }
    }
    Ok(())
}

/// 「能不能解密」探针：读页 1 的 schema cookie。
///
/// 为什么必须单独探一次：错误密钥下 `PRAGMA key` 与参数 pragma 都会**成功** ——
/// SQLCipher 要到真正读页时才报 `file is not a database`。不先探的话，
/// 错误密钥会表现为「迁移失败 → 只读」，而 spec 标准 4 要的是**硬失败**：
/// 打不开的库不是「只读的库」，当只读处理会让 HR 以为数据还在。
///
/// 报错文案不许假装知道原因：实测**参数漂移、错误密钥、真·损坏三者的报错完全相同**
/// （ADR-0010 第 4 条），所以这里同时提两种可能，由 `user_message()` 落到界面。
fn assert_decryptable(conn: &Connection) -> AppResult<()> {
    // `get_ref` 接受任意列类型：这里要的不是值，而是「这一行读得出来」这件事本身。
    conn.query_row("PRAGMA schema_version", [], |row| {
        row.get_ref(0).map(|_| ())
    })
    .map_err(|e| {
        AppError::Database(format!(
            "本机加密库读不出页头：{e}（密钥与库不匹配、加密参数漂移、文件损坏三者的报错相同）"
        ))
    })
}

/// 保持 `journal_mode = delete`（ADR-0010 第 5 条）：设计文档称 data.db 是「唯一持久数据」，
/// WAL 的旁文件会让 TC-104「复制 data.db 即备份」漏页。
///
/// **只断言不设**：实测默认就是 delete；主动设成 delete 反而会在库已是 WAL 时
/// 静默改模式，掩盖「别的卡里把 WAL 打开了」这件事。
/// 读不到模式时不报错（不同版本对 pragma 的返回行为不一致，见 [`pragma_str`]），
/// 严格断言交给 `db_journal_mode_stays_delete` 单测。
fn assert_journal_delete(conn: &Connection) -> AppResult<()> {
    match pragma_str(conn, "journal_mode").as_deref() {
        None | Some("delete") => Ok(()),
        Some(other) => Err(AppError::Database(format!(
            "journal_mode 必须是 delete（备份要能整文件复制），实际是 {other}"
        ))),
    }
}

/// 打开一个加密库：开文件 → 设 key → 设参数 → 读回比对 → 探解密 → 查 journal。
///
/// 失败时连接直接 drop（不会留下「半开」的连接），调用方的状态保持 `None`。
fn connect(db_path: &Path, key: &[u8], params: &CipherParams) -> AppResult<Connection> {
    // READ_WRITE | CREATE：首启要建库。不退化成只读打开 —— 那会把权限问题
    // 伪装成「能读不能写」，而 HR 看到的是一句莫名其妙的只读提示。
    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
    )
    .map_err(|e| AppError::Database(format!("打开库文件失败（{}）：{e}", db_path.display())))?;

    apply_key_and_params(&conn, key, params)?;
    assert_decryptable(&conn)?;
    assert_journal_delete(&conn)?;
    Ok(conn)
}

/// 取 DB key。**只有「钥匙串没有 + 库文件也不存在」的首启情形才生成新密钥**。
///
/// 关键在 `KeychainMissing` 那个守卫：库文件在、密钥没了（未签名升级重置 ACL，ADR-0002）
/// 时必须原样报错。若顺手生成新密钥，会得到一个空库盖在老数据上 ——
/// 工资没了，应用一声不响。spec 标准 3 就是堵这一条。
fn load_or_create_key(store: &KeychainStore, db_path: &Path) -> AppResult<Secret> {
    match store.get(SecretKind::DbKey, SecretKind::DEFAULT_ACCOUNT) {
        Ok(secret) => Ok(secret),
        Err(AppError::KeychainMissing) if !db_path.exists() => {
            let mut bytes = vec![0u8; KEY_BYTES];
            // CSPRNG 来自已经在依赖里的 security-framework（Apple 的 SecRandomCopyBytes），
            // 不必为此加 getrandom —— Cargo.lock 里它有两个 major 版本，解析歧义本身就是成本。
            SecRandom::default()
                .copy_bytes(&mut bytes)
                .map_err(|e| AppError::Internal(format!("生成 DB 密钥失败：{e}")))?;
            let secret = Secret::new(bytes);
            store.put(SecretKind::DbKey, SecretKind::DEFAULT_ACCOUNT, &secret)?;
            Ok(secret)
        }
        Err(e) => Err(e),
    }
}

/// 一条版本化迁移。SQL 放 `src-tauri/migrations/NNNN_name.sql`，用 `include_str!`
/// 编进二进制 —— 磁盘上不需要设计文档写的 `migrations/` 目录（ADR-0010 第 6 条：
/// 版本记录跟库文件走，备份才自解释）。
#[derive(Debug, Clone, Copy)]
pub struct Migration {
    pub version: u32,
    pub name: &'static str,
    pub sql: &'static str,
}

impl Migration {
    fn checksum(&self) -> String {
        fnv1a_64_hex(self.sql)
    }
}

/// 生产迁移表。追加规则：**版本只增不减、已应用的条目不许改 SQL**
/// （改了 checksum 对不上 → `MIGRATION_DRIFT`，TC-100 标准 7 钉的是「会失败」这件事，
/// 具体码在 TC-103 按 ADR-0012 第 4 条从 `DATABASE` 换成了专属码）。
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "0001_baseline",
        sql: include_str!("../../migrations/0001_baseline.sql"),
    },
    Migration {
        version: 2,
        name: "0002_admins",
        sql: include_str!("../../migrations/0002_admins.sql"),
    },
    Migration {
        version: 3,
        name: "0003_salary_fields",
        sql: include_str!("../../migrations/0003_salary_fields.sql"),
    },
    Migration {
        version: 4,
        name: "0004_members",
        sql: include_str!("../../migrations/0004_members.sql"),
    },
    Migration {
        version: 5,
        name: "0005_email_templates",
        sql: include_str!("../../migrations/0005_email_templates.sql"),
    },
    Migration {
        version: 6,
        name: "0006_salary_periods",
        sql: include_str!("../../migrations/0006_salary_periods.sql"),
    },
    Migration {
        version: 7,
        name: "0007_salary_records",
        sql: include_str!("../../migrations/0007_salary_records.sql"),
    },
    Migration {
        version: 8,
        name: "0008_send_jobs",
        sql: include_str!("../../migrations/0008_send_jobs.sql"),
    },
    Migration {
        version: 9,
        name: "0009_identity_verified",
        sql: include_str!("../../migrations/0009_identity_verified.sql"),
    },
    Migration {
        version: 10,
        name: "0010_smtp_settings",
        sql: include_str!("../../migrations/0010_smtp_settings.sql"),
    },
];

/// 迁移记录表。放在**加密库内**，所以「明文搜不到表名」那条测试（标准 2）
/// 顺带证明了迁移记录本身也是密文。
const MIGRATIONS_TABLE_DDL: &str = "CREATE TABLE IF NOT EXISTS schema_migrations (\
     version INTEGER PRIMARY KEY, \
     name TEXT NOT NULL, \
     checksum TEXT NOT NULL, \
     applied_at TEXT NOT NULL)";

/// 迁移失败：错误 + **本次运行**在失败前新应用的版本。
///
/// 设计文档 ch.5「失败则回滚并保持只读」——只读是**应用状态**，所以这里必须把
/// 足够信息交给调用方（`open_with`）去决定「只读继续」还是「硬失败」，
/// 而不是在库这一层自作主张。
#[derive(Debug)]
pub struct MigrationFailure {
    pub error: AppError,
    pub applied_before_failure: Vec<u32>,
}

/// 建迁移记录表并读出已应用的 (version, name, checksum)。
fn applied_migrations(conn: &Connection) -> AppResult<Vec<(u32, String, String)>> {
    conn.execute_batch(MIGRATIONS_TABLE_DDL)
        .map_err(|e| AppError::Database(format!("建迁移记录表失败：{e}")))?;

    let mut stmt = conn
        .prepare("SELECT version, name, checksum FROM schema_migrations ORDER BY version")
        .map_err(|e| AppError::Database(format!("读迁移记录失败：{e}")))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)? as u32,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|e| AppError::Database(format!("读迁移记录失败：{e}")))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| AppError::Database(format!("读迁移记录失败：{e}")))
}

/// 应用一条迁移：一条 = 一个事务，中途失败整条消失（spec 标准 6）。
///
/// 回滚的前提成立：SQLite 的 DDL 是事务性的（实测 `BEGIN; CREATE TABLE; INSERT; ROLLBACK;`
/// 之后 `sqlite_master` 里查不到那张表）。
/// 用 `transaction()` 而不是 `unchecked_transaction()`：前者取 `&mut self`，
/// **编译期**就挡住嵌套事务（rusqlite `transaction.rs` L436-439 明说 unchecked
/// 是把检查推到运行时）。失败时 `tx` 离开作用域即回滚。
fn apply_one(conn: &mut Connection, m: &Migration) -> AppResult<()> {
    let tx = conn
        .transaction()
        .map_err(|e| AppError::Database(format!("开始迁移事务失败：{e}")))?;

    tx.execute_batch(m.sql).map_err(|e| {
        AppError::Database(format!("迁移 v{}（{}）执行失败：{e}", m.version, m.name))
    })?;
    tx.execute(
        "INSERT INTO schema_migrations (version, name, checksum, applied_at) \
             VALUES (?1, ?2, ?3, ?4)",
        params![m.version, m.name, m.checksum(), now_epoch_secs()],
    )
    .map_err(|e| AppError::Database(format!("记录迁移 v{} 失败：{e}", m.version)))?;
    tx.commit()
        .map_err(|e| AppError::Database(format!("提交迁移 v{} 失败：{e}", m.version)))
}

/// 跑迁移：先校验库里记录的每一条，再按版本升序补跑缺的。
/// 返回**本次新应用**的版本（重复打开时是空 Vec —— spec 标准 5 的判据）。
pub fn migrate(
    conn: &mut Connection,
    migrations: &[Migration],
) -> Result<Vec<u32>, MigrationFailure> {
    let recorded = match applied_migrations(conn) {
        Ok(rows) => rows,
        Err(error) => {
            return Err(MigrationFailure {
                error,
                applied_before_failure: Vec::new(),
            });
        }
    };
    let highest = migrations.iter().map(|m| m.version).max().unwrap_or(0);

    // ① 库里记的每一条都必须能在程序里找到，且 SQL 没被改过。
    //   「库里记的是 v1、跑的是改过的 v1」这种漂移不会报错，只会让库结构与代码悄悄错开。
    for (version, name, checksum) in &recorded {
        let Some(want) = migrations.iter().find(|m| m.version == *version) else {
            // 专属码而不是 DATABASE：这里**确切知道**是第几版对不上（ADR-0012 第 4 条）。
            return Err(MigrationFailure {
                error: AppError::MigrationDrift {
                    version: *version,
                    detail: format!(
                        "库里有一条程序不认识的迁移（{name}），程序最高只到 v{highest}：\
                         库版本比程序新，请用较新的应用打开，不要手工改库"
                    ),
                },
                applied_before_failure: Vec::new(),
            });
        };
        if want.checksum() != *checksum {
            // 两个校验和都留在 detail 里（进日志可排查），使用者文案只有版本号。
            return Err(MigrationFailure {
                error: AppError::MigrationDrift {
                    version: *version,
                    detail: format!(
                        "迁移 v{}（{}）库里记录的 SQL 与程序内置的不一致：\
                         库里记录 {checksum}，程序内置 {}。\
                         已应用的迁移只能追加不能改，要改结构请新增一条迁移",
                        *version,
                        want.name,
                        want.checksum()
                    ),
                },
                applied_before_failure: Vec::new(),
            });
        }
    }

    // ② 补跑未应用的迁移
    let applied: Vec<u32> = recorded.iter().map(|(version, ..)| *version).collect();
    let mut pending: Vec<&Migration> = migrations
        .iter()
        .filter(|m| !applied.contains(&m.version))
        .collect();
    pending.sort_by_key(|m| m.version);

    let mut newly: Vec<u32> = Vec::new();
    for m in pending {
        if let Err(error) = apply_one(conn, m) {
            return Err(MigrationFailure {
                error,
                applied_before_failure: newly,
            });
        }
        newly.push(m.version);
    }
    Ok(newly)
}

/// 一次会话：一条连接 + 它此刻能不能写 + 它在哪个版本 + 只读的话是被什么卡住的。
struct Session {
    conn: Connection,
    read_only: bool,
    version: u32,
    /// 只读的原因若是**迁移漂移**，这里记下是哪一版（`None` = 不是漂移，或不是只读）。
    ///
    /// 只存版本号不存整个 `AppError`：使用者文案只需要「第几版」，
    /// 校验和那部分留在日志里（ADR-0012 第 4 条）。
    drift_version: Option<u32>,
}

/// 只读态的整句使用者文案：一句话回答「发生了什么 / 影响范围 / 下一步」。
///
/// 首句保持 TC-100 起就写下的那句（spec 标准 16 引用的也是它），漂移时补一句点名是哪一版。
/// 横幅与写拒绝共用这一个函数，所以两边**逐字相同** —— 这是 TC-100 结案遗留
/// 「文案与状态不符」的修法：以前写被拒返回 `DATABASE`，经 `user_message()` 变成
/// 「本机加密数据打不开」，而真实状态是「已解锁、能读、只是不能写」（ADR-0012 第 5 条）。
fn read_only_message(drift_version: Option<u32>) -> String {
    let cause = match drift_version {
        Some(version) => format!("原因：第 {version} 版迁移与本机数据记录的不一致。"),
        None => String::new(),
    };
    format!(
        "数据库处于只读状态（迁移未完成），本次修改未写入。{cause}\
         本机数据仍可正常查看；请不要手工修改数据文件，可从备份恢复或联系维护人员。"
    )
}

/// 解锁开库的结果。两种都算「开成功」（连接建立了），区别只在能不能写。
///
/// 同一份信息在**构造处**就进了日志（见 `open_with`）：界面只会显示「当前只能查看」，
/// 而「为什么只能查看」「这台机器跑到 v几」只有日志里有。字段本身留给测试解构断言
/// （TC-100 的「第二次打开不重放迁移」「只读态里 error 是什么」），生产代码不再读它们，
/// 所以逐个标 `allow(dead_code)` —— 哪天连测试也不读某个字段了，删掉那一行就会重新报警。
#[derive(Debug)]
pub enum OpenOutcome {
    /// 迁移跑到最新，可读写。`applied` 是本次新应用的迁移版本。
    #[allow(dead_code)] // 构造处已记日志；解构断言在测试里
    ReadWrite { applied: Vec<u32> },
    /// 迁移失败但库读得出来：设计文档 ch.5 的「保持只读」。
    ReadOnly {
        #[allow(dead_code)] // 同上：warn 日志里有同一份原因
        error: AppError,
        #[allow(dead_code)]
        applied_before_failure: Vec<u32>,
    },
}

/// 应用状态里的数据库句柄（设计文档 ch.5：锁定 / 休眠 / 切后台即释放连接）。
///
/// `Option` 就是「解锁了没有」：`None` 时连接不存在、密钥不在内存里。
/// 不是「还开着但标记为锁定」—— 那种写法做不到 ch.5，spec 标准 8 专门断言这点。
#[derive(Default)]
pub struct DbState {
    inner: Mutex<Option<Session>>,
}

impl DbState {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, Option<Session>> {
        // 中毒不当致命：release 构建 `panic = "abort"`，根本不会中毒；
        // debug 下若某次查询 panic，让后续操作各自报错也比把整个应用永久卡死好 ——
        // 「本次操作未完成」是错误文案的承诺，「应用从此不可用」不是。
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 解锁：取密钥 → 开连接 → 校验参数 → 跑迁移 → 放进状态。已有会话被替换。
    ///
    /// TC-102 的 `infra::admins::unlock` 是它的第一个生产调用方（先开库才读得到口令哈希，
    /// 见 ADR-0011 §5）；TC-103 的开机自动开库走同一条路。
    pub fn open(
        &self,
        store: &KeychainStore,
        data_dir: &Path,
        params: &CipherParams,
    ) -> AppResult<OpenOutcome> {
        self.open_with(store, data_dir, params, MIGRATIONS)
    }

    /// [`DbState::open`] 的实现，多一个迁移表参数：生产走 [`MIGRATIONS`]，
    /// 测试注入一条会中途失败的迁移，才能验 spec 标准 6 的**只读信号** ——
    /// 只单测 `migrate()` 的话，「只读是应用状态」这件事就没被覆盖。
    pub fn open_with(
        &self,
        store: &KeychainStore,
        data_dir: &Path,
        params: &CipherParams,
        migrations: &[Migration],
    ) -> AppResult<OpenOutcome> {
        // 数据目录自己建出来。真机首启就是这个情形：ADR-0012 第 1 条实测过
        // `~/Library/Application Support/cn.payslipmailer.desktop/` 原本**不存在**，
        // 而 SQLite 打不开不存在的目录里的文件 —— 少了这一步，首启的
        // `admin_status` 直接失败，使用者看到的是「没能读到本机状态」，
        // 而不是 S01 向导（TC-103 标准 1 就是这个场景）。
        //
        // 只建**目录**，不建库文件：`load_or_create_key` 的守卫靠
        // 「库文件在不在」判断，目录被建出来不影响它（`db_path.exists()` 仍是 false）。
        std::fs::create_dir_all(data_dir).map_err(|e| {
            AppError::Io(format!("无法创建本机数据目录 {}：{e}", data_dir.display()))
        })?;
        let db_path = data_dir.join(DB_FILE_NAME);
        let secret = load_or_create_key(store, &db_path)?;
        let mut conn = connect(&db_path, secret.as_bytes(), params)?;
        // `secret` 到此离开作用域：连接建好后我们不再持有那份密钥副本。
        // （不做 memset：Rust 的 `Vec<u8>` drop 只释放不擦除，要擦得引 zeroize。
        //  本卡的范围是「关闭后我们不再持有」，内存残留归威胁模型另议。）

        match migrate(&mut conn, migrations) {
            Ok(applied) => {
                // 成功分支不用回读记录表：migrate() 成功意味着程序列表里每一条
                // 都已记录（待跑的刚跑完，其余本来就在表里），而「库比程序新」
                // 已在前面拦掉，所以 max(程序列表) == max(记录表)。
                let version = migrations.iter().map(|m| m.version).max().unwrap_or(0);
                *self.lock() = Some(Session {
                    conn,
                    read_only: false,
                    version,
                    drift_version: None,
                });
                // 记的是迁移版本，不是工资数值（红线 2）。每次解锁一行，是为了让
                // 「这台机器的库停在 v几」在现场日志里查得到，不必停下来读库。
                tracing::info!(version, applied = ?applied, "加密库已解锁");
                Ok(OpenOutcome::ReadWrite { applied })
            }
            Err(failure) => {
                // 只读的原因若是**漂移**，把版本号留在会话里：横幅与写拒绝的文案要点名是哪一版
                // （ADR-0012 第 4 条 · spec 标准 17）。必须在 `failure.error` 被移走之前取。
                let drift_version = match &failure.error {
                    AppError::MigrationDrift { version, .. } => Some(*version),
                    _ => None,
                };
                // 版本 = 库里已记录的版本 ∪ 本轮新应用的版本。不能只拿
                // `applied_before_failure` 算：那是**本轮**新应用的列表，
                // v1 往往在上一轮就应用过了、本轮为空，而库实际停在 v1
                // （首轮跑出来就是 `version() == Some(0)` 的假象）。
                let mut versions: Vec<u32> = applied_migrations(&conn)?
                    .iter()
                    .map(|(v, ..)| *v)
                    .collect();
                versions.extend(failure.applied_before_failure.iter().copied());
                let version = versions.iter().max().copied().unwrap_or(0);
                *self.lock() = Some(Session {
                    conn,
                    read_only: true,
                    version,
                    drift_version,
                });
                // 只读态必须留痕：TC-103 的横幅只会说「当前只能查看」，不带原因；
                // 排查的人要的是这一行里的驱动报错与「停在哪个版本之前」。
                tracing::warn!(
                    version,
                    applied_before_failure = ?failure.applied_before_failure,
                    error = %failure.error,
                    "迁移未完成，加密库以只读模式打开：本次的修改会被拒绝"
                );
                Ok(OpenOutcome::ReadOnly {
                    error: failure.error,
                    applied_before_failure: failure.applied_before_failure,
                })
            }
        }
    }

    /// 锁定 / 退出：置 `None` 就是关闭 —— `Session` drop → `Connection` drop → `sqlite3_close`。
    pub fn close(&self) {
        *self.lock() = None;
    }

    /// 「解锁了没有」。TC-102 的 `admin_status` 用它回前端，空闲/唤醒锁定后它必须变 `false`。
    pub fn is_open(&self) -> bool {
        self.lock().is_some()
    }

    /// 只读态（迁移失败）。TC-102 的 `unlock` 用它决定「验过口令后是否把库留着」，
    /// TC-103 的常驻横幅用它决定要不要显示。
    pub fn is_read_only(&self) -> bool {
        self.lock().as_ref().is_some_and(|s| s.read_only)
    }

    /// 只读态的整句使用者文案；未解锁或可写时 `None`。
    ///
    /// TC-103 的横幅显示它，`write` 拒绝时也返回它（同一个 [`read_only_message`]），
    /// 所以「横幅说的」与「写被拒时看到的」逐字相同 —— spec 标准 16 钉的就是这个
    /// （ADR-0012 第 5 条：只读是整会话的持续状态，不是可关掉的一次事件）。
    pub fn read_only_message(&self) -> Option<String> {
        self.lock()
            .as_ref()
            .filter(|s| s.read_only)
            .map(|s| read_only_message(s.drift_version))
    }

    /// 当前库版本（TC-104 恢复时要「校验版本」）。未解锁返回 `None`。
    #[allow(dead_code)] // 未接线：TC-104 备份/恢复时读
    pub fn version(&self) -> Option<u32> {
        self.lock().as_ref().map(|s| s.version)
    }

    /// 读。未解锁 → `DATABASE`。
    ///
    /// 「未解锁」不当成一种错误呈现给使用者：TC-102 起，业务读都在 `unlock` / `probe`
    /// 之后发生，锁屏时前端要的是「有没有解锁」这个状态（`admin_status` 的 `unlocked`），
    /// 不是一个错误码。这里保留 `DATABASE` 只是兜底：真走到了说明调用顺序写错了。
    pub fn read<T>(&self, f: impl FnOnce(&Connection) -> AppResult<T>) -> AppResult<T> {
        let guard = self.lock();
        let session = guard
            .as_ref()
            .ok_or_else(|| AppError::Database("数据库未解锁，无法读取".into()))?;
        f(&session.conn)
    }

    /// 写。只读态（迁移失败）必须被挡在这里，而不是靠每个调用方自觉。
    ///
    /// 已知的一个「写不进去但不该报错」的调用方：TC-102 只读态下验口令失败要回写锁定计数，
    /// 这里会返回 `READ_ONLY`。调用方**只记日志不谎称落盘**（见 `infra/admins.rs`）。
    ///
    /// 只读用 `READ_ONLY` 而不是 `DATABASE`：后者经 `user_message()` 会变成
    /// 「本机加密数据打不开」，与真实状态「已解锁、能读、只是不能写」矛盾
    /// （TC-100 结案遗留 · ADR-0012 第 5 条）。文案与 [`DbState::read_only_message`]
    /// 同源，所以横幅与这里的提示必然一致（spec 标准 16）。
    pub fn write<T>(&self, f: impl FnOnce(&Connection) -> AppResult<T>) -> AppResult<T> {
        let guard = self.lock();
        let session = guard
            .as_ref()
            .ok_or_else(|| AppError::Database("数据库未解锁，无法写入".into()))?;
        if session.read_only {
            return Err(AppError::ReadOnly(read_only_message(session.drift_version)));
        }
        f(&session.conn)
    }

    /// **事务**里写。
    ///
    /// 为什么单独一个方法：`write` 给的是 `&Connection`，而 rusqlite 的
    /// `transaction()` 要 `&mut Connection`。给它一个 `&mut` 的入口，
    /// 调用方就不用自己 `unsafe` transmute（那会把「借用规则」变成「约定」）。
    ///
    /// 只读态的拦截与 [`DbState::write`] 完全一样 —— 事务不是绕过只读的门。
    pub fn write_tx<T>(
        &self,
        f: impl FnOnce(&rusqlite::Transaction) -> AppResult<T>,
    ) -> AppResult<T> {
        let mut guard = self.lock();
        let session = guard
            .as_mut()
            .ok_or_else(|| AppError::Database("数据库未解锁，无法写入".into()))?;
        if session.read_only {
            return Err(AppError::ReadOnly(read_only_message(session.drift_version)));
        }
        let tx = session
            .conn
            .transaction()
            .map_err(|e| AppError::Database(format!("开始事务失败：{e}")))?;
        let out = f(&tx)?;
        tx.commit()
            .map_err(|e| AppError::Database(format!("提交事务失败：{e}")))?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    // 测试基座复用 keychain 模块的临时钥匙串与「禁交互」锁：
    // 「测试绝不碰登录钥匙串、绝不在无 GUI 会话里挂」只有在所有钥匙串使用者
    // 共用同一套基座时才成立，各模块各造一套就会各漏一处（ADR-0008 / ADR-0009 第 5 条）。
    use crate::infra::keychain::{TempKeychain, no_user_interaction};

    /// 测试用参数：**故意用非默认页大小**。
    ///
    /// 用默认的 4096 建库，「参数在 key 之前设」这个错误顺序测不出来（ADR-0010 第 3 条），
    /// 因为没生效时文件也是 4096 页。1024 生效与否会在文件大小上现形。
    const TEST_PARAMS: CipherParams = CipherParams {
        page_size: 1024,
        kdf_iter: 256000,
        cipher: "aes-256-cbc",
    };

    /// 每个测试一个临时数据目录，Drop 时整个删掉（spec 标准 9）。
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        /// 目录名带 pid + 纳秒，与 keychain.rs 的临时钥匙串同一套命名，
        /// 于是 `ls $TMPDIR | grep xinyou-` 一眼能看出谁留了垃圾。
        fn new(label: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default();
            let path = std::env::temp_dir().join(format!(
                "xinyou-db-test-{}-{nanos}-{label}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).expect("建临时数据目录失败");
            Self { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    /// 测试道具：临时数据目录 + 临时钥匙串 + 一个 `DbState`。
    /// 绝不碰登录钥匙串，也绝不碰应用真实使用的数据目录
    /// （`app_data_dir()` 那个，口径见 ADR-0012 第 1 条；守卫测试是
    /// `db_tests_do_not_touch_the_real_data_dir`）。
    struct Fixture {
        dir: TempDir,
        kc: TempKeychain,
        state: DbState,
    }

    impl Fixture {
        fn new(label: &str) -> Self {
            no_user_interaction();
            Self {
                dir: TempDir::new(label),
                kc: TempKeychain::new(label),
                state: DbState::default(),
            }
        }

        fn db_file(&self) -> PathBuf {
            self.dir.path.join(DB_FILE_NAME)
        }

        fn open(&self) -> AppResult<OpenOutcome> {
            self.state
                .open(&self.kc.store, &self.dir.path, &TEST_PARAMS)
        }

        fn open_with(&self, migrations: &[Migration]) -> AppResult<OpenOutcome> {
            self.state
                .open_with(&self.kc.store, &self.dir.path, &TEST_PARAMS, migrations)
        }

        /// 往基线表里写一条哨兵。工资数值也走这条路，所以顺带验证「金额不落明文」。
        fn put_meta(&self, key: &str, value: &str) {
            self.state
                .write(|conn| {
                    conn.execute(
                        "INSERT INTO app_meta (key, value) VALUES (?1, ?2)",
                        params![key, value],
                    )
                    .map_err(|e| AppError::Database(format!("写 {key} 失败：{e}")))?;
                    Ok(())
                })
                .expect("写入失败");
        }

        fn get_meta(&self, key: &str) -> AppResult<String> {
            self.state.read(|conn| {
                conn.query_row(
                    "SELECT value FROM app_meta WHERE key = ?1",
                    params![key],
                    |row| row.get::<_, String>(0),
                )
                .map_err(|e| AppError::Database(format!("读 {key} 失败：{e}")))
            })
        }

        fn migration_rows(&self) -> i64 {
            self.state
                .read(|conn| {
                    conn.query_row("SELECT count(*) FROM schema_migrations", [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .map_err(|e| AppError::Database(e.to_string()))
                })
                .expect("数迁移记录失败")
        }
    }

    /// 首启：钥匙串里没有密钥且库不存在 → 生成 32 字节、写回钥匙串、建库。
    #[test]
    fn db_first_launch_generates_and_stores_the_db_key() {
        let fx = Fixture::new("first-launch");
        assert!(!fx.db_file().exists(), "开库前不该已经有库文件");

        let outcome = fx.open().expect("首启应能开库");
        assert!(matches!(outcome, OpenOutcome::ReadWrite { .. }));
        assert!(fx.db_file().exists(), "首启应建出库文件");

        // 密钥必须已经躺在钥匙串里，否则下次开机打不开
        let stored = fx
            .kc
            .store
            .get(SecretKind::DbKey, SecretKind::DEFAULT_ACCOUNT)
            .expect("首启必须把生成的密钥写进钥匙串");
        assert_eq!(
            stored.len(),
            KEY_BYTES,
            "DB key 必须是 32 字节（设计文档 ch.5）"
        );
    }

    /// 验收标准 3：删掉钥匙串条目后再打开**已有**库，必须 `KEYCHAIN_MISSING`，
    /// 绝不能生成新密钥、盖出一个空库。
    #[test]
    fn db_missing_keychain_entry_never_creates_an_empty_db() {
        let fx = Fixture::new("missing-key");
        fx.open().expect("首启开库失败");
        fx.put_meta("哨兵姓名", "欧阳傲天");
        let before = std::fs::metadata(fx.db_file()).expect("库文件应存在").len();
        fx.state.close();

        fx.kc
            .store
            .delete(SecretKind::DbKey, SecretKind::DEFAULT_ACCOUNT)
            .expect("删钥匙串条目失败");

        let err = fx.open().expect_err("密钥没了必须报错，不能给一个空库");
        assert!(
            matches!(err, AppError::KeychainMissing),
            "实际是 {err:?}：必须是 KEYCHAIN_MISSING，前端才给得出「重新初始化」的引导"
        );

        // 反向验证：如果实现是「读不到就生成新 key 并新建库」，
        // 上面那条断言可能照样过（取决于先查哪个），所以还要证明库没被动过。
        let after = std::fs::metadata(fx.db_file()).expect("库文件应还在").len();
        assert_eq!(before, after, "报错这条路不该改动库文件");
    }

    /// 验收标准 4：错误密钥打不开，且文案同时提「密钥不匹配」与「文件损坏」
    /// （实测三者的报错完全相同，不许假装知道原因）。
    #[test]
    fn db_wrong_key_is_rejected_and_message_names_both_causes() {
        let fx = Fixture::new("wrong-key");
        fx.open().expect("首启开库失败");
        fx.put_meta("哨兵姓名", "欧阳傲天");
        let original_key = fx
            .kc
            .store
            .get(SecretKind::DbKey, SecretKind::DEFAULT_ACCOUNT)
            .expect("读回首启密钥失败")
            .as_bytes()
            .to_vec();
        fx.state.close();

        // 换一把**不同**的 32 字节密钥，开同一个库文件
        assert_ne!(
            original_key,
            vec![0xA7u8; KEY_BYTES],
            "两把密钥必须真的不同"
        );
        fx.kc
            .store
            .put(
                SecretKind::DbKey,
                SecretKind::DEFAULT_ACCOUNT,
                &Secret::new(vec![0xA7u8; KEY_BYTES]),
            )
            .expect("覆盖密钥失败");

        let err = fx.open().expect_err("错误密钥必须报错");
        assert!(
            matches!(err, AppError::Database(_)),
            "实际是 {err:?}：错误密钥必须映射成 DATABASE"
        );
        let msg = err.user_message();
        assert!(
            msg.contains("密钥"),
            "文案必须提到「密钥不匹配」这一可能 · {msg}"
        );
        assert!(
            msg.contains("损坏"),
            "文案必须提到「文件损坏」这一可能 · {msg}"
        );
        assert!(
            !fx.state.is_open(),
            "打不开时状态里必须是 None，不能留一条半开连接"
        );

        // spec 标准 4 的后半句：错误密钥**不是得到一个空库**。
        // 换回正确密钥重开，老数据必须还在 —— 那次失败的尝试没有动过库文件。
        fx.kc
            .store
            .put(
                SecretKind::DbKey,
                SecretKind::DEFAULT_ACCOUNT,
                &Secret::new(original_key),
            )
            .expect("还原密钥失败");
        fx.open().expect("还原密钥后应能开库");
        assert_eq!(
            fx.get_meta("哨兵姓名").expect("读回失败"),
            "欧阳傲天",
            "错误密钥那次尝试不该损坏或替换掉库"
        );
    }

    /// 验收标准 2：全库加密，明文搜不到 —— 哨兵、金额、表名、列名、迁移名都搜不到，
    /// 连 SQLite 的文件头魔数都不该在（加密库没有明文头）。
    #[test]
    fn db_whole_file_is_encrypted_no_plaintext_anywhere() {
        let fx = Fixture::new("plaintext-scan");
        fx.open().expect("开库失败");
        fx.put_meta("sentinel_name", "欧阳傲天");
        fx.put_meta("sentinel_amount", "12345.67");
        fx.state.close(); // 关掉才保证页都落盘、journal 已合并

        let bytes = std::fs::read(fx.db_file()).expect("读库文件失败");
        assert!(
            bytes.len() > 1024,
            "库文件太小，不像真写了东西：{} 字节",
            bytes.len()
        );

        for needle in [
            "欧阳傲天",
            "12345.67",
            "app_meta",
            "sentinel_name",
            "schema_migrations",
            "0001_baseline",
            "SQLite format 3",
        ] {
            let found = bytes
                .windows(needle.len())
                .any(|window| window == needle.as_bytes());
            assert!(!found, "库文件里搜得到「{needle}」，说明没加密或头没加密");
        }
    }

    /// 验收标准 11：加密参数**真的生效**。
    ///
    /// 两条互补的证据，缺一不可：
    /// ① 读回比对（实现里 `verify_params` 已经在做，这里独立复核一遍，
    ///    免得「自己比对、自己放行」成为闭环）；
    /// ② 用**漂移的参数**重开同一个文件 —— 页大小若真落进了磁盘，
    ///    换一种分页几何就读不开，这正是探针 10 对照组的观察
    ///    （「只改页 1024→4096 → file is not a database」）。
    ///    不用文件大小当证据：4 页 × 1024 也是 4096 字节，
    ///    「≥4096 就说明参数没生效」这个推断不成立（首轮跑出来就是这个假阳性）。
    #[test]
    fn db_cipher_params_actually_take_effect() {
        let fx = Fixture::new("cipher-params");
        fx.open().expect("开库失败");
        fx.put_meta("k", "v");
        fx.state.close();

        // ① 读回：用同一把密钥另开一条裸连接，绕开 DbState 自己量一次
        let key = fx
            .kc
            .store
            .get(SecretKind::DbKey, SecretKind::DEFAULT_ACCOUNT)
            .expect("读密钥失败");
        let conn = connect(&fx.db_file(), key.as_bytes(), &TEST_PARAMS).expect("重开失败");
        assert_eq!(
            pragma_str(&conn, "cipher_page_size").as_deref(),
            Some("1024"),
            "cipher_page_size 没生效（多半是参数在 PRAGMA key 之前设，被静默忽略）"
        );
        assert_eq!(
            pragma_str(&conn, "kdf_iter").as_deref(),
            Some("256000"),
            "kdf_iter 读回不符"
        );
        drop(conn);

        // ② 磁盘：同一把密钥、只把页大小漂到默认 4096，必须打不开。
        //    能打开才说明页大小没写进文件（参数被静默忽略的另一种表现）。
        let mut drifted = TEST_PARAMS;
        drifted.page_size = 4096;
        match connect(&fx.db_file(), key.as_bytes(), &drifted) {
            Err(err) => assert!(
                err.to_string().contains("file is not a database"),
                "页大小漂移应报 `file is not a database`，实际：{err}"
            ),
            Ok(_) => panic!("1024 页建的库用 4096 页重开竟然成功，说明页大小没落到磁盘"),
        }
    }

    /// 验收标准 5：空库自动跑到最新；重复打开不重复执行。
    #[test]
    fn db_migrations_are_replayable_and_not_rerun_on_reopen() {
        let fx = Fixture::new("replay");
        let first = fx.open().expect("首次开库失败");
        let OpenOutcome::ReadWrite { applied } = first else {
            panic!("迁移应全部成功");
        };
        // 期望值**从生产迁移表推导**，不写死版本号：
        // 加一条迁移（TC-200 的 0003_salary_fields）就会让写死的 `vec![1, 2]` 失效，
        // 而这条测试的本意是「生产迁移表里的每一条都被应用了」——
        // 写成推导式，那条本意才不会因为加卡而误报。
        let expected: Vec<u32> = MIGRATIONS.iter().map(|m| m.version).collect();
        assert_eq!(applied, expected, "首次应把生产迁移表里的每一条都新应用");
        assert_eq!(fx.migration_rows(), MIGRATIONS.len() as i64);

        fx.put_meta("哨兵姓名", "欧阳傲天");
        fx.state.close();

        let second = fx.open().expect("重开失败");
        let OpenOutcome::ReadWrite { applied } = second else {
            panic!("重开也应可写");
        };
        assert!(
            applied.is_empty(),
            "重开不该再跑一遍迁移，实际新应用了 {applied:?}"
        );
        assert_eq!(
            fx.migration_rows(),
            MIGRATIONS.len() as i64,
            "schema_migrations 行数不该增长"
        );
        assert_eq!(
            fx.get_meta("哨兵姓名").expect("读回失败"),
            "欧阳傲天",
            "重开后数据必须还在（已建的表不该报「已存在」）"
        );
    }

    /// 验收标准 6：中途失败的迁移会回滚、不写记录、库停在上一版，
    /// 并且**应用侧进入只读**（只读是应用状态，必须在 DbState 上可见）。
    #[test]
    fn db_failed_migration_rolls_back_and_signals_read_only() {
        let fx = Fixture::new("failed-migration");
        let good = Migration {
            version: 1,
            name: "good",
            sql: "CREATE TABLE good_table (x INTEGER);",
        };
        // 前两条成功、第三条撞墙：execute_batch 停在失败处，事务必须把前两条一起回滚
        let bad = Migration {
            version: 2,
            name: "bad",
            sql: "CREATE TABLE gone_table (x INTEGER); \
                  INSERT INTO gone_table VALUES (1); \
                  INSERT INTO no_such_table VALUES (1);",
        };

        fx.open_with(&[good]).expect("第一条迁移应成功");
        assert_eq!(fx.migration_rows(), 1);

        let outcome = fx.open_with(&[good, bad]).expect("第二条失败应走只读分支");
        let OpenOutcome::ReadOnly {
            error,
            applied_before_failure,
        } = outcome
        else {
            panic!("失败的迁移必须给出只读信号，实际是 {outcome:?}");
        };
        assert!(matches!(error, AppError::Database(_)));
        assert!(
            applied_before_failure.is_empty(),
            "本次没有新应用任何版本，实际 {applied_before_failure:?}"
        );

        // ③ 只读信号落在状态上，且写被挡住、读还可以
        assert!(fx.state.is_read_only(), "状态必须是只读");
        let write_err = fx
            .state
            .write(|conn| {
                conn.execute("INSERT INTO good_table VALUES (1)", ())
                    .map_err(|e| AppError::Database(e.to_string()))?;
                Ok(())
            })
            .expect_err("只读态必须拒绝写入");
        // 必须是 `READ_ONLY`。TC-100 结案时这里钉的是 `DATABASE`，当时的注释写着
        // 「只读态的专属文案归 TC-103 的界面层」——TC-103 就是来兑现它的：
        // `DATABASE` 经 `user_message()` 会变成「本机加密数据打不开」，
        // 与本测试下一行断言的「只读不等于没解锁」正好矛盾（ADR-0012 第 5 条）。
        assert!(
            matches!(write_err, AppError::ReadOnly(_)),
            "写被挡住的错误必须是 READ_ONLY · {write_err:?}"
        );
        // 这条路的只读原因是「待跑的迁移执行失败」，不是 checksum 漂移，
        // 所以文案**不该**点名某一版——不知道原因就不要假装知道。
        let msg = write_err.user_message();
        assert!(msg.contains("本次修改未写入"), "缺：影响范围 · {msg}");
        assert!(!msg.contains("版"), "不该点名某一版迁移 · {msg}");
        assert_eq!(
            Some(msg),
            fx.state.read_only_message(),
            "写被拒的文案必须与横幅文案同源"
        );
        assert!(fx.state.is_open(), "只读不等于没解锁");

        // ① 库停在上一版 ② 失败那条不写进 schema_migrations
        assert_eq!(fx.migration_rows(), 1, "失败的迁移不该写进记录表");
        assert_eq!(fx.state.version(), Some(1), "库版本必须停在 v1");
        let gone = fx
            .state
            .read(|conn| {
                conn.query_row(
                    "SELECT count(*) FROM sqlite_master WHERE name = 'gone_table'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|e| AppError::Database(e.to_string()))
            })
            .expect("查 sqlite_master 失败");
        assert_eq!(gone, 0, "回滚没生效：gone_table 还留在库里");
    }

    /// 验收标准 7（TC-100）+ 标准 17（TC-103）：已应用的迁移被改动 → 不静默漂移，
    /// 专属码 `MIGRATION_DRIFT`、使用者文案点出是哪一版、两个校验和只进 `Display`/日志。
    ///
    /// 码从 `DATABASE` 换成 `MIGRATION_DRIFT` 是 TC-103 按 ADR-0012 第 4 条改的，
    /// 判据是「谁知道原因」：这里是我们自己的 checksum 比对报出来的，确切知道第几版对不上；
    /// 而驱动那句 `file is not a database` 是歧义的（ADR-0010 第 4 条），那种才只能给 `DATABASE`。
    #[test]
    fn db_editing_an_applied_migration_fails_with_a_checksum_message() {
        let fx = Fixture::new("checksum");
        let original = Migration {
            version: 1,
            name: "one",
            sql: "CREATE TABLE t (a TEXT);",
        };
        let edited = Migration {
            version: 1,
            name: "one",
            sql: "CREATE TABLE t (a TEXT, b TEXT);", // 改了已应用的 SQL
        };

        fx.open_with(&[original]).expect("首次应成功");
        fx.state.close();

        let outcome = fx.open_with(&[edited]).expect("校验失败应走只读分支");
        let OpenOutcome::ReadOnly { error, .. } = outcome else {
            panic!("改了已应用的迁移必须报错，实际是 {outcome:?}");
        };
        assert!(
            matches!(error, AppError::MigrationDrift { version: 1, .. }),
            "必须是第 1 版的迁移漂移，实际 {error:?}"
        );

        // 使用者文案：三要素 + 点名是哪一版，不带校验和
        let msg = error.user_message();
        assert!(msg.contains("迁移"), "缺：发生了什么 · {msg}");
        assert!(msg.contains("第 1 版"), "必须点出是哪一版 · {msg}");
        assert!(msg.contains("只能查看"), "缺：影响范围 · {msg}");
        assert!(
            msg.contains("备份") || msg.contains("维护人员"),
            "缺：下一步 · {msg}"
        );

        // 校验和：不进文案，但必须还能从 Display 进日志（spec 标准 17 的后半句）
        let display = error.to_string();
        assert!(
            display.contains(&original.checksum()),
            "库里记录的校验和没进日志 · {display}"
        );
        assert!(
            display.contains(&edited.checksum()),
            "程序内置的校验和没进日志 · {display}"
        );
        assert!(
            !msg.contains(&original.checksum()) && !msg.contains(&edited.checksum()),
            "校验和不得进使用者文案 · {msg}"
        );

        let json = serde_json::to_string(&error).expect("错误必须可序列化");
        assert!(
            json.starts_with("{\"code\":\"MIGRATION_DRIFT\""),
            "code 必须是 MIGRATION_DRIFT · {json}"
        );
    }

    /// 验收标准 16（TC-103）：只读态下**横幅文案与写被拒的文案逐字相同**，
    /// 且写被拒不再返回那句与「已解锁、能读」矛盾的「本机加密数据打不开」。
    #[test]
    fn db_read_only_write_rejection_carries_the_banner_message() {
        let fx = Fixture::new("readonly_copy");
        let original = Migration {
            version: 1,
            name: "one",
            sql: "CREATE TABLE t (a TEXT);",
        };
        let edited = Migration {
            version: 1,
            name: "one",
            sql: "CREATE TABLE t (a TEXT, b TEXT);",
        };
        fx.open_with(&[original]).expect("首次应成功");
        fx.state.close();
        fx.open_with(&[edited]).expect("校验失败应走只读分支");

        // 横幅的数据源：漂移时必须点出是哪一版（标准 17 的「界面文案里出现版本号」）
        let banner = fx
            .state
            .read_only_message()
            .expect("只读态必须给出横幅文案");
        assert!(
            banner.contains("第 1 版"),
            "横幅必须点出是哪一版迁移 · {banner}"
        );
        assert!(banner.contains("本次修改未写入"), "缺：影响范围 · {banner}");
        assert!(
            !banner.contains("打不开"),
            "不得复用 DATABASE 那句与「能读」矛盾的文案 · {banner}"
        );

        // 写被拒：专属码 + 与横幅逐字相同
        let err = fx.state.write(|_| Ok(())).expect_err("只读态的写必须被拒");
        let json = serde_json::to_string(&err).expect("错误必须可序列化");
        assert!(
            json.starts_with("{\"code\":\"READ_ONLY\""),
            "code 必须是 READ_ONLY · {json}"
        );
        assert_eq!(
            err.user_message(),
            banner,
            "横幅与写被拒必须说的是同一件事（同一份文案）"
        );

        // 反面对照：可写态既没有横幅，写也不被拒
        fx.state.close();
        fx.open_with(&[original]).expect("迁移表恢复正常后应可读写");
        assert!(fx.state.read_only_message().is_none(), "可写态不该有横幅");
        fx.state.write(|_| Ok(())).expect("可写态的写不该被拒");
    }

    /// 验收标准 8：关闭后状态里是 `None`（不是「还开着但标记为锁定」），
    /// 重新解锁能读到之前的数据。这条同时是 ADR-0010「不用池」的守护测试。
    #[test]
    fn db_close_releases_the_connection_and_reopen_reads_data() {
        let fx = Fixture::new("close");
        fx.open().expect("开库失败");
        assert!(fx.state.is_open());
        fx.put_meta("哨兵姓名", "欧阳傲天");

        fx.state.close();
        assert!(!fx.state.is_open(), "关闭后状态里必须是 None");
        assert!(fx.state.read(|_| Ok(())).is_err(), "关闭后不该还能读");

        fx.open().expect("重开失败");
        assert_eq!(
            fx.get_meta("哨兵姓名").expect("读回失败"),
            "欧阳傲天",
            "重新解锁必须能读到之前的数据"
        );
    }

    /// ADR-0010 第 5 条：不开 WAL。设计文档称 data.db 是「唯一持久数据」，
    /// WAL 的 `-wal` / `-shm` 旁文件会让 TC-104「复制 data.db 即备份」漏页。
    #[test]
    fn db_journal_mode_stays_delete() {
        let fx = Fixture::new("journal");
        fx.open().expect("开库失败");
        let mode = fx
            .state
            .read(|conn| Ok(pragma_str(conn, "journal_mode")))
            .expect("读 journal_mode 失败");
        assert_eq!(
            mode.as_deref(),
            Some("delete"),
            "必须保持 delete，不能开 WAL"
        );

        fx.put_meta("k", "v");
        let mut leftovers: Vec<String> = Vec::new();
        for entry in std::fs::read_dir(&fx.dir.path).expect("列目录失败") {
            let name = entry
                .expect("读目录项失败")
                .file_name()
                .to_string_lossy()
                .into_owned();
            if name.ends_with("-wal") || name.ends_with("-shm") {
                leftovers.push(name);
            }
        }
        assert!(leftovers.is_empty(), "出现了 WAL 旁文件：{leftovers:?}");
    }

    /// 库版本比程序新（老版本应用打开新库）必须报错，不能「少跑一条迁移就当没事」。
    #[test]
    fn db_newer_db_than_app_is_rejected() {
        let fx = Fixture::new("newer-db");
        fx.open().expect("开库失败"); // 用生产 MIGRATIONS 跑到最新（当前 v2）
        fx.state.close();

        // 程序只认到 v0（空列表），库里却记着 v1、v2
        let outcome = fx.open_with(&[]).expect("版本比程序新应走只读分支");
        let OpenOutcome::ReadOnly { error, .. } = outcome else {
            panic!("库版本比程序新必须报错");
        };
        let detail = error.to_string();
        assert!(
            detail.contains("比程序新"),
            "文案必须说清是版本超前 · {detail}"
        );
    }

    /// 验收标准 9：测试不污染真实数据目录。
    ///
    /// 局限要说清：这是**窗口式**检查（本测试开头与结尾各快照一次），
    /// 不是「所有测试都不写」的结构性证明。结构性保证是 `open` 把数据目录当**参数**收，
    /// 自己从不解析 `~/Library/Application Support/`。
    #[test]
    fn db_tests_do_not_touch_the_real_data_dir() {
        let real = dirs_hint();
        let before = listing(&real);

        // 跑一段真实路径：开库、写、关
        let fx = Fixture::new("no-pollution");
        fx.open().expect("开库失败");
        fx.put_meta("k", "v");
        fx.state.close();

        let after = listing(&real);
        assert_eq!(
            before,
            after,
            "测试往真实数据目录 {} 里写了东西：{:?}",
            real.display(),
            after.difference(&before).collect::<Vec<_>>()
        );
    }

    /// 验收标准 1 的前提：真机首启时数据目录**还不存在**，应用要能自己建出来。
    ///
    /// 这条是 TC-103 验收时才发现的生产 bug：原来 `open_with` 直接拿目录去连，
    /// 目录不存在时 SQLite 报「unable to open database file」，首启的
    /// `admin_status` 直接失败，使用者看到「没能读到本机状态」而不是 S01 向导。
    /// 测试基座一直自己建目录，所以几百条测试都没碰到它。
    #[test]
    fn db_creates_the_data_directory_on_first_open() {
        no_user_interaction();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        // 刻意**不**建这个目录：模拟全新机器
        let dir = std::env::temp_dir().join(format!(
            "xinyou-db-nodir-{}-{nanos}-first-open",
            std::process::id()
        ));
        assert!(!dir.exists(), "前置条件：这个目录现在不该存在");

        let kc = TempKeychain::new("first-open");
        let state = DbState::default();
        let outcome = state
            .open(&kc.store, &dir, &TEST_PARAMS)
            .expect("目录不存在时也要能开库（首启场景）");
        assert!(
            matches!(outcome, OpenOutcome::ReadWrite { .. }),
            "首启应该是可写：{outcome:?}"
        );
        assert!(dir.exists(), "数据目录应被创建出来");
        assert!(dir.join(DB_FILE_NAME).exists(), "库文件应被创建出来");
        state.close();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 建目录这件事**不能破坏**钥匙串守卫：库文件在、密钥没了，仍然要报
    /// `KEYCHAIN_MISSING`，绝不能因为「目录被建过」就放行或生成新密钥。
    #[test]
    fn db_creating_the_directory_does_not_break_the_keychain_guard() {
        let fx = Fixture::new("guard-after-mkdir");
        fx.open().expect("开库失败");
        fx.state.close();
        assert!(fx.db_file().exists(), "库文件在");

        // 删掉钥匙串里的密钥，留着库文件
        fx.kc
            .store
            .delete(SecretKind::DbKey, SecretKind::DEFAULT_ACCOUNT)
            .expect("删钥匙串条目失败");

        let state = DbState::default();
        let err = state
            .open(&fx.kc.store, &fx.dir.path, &TEST_PARAMS)
            .expect_err("库在而密钥没了，必须报 KEYCHAIN_MISSING");
        assert!(
            matches!(err, AppError::KeychainMissing),
            "必须是 KEYCHAIN_MISSING，实际 {err:?}"
        );
    }

    /// 应用真正会用的数据目录，**与 Tauri `app_data_dir()` 同一口径**（ADR-0012 第 1 条）。
    /// 只读不写。
    ///
    /// 测试里没有 `AppHandle`，所以按 Tauri 的算法自己拼一遍：
    /// `$HOME/Library/Application Support/<identifier>`（三段源码链条记在 ADR-0012 第 1 条：
    /// `dirs-6.0.0/src/mac.rs:7` + `tauri-2.12.5/src/path/desktop.rs:251-257`）。
    ///
    /// identifier **不硬编码**：从 `tauri.conf.json` 里读。以前这里写死的是设计文档里那个
    /// 旧目录名（字面见 ADR-0012 第 1 条，代码里不再重复），它根本不会被应用创建，于是这条「不污染真实目录」的检查
    /// 一直在看一个空目录、恒真（spec 标准 18 要治的就是这种第二个「真相源」）。
    fn dirs_hint() -> PathBuf {
        let home = std::env::var("HOME").expect("没有 HOME");
        let conf: serde_json::Value = serde_json::from_str(include_str!("../../tauri.conf.json"))
            .expect("tauri.conf.json 读不动");
        let identifier = conf["identifier"]
            .as_str()
            .expect("tauri.conf.json 里没有 identifier");
        PathBuf::from(home)
            .join("Library/Application Support")
            .join(identifier)
    }

    fn listing(dir: &Path) -> std::collections::BTreeSet<String> {
        match std::fs::read_dir(dir) {
            Ok(entries) => entries
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect(),
            Err(_) => std::collections::BTreeSet::new(), // 目录不存在也算一种状态
        }
    }
}
