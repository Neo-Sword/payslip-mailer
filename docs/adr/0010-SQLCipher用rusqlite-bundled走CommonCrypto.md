# ADR-0010 · SQLCipher：rusqlite bundled 静态编译 + CommonCrypto + 参数在 key 之后设 + 不用池 + 迁移记录进库

日期：2026-11-11 · 状态：accepted（TC-100 验收通过，实测证据见 TC-100 spec 评审记录）

## 背景

TC-100 要打开一个加密的 `data.db`。设计文档把约束写死了五条：

1. **免装任何东西**：磁盘布局一节标「SQLCipher 系统库 → 免装」，且 ch.11 明确
   「选 Rust crate 的 `bundled` 特性静态编译，避免 brew 装 libsqlcipher 与 pkg-config 依赖」。
   受众是 HR，未签名 dmg 官网直发（ADR-0002），要求对方先装 brew 等于劝退。
2. **参数是指定的**：`cipher_page_size=4096`、`kdf_iter=256000`、`AES-256-CBC + HMAC`。
   「用了 SQLCipher」不算达标，得能证明**这几个参数**生效。
3. **连接生命周期归应用状态**：「应用锁定 / 休眠 / 切后台即释放连接并清内存中的明文密钥」，
   而 S02 的锁定态是「每次启动、休眠恢复或空闲 10 分钟后」都会进入的**反复**状态。
4. **迁移失败要回滚并保持只读**：只读是应用状态，不是库的状态。
5. **产物自带一切**：无自动更新、无遥测、数据不出本机（ADR-0001），供应链越小越好。

另有一条本卡特有的坑：本卡要编译 C，会撞 ADR-0003 记下的 SDKROOT 问题。

## 决策

以下每条后面都跟**实测输出**，不是文档转述。spike 在 `/tmp/sqlcipher-spike/`，
一次性工程，不进仓库；结论靠它，代码不靠它。

### 1. `rusqlite 0.37` + `bundled-sqlcipher`，不装系统 SQLCipher

- 干净编译（`cargo clean` 后）：`real 2.62s / user 3.57s`，退出码 0。
  编的是 crate 里 **9,301,494 字节**的 `sqlcipher/sqlite3.c`；crate 内**没有任何预置 `.a`/`.o`**
  （`find` 无结果），产物归档 `ar t` 只有一个成员 `…-sqlite3.o`（4.1MB，时间戳即本次编译）。
  所以 2.62s 是真编译，不是缓存或预置二进制——机器是 M5 Max 18 核，`-O0` 下说得通。
- 确认是 SQLCipher 而非普通 SQLite：`PRAGMA cipher_version` → `4.6.1 community`。
- 版本事实（写下来免得以后再查）：`rusqlite 0.37.0` 实际解析到 `libsqlite3-sys 0.35.0`，
  本 ADR 引用的 build.rs 行号都是 **0.35.0** 的。
- 不用 `diesel` / `sqlx`：两者都要异步运行时或额外一层抽象，而红线要求所有外部副作用走
  Rust Command 层（同步即可），单机单管理员没有并发要摊。多引进的抽象换不到东西。

### 2. 加密后端是 Apple CommonCrypto——而且它是 Apple 主机上的默认分支

`libsqlite3-sys-0.35.0/build.rs` L146-222 的选择树（逐行读过）：

| 条件 | 结果 |
|---|---|
| L146-147 | `bundled-sqlcipher` → 加 `-DSQLITE_HAS_CODEC`、`-DSQLITE_TEMP_STORE=2` |
| L155-202 | 设了 `OPENSSL_LIB_DIR`+`OPENSSL_INCLUDE_DIR`，**或**设了 `OPENSSL_DIR` → `use_openssl = true` |
| L208-214 | `use_openssl` → `cargo:rustc-link-lib=dylib=crypto`（**动态链 OpenSSL**） |
| L215-218 | 否则若 `is_apple` → `-DSQLCIPHER_CRYPTO_CC` + 链 `Security` / `CoreFoundation` framework |
| L219-222 | 否则 → `-lcrypto` |

- 实测产物 `otool -L`：只有 `Security.framework`、`CoreFoundation.framework`、`libSystem.B.dylib`，
  **没有 `libcrypto`**。零外部依赖，正是 ch.11 要的效果。
- **关键澄清**：`find_openssl_dir`（L316-319）只读 `OPENSSL_DIR` 环境变量，**不探测系统**。
  本机 `/usr/lib/libcrypto.dylib` 根本不存在（macOS 早就不随附），但照样走 CommonCrypto。
  所以「走 CommonCrypto」不是「这台机器碰巧没装 openssl」，而是「没人设那个变量」——
  换一台装了 brew openssl 的机器也不一定会切走。
