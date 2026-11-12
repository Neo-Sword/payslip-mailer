# TC-102 · Argon2id 管理员口令 + 失败锁定 + 空闲/唤醒锁定

状态：accepted（评审记录见文末） · 依赖：TC-100（accepted） · 最后更新：2026-11-11

设计依据：设计文档 ch.4 安全模型（179-183「Argon2id + 随机盐」「绝不明文或 MD5 保存」、
795「5 次失败锁定；空闲 10 分钟自动锁定」）、ch.5 数据目录（542「应用锁定 / 休眠 /
切后台即释放连接并清内存中的明文密钥」）、ch.6 S02 登录解锁（1007-1026，含
「已锁定」「密码连续错误 5 次将锁定 60 秒。本机数据已加密，无法通过重装应用绕过。」
「成功后才从 Keychain 取 DB key 并打开加密库」）、ch.6 S01 首启（354「登录密码（≥10 位），
写 Argon2id 哈希」）、ch.9 表结构（461-464 `admins`）、ch.10 交互细节（966-967「⌘L 立即锁定」）、
ch.10 设置页（2011-2015「安全与锁定」「空闲自动锁定 10 分钟 ▾」「修改管理员密码」）、
ch.11 P1 验收（2130-2135）、ch.11 测试矩阵（2158「系统休眠唤醒」、2160「主界面交互响应 < 100ms」）。
决策依据：ADR-0011（Argon2id 的 crate 与参数、PHC 单列、锁定计数进库、唤醒通道选型）。

## 目标

做完之后使用者能干什么：

- 第一次打开应用时设一个管理员口令（≥10 位），此后每次唤醒、休眠恢复、空闲超时都要重新输这个口令才能进工作台。
- 输错口令看得见「还剩几次」；连续错 5 次会被锁 60 秒，锁屏上显示倒计时，期间怎么试都不动计数。
- 合上盖子再打开、或者放着 10 分钟没动，应用自己回到锁屏——工资条在 HR 机器上不会一直敞着。
- 忘记密码不会被「重装应用」绕过：计数与哈希都在加密库里，重装后仍是锁定态。

**这张卡不做界面。** S01/S02 的视觉、⌘L 的键绑定、设置页那个 5/10/30 分钟下拉框都归 TC-103。
本卡交付的是它们要调用的 Rust 层：哈希、锁定判定、空闲与唤醒的触发机制，以及注册好的 Command。

## 范围

**做**：

1. `migrations/0002_admins.sql`（version 2）：`admins` 单管理员表 + `app_meta` 里的空闲阈值键。
2. `infra/password.rs`：Argon2id 哈希 / 校验 / 「参数是否偏旧」判定；盐由 SecRandom 给（ADR-0011 §1）。
3. `domain/admin.rs`：口令策略（≥10 位，按字符数）。纯函数。
4. `domain/auth.rs`：失败计数与 60 秒锁定的**纯状态机**，时钟注入，不 sleep。
5. `infra/admins.rs`：`admins` 行的读写 + `unlock()` 的组合流程（开库 → 读行 → 判定 → 回写 → 决定留库还是关库）。
6. `infra/wake.rs`：`LockWatch`——单调时钟空闲计时 + 分布式通知中心唤醒观察者 + 锁定回调。
7. `commands/auth.rs`：`unlock` / `lock_now` / `change_password` / `set_idle_lock_minutes` / `admin_status`，注册进 `lib.rs`。
8. 删掉 `error.rs` 里 `BadCredentials` / `Locked` 的 `allow(dead_code)`（构造方就位）。

**不做**：

- 界面与文案渲染（TC-103）。本卡只保证错误码与剩余秒数拿得到。
- 「忘记密码」的备份恢复流程（TC-104）；本卡只在 `change_password` 上要求原口令。
- 真实合盖唤醒的真人验证（TC-103 验收标准里那条「真人起界面」一起做，见标准 12 的说明）。
- 多管理员、口令有效期、二次验证——设计文档写死 v1 单管理员。
- 把管理员口令变成库加密密钥（ADR-0011 §9 明确它不是，也别顺手改成是）。

## 验收标准

每条都要在验收时给出**实际观察到的输出**，不接受「已验证」。

