# ADR-0011 · 管理员口令：argon2 crate 特征裁剪 + 参数钉 OWASP 下限 + PHC 单列 + 锁定计数进库 + 唤醒走分布式通知中心

日期：2026-11-11 · 状态：accepted（TC-102 验收通过，实测证据见 TC-102 spec 评审记录）

## 背景

TC-102 要实现「管理员口令 + 失败锁定 + 空闲/唤醒锁定」。设计文档把六条约束写死了：

1. **算法是指定的**：ch.4「Argon2id（`argon2` crate）+ 随机盐」「本地管理员口令绝不明文或
   MD5 保存」。**但没给参数**——m/t/p 一个都没写，而这三个数决定它是「10 毫秒」还是「65 毫秒」。
2. **策略是指定的**：ch.6 S01「登录密码（至少 10 位）」；ch.6 S02「密码连续错误 5 次将锁定
   60 秒。本机数据已加密，无法通过重装应用绕过。」
3. **锁定是反复进入的状态**：S02 的进入条件是「每次启动、休眠恢复或空闲 10 分钟后」，
   ch.5 要求「应用锁定 / 休眠 / 切后台即释放连接并清内存中的明文密钥」（ADR-0010 §5 已据此
   定了 `Mutex<Option<Connection>>`，不用池）。
4. **表结构写的是 `admins(id, email, pwd_hash, salt, created_at, last_login)`**——有一列独立的 `salt`。
5. **S02 的时序写的是**「成功后才从 Keychain 取 DB key 并打开加密库」。
6. **性能目标**：ch.11「主界面交互响应 < 100ms」；解锁是 HR 每天要点开的路径。
7. **必测边界含「系统休眠唤醒」**（ch.11 测试矩阵）——唤醒这条路径必须有测试，不能靠人肉。

另外两条是既有决定，本卡不能推翻：ADR-0002/0010 已定 **DB 密钥是 32 随机字节、放 Keychain**；
ADR-0001 已定 **无遥测、产物自带一切、供应链越小越好**。

## 决策

以下每条后面都跟**实测输出**，不是文档转述。两个 spike 在 `/tmp/argon2-spike/` 与
`/tmp/wake-spike/`，一次性工程，不进仓库；结论靠它，代码不靠它。日志留档在
`/tmp/argon2-run.log`、`/tmp/argon2-tree.log`、`/tmp/wake-spike.log`、`/tmp/wake-selfpost.log`。

### 1. `argon2 0.6.0`，关掉默认特征：`default-features = false, features = ["alloc", "password-hash"]`

`cargo tree`（`/tmp/argon2-tree.log`，`TREE_EXIT=0`）——整棵子树 12 个 crate，全是纯 Rust：

```
└── argon2 v0.6.0
    ├── base64ct v1.8.3
    ├── blake2 v0.11.0
    │   └── digest v0.11.3
    │       ├── block-buffer v0.12.1
    │       │   └── hybrid-array v0.4.15
    │       │       └── typenum v1.20.1
    │       ├── crypto-common v0.2.2
    │       │   └── hybrid-array v0.4.15 (*)
    │       └── ctutils v0.4.8
    │           └── cmov v0.5.4
    └── password-hash v0.6.1
        └── phc v0.6.1
            ├── base64ct v1.8.3
            └── ctutils v0.4.8 (*)
```

逐个 crate 扫非 Rust 文件与构建脚本（12 个全查，含 `-name '*.c' -o -name '*.h' -o -name '*.S' -o -name 'build.rs'`）：