- 隐患实测：`OPENSSL_DIR=/nonexistent-openssl cargo build` → 退出码 **101**，
  报 `OpenSSL include directory does not exist: /nonexistent-openssl/include`。
  证明 ① 一个环境变量就能把分支从 CommonCrypto 切走，且优先级高于 Apple 分支；
  ② 变量指向**无效**目录时会大声失败；但指向**有效**目录时是**静默**改成动态链 OpenSSL——
  所有测试仍然全绿，只有产物变了（未签名 dmg 里可能带不动）。
- 因此把 `otool -L` 检查写进 `scripts/gate.sh`（TC-100 标准 10），而不是只写在这段话里。

### 3. 加密参数必须在 `PRAGMA key` **之后**设——与 SQLCipher 文档的常见顺序相反

本 ADR 最重要的一条，因为它推翻了我原本准备写进实现的顺序。

| 顺序 | 读回 `cipher_page_size` | 库文件真实大小（要求 1024 页） |
|---|---|---|
| 参数先设，再设 key | `Query returned no rows`（**根本没登记**），设完 key 后读回 4096 | **8192 字节** = 2 × 4096（默认页） |
| 先设 key，再设参数 | **1024** | **2048 字节** = 2 × 1024（**真的生效**） |

- 为什么这条危险到必须写进 ADR：设计文档指定的 `4096 / 256000 / aes-256-cbc`
  **正好就是 SQLCipher 4 的默认值**。所以「参数没生效」不产生任何可观察差异——
  库照样全密文、错误密钥照样打不开，只测「加密了吗」的测试**全绿**。
  参数先设这个错误顺序会一路绿灯进 dmg。
- 所以实现必须：`open → key → 参数 → 读回比对`，读回与代码常量不等即报错。
  而**测试必须用非默认值**（如 1024）建库，否则测不出顺序回归——这是 TC-100 新增标准 11 的全部理由。
- 读回比对的范围实现时实测后要**缩到 `cipher_page_size` 与 `kdf_iter` 两项**，`cipher` 不在其中：
  key 之后设 `PRAGMA cipher` 也是**惰性的**——磁盘仍是 aes-256-cbc，读回永远自报
  `aes-256-cbc`（探针 10：「实验·只改 cipher 128→256 → 读到」，
  同库「对照·只改页 1024→4096 → `file is not a database`」——页参数生效而 cipher 不生效）。
  把一项**恒等于默认值**的检查放进守卫就是恒真的假安全感：它永远不会红，
  却让人以为算法被钉住了。代码里仍照设 `PRAGMA cipher = 'aes-256-cbc'`，
  但它的定位是**意图声明**（万一 SQLCipher 默认值漂移，这行是依据），不是能钉住算法的手段。
- raw key 的写法也是实测出来的：`PRAGMA key = x'…'`（裸写）报
  `near "x'0001…'": syntax error`；必须整体当字符串 `PRAGMA key = "x'…'"`。
- 不是 rusqlite 的锅：`pragma_update`（`pragma.rs` L231-238）把值**内联**进 SQL 文本再
  `execute_batch`（读路径 L206-210 用空参数 `query([])` 也印证了内联），生成的 SQL 是对的。
  但 cipher 类 pragma 一律用 `execute_batch` 写原始 SQL，语义直白，不依赖 crate 的引号行为。
- 附带发现：设了 key 之后，普通 `PRAGMA page_size` **不再返回行**，只有 `cipher_page_size` 有值。
  加上已知的 `PRAGMA page_size` 返回 Text、`user_version` 返回 Integer——
  **PRAGMA 的返回类型与列名在 SQLCipher 下不统一**，探测代码一律按字符串读、读不到就报错。

### 4. `kdf_iter` 对 raw key 库不参与解密（实测，带两个对照组）

DB key 是 32 随机字节，用 raw key 形式，不是人类口令。

- raw key 库建库时 `kdf_iter=64`，重开时改成 `256000` → **照样读到那一行**。
- 对照组一（证明「报错」这条路可达）：同一个库只改 `cipher_page_size` 1024→4096 →
  `file is not a database`。
- 对照组二（证明 KDF 本身是参与得了的）：**口令**库改 `kdf_iter` → `file is not a database`。
- 结论：设计文档的 `kdf_iter=256000` 对本库是**记录性**的（只有口令派生场景才影响解密）。
  仍然照设——将来若改成口令派生就要用，且它会被写进库 header，改了会打不开旧库（对照组一/二就是证据）。
- 顺带一条对文案的硬要求：**参数漂移、错误密钥、真·损坏三者的报错完全相同**
  （都是 `file is not a database`）。所以 TC-100 标准 4 要求 `user_message()`
  同时提「密钥不匹配」与「文件损坏」，不许假装知道原因。

### 5. 不用连接池：`Mutex<Option<Connection>>`，解锁时开、锁定时关