**1. 供应链：argon2 子树 12 个 crate，零 C、零 build.rs；产物加密栈不变。**
`cargo tree -i argon2` 列出的依赖闭包恰好 12 个 crate；逐个 `find` 扫 `.c`/`.h`/`.S`/`build.rs`
结果全为 0；门禁第 9 项 `otool -L` 仍是有 `Security.framework`、无 `libcrypto`/`libssl`。
*为什么必须有这条*：ADR-0011 §1 的选型结论就是「纯 Rust、不新增 C 编译单元」。若实现时
把默认特征打开（`getrandom` 会进来），这条会立刻红，而不是等到发布时才发现产物多了东西。

**2. 口令策略按字符数，不按字节数。**
`check_password_policy` 对 9 个 ASCII 字符返回错误、对 10 个 ASCII 字符通过；
对 10 个汉字（30 字节）通过、对 9 个汉字（27 字节）返回错误。
*为什么必须有这条*：设计文档写的是「至少 10 位」。HR 用中文口令很常见，按字节算会把
「10 个汉字」这种更强的口令拒掉，而且报错文案（「至少 10 位」）与实现不符没人看得出来。

**3. 库里存的是 Argon2id PHC 串，且明文口令不在库文件里。**
建管理员后读回 `pwd_hash`，以 `$argon2id$` 开头；对 `data.db` 原始字节 `grep` 该口令，
命中 0 次（SQLCipher 是整页加密，这条同时钉住「加密真的生效」）。
*为什么必须有这条*：设计文档 179-183 的原文是「绝不明文或 MD5 保存」。只断言前缀
挡不住「哈希算对了但顺手把口令也写进日志/别的列」这类回归。

**4. 同一口令两次建档得到两个不同的 PHC 串。**
用两个不同的临时 Keychain/数据目录各建一次管理员，口令相同，两条 `pwd_hash` 字符串不等，
但都能验过同一口令。
*为什么必须有这条*：盐的意义就是「同一口令的两个哈希不长得一样」，否则一张泄露的哈希
能认出另一台机器上的同一口令。spike 里这条是打印出来的，代码里必须变成断言。

**5. 口令错误返回 `BAD_CREDENTIALS`，且错误里不含口令本身。**
错口令解锁 → 前端收到的 JSON `code == "BAD_CREDENTIALS"`；把该 JSON 与 `Display`/日志行
一起 `grep` 口令明文，命中 0 次。
*为什么必须有这条*：红线 2 的口径是「不把敏感数值写进日志与错误上报」，口令同理。
`user_message()` 是给人看的固定文案，剩余次数走独立字段而不是拼进文案。

**6. 连续 5 次错 → 第 6 次是 `LOCKED`，且锁定期内不延长、计数不动。**
注入时钟：错 1..5 依次返回 `BAD_CREDENTIALS` 且剩余次数 4/3/2/1/0；第 6 次返回
`LOCKED(60)`；把时钟推进 30 秒再试，仍是 `LOCKED(30)`，且库里的 `failed_attempts`
与 `locked_until` 两个值与第 6 次后完全相同。
*为什么必须有这条*：设计文档 1023 的文案是「连续错误 5 次将锁定 60 秒」。若锁定期内的
尝试会重置 `locked_until`，一个不停乱按的人会被永久锁死，HR 会以为应用坏了。

**7. 锁定窗口过期后首次尝试清零计数，给回 5 次预算。**
承上：把时钟推到 60 秒整之后用**正确**口令解锁成功，且 `failed_attempts` 变 0、`locked_until` 变 NULL。
*为什么必须有这条*：否则「锁定 60 秒」实际变成「锁定之后每次开机都只剩 4 次」，
计数永远不清零，与文案的「连续」二字不符。

**8. 锁定计数跨进程存活。**
错 3 次后 `close()` 库、用同一个数据目录与同一个 Keychain 重建 `DbState` 再错 1 次，
返回的剩余次数必须是 1（不是 3）。
*为什么必须有这条*：设计文档 1023 明写「无法通过重装应用绕过」。计数放内存的话，
`⌘Q` 重开就清零，这句话就是假的。

**9. 参数偏旧时自动升级重哈希。**
手工把库里的 `pwd_hash` 换成 `m=2048,t=2,p=1` 的串（spike 实测过这组能跑），
用正确口令解锁 → 成功，且返回体标记「已重哈希」；再读库，串里的参数段等于代码常量
`m=19456,t=2,p=1`；用旧口令再验仍通过。
*为什么必须有这条*：ADR-0011 §2 钉的参数是 2026 年的下限，硬件会变强。没有这条，
「以后调强参数」需要一次全库迁移脚本；有这条，下次解锁顺手就升级了。