```
argon2-0.6.0 → 非 Rust 文件/build.rs 数=0 · rust-version="1.85"
base64ct-1.8.3 → 非 Rust 文件/build.rs 数=0 · rust-version="1.85"
blake2-0.11.0 → 非 Rust 文件/build.rs 数=0 · rust-version="1.85"
digest-0.11.3 → 非 Rust 文件/build.rs 数=0 · rust-version="1.85"
block-buffer-0.12.1 → 非 Rust 文件/build.rs 数=0 · rust-version="1.85"
hybrid-array-0.4.15 → 非 Rust 文件/build.rs 数=0 · rust-version="1.85"
typenum-1.20.1 → 非 Rust 文件/build.rs 数=0 · rust-version="1.41.0"
crypto-common-0.2.2 → 非 Rust 文件/build.rs 数=0 · rust-version="1.85"
ctutils-0.4.8 → 非 Rust 文件/build.rs 数=0 · rust-version="1.87"
cmov-0.5.4 → 非 Rust 文件/build.rs 数=0 · rust-version="1.85"
password-hash-0.6.1 → 非 Rust 文件/build.rs 数=0 · rust-version="1.85"
phc-0.6.1 → 非 Rust 文件/build.rs 数=0 · rust-version="1.85"
```

**零 C、零汇编、零 `build.rs`**：与 ADR-0010 的 `rusqlite bundled`（要编 9.3MB 的 `sqlite3.c`）不同，
这 12 个 crate 不新增任何 C 编译单元，也不碰 ADR-0003 的 SDKROOT 问题。
声明的 MSRV 最高是 `ctutils` 的 1.87，低于项目 `Cargo.toml` 里的 `rust-version = "1.88"`，
所以版本上兼容。**注意口径**：本机只装 `stable`（rustc 1.99.1），没有 1.88 工具链，
「兼容 1.88」是**按 crate 声明**得出的结论，不是用 1.88 编出来的。

**为什么关掉 `getrandom`**：诚实的说法是——Cargo.lock 里**已经有** `getrandom` 0.3.4 与 0.4.3
（Tauri 侧带进来的），所以关掉它**不是**「避免引入第三个 major 版本」。真实理由是
**只用一个 CSPRNG 源**：本项目的随机数一律走 `security_framework::random::SecRandom`
（`infra/db.rs:20` 已 `use` 它，L223-225 的注释记录了 DB key 的同源决定）。盐跟 DB key
同一个源，读代码的人不用想「这两处随机为什么不一样」。
副作用是 `hash_password()`（crate 自带的自动盐路径）被 feature 门控掉，只能用
`hash_password_with_salt(pw, salt)`——盐由调用方给裸字节，正好也是我们要的。

**API 口径（实测，与 0.5 的文档不同）**：`PasswordHasher::hash_password_with_salt(&self, password: &[u8], salt: &[u8]) -> Result<H>`；
校验是 `PasswordVerifier<str> for Argon2<'_>` 的 `verify_password(pw, &str)`，入参是 PHC 串本身；
`PasswordHash` 在 `password_hash::phc::PasswordHash`；`RECOMMENDED_SALT_LEN = 16`。

### 2. 参数钉 `m=19456 KiB, t=2, p=1`（= crate 默认 = OWASP 2024 下限）

`/tmp/argon2-run.log`（release，M5 Max，`RUN_EXIT=0`）。先回答「实现里若偷懒用默认会多强」：

```
Params::default() = Params { m_cost: 19456, t_cost: 2, p_cost: 1, … }
```

四组候选各跑 3 轮。**首轮要把 19–64 MiB 内存摸一遍（缺页），耗时偏高，所以取 round3 当稳态**：

```
round1 m=19456KiB t=2 p=1 → hash 19.572209ms · verify 14.475583ms · wrong 13.236583ms
round2 m=19456KiB t=2 p=1 → hash 12.297958ms · verify 11.421709ms · wrong 10.767333ms
round3 m=19456KiB t=2 p=1 → hash 10.503875ms · verify 10.242459ms · wrong 10.090709ms

round3 m=65536KiB t=3 p=4 → hash 64.592417ms · verify 65.518917ms · wrong 65.526417ms
round3 m=2048KiB  t=2 p=1 → hash 1.000333ms  · verify 997.375µs  · wrong 999.708µs
round3 m=47104KiB t=1 p=1 → hash 14.173625ms · verify 14.322333ms · wrong 14.1595ms
```

