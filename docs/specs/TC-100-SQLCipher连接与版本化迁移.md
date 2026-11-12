# TC-100 · SQLCipher 连接 + 版本化迁移

状态：accepted（2026-11-11 验收，见评审记录）
依赖：TC-101（accepted，提供 `infra::keychain` 的 put/get/delete 与 `KEYCHAIN_MISSING`）
设计依据：设计文档 ch.5（磁盘布局 / 加密与生命周期 / 「迁移失败则回滚并保持只读」）·
ch.11（「选 Rust crate 的 `bundled` 特性静态编译，避免 brew 装 libsqlcipher 与 pkg-config 依赖」）·
ch.12 S15（错误码三要素）· P1 验收行（「直接打开 data.db 无法读取明文」「重启后数据完整」）·
ADR-0001（SQLCipher 选型）· ADR-0002（升级后读不到 Keychain 条目是正常路径）·
ADR-0003（SDKROOT 钉 CLT SDK：本卡要编 C，会撞同一个坑）· ADR-0009（Keychain 层）·
**ADR-0010（本卡的 crate 选型、加密后端、迁移器与连接生命周期）**

## 目标

把「数据不出本机」从一句话变成一件可验证的事：一个加密的 `data.db`，
密钥只存在于 Keychain，库文件里搜不到任何明文痕迹；
库结构靠版本化 SQL 迁移演进，失败能回滚、能让上层转入只读；
锁定时连接与内存里的密钥一起消失。

## 范围

**做**：`infra::db` 模块——按设计文档参数打开/关闭 SQLCipher 连接
（顺序必须是 **key → 参数 → 读回比对**，见 ADR-0010 第 3 条）、
从 Keychain 取 32 字节 DB key（首启生成并写入，raw key 形式）、版本化迁移器（含 `schema_migrations` 表、
checksum 校验、事务回滚、只读信号）、数据目录定位、
以及一条「产物不依赖外部加密库」的门禁检查。

**不做**：任何业务表（`admins` 是 TC-102、`salary_fields` 是 TC-200、`send_jobs` 是 TC-402 的迁移）；
本卡只放一条基线迁移证明机制在跑；
管理员口令与锁定（TC-102）；备份导出与恢复（TC-104，它依赖本卡的版本号）；
首启向导界面（TC-103）；`PRAGMA cipher_migrate`（SQLCipher 大版本升级时才需要，见 ADR-0010 遗留）。

## 验收标准

每条都可证伪：能跑一条命令，或能在测试输出里指出一行。

1. **单测全绿**：`cargo test db` 退出码 0，且测试名可逐条对上下面 2–8 与 11 条。
2. **全库加密，明文搜不到**：写入含哨兵中文姓名与一笔金额后，直接读库文件的字节，
   搜不到哨兵、搜不到该金额、**也搜不到表名与列名**。
   *为什么必须有这条*：这是设计文档 P1 验收「直接打开 data.db 无法读取明文」的机器化。
   只测「能写能读」抓不出「忘了开加密」——那种实现所有 CRUD 测试照样全绿。
3. **密钥只来自 Keychain，读不到就绝不新建**：DB key 只能经 `infra::keychain` 取；
   删掉 Keychain 条目后再打开已有库，必须返回 `KEYCHAIN_MISSING`，
   **不能**返回一个新建的空库。
   *为什么*：这是 ADR-0002 那条正常路径在数据库侧的落点。若实现是「读不到就生成新 key 并新建库」，
   升级后会静默把老数据留在原地、给 HR 一个空应用——工资没了还不报错。
4. **错误密钥打不开，且文案不假装知道原因**：用错误的 key 打开已有库必须报错而不是得到空库；
   错误映射为 `DATABASE`，其 `user_message()` 必须**同时**提到「密钥不匹配」与「文件损坏」两种可能。
   *为什么*：实测错误密钥的报错是 `file is not a database`，与真·损坏**无法区分**；
   文案若写死「密钥错了」会把一次真损坏引导成徒劳的重新初始化。
5. **迁移可重放**：空库打开后自动跑到最新版本；同一库重复打开不重复执行
   （`schema_migrations` 行数不变，已建的表不报「已存在」）。
6. **迁移失败会回滚并给出只读信号**：故意插入一条中途失败的迁移，结果必须是
   ① 库停在上一版 ② 失败那条不写进 `schema_migrations` ③ 返回的错误足以让调用方进入只读态。
   *为什么*：设计文档 ch.5 写死了「失败则回滚并保持只读」，这是工资数据最怕的场景——
   迁移到一半继续跑，改错的表结构会污染当月工资。
7. **改历史迁移要报错，不静默**：已应用迁移的 SQL 被改动后（checksum 不符），
   启动时报 `DATABASE` 且文案含「校验」，不允许「库里记的是 v1、跑的是改过的 v1」这种漂移。