**10. 空闲到期触发锁定并关库；活动信号重置计时。**
把阈值设成 1 秒（阈值是参数，不依赖真等 10 分钟），不 touch → 回调触发且
`DbState::is_open() == false`；期间每 0.5 秒 touch 一次 → 同样时长内不触发。
*为什么必须有这条*：设计文档 795 的「空闲 10 分钟自动锁定」是安全模型的一部分。
用单调时钟 + 可注入阈值，才能在没有 GUI、没有真 10 分钟的测试里证明它工作。

**11. 唤醒立即锁定，且这条有自动化测试（由子进程发通知）。**
测试起 `current_exe()` 子进程发 `NSWorkspaceDidWakeNotification`，父进程的观察者在
超时前收到并触发锁定（连接关闭）。退出码/断言写进验收记录。
*为什么必须有这条*：ADR-0011 §7 实测「选错通知中心不会报错，只会永远不锁」；
§8 实测「分布式中心不自投递」——所以在测试进程里自己发通知会得到**假绿**，
必须走跨进程这条真实路径。
**验收中补的口径**（发卡时没预见到）：投递那半必须在**有 `main()` 的测试**里做，
放在 `tests/wake_delivery.rs`（`Cargo.toml` 里 `harness = false`）。原因是分布式通知
只由**主线程的 runloop** 派发，而 libtest 把 `#[test]` 跑在工作线程上
（ADR-0011 §10 的 A/B/C/E 四组实验 + `pthread_main_np()` 实测）。
「计数变了 → 关库」那半仍在 `#[test]` 里（`inject_wake()` 触发，确定性强、CI 里必跑）。

**12. 拿不到分布式中心时测试明确跳过而不是挂住。**
观察者注册路径要有超时；在无 GUI 会话（或 CI）里跑，测试要么通过要么打印
`SKIP: <原因>` 后返回，不能卡到门禁超时。
跳过分支要**能被观察到**，不能只是「写在代码里」：`PAYSLIP_WAKE_TEST_SKIP=1` 把
「观察者没挂上」这个判定条件手动置真，走的仍是真实条件下同一段跳过代码。
*为什么必须有这条*：TC-101 已经踩过一次「依赖 GUI 会话」的坑（ADR-0008/0009）。
唤醒通知同样可能在没有窗口会话的环境里拿不到投递；一条会挂死的测试比一条跳过的测试糟得多。
**真人合盖再打开必须出现锁屏**这件事本卡测不了（不想擅自睡使用者的机器），
转成 TC-103 的验收标准，与 TC-000 遗留的「真人起界面确认红绿灯」同批做。

**13. 改口令必须验原口令，且不改锁定计数。**
`change_password(旧, 新)`：旧口令错 → `BAD_CREDENTIALS` 且库里哈希不变；旧口令对 →
新哈希可验、旧口令验不过；两种情况下 `failed_attempts`/`locked_until` 均不变。
*为什么必须有这条*：设计文档 313-314 写「密码重置需原密码」。计数被顺手清零的话，
攻击者猜对一次旧口令就能同时拿到「改口令」和「重置锁定预算」两件事。

**14. `BAD_CREDENTIALS` / `LOCKED` 不再是死码。**
`error.rs` 里这两个变体的 `allow(dead_code)` 已删；`cargo clippy --all-features -- -D warnings`
全绿（门禁第 5 项）。
*为什么必须有这条*：TC-100/TC-101 结案时把「未接线」都标在 `allow` 注释上。
本卡是它们的构造方，留着注释就是在文档里留一句过期的谎。

**15. 门禁 9 项全绿。**
`bash scripts/gate.sh`（`FAST=0`）9 项退出码全 0，含新增依赖后的 `cargo test` 与 `npm run build`。
*为什么必须有这条*：TC-100 结案时定下的口径——门禁是验收的第一道，不是最后一道。

## 评审记录

验收日期：2026-11-11 · 结论：**通过**（15 条全过；标准 11/12 在验收中补了口径，见文末「spec 修订」）

以下每条写的是**实际观察到的输出**，不是「已验证」。
测试总数 `57 passed; 0 failed`（`cargo test` 与门禁第 7 项两次一致），另加
`tests/wake_delivery.rs` 这个 `harness = false` 的跨进程测试。

**1. 供应链：argon2 子树 12 个 crate、零 C、零 build.rs；产物加密栈不变**
—— `cargo tree -e normal` 按缩进切出 `argon2 v0.6.0` 的子树，唯一 crate 恰好 **12** 个：

