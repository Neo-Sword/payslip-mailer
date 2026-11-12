# TC-101 · macOS Keychain 接入

状态：accepted（评审记录见文末）
依赖：无（P1 起点；TC-100 / TC-104 / TC-400 挂在本卡之后）
设计依据：设计文档 ch.5（`Keychain (login) # service = cn.payslipmailer.*`、
「凭据只进 Keychain，禁止落库」）· ch.12 S15（错误码三要素）·
ADR-0002（未签名 → ACL 每次升级重置）· ADR-0004（错误词表先行）· ADR-0008（无 GUI 会话要能跑）·
**ADR-0009（本卡的库选型与测试策略）**

## 目标

给 P1 后续三张卡一个**可测的**秘密存取层：写入 / 读取 / 删除三类条目，
错误能区分「没有这个条目」与「钥匙串拒绝了我」，
且这一切在**临时钥匙串**上验证过——不碰开发机的登录钥匙串，不在无 GUI 会话里挂死。

## 范围

**做**：`infra::keychain` 模块——`Secret` 新类型（`Debug` 不打印内容）、
三类条目（db-key / smtp-credential / oauth-token）的 service+account 命名、
`put` / `get` / `delete` 三个操作、`KEYCHAIN_MISSING` 新错误码、
临时钥匙串测试基座 + `disable_user_interaction` 守卫。

**不做**：任何界面（引导 HR 输入授权码是 TC-103 / TC-500 的事）；
OAuth2 授权流程本身（本卡只存 token，不发起授权）；
SQLCipher 连接（TC-100）；备份导出（TC-104）；
iCloud 同步钥匙串与 DataProtectionKeychain（ADR-0009 第 4 条）；
真实升级后的 ACL 演练（TC-601）。

## 验收标准

每条都可证伪：能跑一条命令，或能在测试输出里指出一行。

1. **单测全绿**：`cargo test keychain` 退出码 0，且测试名可逐条对上下面 2–7 条。
2. **二进制字节往返**：写入含 `0x00` 与非 UTF-8 字节的 32 字节，读回逐字节相等。
   *为什么必须有这条*：DB key 是随机字节，任何「顺手 String 化」的实现会在
   写入时成功、下次开机打开库时失败——那是 P1 最难查的一种失败。
3. **覆盖写不产生重复条目**：同一 service+account 连续写两次，读回第二次的值，
   且按 `Limit::All` 搜索命中数 **== 1**。
   *为什么*：ADR-0009 第 2 条点名了「find 失败就 add」会产生重复条目的坑，这条是它的反证。
4. **未命中是错误码不是崩溃**：读一个不存在的 account，返回 `KEYCHAIN_MISSING`，
   不 panic；`user_message()` 含「重新」类下一步引导。
5. **秘密不进日志也不进 payload**：`Secret` 的 `Debug` 输出不含明文内容（只报字节数）；
   写入 ASCII 哨兵后，错误的 `Display` 与 JSON payload 都不含该哨兵。
   这是红线 2「绝不把敏感值写进日志」在秘密这条线上的机器化。
6. **测试不污染登录钥匙串**：测试结束后，用系统命令
   `security find-generic-password -s cn.payslipmailer.db-key` 在登录钥匙串里**查不到**测试写入的条目。
   *为什么必须有这条*：反向验证——如果测试其实写进了登录钥匙串，上面几条照样全绿。
7. **无 GUI 会话不挂**：`bash scripts/gate.sh` 在后台任务（无 GUI 会话）里退出码 0，
   不弹框、不超时。测试全程持 `disable_user_interaction()` 锁。
8. **命名不漂移**：单测读 `tauri.conf.json` 的 `identifier`，断言 service 前缀与之一致
   （`cn.payslipmailer.`）。设计文档写的命名靠测试守，不靠人记。
9. **错误码契约不破**：`cargo test error` 仍全绿；`KEYCHAIN_MISSING` 出现在
   `Serialize` 的 code 映射里，且 `user_message()` 满足三要素（发生了什么 / 影响 / 下一步）。

## 评审记录

验收日期：2026-11-11 · 结论：**通过**（9 条全过；两项本卡测不到的记在末尾）

以下每条写的是**实际观察到的输出**，不是「已验证」。

**1. 单测全绿** —— `cargo test keychain` → `test result: ok. 9 passed; 0 failed; 9 filtered out`。
9 条 = 本模块 8 条 + `error::tests::keychain_missing_is_a_distinct_code_with_its_own_next_step`
（被同一个过滤词命中）。测试名与标准的对应：

| 标准 | 测试名 |
|---|---|
| 2 | `binary_bytes_roundtrip_identically` |
| 3 | `overwrite_updates_in_place_without_duplicating` |
| 4 | `missing_entry_is_keychain_missing_not_panic` |
| 5 | `secret_never_appears_in_debug_or_error_output` |
| 6 | `tests_run_against_a_temporary_keychain_not_the_login_one` |
| 8 | `service_prefix_matches_bundle_identifier` |
| 额外 | `delete_is_idempotent`、`accounts_under_one_service_are_independent` |

**标准 7 没有对应测试名**：「不挂」只能靠整体运行观察 + 每条测试内的锁断言，写不成一条断言。