**为什么是下限而不是 OWASP 推荐的 64 MiB / t=3 / p=4**：后者要 65 ms，是前者的 6 倍。
解锁路径上还要过一次 Keychain 取 key 与开库（ADR-0010），65 ms 会把「主界面交互响应 < 100ms」
的性能目标吃掉一大半，而换来的强度增益在这个场景里买不回来——**离线爆破者面对的第一道墙不是
口令哈希，而是他得先拿到整份 `data.db` 并且过 SQLCipher**（DB key 在 Keychain 里，
`kSecAttrAccessibleWhenUnlockedThisDeviceOnly`，ADR-0002/0009）。口令层是第二道闸门，
它的任务是「让猜口令变贵」+「让 5 次/60 秒这条 UI 锁定有意义」，不是独自扛住 GPU 字典攻击。
10 ms 已经比 1 ms 那组贵 11 倍，而 1 ms 那组（`m=2048`）是明显偏松的对照组。

**参数不是终点**：见 §3 的 PHC 自描述 + spec 标准 9 的重哈希路径。以后要把参数调强，
不需要迁移脚本，下次解锁顺手升级。

### 3. 只存一列 `pwd_hash`，**不建**设计文档 ch.10 的 `salt` 列

实测 PHC 串自带算法、版本、参数、盐，且能被独立解析回来：

```
PHC: $argon2id$v=19$m=19456,t=2,p=1$MDEyMzQ1Njc4OWFiY2RlZg$k+kzyLZyWlOeBd09HM8qXOT9zS+EJ3zWdiSH6o7EfvE
解析回来: alg=Ident("argon2id") v=19 params={Ident("m"): Value("19456"), Ident("t"): Value("2"), Ident("p"): Value("1")}
```

盐（`MDEyMzQ1Njc4OWFiY2RlZg` = base64 的 `0123456789abcdef`）就在串里，校验时不需要调用方再提供。
盐的有效性也实测过：

```
同一口令·不同盐 → 哈希不同: true
  $argon2id$v=19$m=19456,t=2,p=1$MDEyMzQ1Njc4OWFiY2RlZg$k+kzyLZyWlOeBd09HM8qXOT9zS+EJ3zWdiSH6o7EfvE
  $argon2id$v=19$m=19456,t=2,p=1$ZmVkY2JhOTg3NjU0MzIxMA$T6HFCPY4AVeftymZlsDYatgMvPCWt8qRaqxwnIHmN+Y
  旧盐哈希 + 旧口令 → true
```

**为什么偏离设计文档**：两列存（`pwd_hash` + `salt`）会诱发「改了 hash 忘了改 salt」这类错位，
而错位的结果是**验不过**（上面第三行反过来就是 false），不是报错而是「口令明明对却进不去」。
PHC 单列把这件事变成不可能——盐和哈希物理上是一个字符串。
**这不违反红线 8（字段只能停用不能删除）**：红线 8 说的是「已被历史数据引用的字段」，
而 `admins` 表在本卡之前**根本不存在**（基线迁移头注释把 `admins` 归给 TC-102）。
设计文档写的是意图（「Argon2id + 随机盐」），不是磁盘契约；盐仍在，只是换了位置。

### 4. 定时侧信道：实测「错口令不比猜对快」

上面四组参数里，`wrong`（错口令）与 `verify`（对口令）的耗时**同量级、且互有胜负**：
`10.242459 / 10.090709`、`65.518917 / 65.526417`、`997.375µs / 999.708µs`、`14.322333 / 14.1595`。
所以「失败 5 次」这条锁定不能靠计时差来加速猜测。这条**不是我们代码保证的**——
是依赖树里 `ctutils 0.4.8` + `cmov 0.5.4` 这两个 crate 在做定长比较（§1 的树里能看到它们），
这也是为什么 §1 的特征裁剪不能把它们裁掉。

### 5. 失败计数与 `locked_until` **进库**，用 INTEGER epoch 秒

设计文档 1023 那句「无法通过重装应用绕过」只有在计数落盘时才成立。放内存的话 `⌘Q` 重开就清零，
那句话就成了谎（spec 标准 8 钉这条）。