```text
argon2 0.6.0        base64ct 1.8.3     blake2 0.11.0     block-buffer 0.12.1
cmov 0.5.4          crypto-common 0.2.2  ctutils 0.4.8   digest 0.11.3
hybrid-array 0.4.15 password-hash 0.6.1  phc 0.6.1       typenum 1.20.1
```

逐个在 `~/.cargo/registry/src/*/NAME-VER` 里 `find -name '*.c' -o -name '*.h' -o -name '*.S' -o -name 'build.rs'`
→ 12 个全是 0，`TOTAL_C_UNITS=0`。加密栈（门禁「加密栈链接」项）实测：

```text
/System/Library/Frameworks/Security.framework/Versions/A/Security (compatibility version 1.0.0, current version 61901.120.67)
  ✓ 加密走 CommonCrypto，未链 OpenSSL
```

**2. 口令策略按字符数** —— `domain::admin::tests::policy_counts_characters_not_bytes` 通过：
9 个 ASCII 拒绝、10 个 ASCII 通过、10 个汉字（30 字节）通过、9 个汉字（27 字节）拒绝。
`error_names_the_rule_but_never_the_password` 通过：错误里只有字符数，没有口令本身。

**3. 库里是 Argon2id PHC 串、库文件里没有明文** ——
`infra::password::tests::password_hash_is_argon2id_phc_with_the_pinned_params`：产出串前缀
`$argon2id$v=19$`、参数段 `m=19456,t=2,p=1`，且自产的串自检为「参数不旧」。
`infra::admins::tests::admin_row_holds_an_argon2id_hash_and_the_db_file_holds_no_plaintext`：
读回 `pwd_hash` 以 `$argon2id$` 开头，对 `data.db` 原始字节搜哨兵口令 → **0 命中**。

**4. 同一口令两次建档得到两个不同 PHC 串** ——
`infra::password::tests::same_password_yields_different_hashes_that_both_verify`（两串不等、都验得过、都不标 `needs_rehash`）
+ `infra::admins::tests::the_same_password_hashes_differently_on_two_machines`
（两个临时钥匙串 + 两个数据目录，各建一次管理员，两条哈希不等）。

**5. 口令错是 `BAD_CREDENTIALS` 且不含口令** ——
`infra::admins::tests::wrong_password_is_bad_credentials_and_never_carries_the_password`：
错误码等于 `BAD_CREDENTIALS`，JSON payload 与 `Display` 文本里搜哨兵口令 → 0 命中；
剩余次数走 `admin_status` 的独立字段，不拼进文案。
底层两条边界：`password::tests::wrong_password_is_a_negative_not_an_error`（口令错是
`Ok(password_ok=false)` 而不是 `Err`）、`password::tests::unreadable_hash_string_is_an_internal_error_not_a_wrong_password`
（哈希串读不出是 `INTERNAL`，不冒充「你口令错了」）。

**6. 连续 5 次错 → 锁定 60 秒，锁定期内不延长、计数不动** ——
纯状态机：`domain::auth::tests::five_consecutive_failures_lock_for_sixty_seconds`
（第 1..5 次剩余 4/3/2/1/0，第 6 次 `Locked(60)`）、`attempts_inside_the_window_change_nothing`
（推进 30 秒再试仍是 `Locked(30)`，两个计数一字未动）、`status_matches_the_lockout_rules`、
`absurd_counts_do_not_panic`。落库版：`infra::admins::tests::five_failures_lock_and_the_window_freezes_the_counters`
断言库里 `failed_attempts` / `locked_until` 两列在第 6 次前后完全相同。
判定口径写死在 `domain/auth.rs` 与 `error.rs:35` 的文案里：**第 5 次失败仍返回
`BAD_CREDENTIALS` 且 `remaining = 0`，锁定从第 6 次尝试起生效** —— 对应设计文档 1023
「连续错误 5 次**将**锁定 60 秒」（「将」= 第 5 次错完还没锁）。
*验收中修正*：这两条断言最初与实现的判定口径不一致（以为第 5 次就返回 `LOCKED`），
跑出来是红的。没有把断言调绿了事，而是回到设计文档原文定口径，再把口径写进
`domain/auth.rs` 的注释与 `BadCredentials` 的文案，测试随之对齐。

**7. 锁定窗口过期后预算重置** —— `domain::auth::tests::expired_window_gives_back_the_full_budget`
+ `infra::admins::tests::the_lockout_expires_and_the_budget_resets`：推到 60 秒整之后用正确口令
解锁成功，库里 `failed_attempts = 0`、`locked_until = NULL`。