**2. 二进制字节往返** —— 32 字节含 `0x00`、`0xff`、非法 UTF-8（`0xc3 0x28`）写入读回逐字节相等；
测试内 `assert_eq!(key.len(), 32)` 同时钉住设计文档 ch.5 的密钥长度。

**3. 覆盖写不产生重复条目** —— 连写两次后读回 `"second-授权码"`，
且 `match_count(...)`（`Limit::All` + `load_attributes`）返回 **1**。
这条是 ADR-0009 第 2 条那个坑的反证，也是本卡唯一能证它的地方：
只比对读回值抓不出重复（读到哪一份都可能碰巧对）。

**4. 未命中是错误码不是崩溃** —— `missing_entry_is_keychain_missing_not_panic` 通过：
`matches!(err, AppError::KeychainMissing)` 且 `user_message()` 含「重新」。

**5. 秘密不进日志也不进 payload** —— 哨兵 `"SENTINEL-勿外泄-授权码-9f3a"`：
`Debug` 输出为 `Secret(30 字节·内容不打印)`（不含哨兵、含字节数）；
未命中错误的 JSON payload 不含哨兵也不含 account。
*验收中修正*：原断言写的是「payload 不含『授权码』」，会**假失败**——
`KEYCHAIN_MISSING` 的引导文案里合法地有「更新发件授权码」，那是给 HR 的下一步不是泄漏。
改成查 account（它编码 SMTP 行标识 / 邮箱，设计文档同样要求邮箱不进日志）。

**6. 测试不污染登录钥匙串** —— 进程内：同一 service+account 在 `KeychainStore::login()` 里
返回 `Err(KeychainMissing)`（account 用进程唯一名，即使开发机真初始化过也不撞）。
进程外，测试跑完后：

```text
$ security find-generic-password -s cn.payslipmailer.db-key
security: SecKeychainSearchCopyNext: The specified item could not be found in the keychain.
退出码=44
$ security dump-keychain | grep -c "cn.payslipmailer"
0
$ ls $(python3 -c 'import tempfile;print(tempfile.gettempdir())') | grep -c xinyou-keychain-test
0        # 临时钥匙串文件已被 Drop 清掉，无残留
```

**7. 无 GUI 会话不挂** —— `bash scripts/gate.sh` 在后台任务里 `GATE_EXIT=0`，8 项全 ✓，
末行「✓ 门禁全绿」；18 条单测 `finished in 0.11s`，无等待。
每条 keychain 测试开头断言 `SecKeychain::user_interaction_allowed() == false`，全部通过。
锁用进程级 `OnceLock` 而不是逐测试取放：这把锁是进程级的，先结束的测试 Drop 时
会把开关恢复成「允许」，正在跑的另一个测试就暴露在可能弹框下（并行竞态）。
*保留意见*：后台任务仍属于本机登录会话，不是真正 headless。更强的证据是 CI runner，
但仓库至今零 commit，CI 首跑要等首次 commit 之后。

**8. 命名不漂移** —— `service_prefix_matches_bundle_identifier` 读 `tauri.conf.json`，
断言 `identifier` 以 `cn.payslipmailer.` 开头，并逐个比对三类 service 名与设计文档 ch.5 一致。

**9. 错误码契约不破** —— `cargo test error` → `test result: ok. 8 passed; 0 failed`；
`KEYCHAIN_MISSING` 在 `Serialize` 的 code 映射里，且与 `KEYCHAIN` 不同码
（测试显式断言 payload 不含 `"code":"KEYCHAIN"`，防两个码塌成一个）。

### 顺带关掉 ADR-0009 的遗留项

`error.rs` 顶部的 `#[allow(dead_code)]` 已从「整个 enum 标一次」收窄为「逐个变体标，
注释写明哪张卡会构造」。已接线的 `Keychain` / `KeychainMissing` / `Io` 不标。
**反向验证它有牙**：临时摘掉 `Locked` 的标注 → clippy 报
`error: variant 'Locked' is never constructed`；还原后 `cargo clippy -D warnings` 退出码 0。

### 本卡测不到的两件事

1. **真实 ACL 重置**：临时钥匙串里没有「升级后签名变了」这回事，
   `put` 的「只有 not-found 才 add」分支在测试里只覆盖了「条目不存在」一侧，
   「ACL 拒绝」一侧只能靠 TC-601 真机演练（ADR-0009 已列为负面）。
2. **`Secret` 不做内存清零**：Drop 不 `zeroize`，秘密会留在已释放的堆内存里直到被复用。
   设计文档没要求，本卡不做；若将来要求，是一次显式改动。

### 实现期发现（记下来免得下次再踩）

- 字节字符串字面量 `b"码一"` 只允许 ASCII，中文写哨兵直接编译失败（23 条错误），改用 `"码一".as_bytes()`。
- `SecKeychain::user_interaction_allowed()` 返回 `Result<bool, base::Error>`，
  而 `base::Error` 没实现 `PartialEq`，所以 `assert_eq!(..., Ok(false))` 编译不过，只能用 `assert!`。
- `gate.sh | tail -60` 会把退出码换成 `tail` 的：那次门禁末行明明是「✗ 有门禁未通过」，
  通知里却是退出码 0。**门禁的退出码必须不带管道取**，否则红会显示成绿。
  当时失败原因是 clippy 报 `match_single_binding`（`get` 里写了个单分支 match），已改。
