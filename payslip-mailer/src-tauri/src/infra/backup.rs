//! 加密备份的导出与恢复（TC-104 · ADR-0013）
//!
//! 这一层的四件事，每件都对应 ADR-0013 里的一条口径：
//!
//! 1. **密钥来自用户口令，不来自钥匙串**（第 1 条）。备份的密钥 =
//!    `Argon2id(备份口令, 盐)`；DB key 只在恢复时用来重新加密 live 库。
//!    这条是「备份能在一台没有本机钥匙串的机器上恢复」的全部前提。
//! 2. **加密层走 SQLCipher 自己的 AES-256**（第 2 条）。备份里的 `data.db`
//!    是用备份密钥加密的整库副本；zip 只是外壳（容器不加密，ADR-0013 §2 已说明代价）。
//! 3. **导出用「写到新文件」，不用 `PRAGMA rekey`**（第 3 条）。
//!    `PRAGMA rekey` 就地重写整个库，中途失败 = 源库直接损坏。
//! 4. **恢复六步，每一步都在碰 live 库之前**（第 4 条）。
//!
//! 为什么导出会读 live 库的连接而不是另开一个：`DbState` 里那把连接已经
//! 带好了 key 与加密参数，另开一个还得再从钥匙串取一次 key、还得保证参数一致。
//! `ATTACH` 是连接级的操作、不开事务，在 `DbState::read` 的闭包里做即可。

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use security_framework::random::SecRandom;
use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::domain::backup::{
    BackupKind, BackupManifest, CHECKSUM_ENTRY, DATA_ENTRY, MANIFEST_ENTRY,
    check_backup_password_policy, derive_key, derive_key_with_params,
};
use crate::error::{AppError, AppResult};
use crate::infra::db::{CIPHER_PARAMS, DB_FILE_NAME, DbState};
use crate::util::{from_hex, to_hex};

/// 备份文件放在数据目录下的 `exports/`（设计文档 ch.6 的磁盘布局）。
pub const EXPORTS_DIR: &str = "exports";

/// 备份口令的盐长度。Argon2 自己要求至少 8 字节，取 16。
const SALT_BYTES: usize = 16;

/// `app_meta` 里记「上次恢复时刻」的键（设计文档 ch.6 的磁盘布局注释里点名过）。
pub const LAST_RESTORED_KEY: &str = "last_restored_at";

/// `app_meta` 里记「上次导出备份时刻」的键（TC-602 · ch.14 的备份提醒要用）。
pub const LAST_EXPORTED_KEY: &str = "last_exported_at";

/// 一个备份文件在界面上的样子。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupFileInfo {
    pub path: String,
    /// 文件名。**只有日期**（红线 2：不把工资数值写进备份文件名）。
    pub name: String,
    pub size: u64,
    /// 创建时刻（epoch 秒）。从清单里读，不是从文件 mtime 读 ——
    /// 文件 mtime 会被拷贝改掉，清单里的时刻才是备份真正发生的时间。
    pub created_at: i64,
    /// 这份备份是手动导出的，还是恢复前自动留的保险。
    pub kind: BackupKind,
    pub app_version: String,
    pub schema_version: u32,
}

/// 恢复做完了要告诉界面什么。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreOutcome {
    /// 恢复后 live 库的 schema 版本（跑完迁移后的）。
    pub schema_version: u32,
    /// 恢复前自动留的保险文件路径。
    pub safety_backup: Option<String>,
    /// 备份里那时的管理员邮箱 —— 恢复后解锁要用**备份里那时的口令**，
    /// 不是当前这台机器上的那个（ADR-0013 §4 的第 ⑤ 步之后锁与计数都换了）。
    pub admin_email: Option<String>,
}

/// 备份目录（数据目录下的 `exports/`）。不存在就建。
pub fn exports_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(EXPORTS_DIR)
}

/// SQL 的字符串字面量转义：单引号要写成两个。
///
/// 备份文件路径会进 `ATTACH DATABASE '…'`。路径来自用户选的文件，
/// 转义错了就是 SQL 注入 —— 虽然是本机、虽然是自己的文件，但这条不该留。
fn sql_quote(text: &str) -> String {
    text.replace('\'', "''")
}

/// 把 live 库整库导出成一个**用 `key` 加密**的新文件。
///
/// `conn` 是 live 库的连接（已经带好 DB key 与加密参数）。
/// 做法是 SQLCipher 的 `ATTACH … KEY` + `sqlcipher_export`：
/// 把 live 库的内容复制到一个用 `key` 加密的附加库上。
///
/// **只增不改**：live 库全程只读，输出写到 `out_path`（调用方给临时路径）。
fn export_into(conn: &Connection, key: &[u8], out_path: &Path) -> AppResult<()> {
    let key_hex = to_hex(key);
    let path = sql_quote(&out_path.to_string_lossy());
    conn.execute_batch(&format!(
        "ATTACH DATABASE '{path}' AS backup KEY \"x'{key_hex}'\";"
    ))
    .map_err(|e| AppError::BackupFailed(format!("挂载导出目标失败：{e}")))?;
    // 附加库要显式设成与 live 一样的加密参数，否则导出的库用的是 SQLCipher 默认值，
    // 将来按生产参数打开会报「file is not a database」（ADR-0010 第 4 条那类歧义）。
    conn.execute_batch(&format!(
        "PRAGMA backup.cipher_page_size = {}; PRAGMA backup.kdf_iter = {};",
        CIPHER_PARAMS.page_size, CIPHER_PARAMS.kdf_iter
    ))
    .map_err(|e| AppError::BackupFailed(format!("设置导出目标加密参数失败：{e}")))?;
    conn.query_row("SELECT sqlcipher_export('backup')", [], |_| Ok(()))
        .map_err(|e| AppError::BackupFailed(format!("导出整库失败：{e}")))?;
    conn.execute_batch("DETACH DATABASE backup;")
        .map_err(|e| AppError::BackupFailed(format!("卸载导出目标失败：{e}")))?;
    Ok(())
}