**列型与设计文档 ch.10 的偏差**：`failed_attempts` / `locked_until` / `last_login` / `created_at`
用 INTEGER（epoch 秒），而 TC-100 的 `app_meta` 全是 TEXT。理由：锁定判定要在 SQL 里做
`locked_until > ?` 这种数值比较，TEXT 会比较字典序；`app_meta` 是键值配置、不参与比较，
所以那边全 TEXT 是对的，这边不是。

**负面（写清楚）**：epoch 秒是**墙钟**，所以「把系统时间往前调」可以提前解锁，往后调会拉长锁定。
接受它的理由：能改系统时钟的人，本来也能直接读 `data.db` 文件；这条锁定是给
**诚实使用者**的摩擦控制（防同事乱按、防离开座位被瞥见），硬边界始终是 SQLCipher + Keychain。
不引入单调时钟持久化（重启后单调时钟归零，反而更难解释）。

### 6. 解锁顺序：**先开库、后验口令**——与设计文档 1026 的字面顺序相反

设计文档写「成功后才从 Keychain 取 DB key 并打开加密库」。但 `pwd_hash`、`failed_attempts`、
`locked_until` 全在 `admins` 表里，而表在加密库内——**不开库读不到哈希，不读哈希验不了口令**。
所以实现是：

```
unlock(口令):
  ① Keychain 取 DB key → 开库（ADR-0010 的 key→参数→读回顺序）
  ② 读 admins 行；无行 → NOT_INITIALIZED（首启向导路径，界面归 TC-103）
  ③ 若 locked_until 未到期 → 返回 LOCKED(剩余秒)，**关库**
  ④ Argon2id 校验（§2 的参数）
  ⑤ 错 → failed_attempts += 1（第 5 次同时置 locked_until = now + 60），**关库**
  ⑥ 对 → 计数清零、更新 last_login、必要时重哈希（§3 路径），**保持开库**
```

设计文档那句话按**使用者可见的状态**解释：解锁成功后才进入「库开着的工作台」；
失败与锁定的分支立刻 `close()`，回到 ch.5 要求的「释放连接并清内存中的明文密钥」。
`DbState` 的 `Mutex<Option<Connection>>`（ADR-0010 §5）本来就是为这个状态机准备的。

**被否掉的替代方案**：用管理员口令派生 DB key（这样口令错就字面上打不开库，顺序天然符合文档）。
否掉三条理由：(a) 推翻 ADR-0002/0010 已 accepted 的「32 随机字节放 Keychain」；
(b) TC-104 的备份要带上库密钥，口令派生会让「备份」和「口令」耦合，改口令等于换备份格式；
(c) 忘记密码时数据会**真的**不可恢复（现在只是应用层闸门重设，见 ch.11 2176 引导备份恢复）。

**负面（写清楚）**：管理员口令**不是**库加密密钥。同一个 macOS 账户下的进程可以在不知道
管理员口令的情况下打开 `data.db`（Keychain 授权给的就是这个签名/这个用户）。
口令是应用层闸门 + 防重装绕过，别把它当成加密边界宣传——这也是 ADR-0002 未签名分发
后果一节的同一件事。

### 7. 唤醒信号：`core-foundation-sys` 的**分布式**通知中心 + `Hold`（依赖图零新 crate）

三个候选实测过（`/tmp/wake-spike.log`，`BUILD_EXIT=0`；探针源码 `/tmp/wake-spike/src/main.rs`）：

```
=== 实验 A：分布式中心观察 + 另一进程 post（模拟 loginwindow 唤醒通知）===
已向分布式通知中心发出 NSWorkspaceDidWakeNotification
A_EXIT=0
观察 分布式 通知中心，等 6s …
[回调] 收到 "NSWorkspaceDidWakeNotification" · 线程 ThreadId(1) · 累计 1
结束：收到 1 条

=== 实验 B（对照）：Darwin 中心观察同一个名字 + 分布式 post ===
B_EXIT=3
观察 Darwin 通知中心，等 6s …
结束：收到 0 条

=== 实验 C（对照）：Darwin 中心观察 + Darwin 中心 post ===
C_EXIT=0
观察 Darwin 通知中心，等 6s …
[回调] 收到 "NSWorkspaceDidWakeNotification" · 线程 ThreadId(1) · 累计 1
结束：收到 1 条
```