8. **锁定即释放连接**：关闭后状态里的连接必须是 `None`（不是「还开着但标记为锁定」），
   重新解锁后能读到之前的数据。
   *为什么*：设计文档 ch.5「应用锁定 / 休眠 / 切后台即释放连接并清内存中的明文密钥」。
   连接池做不到这件事，所以这条同时是 ADR-0010「不用池」决策的守护测试。
9. **测试不污染真实数据目录**：测试全程用临时目录；跑完后
   `~/Library/Application Support/PayslipMailer/` 下**没有新增文件**。
   反向验证：如果测试其实写进了真实目录，上面几条照样全绿。
10. **产物不依赖外部加密库**：对构建产物跑 `otool -L`，**不得**出现 `libcrypto`，
    且应出现 `Security` / `CoreFoundation`（加密来自 Apple CommonCrypto）。
    这一项加进 `scripts/gate.sh`。
    *为什么*：加密后端是**按环境**选的（见 ADR-0010 第 2 条）——谁设了 `OPENSSL_DIR`，
    构建会静默改成链 OpenSSL 动态库，在未签名的 dmg 里可能带不动，而所有测试仍然全绿。
11. **加密参数真的生效**：用 `cipher_page_size = 1024`（**不是** SQLCipher 4 的默认值）建库，
    建完必须①读回 `PRAGMA cipher_page_size` 得到 `1024` ②用**漂移到 4096 的页参数**重开
    同一个文件必须报 `file is not a database`——页大小物理落在文件里，换分页几何就读不开。
    （不用「文件大小 = 2 页」当证据：真实开库要建两张表，4 页 × 1024 也是 4096 字节，
    「≥4096 即参数没生效」的推断不成立，首轮跑出来就是这个假阳性。）
    实现的 `open` 路径要把 `cipher_page_size` 与 `kdf_iter` **两项**读回并与代码常量比对，
    不等即报 `DATABASE`；`cipher` **不进比对**——实测 key 之后设它同样是惰性的，
    读回恒为 `aes-256-cbc`，把它放进比对是恒真的假安全感（探针 10，ADR-0010 第 3 条）。
    *为什么*：实测 `cipher_page_size` / `kdf_iter` / `cipher` 在 `PRAGMA key` **之前**设会被**静默忽略**——
    读回返回「无行」、文件仍是 8192 字节（2 × 4096 默认页）。而设计文档指定的
    `4096 / 256000 / aes-256-cbc` **正好就是 SQLCipher 4 的默认值**，所以顺序写反时
    第 2 条（全库加密）与第 4 条（错误密钥打不开）照样全绿，参数没生效会一路绿灯进 dmg。
    **只有用非默认值建库才测得出来**，见 ADR-0010 第 3 条。

## 评审记录

验收日期 2026-11-11。命令都在 `payslip-mailer/` 下跑，环境按 `scripts/env.sh`
（PATH 含 cargo、SDKROOT 钉 CLT SDK）。日志留档 `/tmp/tc100-*.log`。

1. **单测全绿** — `cargo test db` 退出码 **0**：
   `test result: ok. 12 passed; 0 failed; 0 ignored; 18 filtered out`。
   测试名与下面各条一一对上：2→`db_whole_file_is_encrypted_no_plaintext_anywhere`、
   3→`db_first_launch_generates_and_stores_the_db_key` + `db_missing_keychain_entry_never_creates_an_empty_db`、
   4→`db_wrong_key_is_rejected_and_message_names_both_causes`、
   5→`db_migrations_are_replayable_and_not_rerun_on_reopen`、
   6→`db_failed_migration_rolls_back_and_signals_read_only`、
   7→`db_editing_an_applied_migration_fails_with_a_checksum_message`、
   8→`db_close_releases_the_connection_and_reopen_reads_data`、
   9→`db_tests_do_not_touch_the_real_data_dir`、11→`db_cipher_params_actually_take_effect`。
   另有两个守护 ADR-0010 决策而非验收条的测试：`db_journal_mode_stays_delete`（第 5 条「不用 WAL」）、
   `db_newer_db_than_app_is_rejected`（第 6 条「库比程序新」拦截）。
   全量 `cargo test`：`30 passed; 0 failed`——TC-101 与 error 的既有测试无回归。
2. **全库加密** — `db_whole_file_is_encrypted_no_plaintext_anywhere` 写入哨兵后直接读文件字节，
   7 根针全部搜不到：`欧阳傲天`、`12345.67`、`app_meta`、`sentinel_name`、`schema_migrations`、
   `0001_baseline`、`SQLite format 3`（最后一根证明连未加密 SQLite 的魔数头都不存在）。