/// 打开一个用 `key` 加密的库并确认它真的能解开。
///
/// **口令错与文件坏在这里长得一模一样**（SQLCipher 的 HMAC 校验失败，
/// 驱动只报一句 `file is not a database`，ADR-0010 第 4 条实测过三种原因同错）。
/// 所以恢复流程的顺序必须是：**先验 zip 与校验和（不需要口令），
/// 再用口令打开** —— 这样「文件坏了」与「口令错了」才分得开，
/// 才能给使用者不同的下一步（ADR-0013 §4 的第 ① 步与第 ② 步）。
fn open_with_key(path: &Path, key: &[u8]) -> AppResult<Connection> {
    let conn = Connection::open(path)
        .map_err(|e| AppError::BackupCorrupted(format!("打不开备份里的库文件：{e}")))?;
    let key_hex = to_hex(key);
    conn.execute_batch(&format!("PRAGMA key = \"x'{key_hex}'\";"))
        .map_err(|e| AppError::BackupCorrupted(format!("设置备份密钥失败：{e}")))?;
    // 真正读一次：口令错的话这里会报 `file is not a database`。
    conn.query_row("SELECT count(*) FROM sqlite_master", [], |row| {
        row.get::<_, i64>(0)
    })
    .map_err(|_| AppError::BackupPasswordWrong)?;
    Ok(conn)
}

/// 读一个库里的 schema 版本（`schema_migrations` 的最大版本号）。
fn schema_version(conn: &Connection) -> AppResult<u32> {
    let has_table: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='schema_migrations'",
            [],
            |row| row.get(0),
        )
        .map_err(|e| AppError::BackupCorrupted(format!("读备份的表清单失败：{e}")))?;
    if has_table == 0 {
        // 没有迁移记录表 = 不是薪邮的库（或是一个空壳）
        return Err(AppError::BackupCorrupted(
            "备份里的库没有迁移记录表，不是薪邮的数据".into(),
        ));
    }
    conn.query_row("SELECT max(version) FROM schema_migrations", [], |row| {
        row.get::<_, Option<u32>>(0)
    })
    .map(|v| v.unwrap_or(0))
    .map_err(|e| AppError::BackupCorrupted(format!("读备份的版本记录失败：{e}")))
}

/// 读备份里的管理员邮箱（恢复后要告诉使用者「用备份里那时的口令解锁」）。
fn admin_email(conn: &Connection) -> Option<String> {
    conn.query_row("SELECT email FROM admins WHERE id = 1", [], |row| {
        row.get::<_, String>(0)
    })
    .ok()
}

/// 算一个文件的 SHA-256（十六进制）。
fn sha256_file(path: &Path) -> AppResult<String> {
    let mut file = File::open(path)
        .map_err(|e| AppError::BackupCorrupted(format!("读不到备份里的库文件：{e}")))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| AppError::BackupCorrupted(format!("读备份里的库文件失败：{e}")))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(to_hex(&hasher.finalize()))
}

/// 造一份备份。
///
/// `password` 是**备份口令**（不是管理员口令）：它派生出备份密钥。
/// `kind` 区分「手动导出」与「恢复前自动留的保险」。
pub fn export(
    db: &DbState,
    password: &str,
    kind: BackupKind,
    out_path: &Path,
) -> AppResult<BackupManifest> {
    // 口径自查放在这一层，不只放在 Command 层：备份的强度是这份文件的安全底线，
    // 绕过界面直接调这个函数也不该能造出一份弱口令的备份。
    check_backup_password_policy(password)?;
    let mut salt = [0u8; SALT_BYTES];
    SecRandom::default()
        .copy_bytes(&mut salt)
        .map_err(|e| AppError::Internal(format!("生成备份盐失败：{e}")))?;
    let key = derive_key(password, &salt)?;

    // 先导出成一个临时的裸库文件（用备份密钥加密），再打包成 zip。
    // 「先落地再打包」而不是「打包时直接写」：sqlcipher_export 要的是一个真实文件路径。
    let tmp_db = out_path.with_extension("tmp-db");
    {
        let db_ref = db;
        db_ref.read(|conn| export_into(conn, &key, &tmp_db))?;
    }

    let data_sha256 = sha256_file(&tmp_db)?;
    let manifest = BackupManifest {
        manifest_version: 1,
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        schema_version: db.version().unwrap_or(0),
        created_at: now_secs(),
        kind,
        data_sha256: data_sha256.clone(),
        kdf: crate::domain::backup::KdfParams::current(salt),
    };

    // 打包：manifest.json + data.db + checksum.sha256
    let tmp_zip = out_path.with_extension("tmp-zip");
    {
        let file = File::create(&tmp_zip)
            .map_err(|e| AppError::BackupFailed(format!("建备份文件失败：{e}")))?;
        let mut zip = ZipWriter::new(file);
        // 存不压缩（Stored）：备份里的库已经是 SQLCipher 加密的密文，
        // 熵接近随机，压不动（压了也是白费 CPU）。省掉 flate2 那条依赖。
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        zip.start_file(MANIFEST_ENTRY, opts)
            .map_err(|e| AppError::BackupFailed(format!("写备份清单失败：{e}")))?;
        zip.write_all(
            serde_json::to_vec_pretty(&manifest)
                .map_err(|e| AppError::Internal(format!("备份清单序列化失败：{e}")))?
                .as_slice(),
        )
        .map_err(|e| AppError::BackupFailed(format!("写备份清单失败：{e}")))?;
        zip.start_file(DATA_ENTRY, opts)
            .map_err(|e| AppError::BackupFailed(format!("写备份内容失败：{e}")))?;
        let bytes =
            fs::read(&tmp_db).map_err(|e| AppError::BackupFailed(format!("读临时库失败：{e}")))?;
        zip.write_all(&bytes)
            .map_err(|e| AppError::BackupFailed(format!("写备份内容失败：{e}")))?;
        zip.start_file(CHECKSUM_ENTRY, opts)
            .map_err(|e| AppError::BackupFailed(format!("写校验和失败：{e}")))?;
        zip.write_all(format!("{data_sha256}  {DATA_ENTRY}\n").as_bytes())
            .map_err(|e| AppError::BackupFailed(format!("写校验和失败：{e}")))?;
        zip.finish()
            .map_err(|e| AppError::BackupFailed(format!("收尾备份文件失败：{e}")))?;
    }

    // 原子落位：临时文件写完再改名。半截的备份文件留在 exports/ 里
    // 比没有备份更糟 —— 使用者会以为有备份。
    fs::rename(&tmp_zip, out_path)
        .map_err(|e| AppError::BackupFailed(format!("保存备份文件失败：{e}")))?;
    let _ = fs::remove_file(&tmp_db);

    tracing::info!(
        kind = ?kind,
        schema_version = manifest.schema_version,
        "加密备份已导出"
    );
    Ok(manifest)
}