（A 段里 `A_EXIT=0` 打在观察行之前是**日志缓冲假象**：观察进程的 stdout 被重定向到文件时
块缓冲，退出才 flush；执行顺序仍是「观察 → post → 收到」，退出码 0 就是收到的证据。
这个坑记在这里，免得下次读日志以为顺序反了。）

- **A 证明**：真实的唤醒通知由 `loginwindow` 进程发出、跨进程投递到分布式中心，我们的进程能收到。
- **B 证明**：在 Darwin 中心观察这个名字**收不到**唤醒通知。B 的 0 条不是「Darwin 中心坏了」——
  C 用同一个 Darwin 中心自己发自己收，收到了。所以 B 的静默是**通道不匹配**。
- **代价**：选错中心不会报错、不会 panic，只会「永远不锁」。这类静默失效必须靠对照组实验排除，
  不能靠读文档，因为两个 API 长得几乎一样。

**为什么用 `core-foundation-sys` 而不是 `objc2-app-kit` 的 `NSWorkspace` 通知**：
`core-foundation-sys 0.8.7` 已经在项目的 `Cargo.lock` 里（`core-foundation 0.10` 的依赖，
而 Keychain 层直接用了 `core-foundation` 造 CFData），用它 = **依赖图零新 crate**。
`NSWorkspace` 那条路要引 `objc2` 家族并且需要 ObjC block。
FFI 类型按源码读准（不猜）：`base.rs:14 Boolean = u8`、`base.rs:31 CFIndex = isize`、
`date.rs:19 CFTimeInterval = f64`、`dictionary.rs:51 CFDictionaryRef = *const __CFDictionary`；
`CFNotificationCenterGetDistributedCenter()` 有 `#[cfg(any(target_os = "macos", target_os = "windows"))]`。
挂观察者用 `CFNotificationSuspensionBehaviorHold`（=3）：休眠期间发的通知要在唤醒后补投，
`Drop`（=1）会把它们丢掉——那正是我们要的场景。

**回调签名与「有没有返回值」（源码事实：`notification_center.rs:22` 与 `:52`）**：
`CFNotificationCallback` 是**五个**入参的 `extern "C" fn` —— `center`、`observer: *mut c_void`、
`name: CFNotificationName`（= `CFStringRef`）、`object: *const c_void`、`userInfo: CFDictionaryRef`，
**没有**第六个 `SuspensionBehavior` 参数；`CFNotificationSuspensionBehavior` 只是
`AddObserver` 的入参（`= CFIndex`，`Drop`/`Coalesce`/`Hold`/`DeliverImmediately` = 1/2/3/4）。
回调与 `CFNotificationCenterPostNotification` 都**没有返回值**。两条后果已经写进代码：
① 「收到了」没有别的通道，回调只能往观察者令牌指向的对象里写 —— 所以令牌必须是稳定地址，
`on_wake` 只做一个 `AtomicU64` 加一（`infra/wake.rs:71-85`）；
② 发通知的一方无法知道自己有没有被听到，所以 `tests/wake_delivery.rs` 的子进程只能报
「已发出」，判定权在观察方。

**验收中更正的两处措辞**（原文照抄在下面，免得下次读 ADR 以为它一开始就对）：

- 原文：「用它 = **零新 crate**、零新直接依赖」。后半句不成立 —— `Cargo.toml` 里新增了
  `core-foundation-sys = "0.8"` 这条**直接依赖声明**，只是它没往依赖图里带任何新 crate。
  本卡真正的「新」是**两条声明**：`argon2 = "0.6.0"`（§1，连带 12 个纯 Rust crate）
  与这条把传递依赖提成直接依赖的声明。