**8. 锁定计数跨进程存活** —— `infra::admins::tests::counters_survive_a_process_restart`：
错 3 次 → `close()` → 同一数据目录 + 同一钥匙串重建 `DbState` → 再错 1 次 → 剩余次数 **1**（不是 3）。

**9. 参数偏旧自动升级重哈希** ——
`infra::password::tests::older_params_hash_verifies_and_is_flagged_for_rehash`：
`m=2048,t=2,p=1` 的旧串用当前策略实例验 → `password_ok = true` 且 `needs_rehash = true`；
重哈希后参数段等于 `m=19456`；旧串本身仍可验（升级中途断电不会把人锁死）。
`infra::admins::tests::an_outdated_hash_is_upgraded_during_a_successful_unlock`：解锁成功
**且库里那一列真的被改写**。机制依据（读过源码）：`password-hash 0.6.1` 的
`PasswordVerifier::verify_password` 用 `T::Params::try_from(hash)` 取**串里**的参数，
不用实例参数（lib.rs:197-224）。另有 `missing_params_are_treated_as_outdated`：
串里不写 `m/t/p` 也算偏旧（`get_decimal` 不会补默认值）。

**10. 空闲到期锁库；活动信号重置** —— `infra::wake::tests::idle_timeout_locks_the_database`
（阈值 1 秒、不 touch → 回调触发且 `DbState::is_open() == false`）、
`touching_defers_the_idle_lock`（每 0.5 秒 touch，同样时长内不触发）、
`changing_the_threshold_takes_effect_immediately`（`set_idle_secs` 下一片就生效）。
阈值默认值另有 `infra::admins::tests::idle_threshold_defaults_to_ten_and_round_trips`
（默认 10 分钟，写进 `app_meta` 再读回一致）。

**11. 唤醒立即锁定，且有跨进程自动化测试** —— 拆成两段，各测各的：

- **投递段**（另一个进程发 → 本进程收到）：`tests/wake_delivery.rs`，门禁日志里的实际输出：

  ```text
       Running tests/wake_delivery.rs (src-tauri/target/debug/deps/wake_delivery-a7b92941824a0c7a)
  [子进程] 已向分布式通知中心发出 NSWorkspaceDidWakeNotification
  PASS: 另一个进程发的唤醒通知触发了锁定，加密库连接已关闭
  ```

  退出码 0。父进程用真 `DbState` + 真临时钥匙串开真加密库，锁定阈值 **3600 秒**
  （最多等 5 秒，所以「锁了」只可能是唤醒那条腿），锁定回调只做 `db.close()`；
  断言是 `locked == true && !db.is_open()`，两个都为真才打 PASS。
- **触发段**（计数变了 → 关库）：`infra::wake::tests::a_wake_signal_locks_the_database`
  （`inject_wake()` 模拟 CF 回调打进来一次）：阈值 600 秒下触发锁定、**只锁一次**、
  `is_open() == false`；`touch()` 之后下一条信号仍能触发；`wake_count() == 1`、
  `lock_count() == 1` 与闭包计数一致（对不上就说明「锁了一次」是巧合）。

**为什么投递段不在 `#[test]` 里**（本卡验收查出来的，发卡时没料到）：
分布式通知的回调**只在主线程的 runloop 上**被派发。四组对照实验（`/tmp/wake-spike`，同一台机器同一个会话）：

| 实验 | 注册线程 | 泵 runloop 的线程 | 结果 |
|---|---|---|---|
| A | 主 | 主 | 收到（回调 `ThreadId(1)`，EXIT=0） |
| B | 后台 | 后台 | 0 条（EXIT=3） |
| C | 后台 | **主** | **收到**（EXIT=0，回调仍 `ThreadId(1)`） |
| E | 主 | 后台 | 0 条（EXIT=3） |

→ 约束不是「必须在主线程注册」，而是「必须由**主线程的 runloop** 派发」。
真应用里主线程由 Tauri/Cocoa 的主事件循环泵着，所以 `LockWatch` 在后台线程注册是成立的（C 就是它的形状）。
而 libtest 把 `#[test]` 跑在**工作线程**上（`/tmp/lt` 实测：即使 `--test-threads=1`，
`pthread_main_np()` 仍返回 0、`ThreadId(2)`、线程名 `am_i_main`），单元测试里没人泵主线程 runloop
→ 这条断言在 `#[test]` 里永远只能 SKIP，留着一个只会 SKIP 的「绿」比没有更糟。
**由此改产码**：`wake.rs` 的观察线程原先每 0.25 秒泵一次自己的 runloop —— 按 E 那格，
那个 runloop 对唤醒通知完全无用，已删成 `thread::sleep`；模块文档与 `register_wake_observer`
文档里「必须在跑 runloop 的那个线程上注册」这句错的断言也已按 A/B/C/E 改写。