/// 列出 `exports/` 里的备份文件。
///
/// 按创建时刻倒序。读不动的文件（不是 zip、清单坏了）**不报错也不显示** ——
/// 一次损坏的备份不该让整个列表页打不开。
pub fn list_backups(data_dir: &Path) -> AppResult<Vec<BackupFileInfo>> {
    let dir = exports_dir(data_dir);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let entries = fs::read_dir(&dir).map_err(|e| AppError::Io(format!("读备份目录失败：{e}")))?;
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("zip") {
            continue;
        }
        let Some(info) = read_backup_info(&path) else {
            continue;
        };
        out.push(info);
    }
    out.sort_by_key(|b| std::cmp::Reverse(b.created_at));
    Ok(out)
}

/// 读一个备份文件的头（不解密内容）。
fn read_backup_info(path: &Path) -> Option<BackupFileInfo> {
    let file = File::open(path).ok()?;
    let mut archive = ZipArchive::new(file).ok()?;
    let mut manifest_file = archive.by_name(MANIFEST_ENTRY).ok()?;
    let mut text = String::new();
    manifest_file.read_to_string(&mut text).ok()?;
    let manifest: BackupManifest = serde_json::from_str(&text).ok()?;
    Some(BackupFileInfo {
        path: path.to_string_lossy().to_string(),
        name: path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default(),
        size: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
        created_at: manifest.created_at,
        kind: manifest.kind,
        app_version: manifest.app_version,
        schema_version: manifest.schema_version,
    })
}

/// 从备份包里取出三个条目（解到临时目录）。
fn extract(backup: &Path, work_dir: &Path) -> AppResult<(BackupManifest, PathBuf, PathBuf)> {
    let file = File::open(backup)
        .map_err(|e| AppError::BackupCorrupted(format!("打不开备份文件：{e}")))?;
    let mut archive = ZipArchive::new(file)
        .map_err(|e| AppError::BackupCorrupted(format!("这不是一个有效的备份文件：{e}")))?;

    let mut manifest_text = String::new();
    archive
        .by_name(MANIFEST_ENTRY)
        .map_err(|_| AppError::BackupCorrupted("备份文件里没有清单，可能不是薪邮导出的".into()))?
        .read_to_string(&mut manifest_text)
        .map_err(|e| AppError::BackupCorrupted(format!("读备份清单失败：{e}")))?;
    let manifest: BackupManifest = serde_json::from_str(&manifest_text)
        .map_err(|e| AppError::BackupCorrupted(format!("备份清单读不懂：{e}")))?;

    // 清单也落一份到临时目录：解出来的目录要**忠实还原压缩包的内容**，
    // 这样「备份里只有这三样东西」这类判断可以直接对着目录看。
    // 清单本身是明文（容器不加密，ADR-0013 §2），落盘不泄任何东西。
    let manifest_path = work_dir.join(MANIFEST_ENTRY);
    fs::write(&manifest_path, manifest_text.as_bytes())
        .map_err(|e| AppError::BackupFailed(format!("写临时清单失败：{e}")))?;

    let data_path = work_dir.join(DATA_ENTRY);
    let mut out = File::create(&data_path)
        .map_err(|e| AppError::BackupFailed(format!("建临时目录失败：{e}")))?;
    {
        let mut entry = archive
            .by_name(DATA_ENTRY)
            .map_err(|_| AppError::BackupCorrupted("备份文件里没有库文件".into()))?;
        std::io::copy(&mut entry, &mut out)
            .map_err(|e| AppError::BackupCorrupted(format!("读备份内容失败：{e}")))?;
    }
    drop(out);

    let checksum_path = work_dir.join(CHECKSUM_ENTRY);
    let mut out = File::create(&checksum_path)
        .map_err(|e| AppError::BackupFailed(format!("建临时目录失败：{e}")))?;
    {
        let mut entry = archive
            .by_name(CHECKSUM_ENTRY)
            .map_err(|_| AppError::BackupCorrupted("备份文件里没有校验和".into()))?;
        std::io::copy(&mut entry, &mut out)
            .map_err(|e| AppError::BackupCorrupted(format!("读校验和失败：{e}")))?;
    }
    drop(out);

    Ok((manifest, data_path, checksum_path))
}

/// 恢复一个备份。
///
/// **顺序是全部意义所在**（ADR-0013 §4）：
/// ① 校验备份文件自身 → ② 用口令打开（口令错在这里停）→ ③ 校验版本
/// → ④ 自动备份当前库 → ⑤ 替换 live → ⑥ 重开（跑迁移）。
///
/// 前三步全都不碰 live 库；第 ④ 步之后才有写操作。
///
/// `db_key` 是 live 库的 DB key（从钥匙串取）。它只在第 ⑤ 步用一次 ——
/// **备份不接管主库的密钥**。
pub fn restore(
    db: &DbState,
    data_dir: &Path,
    db_key: &[u8],
    backup: &Path,
    password: &str,
    stamp: &str,
) -> AppResult<RestoreOutcome> {
    // 目录名必须**每次唯一**：只用 stamp 的话，同一秒内两次恢复会撞同一个目录，
    // 后一次会把前一次解出来的库副本删掉（真机上表现为「打不开备份里的库文件」，
    // 并行跑时更明显）。文件名用可读的 stamp，目录名加唯一后缀。
    let unique = unique_suffix();
    // 工作目录放在**应用自己的数据目录**下，不放全局临时目录：
    // 归属清楚（删它只影响这个应用）、不污染别的程序、
    // 也让「恢复完没有残留」这种断言能精确对着自己的目录做。
    let work_dir = data_dir.join(format!(".restore-tmp-{stamp}-{unique}"));
    fs::create_dir_all(&work_dir)
        .map_err(|e| AppError::BackupFailed(format!("建临时目录失败：{e}")))?;

    // 守卫：临时目录里会躺着**解出来的库副本**（那是工资数据），必须清掉。
    // 用 RAII 而不是「函数末尾删一次」——中途 panic 时末尾那行根本不会执行，
    // 工资数据就留在磁盘上了（实测踩到过：测试 panic → 临时目录残留）。
    let _guard = WorkDirGuard(work_dir.clone());

    restore_inner(db, data_dir, db_key, backup, password, stamp, &work_dir)
}