- 原文：「回调要在 runloop 里跑，所以 `LockWatch` 自带一个观察线程
  （`CFRunLoopRunInMode(mode, 0.5, 1)` 轮询），不阻塞主线程」—— **这句的因果是错的**，
  见 §10。分布式通知的回调只在**主线程的 runloop** 上派发，后台线程泵自己的 runloop
  一条都收不到。所以观察线程现在只做两件事（数唤醒计数、判空闲），每片
  `std::thread::sleep(0.25s)`，**不再泵 runloop**（`infra/wake.rs:194-199`）；
  泵主线程是宿主的责任 —— 真应用里由 Tauri/Cocoa 的主事件循环泵，
  测试里由 `tests/wake_delivery.rs` 自己的 `main()` 泵。

### 8. 唤醒的自动化测试必须由**子进程**发通知

实验 D（`/tmp/wake-selfpost.log`，`BUILD_EXIT=0`）：

```
=== 实验 D：同进程自投递（另一个线程发，同一个分布式中心）===
观察分布式中心 5s，300ms 后由**本进程另一个线程**自发 …
观察 分布式 通知中心，等 5s …
已向分布式通知中心发出 NSWorkspaceDidWakeNotification
post 线程退出码 0
结束：收到 0 条
D_EXIT=3
```

**分布式通知中心不向自己投递**（同进程另一个线程发也算自己）。所以测试里「自己发一条通知
假装唤醒」会得到**假绿**——看着测了，实际什么都没测。spec 标准 11 因此规定：
测试用 `std::process::Command::new(std::env::current_exe())` 起子进程发（正是 A 证明的跨进程路径），
父进程观察。标准 12 另外要求这条测试**不能挂**：拿不到投递就打印 `SKIP` 走人，
因为 TC-101 已经为「依赖 GUI 会话」付过一次学费（ADR-0008/0009）。

**真人合盖唤醒**（ch.11 2158 的必测边界）本机没测——不想擅自把使用者的机器睡掉。
转成 TC-103 的验收标准（真人合盖再打开必须出现锁屏），与 TC-000 遗留的
「真人起界面确认红绿灯」同批做。

### 9. 空闲计时用单调时钟，阈值可配且可注入

空闲判定用 `Instant`（单调），不用墙钟：改系统时间不该让应用提前锁或永不锁
（与 §5 的墙钟取舍相反，因为这两件事的代价不对称——空闲锁错了使用者看得见）。
阈值来自 `app_meta` 的 `idle_lock_minutes`（默认 10，对应设计文档 795/2013），
设置页那个 5/10/30 下拉框（2013）由 TC-103 写这个键。
活动信号只在 **Command 层** `touch()`（红线 5：外部副作用走 Rust，前端不能自己声明「我还活跃」）。

### 10.（验收中追加）唤醒回调只由**主线程的 runloop** 派发 —— §7 那句推断是错的

§7 原文「回调要在 runloop 里跑，所以观察线程自己泵」是**没验证过的推断**。验收标准 11 时
把它当预设立进测试，结果单元测试里怎么等都收不到，于是把缠在一起的两个变量
（**谁注册**、**谁泵 runloop**）分开做四组对照。探针仍是 `/tmp/wake-spike/src/main.rs`
（`/tmp/wake-spike-build3.log`，`BUILD_EXIT=0`），四个模式对应四组：
`watch` / `watch-bg` / `watch-bg-reg` / `watch-main-reg-bg-pump`。

**四组的发帖方都是另一个进程**（`spike post`，即 §7 实验 A 的同一条跨进程路径）。
这一点可以从日志本身反证：四份日志里**都没有**「已向分布式通知中心发出 …」那一行 ——
那行只有发帖进程会打印。所以四组里的「0 条」都不是 §8 那条自投递规则造成的，
变量确实只剩线程分工一个。