3. **密钥只来自 Keychain** — `db_missing_keychain_entry_never_creates_an_empty_db`：
   建库写数据 → 删 Keychain 条目 → 重开返回 `KEYCHAIN_MISSING`，库文件原样在原地
   （不是空库盖上去）。首启路径由 `db_first_launch_generates_and_stores_the_db_key` 覆盖：
   生成 32 字节、写入钥匙串、重开复用同一把。
4. **错误密钥** — `db_wrong_key_is_rejected_and_message_names_both_causes`：
   换一把 key 打开 → `Database` 硬错且状态保持 `None`（不是只读、不是空库）；
   断言 `user_message()` 同时含「密钥」与「损坏」。
   这条在实现期逼出一个真问题：`PRAGMA key` 对错钥不报错，只有读页才炸——
   没有 `assert_decryptable` 探针时错误密钥会伪装成「迁移失败 → 只读」，与标准 4 冲突。
5. **迁移可重放** — `db_migrations_are_replayable_and_not_rerun_on_reopen`：
   首开 `applied == [1]`，重开 `applied == []`；`schema_migrations` 行数仍 1，表不报「已存在」。
6. **失败回滚 + 只读** — `db_failed_migration_rolls_back_and_signals_read_only`：
   注入中途失败的 v2 后：① `version() == Some(1)` ② 记录表仍 1 行（失败的 v2 没写进去）
   ③ 返回 `OpenOutcome::ReadOnly`、`is_read_only() == true`、`write()` 被拒且错误是 `DATABASE`、
   `is_open() == true`（只读 ≠ 没解锁）、读仍可用。
   **验收里修出一个生产 bug**：只读分支的 `version` 原本只数「本轮新应用」，
   而 v1 是上一轮应用的 → `version()` 返回 `Some(0)`；已改为「记录表 ∪ 本轮新应用」。
7. **改历史迁移报错** — `db_editing_an_applied_migration_fails_with_a_checksum_message`：
   改动已应用 v1 的 SQL 后重开 → `DATABASE`，`Display` 含「校验」
   （细节按 error.rs 约定只进日志，`user_message()` 是通用 DATABASE 文案）。
8. **锁定即释放** — `db_close_releases_the_connection_and_reopen_reads_data`：
   `close()` 后 `version() == None`（`Mutex<Option<Session>>` 里是 `None`，连接随 `Session` drop），
   重开后读到关闭前写入的值。
9. **不污染真实目录** — `db_tests_do_not_touch_the_real_data_dir`：
   对 `~/Library/Application Support/PayslipMailer/` 做前后快照比对，无新增文件；
   测试目录一律 `xinyou-db-test-{pid}-{nanos}-{label}`，`Drop` 里递归删除。
10. **产物不链外部加密库** — 门禁第 9 项已加进 `scripts/gate.sh`（FAST=0 段），实测输出：
    `otool -L src-tauri/target/debug/payslip-mailer` 出现
    `/System/Library/Frameworks/Security.framework/Versions/A/Security (…)`，
    `libcrypto` / `libssl` 零命中。`bash scripts/gate.sh` 退出码 **0**，9 项全 ✓。
11. **参数真的生效** — `db_cipher_params_actually_take_effect`：
    ① 裸连接读回 `cipher_page_size == 1024`、`kdf_iter == 256000`；
    ② 同一把密钥、页参数漂到 4096 重开 → 报 `file is not a database`。
    **验收里修掉一个测试假阳性**：原证据是「文件 < 4096 字节」，但真实开库要建两张表，
    4 页 × 1024 也是 4096 字节 → 误判「参数没生效」；换成漂移重开（探针 10 的对照组）后通过。

**验收期间发现并修掉的问题**（每条有复现，不是顺手改）：
① 只读分支 `version()` 少数已记录版本（第 6 条）；② 标准 11 的文件大小证据是假阳性（第 11 条）；
③ 写拒绝的断言原本查 `user_message()` 含「数据库」，但 error.rs 的约定是内部类错误细节只进
`Display`，通用文案里没有这三个字 → 改断言为 `matches!(…, Database(_))`；
④ `cargo fmt` 首轮红（手写换行），`cargo fmt` 后归零。

**遗留（已登记 TASKBOARD）**：
- 只读态的专属用户文案（「已解锁但只读」而非「打不开」）归 TC-103 界面层；
- checksum 不符的细节只在 `Display`，UI 只见通用 DATABASE 文案，要不要专属码归 TC-103；
- `KEYCHAIN_MISSING` 的引导界面归 TC-103（TC-101 遗留合并）；
- Tauri `app_data_dir()` 按 bundle id 推出 `cn.payslipmailer.desktop` 目录，与设计文档写的
  `~/Library/Application Support/PayslipMailer/` 不一致；`db` 的目录是参数传入所以本卡不受影响，
  口径必须在 TC-103 / TC-104 定死（备份与恢复都按目录找）。