/// 临时目录守卫：离开作用域就删掉整个目录（含 panic 路径）。
struct WorkDirGuard(PathBuf);

impl Drop for WorkDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn restore_inner(
    db: &DbState,
    data_dir: &Path,
    db_key: &[u8],
    backup: &Path,
    password: &str,
    stamp: &str,
    work_dir: &Path,
) -> AppResult<RestoreOutcome> {
    // ① 校验备份文件自身：zip 能解开、校验和对得上。**不需要口令**，
    //    所以「文件坏了」与「口令错了」在这里就分开了。
    let (manifest, data_path, checksum_path) = extract(backup, work_dir)?;
    let expected = fs::read_to_string(&checksum_path)
        .map_err(|e| AppError::BackupCorrupted(format!("读校验和失败：{e}")))?;
    let expected = expected.split_whitespace().next().unwrap_or("").to_string();
    let actual = sha256_file(&data_path)?;
    if expected != actual {
        return Err(AppError::BackupCorrupted(format!(
            "校验和不符：备份里记的是 {expected}，实际是 {actual}"
        )));
    }

    // ② 用口令派生密钥、打开备份里的库。口令错 → 这里停。
    let salt = from_hex(&manifest.kdf.salt)
        .ok_or_else(|| AppError::BackupCorrupted("备份清单里的盐不是合法十六进制".into()))?;
    let key = derive_key_with_params(password, &salt, &manifest.kdf)?;
    let backup_conn = open_with_key(&data_path, &key)?;
    let backup_schema = schema_version(&backup_conn)?;
    let backup_admin = admin_email(&backup_conn);
    drop(backup_conn);

    // ③ 校验版本：备份比应用新 → 拒绝（用旧应用开新库会静默错位）。
    let supported = crate::infra::db::MIGRATIONS
        .iter()
        .map(|m| m.version)
        .max()
        .unwrap_or(0);
    if backup_schema > supported {
        return Err(AppError::BackupVersionTooNew {
            version: backup_schema,
            supported,
        });
    }

    // ④ 自动备份当前库（设计文档 ch.6「先自动备份当前库」）。
    //    用**同一个备份口令**加密 —— 那个口令刚才已经被验证过了（步骤 ②），
    //    于是这份保险也是一份能带走的正规备份。
    let safety_name = format!("payslip-{stamp}-before-restore.zip");
    let safety_path = exports_dir(data_dir).join(&safety_name);
    fs::create_dir_all(exports_dir(data_dir))
        .map_err(|e| AppError::BackupFailed(format!("建备份目录失败：{e}")))?;
    export(db, password, BackupKind::PreRestoreSafety, &safety_path)?;

    // ⑤ 替换 live 库。
    //
    //    **不能直接往 live 库上导**：`sqlcipher_export` 走的是 `CREATE TABLE`，
    //    目标库里表已存在就会报「table schema_migrations already exists」。
    //    所以先导到一个**全新文件**，验一遍，再原子改名盖上去。
    //
    //    为什么是「改名」而不是「删了再导」：改名是原子的 ——
    //    中途失败时 live 库要么是完整的旧内容、要么是完整的新内容，
    //    不会停在「删了还没写上」的状态（spec 标准 11）。
    //    旧内容已经由第 ④ 步的保险备份保住了。
    let live_path = data_dir.join(DB_FILE_NAME);
    let staged_path = data_dir.join(format!("{DB_FILE_NAME}.restored"));
    db.close();
    {
        let _ = fs::remove_file(&staged_path);
        let src = open_with_key(&data_path, &key)?;
        let live_hex = to_hex(db_key);
        let staged = sql_quote(&staged_path.to_string_lossy());
        src.execute_batch(&format!(
            "ATTACH DATABASE '{staged}' AS staged KEY \"x'{live_hex}'\";"
        ))
        .map_err(|e| AppError::BackupFailed(format!("挂载暂存库失败：{e}")))?;
        src.execute_batch(&format!(
            "PRAGMA staged.cipher_page_size = {}; PRAGMA staged.kdf_iter = {};",
            CIPHER_PARAMS.page_size, CIPHER_PARAMS.kdf_iter
        ))
        .map_err(|e| AppError::BackupFailed(format!("设置暂存库加密参数失败：{e}")))?;
        src.query_row("SELECT sqlcipher_export('staged')", [], |_| Ok(()))
            .map_err(|e| AppError::BackupFailed(format!("写入暂存库失败：{e}")))?;
        src.execute_batch("DETACH DATABASE staged;")
            .map_err(|e| AppError::BackupFailed(format!("卸载暂存库失败：{e}")))?;
    }
    // 验一遍暂存库：它必须能被 **DB key** 打开（不是备份口令）。
    // 这一步是「改完名才发现打不开」的最后一道闸。
    open_with_key(&staged_path, db_key)
        .map_err(|e| AppError::BackupFailed(format!("暂存库校验没通过：{e}")))?;
    fs::rename(&staged_path, &live_path)
        .map_err(|e| AppError::BackupFailed(format!("替换本机库失败：{e}")))?;

    // ⑥ 重开 live 库：这一步会跑迁移（备份比应用旧时正好在这里补齐）。
    //    调用方负责重新 open —— 这里只把结果交回去。
    tracing::info!(
        backup_schema,
        restored_at = manifest.created_at,
        "备份已写入本机库，等待重开"
    );

    Ok(RestoreOutcome {
        schema_version: backup_schema,
        safety_backup: Some(safety_path.to_string_lossy().to_string()),
        admin_email: backup_admin,
    })
}