| 组 | 注册线程 | 泵 runloop 的线程 | 结果 | 日志 |
| -- | -------- | ---------------- | ---- | ---- |
| A | 主线程（`watch` 在调用方即主线程跑） | 主线程 | **收到 1 条**，回调 `ThreadId(1)` | `/tmp/spike-main.log` |
| B | 后台线程 | 后台线程 | 收到 **0 条** | `/tmp/spike-bg.log` |
| C | 后台线程（日志：`已在 ThreadId(2) 注册`） | **主线程** | **收到 1 条**，回调仍 `ThreadId(1)` | `/tmp/spike-watch-bg-reg.log` |
| E | 主线程（日志：`已在 ThreadId(1) 注册`） | 后台线程 | 收到 **0 条** | `/tmp/spike-watch-main-reg-bg-pump.log` |

实测原文（退出码是当场 `echo` 的、不在日志文件里：A/C = 0，B/E = 3）：

```
# C：后台注册 + 主线程泵
后台线程注册，主线程泵 runloop 6s …
已在 ThreadId(2) 注册
[回调] 收到 "NSWorkspaceDidWakeNotification" · 线程 ThreadId(1) · 累计 1
结束：收到 1 条

# E：主线程注册 + 后台线程泵
主线程注册，后台线程泵 runloop 6s …
已在 ThreadId(1) 注册
结束：收到 0 条

# B：注册与泵都在后台线程
观察分布式中心 6s，注册与 runloop 都在后台线程 …
观察 分布式 通知中心，等 6s …
结束：收到 0 条
```

**结论**：约束不是「必须在主线程注册」，而是「**进程的主线程必须在泵 runloop**」。
C 用后台线程注册照样收到，E 主线程注册但由后台线程泵就一条没有 —— 分发点是主线程的
runloop，注册线程无关；回调永远在 `ThreadId(1)` 上执行。

**由此改掉的生产代码**：`LockWatch` 的观察线程去掉 `CFRunLoopRunInMode`，改成
`std::thread::sleep(0.25s)`（`infra/wake.rs:194-199`）。留着那次泵不会报错，只是白占一次
唤醒，还会误导读代码的人以为「这条线程负责分发」。

**第二个实测事实：`#[test]` 里永远测不到投递。** 单独一个 crate 验的（`/tmp/lt/`），
`cargo test` 与 `cargo test -- --test-threads=1` 各一次（`/tmp/lt1-out.log`、
`/tmp/lt2-out.log`），两次输出实质相同：

```
pthread_main_np = 0
thread = ThreadId(2) name = Some("am_i_main")
```

libtest 把测试体放在**工作线程**上，`--test-threads=1` 也不改变这一点；测试进程的 `main()`
在 `run_test` 返回前什么都不做，没人泵主线程 runloop。按上面的结论，分布式通知在 `#[test]`
里根本投递不到 —— 所以标准 11 拆成两半：

- 「计数变了 → 关库」这段（本模块自己写的代码）留在 `#[test]`，用 `inject_wake()` 触发，
  确定性、CI 可跑；
- 「另一个进程发 → 本进程收到 → 连接真的关掉」那段必须在**有 `main()` 的测试**里做，
  即 `tests/wake_delivery.rs`（`Cargo.toml` 里 `harness = false`），主线程自己泵 runloop。

**代价（写清楚）**：`harness = false` 的二进制不参与 libtest 的用例过滤与并行，
`cargo test wake_delivery` 不会挑出用例名（它没有用例名，只有退出码与 stdout），
失败信息得自己打印。换来的是这条路径第一次被真的测到：
`PASS: 另一个进程发的唤醒通知触发了锁定，加密库连接已关闭`。

**触发条件追加一条**（见文末）：宿主哪天不再泵主线程 runloop，C 组的前提就没了，
唤醒锁定会静默失效，而 `wake_registered()` 返回 true 也查不出来。

## 后果

**正面**