**12. 拿不到分布式中心时跳过而不是挂住** —— 跳过分支实测（开发机有窗口会话，真实条件撞不到，
用 `PAYSLIP_WAKE_TEST_SKIP=1` 把判定条件手动置真；走的仍是 `!wake_registered()` 那同一段代码）：

```text
$ /usr/bin/time env PAYSLIP_WAKE_TEST_SKIP=1 cargo test --test wake_delivery
SKIP: 唤醒观察者未挂上（无窗口会话或分布式中心不可用），跳过投递断言
        0.44 real         0.07 user         0.08 sys
退出码=0
```

注册路径本身的超时：`LockWatch::start` 握手等观察线程挂完，上限 1 秒（`wake.rs:241-242`），
挂不上只是 `wake_registered() == false`，`lib.rs:53` 启动时打一行 warn，空闲锁定那条腿照常守。

**13. 改口令必须验原口令、不改计数** ——
`infra::admins::tests::changing_the_password_requires_the_old_one_and_leaves_the_counters_alone`：
旧口令错 → `BAD_CREDENTIALS` 且库里哈希不变；旧口令对 → 新哈希可验、旧口令验不过；
两种情况下 `failed_attempts` / `locked_until` 均不变。

**14. `BAD_CREDENTIALS` / `LOCKED` 不再是死码** —— `error.rs:30-41`：两个变体头上的
`#[allow(dead_code)]` 已删，注释改成「TC-102 已接线：`infra::admins::unlock`」。
门禁「Rust 静态检查」项（`cargo clippy --all-features -- -D warnings`）→ 「✓ Rust 静态检查」。
剩下的 `allow` 都属于别的卡（`Duplicate` 的入口归 TC-103、TC-200/201/202、TC-400/500），逐个带注释。
*编号勘误*：spec 里写的「门禁第 5 项 / 第 9 项」是 TC-000 时期的编号；门禁现在是 9 项，
clippy 是第 6 项、加密栈链接是第 8 项（中间多了一项「Token 引用可解析」）。按名字认，不按序号认。

**15. 门禁全绿** —— `FAST=0 bash scripts/gate.sh` → `GATE_EXIT=0`，末行「✓ 门禁全绿」，
9 项逐个 ✓：Rust 格式 / 前端类型检查 / 设计 Token / Token 引用可解析 / 敏感数据未进 git /
Rust 静态检查 / Rust 单测 / 加密栈链接 / 前端构建。其中单测项：

```text
test result: ok. 57 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 4.93s
     Running tests/wake_delivery.rs …
PASS: 另一个进程发的唤醒通知触发了锁定，加密库连接已关闭
```

### spec 修订（验收中改的，均已写回上文）

1. **标准 11** 补了「投递段必须在有 `main()` 的测试里」这条口径与原因（`harness = false`）。
   原文只要求「测试起 `current_exe()` 子进程」，没料到 libtest 的工作线程问题。
2. **标准 12** 补了「跳过分支要能被观察到」与 `PAYSLIP_WAKE_TEST_SKIP=1` 这个入口。
3. 标准 14 的门禁项编号勘误（见上）。

### 本卡测不到 / 转出的项

- **真人合盖再打开必须出现锁屏**：本卡不擅自睡使用者的机器，转 TC-103 验收标准，
  与 TC-000 遗留的「真人起界面确认红绿灯」同批做。
- **`create_admin` 的 Command 入口与 S01 首启向导**：代码与测试都在（`unlock_before_init_reports_not_initialized_and_a_second_admin_is_refused`），
  但入口界面归 TC-103，所以 `Duplicate` 仍带 `allow(dead_code)`。
- **ADR-0011 的两处措辞已更正**：§7 的「零新 crate」→ 实际是**新增两条直接依赖声明**
  （`argon2 0.6.0` 是新 crate；`core-foundation-sys 0.8` 本就是 `core-foundation 0.10.1` 的
  传递依赖、已在 Cargo.lock，提成直接依赖不新增 crate）。唤醒投递的 A/B/C/E 与 libtest
  线程实测已追加为 **ADR-0011 §10**。
- **CI 首跑**：仓库至今零 commit，「无窗口会话里这条测试会不会 SKIP」的更强证据要等首次 commit 之后。