- 设计文档写的是「SQLCipher 连接池 + 迁移」（`db.rs` 那行注释），但 ch.5 的
  「应用锁定 / 休眠 / 切后台即**释放连接**并清内存中的明文密钥」与「池」直接冲突：
  池里至少常驻一条连接，密钥就常驻内存。做不到，所以不叫池。
- 收益侧本来就是零：单管理员（v1 明确「仅单管理员」）、单机、UI 单线程发命令。
- 不用 WAL，保持 `journal_mode = delete`（实测默认即 delete）：
  设计文档称 `data.db` 是「唯一持久数据」，WAL 的 `-wal` / `-shm` 旁文件会打破这句话，
  还会让 TC-104「复制 data.db 即备份」漏掉尚未合并的页。
- TC-100 标准 8 是这条的守护测试：关闭后状态里必须是 `None`，不是「还开着但标记为锁定」。

### 6. 迁移记录放**库内** `schema_migrations`，不放设计文档的 `migrations/` 目录

这是对设计文档磁盘布局的一处**偏离**（那里写的是 `migrations/ # 已执行版本号`）。

- 理由来自同一份文档的恢复流程：备份 zip 里装的是「data.db + 模板 + 校验和」，
  恢复时要「校验版本」。版本记在库外目录，备份包里就**不知道**这个库是第几版——
  记录必须跟库文件走，备份才自解释。
- 表结构：`schema_migrations(version INTEGER PRIMARY KEY, name TEXT, checksum TEXT, applied_at TEXT)`。
  它在加密库内，所以「明文搜不到表名」这条测试（标准 2）顺带证明了迁移表也是密文。
- `checksum` 用来堵「库里记的是 v1、跑的是改过的 v1」这种漂移（标准 7）。
- 迁移 SQL 用 `include_str!` 编进二进制，磁盘上不再需要 `migrations/` 目录。
  代价：库文件里多一张表。换来：备份自解释、少一个磁盘状态。

### 7. 自研 migrator（约 80 行），不用 refinery

设计文档写的是「refinery / 自研 migrator」，所以这是**在授权范围内二选一**，不是偏离。

- 选自研的三条理由：
  ① 「失败则回滚并**保持只读**」的只读是应用状态，库外迁移工具给不了，接 refinery 也要自己写这层；
  ② 错误要进 ADR-0004 的错误码词表（三要素），refinery 的错误类型还得再包一层；
  ③ 少一个依赖就是少一条供应链，本项目无自动更新、离线可用。
- 回滚的前提成立：SQLite 的 DDL 是事务性的，实测
  `BEGIN; CREATE TABLE bad; INSERT; ROLLBACK;` 之后 `sqlite_master` 里查不到 `bad`（计数 0）。
- 代价：checksum 计算、版本比对、"已应用迁移不许改" 都要自己写、自己测（标准 5/6/7）。

## 后果

- 正面：产物零外部加密依赖（`otool` 可证）；免装任何东西；编译成本 2.6s，比 vendored OpenSSL 便宜得多；
  加密参数生效**可测**（标准 11）；连接生命周期与设计文档一致；备份自解释。
- 负面：**加密后端按环境而变**。`OPENSSL_DIR` 一个变量就能静默改成动态链 OpenSSL，
  测试全绿而产物不同——靠门禁的 `otool` 检查兜，不靠人记住。
- 负面：CommonCrypto 分支是 Apple 专属。将来做 Windows/Linux，这条要换成
  `bundled-sqlcipher-vendored-openssl`，编译成本与产物体积都会跳一档。
- 负面：`kdf_iter` 对本库不起实际作用（第 4 条），设计文档那条参数读起来像是「有 KDF 强度」，
  实际强度来自 32 随机字节本身。文档措辞以后别把它当强度指标写进对外说明。
- 负面：自研 migrator 没有现成工具的成熟度，靠 3 条测试兜。
- 遗留：`PRAGMA cipher_migrate`（SQLCipher 大版本升级时把旧版库搬到新版）本卡不做，
  等真遇到 SQLCipher 4→5 这类升级再开卡。
- 遗留：本 ADR 的参数结论都在 `rusqlite 0.37 / libsqlite3-sys 0.35 / SQLCipher 4.6.1 community` 上测的。
  升 SQLCipher 版本时第 3、4 条要重测——它们描述的是这个版本的行为，不是永恒真理。

## 重新评估的触发条件

- 决定做 Windows / Linux → 第 2 条作废，改 vendored-openssl（与 ADR-0002 的触发条件同时命中）。
- 决定把 DB key 从 raw key 改成人类口令派生 → 第 4 条作废，`kdf_iter` 变成关键参数，
  且要重测「改 kdf_iter 会不会打不开旧库」。
- 出现多管理员或后台并发写库 → 第 5 条重估（那时才需要谈池或连接复用）。
- SQLCipher 大版本升级 → 第 3、4 条重测，并启用 `PRAGMA cipher_migrate`。