- 口令层零新 C：`argon2` 那 12 个 crate 在验收时逐个复扫 `.c`/`.h`/`.S`/`build.rs` 全为 0
  （TC-102 评审记录·标准 1）。唤醒路径零新 crate，只是把已在锁里的 `core-foundation-sys`
  提成直接依赖 —— 本卡新增的是**两条依赖声明**，不是两条依赖。
  门禁的加密栈一项（按名字认，编号见 TC-102 评审记录的勘误）结论不变：
  `otool -L` 仍只有 `Security.framework … current version 61901.120.67`，没有 `libcrypto`/`libssl`。
- 参数、盐、版本全在一列 TEXT 里：换机器、备份恢复、以后调强参数都不需要额外列或迁移脚本。
- 锁定计数进库 → 设计文档「无法通过重装应用绕过」这句话第一次有了可测的实现（spec 标准 8）。
- 唤醒通道用对照组实验定死，排除了「两个几乎一样的 API，选错就静默不锁」这个坑。
- 空闲/锁定判定是纯函数 + 注入时钟，5 次/60 秒的全部边界（含「锁定期内不延长」）
  在毫秒级测试里跑完，不需要真等 60 秒。

**负面**

- 管理员口令不是库密钥：同一 macOS 用户的进程可在不知道口令时打开 `data.db`（§6）。
- 锁定用墙钟，改系统时间可提前解锁（§5）。
- 关掉 `getrandom` 意味着不能用 crate 的自动盐路径，每次哈希都要自己准备 16 字节盐——
  多写几行，换来「全项目一个 CSPRNG 源」。
- 唤醒依赖 macOS 的分布式通知中心，是平台特定代码（`#[cfg(target_os = "macos")]`）；
  本项目 v1 只发 macOS（ADR-0001），可接受。
- 投递还多一个**不由本模块保证、也无法由本模块检测**的前提：进程主线程必须在泵 runloop（§10）。
  `wake_registered()` 返回 true 只说明观察者挂上了，不说明收得到；真应用里由 Tauri/Cocoa 的
  主事件循环兜住，测试里得自己泵（所以那条测试必须 `harness = false`）。
- 真实合盖唤醒未在本机实测，只有跨进程模拟的证据（§8）。

**遗留**

- 真人合盖 → 唤醒出现锁屏：转 **TC-103** 验收标准（与 TC-000 的「真人起界面」同批）。
- 参数以后要不要上调（比如 2028 年硬件翻倍）：靠 spec 标准 9 的重哈希路径，
  改一个常量 + 一次解锁即可，不需要新卡。触发条件见下。
- `idle_lock_minutes` 的下拉框与「记住本机 10 分钟」勾选（设计文档 1018）：TC-103。
- 忘记密码的备份恢复引导（设计文档 2176）：TC-104。

## 重新评估的触发条件

- **换平台**（Linux/Windows）：§7 的分布式通知中心没有对应物，唤醒要换成各自的电源事件 API；
  空闲计时与锁定判定不受影响。
- **决定给多个管理员**（设计文档明写 v1 单管理员）：`CHECK (id = 1)` 要拆，锁定语义要从
  「本机」变成「每个账号」，§5 的列布局要重开。
- **决定把口令变成库密钥**：等于推翻 ADR-0002/0010 的 Keychain 随机 key，连带 TC-104 备份格式，
  必须写新 ADR 并 supersede 本文件的 §6。
- **实测发现解锁明显变慢**（不只是哈希这 10 ms，而是 Keychain 弹窗、开库等）：
  先测各段耗时再谈调参数，别凭感觉降 m/t/p——参数是本机唯一能自己调的那一段。
- **argon2 的 MSRV 声明超过 1.88**，或 `ctutils`/`cmov` 被特征裁剪掉（§4 的定长比较就没了）：
  升级前必须重跑 §1 的两条扫描。
- **宿主不再泵主线程 runloop**（换 `winit` 之类的事件循环、Tauri 改了主线程模型、
  或把观察逻辑搬进没有主事件的辅助进程）：§10 的 C 组前提就不成立了，唤醒锁定静默失效。
  届时先重跑 §10 的 A/E 两组确认分发点，**不要**回头给 `wake.rs` 的观察线程加回 runloop ——
  那次泵在验收中已被证明一条通知都收不到。