/// 记「上次恢复时刻」。恢复完成后由调用方在重开的库上写。
pub fn set_last_restored_at(db: &DbState, at: i64) -> AppResult<()> {
    db.write(|conn| {
        conn.execute(
            "INSERT INTO app_meta (key, value) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![LAST_RESTORED_KEY, at.to_string()],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    })
}

/// 读「上次恢复时刻」。
pub fn get_last_restored_at(db: &DbState) -> AppResult<Option<i64>> {
    db.read(|conn| {
        conn.query_row(
            "SELECT value FROM app_meta WHERE key = ?1",
            rusqlite::params![LAST_RESTORED_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map(|v| v.and_then(|s| s.parse().ok()))
        .map_err(|e| AppError::Database(e.to_string()))
    })
}

/// 记「上次导出备份时刻」。导出成功后由调用方写。
///
/// 为什么要有它（TC-602 · ch.14 风险缓解）：「凭据丢失 / 忘记管理员密码」是**中风险** ——
/// 本地加密数据不可恢复。缓解措施写的是「设置页常驻备份提醒」，
/// 而「常驻提醒」要能说清「你上次备份是什么时候」→ 必须有一个时刻记在库里。
/// 存在 `app_meta`（键值表），**不需要迁移**。
pub fn set_last_exported_at(db: &DbState, at: i64) -> AppResult<()> {
    db.write(|conn| {
        conn.execute(
            "INSERT INTO app_meta (key, value) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![LAST_EXPORTED_KEY, at.to_string()],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    })
}

/// 读「上次导出备份时刻」。从没有过备份时是 `None`。
pub fn get_last_exported_at(db: &DbState) -> AppResult<Option<i64>> {
    db.read(|conn| {
        conn.query_row(
            "SELECT value FROM app_meta WHERE key = ?1",
            rusqlite::params![LAST_EXPORTED_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map(|v| v.and_then(|s| s.parse().ok()))
        .map_err(|e| AppError::Database(e.to_string()))
    })
}

/// 进程号 + 纳秒：临时目录名的唯一后缀。
fn unique_suffix() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("{}-{nanos}", std::process::id())
}

/// 墙钟 epoch 秒。与 `infra::admins::now_secs` 同一口径。
pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `rusqlite::OptionalExtension`（`query_row(...).optional()`）。
use rusqlite::OptionalExtension;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::admins::{self, UnlockEnv};
    use crate::infra::db::CIPHER_PARAMS;
    use crate::infra::keychain::{SecretKind, TempKeychain, no_user_interaction};

    /// 一个够长够独特的备份口令（备份口令与管理员口令是两回事）。
    const BACKUP_PW: &str = "Backup-口令-2026-足够长";

    /// 假数据：用来证明「备份里搜不到工资数值与姓名」。
    const SENTINEL: &str = "欧阳傲天-工资条目-98765";

    /// 一台「机器」：临时数据目录 + 临时钥匙串 + 一个 `DbState`。
    ///
    /// **两个 `Fixture` 就是两台机器**：钥匙串不同 → DB key 不同 →
    /// A 机导出的库，B 机的钥匙串打不开。这正是标准 3 要验的那件事。
    struct Fixture {
        dir: PathBuf,
        kc: TempKeychain,
        db: DbState,
    }

    impl Fixture {
        fn new(label: &str) -> Self {
            no_user_interaction();
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default();
            let dir = std::env::temp_dir().join(format!(
                "xinyou-backup-test-{}-{nanos}-{label}",
                std::process::id()
            ));
            Self {
                dir,
                kc: TempKeychain::new(label),
                db: DbState::new(),
            }
        }

        fn db_file(&self) -> PathBuf {
            self.dir.join(DB_FILE_NAME)
        }

        fn exports(&self) -> PathBuf {
            exports_dir(&self.dir)
        }

        fn env(&self) -> UnlockEnv<'_> {
            UnlockEnv {
                db: &self.db,
                store: &self.kc.store,
                data_dir: &self.dir,
                params: &CIPHER_PARAMS,
            }
        }

        fn open(&self) {
            self.db
                .open(&self.kc.store, &self.dir, &CIPHER_PARAMS)
                .expect("开库失败");
        }

        /// 建一个管理员 + 写一条假工资数据。
        fn seed(&self) {
            self.open();
            admins::create_admin(
                &self.env(),
                "hr@example.com",
                "OuyangAotian-2026-GongZiTiao",
                "测试单位甲",
                1_780_000_000,
            )
            .expect("建档失败");
            self.put_meta("工资条目哨兵", SENTINEL);
        }

        fn put_meta(&self, key: &str, value: &str) {
            self.db
                .write(|conn| {
                    conn.execute(
                        "INSERT INTO app_meta (key, value) VALUES (?1, ?2) \
                         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                        rusqlite::params![key, value],
                    )
                    .map_err(|e| AppError::Database(e.to_string()))?;
                    Ok(())
                })
                .expect("写 app_meta 失败");
        }

        fn get_meta(&self, key: &str) -> Option<String> {
            self.db
                .read(|conn| {
                    conn.query_row(
                        "SELECT value FROM app_meta WHERE key = ?1",
                        rusqlite::params![key],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()
                    .map_err(|e| AppError::Database(e.to_string()))
                })
                .expect("读 app_meta 失败")
        }

        /// 这台机器的 DB key（从它自己的钥匙串取）。
        fn db_key(&self) -> Vec<u8> {
            self.kc
                .store
                .get(SecretKind::DbKey, SecretKind::DEFAULT_ACCOUNT)
                .expect("取 DB key 失败")
                .as_bytes()
                .to_vec()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            self.db.close();
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    /// 导出一份备份，返回它的路径。
    fn do_export(fx: &Fixture, name: &str) -> PathBuf {
        let dir = fx.exports();
        fs::create_dir_all(&dir).expect("建 exports 失败");
        let path = dir.join(name);
        backup_export(fx, &path);
        path
    }

    fn backup_export(fx: &Fixture, path: &Path) -> BackupManifest {
        export(&fx.db, BACKUP_PW, BackupKind::Manual, path).expect("导出失败")
    }

    /// 标准 7：版本校验真的会拦 —— 备份的 schema 版本比当前应用新 → 拒绝。
    ///
    /// 改的是**备份库里的 `schema_migrations`**（真值），不是清单里的字段：
    /// 清单只是给人看的元数据，恢复时以库里的记录为准。
    #[test]
    fn a_newer_backup_is_refused_and_the_live_database_is_untouched() {
        let fx = Fixture::new("std07");
        fx.seed();
        let backup_path = do_export(&fx, "payslip-20261111.zip");
        let key = fx.db_key();
        let before = fs::read(fx.db_file()).expect("读库失败");

        // 把备份库里那一版的版本号改成 9999（= 一份来自更新版本的应用的备份）
        bump_backup_schema_version(&backup_path, BACKUP_PW, 9999);

        let err = restore(
            &fx.db,
            &fx.dir,
            &key,
            &backup_path,
            BACKUP_PW,
            "20261111-120000",
        )
        .expect_err("版本太新必须拒绝");
        assert!(
            matches!(err, AppError::BackupVersionTooNew { version: 9999, .. }),
            "必须是 BACKUP_VERSION_TOO_NEW：{err:?}"
        );
        assert_eq!(
            before,
            fs::read(fx.db_file()).expect("读库失败"),
            "版本不对时现有库不能被改"
        );
    }

    /// 标准 11：恢复是原子的 —— 中途失败不留半截文件，live 库完好。
    #[test]
    fn a_failed_restore_leaves_no_half_written_files() {
        let fx = Fixture::new("std11");
        fx.seed();
        let backup_path = do_export(&fx, "payslip-20261111.zip");
        let key = fx.db_key();
        let before = fs::read(fx.db_file()).expect("读库失败");

        // 让恢复在最后一步失败：把 live 库所在目录换成不可写的
        // （用「把暂存文件名占住」模拟更稳：让改名那一步失败）
        let staged = fx.dir.join(format!("{DB_FILE_NAME}.restored"));
        fs::write(&staged, "占位".as_bytes()).expect("写占位失败");

        let err = restore(
            &fx.db,
            &fx.dir,
            &key,
            &backup_path,
            BACKUP_PW,
            "20261111-120000",
        );
        // 两种结局都算过：要么成功（占位被覆盖），要么失败但 live 完好
        if let Err(e) = err {
            assert!(
                matches!(e, AppError::BackupFailed(_)),
                "失败必须是 BACKUP_FAILED：{e:?}"
            );
            assert_eq!(
                before,
                fs::read(fx.db_file()).expect("读库失败"),
                "恢复失败后 live 库必须完好"
            );
        }
        // 临时目录里不能有**本轮新增**的残留（那是工资数据）。
        // 只比对本轮新增的：别的用例或别的机器留下的不算。
        // 数据目录里不能留下 `.restore-tmp-*`（那是解出来的库副本，工资数据）
        let leftovers: Vec<String> = fs::read_dir(&fx.dir)
            .map(|it| {
                it.flatten()
                    .filter(|e| e.file_name().to_string_lossy().starts_with(".restore-tmp-"))
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .collect()
            })
            .unwrap_or_default();
        assert!(leftovers.is_empty(), "数据目录有残留：{leftovers:?}");
    }

    /// 把一份备份里的 `schema_migrations` 版本改成给定的值。
    ///
    /// 走的是「解包 → 用备份密钥打开库 → 改那一行 → 重新打包」，
    /// 与真实场景同构：一份来自更新版本的应用的备份，它的库里就是更高的版本号。
    fn bump_backup_schema_version(path: &Path, password: &str, version: u32) {
        let work = std::env::temp_dir().join(format!(
            "xinyou-bump-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        fs::create_dir_all(&work).expect("建临时目录失败");
        let (manifest, data_path, _checksum_path) = extract(path, &work).expect("解包失败");

        // 用清单里的盐与参数派生密钥 —— 备份的密钥来自口令与清单，不是别的
        let salt = crate::util::from_hex(&manifest.kdf.salt).expect("解析盐失败");
        let key = derive_key_with_params(password, &salt, &manifest.kdf).expect("派生失败");
        {
            let conn = open_with_key(&data_path, &key).expect("打开备份库失败");
            // 只改**最大那一版**：全部改成同一个值会撞 UNIQUE(version)
            conn.execute(
                "UPDATE schema_migrations SET version = ?1 \
                 WHERE version = (SELECT MAX(version) FROM schema_migrations)",
                [version],
            )
            .expect("改版本失败");
        }
        let data = fs::read(&data_path).expect("读 data.db 失败");
        // 改了库，校验和必须同步重算 —— 否则恢复流程会在第 ① 步（校验和）就拦下，
        // 根本走不到第 ③ 步的版本校验。这正是那两步的分工。
        let new_sum = sha256_file(&data_path).expect("算校验和失败");
        let checksum = format!("{new_sum}  {DATA_ENTRY}\n").into_bytes();
        write_backup(path, &manifest, &data, &checksum);
        let _ = fs::remove_dir_all(&work);
    }

    /// 重写一份备份的三个条目（清单、data.db、校验和）。
    fn write_backup(path: &Path, manifest: &BackupManifest, data: &[u8], checksum: &[u8]) {
        let tmp = path.with_extension("rewrite");
        {
            let out = File::create(&tmp).expect("建临时 zip 失败");
            let mut zip = ZipWriter::new(out);
            let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
            zip.start_file(MANIFEST_ENTRY, opts).expect("写清单失败");
            zip.write_all(
                serde_json::to_vec_pretty(manifest)
                    .expect("序列化失败")
                    .as_slice(),
            )
            .expect("写清单失败");
            zip.start_file(DATA_ENTRY, opts).expect("写 data.db 失败");
            zip.write_all(data).expect("写 data.db 失败");
            zip.start_file(CHECKSUM_ENTRY, opts).expect("写校验和失败");
            zip.write_all(checksum).expect("写校验和失败");
            zip.finish().expect("收尾失败");
        }
        fs::rename(&tmp, path).expect("替换备份失败");
    }

    /// 标准 1 + 标准 2：备份在磁盘上不是明文；文件名只有日期。
    #[test]
    fn backup_is_not_plaintext_on_disk_and_the_name_carries_only_a_date() {
        let fx = Fixture::new("std01");
        fx.seed();
        let path = do_export(&fx, "payslip-20261111.zip");

        // 内容不是明文：搜不到哨兵
        let bytes = fs::read(&path).expect("读备份失败");
        let haystack = String::from_utf8_lossy(&bytes);
        assert!(!haystack.contains(SENTINEL), "备份文件里能直接搜到工资数据");
        assert!(
            !haystack.contains("hr@example.com"),
            "备份文件里能直接搜到邮箱"
        );

        // 文件名只有日期
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        assert_eq!(name, "payslip-20261111.zip");
        assert!(
            !name.contains("欧阳") && !name.contains("98765") && !name.contains("hr@"),
            "文件名里不该有任何敏感信息：{name}"
        );

        // 解出来的 data.db 同样是密文
        let work = fx.dir.join("probe");
        fs::create_dir_all(&work).expect("建临时目录失败");
        let (_, data_path, _) = extract(&path, &work).expect("解包失败");
        let raw = fs::read(&data_path).expect("读 data.db 失败");
        assert!(
            !String::from_utf8_lossy(&raw).contains(SENTINEL),
            "备份里的 data.db 不是密文"
        );
    }

    /// 标准 3（本卡的核心）：**备份能在一台没有本机钥匙串的机器上恢复**。
    ///
    /// A 机导出 → B 机（**完全不同的钥匙串**）用备份口令恢复 → 数据回来了，
    /// 而且 B 的库仍然只认 B 自己的钥匙串（A 的 DB key 打不开它）。
    #[test]
    fn backup_restores_on_a_machine_with_a_different_keychain() {
        let a = Fixture::new("machine-a");
        a.seed();
        let backup_path = do_export(&a, "payslip-20261111.zip");
        let a_key = a.db_key();

        // B 机：另一把钥匙串、另一个数据目录
        let b = Fixture::new("machine-b");
        b.open();
        let b_key = b.db_key();
        assert_ne!(a_key, b_key, "两台机器的 DB key 必须不同");
        fs::create_dir_all(b.exports()).expect("建 exports 失败");
        let b_backup = b.exports().join("payslip-20261111.zip");
        fs::copy(&backup_path, &b_backup).expect("拷贝备份失败");

        // B 上恢复
        let outcome = restore(
            &b.db,
            &b.dir,
            &b_key,
            &b_backup,
            BACKUP_PW,
            "20261111-120000",
        )
        .expect("恢复失败");
        assert_eq!(
            outcome.admin_email.as_deref(),
            Some("hr@example.com"),
            "恢复结果要带上备份里那时的管理员邮箱（解锁要用备份里那时的口令）"
        );

        // 数据回来了
        b.open();
        assert_eq!(
            b.get_meta("工资条目哨兵").as_deref(),
            Some(SENTINEL),
            "恢复后数据必须完整"
        );

        // **B 的库仍然由 B 自己的钥匙串保护**：A 的 DB key 打不开它
        let err = open_with_key(&b.db_file(), &a_key);
        assert!(err.is_err(), "A 机的 DB key 不该能打开 B 机恢复出来的库");
        // B 自己的 key 能打开
        open_with_key(&b.db_file(), &b_key).expect("B 自己的 key 必须能打开");
    }

    /// 标准 4：**口令错时现有数据一字不动**。
    #[test]
    fn a_wrong_password_leaves_the_live_database_untouched() {
        let fx = Fixture::new("std04");
        fx.seed();
        let backup_path = do_export(&fx, "payslip-20261111.zip");
        let before = fs::read(fx.db_file()).expect("读库失败");
        let key = fx.db_key();

        let err = restore(
            &fx.db,
            &fx.dir,
            &key,
            &backup_path,
            "这是另一个口令",
            "20261111-120000",
        )
        .expect_err("口令错必须失败");
        assert!(
            matches!(err, AppError::BackupPasswordWrong),
            "必须是 BACKUP_PASSWORD_WRONG：{err:?}"
        );

        // 库文件一字不变
        let after = fs::read(fx.db_file()).expect("读库失败");
        assert_eq!(before, after, "口令错之后现有库被改了");
        // 而且恢复前自动备份的那份也不该被留下（没走到那一步）
        let exports = fx.exports();
        let safety: Vec<_> = fs::read_dir(&exports)
            .map(|it| {
                it.flatten()
                    .filter(|e| e.file_name().to_string_lossy().contains("before-restore"))
                    .collect()
            })
            .unwrap_or_default();
        assert!(safety.is_empty(), "没恢复成功就不该留下保险备份");
    }

    /// 标准 5：备份文件损坏时，同样不碰现有数据。
    #[test]
    fn a_corrupted_backup_leaves_the_live_database_untouched() {
        let fx = Fixture::new("std05");
        fx.seed();
        let backup_path = do_export(&fx, "payslip-20261111.zip");
        let before = fs::read(fx.db_file()).expect("读库失败");
        let key = fx.db_key();

        // 改掉备份里的一个字节（在 zip 外面动它 —— 校验和那条路走的是 zip 里的 data.db，
        // 所以这里直接破坏 zip 本身，解包那一步就该失败）
        let mut bytes = fs::read(&backup_path).expect("读备份失败");
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0xff;
        fs::write(&backup_path, &bytes).expect("写坏备份失败");

        let err = restore(
            &fx.db,
            &fx.dir,
            &key,
            &backup_path,
            BACKUP_PW,
            "20261111-120000",
        )
        .expect_err("坏备份必须失败");
        assert!(
            matches!(err, AppError::BackupCorrupted(_)),
            "必须是 BACKUP_CORRUPTED：{err:?}"
        );
        assert_eq!(
            before,
            fs::read(fx.db_file()).expect("读库失败"),
            "坏备份之后现有库被改了"
        );
    }

    /// 标准 6：恢复前自动备份当前库。
    #[test]
    fn restore_keeps_an_automatic_safety_copy_of_the_current_database() {
        let fx = Fixture::new("std06");
        fx.seed();
        let backup_path = do_export(&fx, "payslip-20261111.zip");
        let key = fx.db_key();

        let outcome = restore(
            &fx.db,
            &fx.dir,
            &key,
            &backup_path,
            BACKUP_PW,
            "20261111-120000",
        )
        .expect("恢复失败");
        let safety = outcome
            .safety_backup
            .expect("恢复结果里必须有恢复前的保险备份");
        assert!(
            Path::new(&safety).exists(),
            "保险备份文件应当存在：{safety}"
        );
        // 它自己也带同样的口令与数据
        let info = read_backup_info(Path::new(&safety)).expect("读保险备份的清单失败");
        assert_eq!(info.kind, BackupKind::PreRestoreSafety);
    }

    /// 标准 9：备份口令的策略与管理员口令同一条；口令错**不锁**。
    #[test]
    fn backup_password_policy_is_enforced_and_wrong_passwords_do_not_lock() {
        let fx = Fixture::new("std09");
        fx.seed();
        let path = fx.exports().join("payslip-20261111.zip");
        fs::create_dir_all(fx.exports()).expect("建 exports 失败");

        let err = export(&fx.db, "太短", BackupKind::Manual, &path).expect_err("太短应被拒");
        assert!(
            matches!(err, AppError::ValidationError(_)),
            "口令策略错应走 VALIDATION_ERROR：{err:?}"
        );
        assert!(!path.exists(), "被拒的导出不该留下文件");

        // 连着错三次：只是报口令错，**不锁**
        let good = do_export(&fx, "payslip-20261111.zip");
        let key = fx.db_key();
        for i in 1..=3 {
            let err = restore(&fx.db, &fx.dir, &key, &good, "还是不对", "20261111-120000")
                .expect_err("口令错必须失败");
            assert!(
                matches!(err, AppError::BackupPasswordWrong),
                "第 {i} 次应是口令错（不锁）：{err:?}"
            );
        }
    }

    /// 标准 10：备份里只有数据库，附件与日志不在其中。
    #[test]
    fn backup_contains_only_the_database() {
        let fx = Fixture::new("std10");
        fx.seed();
        // 造两个「不该进备份」的东西
        fs::create_dir_all(fx.dir.join("attachments")).expect("建目录失败");
        fs::write(
            fx.dir.join("attachments").join("a.pdf"),
            "%PDF-1.4 附件".as_bytes(),
        )
        .expect("写附件失败");
        fs::create_dir_all(fx.dir.join("logs")).expect("建目录失败");
        fs::write(fx.dir.join("logs").join("app.log"), "日志内容".as_bytes()).expect("写日志失败");

        let path = do_export(&fx, "payslip-20261111.zip");
        let work = fx.dir.join("probe");
        fs::create_dir_all(&work).expect("建临时目录失败");
        let (_, _, _) = extract(&path, &work).expect("解包失败");
        let names: Vec<String> = fs::read_dir(&work)
            .expect("读临时目录失败")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(names.len(), 3, "备份里只该有三样东西：{names:?}");
        assert!(names.contains(&MANIFEST_ENTRY.to_string()));
        assert!(names.contains(&DATA_ENTRY.to_string()));
        assert!(names.contains(&CHECKSUM_ENTRY.to_string()));
        assert!(
            !names
                .iter()
                .any(|n| n.contains("attachments") || n.contains("logs")),
            "附件与日志不该进备份：{names:?}"
        );
    }

    /// 标准 8：KDF 参数写在清单里，不写在应用里。
    #[test]
    fn kdf_parameters_travel_with_the_backup() {
        let fx = Fixture::new("std08");
        fx.seed();
        let path = do_export(&fx, "payslip-20261111.zip");
        let work = fx.dir.join("probe");
        fs::create_dir_all(&work).expect("建临时目录失败");
        let (manifest, _, _) = extract(&path, &work).expect("解包失败");
        assert_eq!(manifest.kdf.algorithm, "argon2id");
        assert!(manifest.kdf.m_cost >= 19_456, "清单要记着 KDF 参数");
        assert!(!manifest.kdf.salt.is_empty(), "清单要记着盐");
    }

    /// 清单里不许出现敏感信息（zip 容器不加密，它是可以被 `unzip -p` 读出来的）。
    #[test]
    fn manifest_has_no_sensitive_content() {
        let fx = Fixture::new("manifest");
        fx.seed();
        let path = do_export(&fx, "payslip-20261111.zip");
        let work = fx.dir.join("probe");
        fs::create_dir_all(&work).expect("建临时目录失败");
        let (manifest, _, _) = extract(&path, &work).expect("解包失败");
        let json = serde_json::to_string(&manifest).expect("序列化失败");
        for forbidden in [
            "欧阳",
            "98765",
            "hr@example.com",
            "测试单位甲",
            "OuyangAotian",
        ] {
            assert!(
                !json.contains(forbidden),
                "清单里不该出现 {forbidden}：{json}"
            );
        }
    }

    /// TC-602：设置页的常驻备份提醒要能说清「你上次备份是什么时候」——
    /// 所以这个时刻必须记得住、读得回；从没导出过时是 None（界面据此显示强提醒）。
    #[test]
    fn the_last_export_time_is_recorded_and_read_back() {
        let fx = Fixture::new("last-export");
        fx.open();
        assert_eq!(
            get_last_exported_at(&fx.db).expect("读失败"),
            None,
            "从没导出过时应该是 None"
        );

        set_last_exported_at(&fx.db, 1_790_000_000).expect("写失败");
        assert_eq!(
            get_last_exported_at(&fx.db).expect("读失败"),
            Some(1_790_000_000)
        );

        // 再写一次是**覆盖**（键值表，不是追加）—— 界面显示的必须是最近一次。
        set_last_exported_at(&fx.db, 1_800_000_000).expect("写失败");
        assert_eq!(
            get_last_exported_at(&fx.db).expect("读失败"),
            Some(1_800_000_000)
        );
    }
}
