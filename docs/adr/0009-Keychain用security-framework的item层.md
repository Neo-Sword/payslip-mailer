# ADR-0009 · Keychain 访问：security-framework 直连 + item 层 + 临时钥匙串测试

日期：2026-11-11 · 状态：accepted（TC-101 验收通过，评审记录见该 spec 文末）

## 背景

TC-101 要存三类秘密：打开 SQLCipher 的 **32 随机字节** DB key、SMTP 授权码、OAuth token。
三条约束把选型空间压得很窄：

1. **未签名分发**（ADR-0002）：钥匙串条目的 ACL 绑代码签名，覆盖安装后旧条目会被判为
   「另一个程序在访问」。**「读不到」是正常路径**，不是异常。
2. **无 GUI 会话也要能跑**（ADR-0008 的教训）：任何会弹授权框的调用，在 CI 与后台任务里
   不是失败而是**挂死**——比失败更糟。
3. **测试不能污染开发机的登录钥匙串**，也不能要求测试机预先存在某个条目。

## 决策

### 1. 用 `security-framework` 3.7 直连，不用 `keyring`

`keyring` 最新版在 macOS 上走的是 `apple-native-keyring-store`（docs.rs 的可选依赖列表），
抽象目标是跨平台。本项目 macOS-only（TC-000 范围明确「不做 Windows」），
跨平台接口买不到任何东西，反而**拿不到两个我们必须有的能力**：

- 指定钥匙串（测试隔离靠它）；
- `SecKeychain::disable_user_interaction()`（把「会弹框」变成「立刻报错」，靠它保证 CI 不挂）。

### 2. 用 `item` 模块（SecItem API），不用 `passwords` 便捷函数

这是本 ADR 最重要的一条，两个理由都是从源码里读出来的，不是推测：

| 候选 | 问题 |
|---|---|
| `passwords::{set,get,delete}_generic_password` | 只作用于**默认搜索列表**，无法指定钥匙串 → 测试无法与登录钥匙串隔离 |
| `os::macos::passwords::SecKeychain::set_generic_password` | 内部是 `match find { Ok→set_password, _→add }`：**把「无权限」也当成「不存在」**去 add。ACL 拒绝时（正是 ADR-0002 的升级场景）会写出**重复条目**，之后每次读都命中不确定的一份。且它走 Carbon 时代的 `SecKeychainAddGenericPassword`，不支持 `kSecAttrSynchronizable` |
| `item::{ItemAddOptions, ItemSearchOptions, update_item}` | ✅ 三条路都能定位具体钥匙串；错误码原样返回，不吞掉再兜底 |

用到的 API（全部在 `security-framework-3.7.0/src/item.rs` 与 `os/macos/keychain.rs` 中核对存在）：

```text
ItemAddOptions::new(ItemAddValue::Data{class,data})   .set_service .set_account_name
                .set_location(Location::FileKeychain(SecKeychain))  .add()
ItemSearchOptions ::class .service .account .keychains(&[SecKeychain])
                .limit(Limit::Max(1)) .load_data(true) .cloud_sync(false)
                .search() -> Vec<SearchResult>   /  .delete()
update_item(&ItemSearchOptions, &ItemUpdateOptions)
SecKeychain::default() / ::open(path) / ::unlock(Option<&str>)
Keychain::CreateOptions::new().password(..).prompt_user(false).create(path) -> SecKeychain
SecKeychain::disable_user_interaction() -> KeychainUserInteractionLock（Drop 时恢复）
```

### 3. 命名沿用设计文档 ch.5，实例区分放 account

设计文档写的是 `Keychain (login) # service = cn.payslipmailer.*`，
与 `tauri.conf.json` 的 `identifier: "cn.payslipmailer.desktop"` 同前缀。

| kind | service | account |
|---|---|---|
| DB key | `cn.payslipmailer.db-key` | `default` |
| SMTP 授权码 | `cn.payslipmailer.smtp-credential` | `smtp_settings` 的行标识 |
| OAuth token | `cn.payslipmailer.oauth-token` | `provider + 邮箱` |

**为什么实例放 account 而不是再造 service**：设计文档里 `admins 1─N smtp_settings`，
且 `credential_ref` 字段存的「只是 Keychain 条目名」——一个 service 名装不下多行配置。
一个 kind 一个 service 的好处是审计与清理可以按前缀一把捞：
`security find-generic-password -s cn.payslipmailer.db-key`。

前缀一致性由**单测**守住（读 `tauri.conf.json` 比对），不靠人记住。

### 4. 显式声明「不同步 iCloud」

`ItemSearchOptions::cloud_sync(false)`，写入不选 `Location::DataProtectionKeychain`。
理由不是「红线要求不同步」这么轻：iCloud 同步与非同步钥匙串是**两个独立的 store**，
同一个 service/account 可以在两边各存一份，靠默认值等于把「读到哪一份」交给系统。
显式写死，未来有人想开同步时是一次显式改动（并要另写 ADR）。

### 5. 测试用临时钥匙串，与生产跑同一条代码路径

`CreateOptions::new().password(一次性口令).prompt_user(false).create(tempdir/测试钥匙串)`，
生产只是把句柄换成 `SecKeychain::default()`（登录钥匙串）。
**两条路径共用同一套 put/get/delete 实现**，测试才有意义；
测试全程持有 `disable_user_interaction()` 锁。

### 6. 「读不到」必须是可区分的错误码

新增 `AppError::KeychainMissing`（code `KEYCHAIN_MISSING`），
映射自 `errSecItemNotFound`；其余 OSStatus 归既有 `AppError::Keychain(detail)`。

理由：ADR-0002 要求「读不到旧条目」走引导重新初始化的路，
而前端只能按 `code` 分支。若两种情况共用 `KEYCHAIN`，
HR 看到的都是「无法访问 macOS 钥匙串」，分不清该点「允许」还是该重新输入授权码。

## 后果

- 正面：测试不碰登录钥匙串、可在无 GUI 会话跑、与生产同路径；错误码可区分。
- 负面：`item` 层是 CFDictionary 级别，代码比 `keyring::set_generic_password(svc, acct, pwd)` 啰嗦。
  换来的是可测性与不被吞掉的错误码，值。
- 负面：**真实 ACL 行为测不出来**——临时钥匙串里没有「升级后签名变了」这回事。
  升级后的授权重置仍只能靠 TC-601 在真机上演练。
- 负面：`security-framework` 是 macOS-only crate，将来若做 Windows 要重写 `infra::keychain`
  （本期决策是不做）。
- 遗留（**已由 TC-101 关闭**）：`error.rs` 顶部的 `#[allow(dead_code)]` 是为「词表先行」加的，
  TC-101 接线 `Keychain` / `KeychainMissing` 后收窄到未接线的码——现改为逐个变体标注，
  注释写明哪张卡会构造；已接线的三个（`Keychain` / `KeychainMissing` / `Io`）不标，
  这样「已接线的码后来没人构造了」仍会被编译器发现。
  为什么不在 enum 上标一次：整块标注会把那种情况一起静默掉。
  收窄的有效性做过反向验证（摘掉一个标注 → `variant 'Locked' is never constructed`），
  过程记在 TC-101 的评审记录里。

## 重新评估的触发条件

- 决定做 Windows / Linux → 本 ADR 作废，回到 `keyring` 或各平台各写一层。
- 买了 Developer ID 证书（ADR-0002 触发条件命中）→ 重估是否改用 `SecAccess` 显式限定 ACL，
  把「升级后重置」从引导重填变成可避免。
- 出现「同一 service/account 在两个 store 里各一份」的真实事故 → 重估第 4 条。

## 补充（2026-11-11 · TC-103 验收期间实测）：开发构建改用专用钥匙串 + 自动授权

TC-103 验收时撞上了本 ADR 第 1 条约束（未签名分发 → ACL 绑签名）在**开发循环**里的具体形态，
实测出几条之前没写下来的事实，值得记在这里。

### 现象

应用每次启动都弹：

> 「payslip-mailer」想要访问你钥匙串中的密钥「cn.payslipmailer.db-key」。
> 若要给予许可，请输入「登录」钥匙串的密码。

**开发时每张卡都要重编译，于是每张卡都被这个弹窗打断一次。**

### 根因（实测，不是推测）

钥匙串条目配一份 ACL，「谁可以读」写在里面。默认 ACL 信任**创建它的那个程序**，
而 macOS 判定「同一个程序」靠的是**代码签名**：

| 程序状态 | macOS 怎么认它 | 重编译后 |
|---|---|---|
| 未签名 | 二进制的 **cdhash** | **变** → 每次都要重新授权 |
| 自签证书 | DR = `identifier + 证书指纹`（确实稳定） | 但 macOS 评估自签根证书时**仍不放行**，照样弹 |
| Apple 锚定签名（Developer ID） | DR = `identifier + anchor apple generic` | **不变** → 一次授权终身有效 |

栈追踪坐实了阻塞点：`KeychainStore::get → SecItemCopyMatching → SecKeychainItemCopyContent`
（`mach_msg` 等 Security 的裁决）。所以它确实是 **item ACL** 在拦，不是解锁、不是别的。

试过并**排除**的三条路：

- `security add-generic-password -A`（允许任意程序）→ 对 legacy 条目不生效，CLI 读得动、应用读不动；
- `security add-generic-password -T <应用路径>` → 同样拦；
- 开发构建自签一份证书（DR 确实稳定了）→ 仍弹。

⇒ **在买不到 Developer ID 之前（路线 A），开发构建无法靠签名获得稳定的钥匙串授权。**

### 决定

**开发构建走专用钥匙串文件 + 脚本自动授权；发布构建完全不动。**

1. `KeychainStore::for_runtime()`：开发构建 → `KeychainStore::dev()`（打开
   `~/Library/Keychains/xinyou-dev.keychain-db`，口令是常量）；
   发布构建 → `KeychainStore::login()`（登录钥匙串，默认 ACL）。
   **`put` / `get` / `delete` 仍是同一段代码**（本 ADR 第 5 条的意义没变），
   只有「用哪个钥匙串实例」不同。
2. `scripts/setup-dev-keychain.sh`：建那个钥匙串 + 用一把 32 字节随机密钥建条目
   （密钥长度必须是 32：应用侧 `apply_key_and_params` 硬性要求，base64 编码后正好 32 字节
   要用 `openssl rand -base64 24`，`rand -base64 32` 会给 44 字节而被拒）。
3. `scripts/dev-run.sh`：**编译 → 签名 → 起 app → 自动处理授权**一条命令。
   授权框一出现就自动填入口令并点「始终允许」，不用人管（口令是脚本里的常量，
   本来就不该让人来输）。
4. `scripts/setup-dev-signing.sh` + `scripts/sign-dev.sh`：给开发构建自签一份。
   **签名不是为了解决授权**（实测解决不了），而是为了让二进制有个稳定的标识，
   方便日后与 TC-600 的签名流程共用一套脚本。

### 后果

- 正面：开发循环不再被人打断；`dev-run.sh` 一条命令到界面。
- 正面：登录钥匙串里不再留开发条目（开发条目在自己的钥匙串文件里）。
- 负面：**开发构建的库密钥放在开发钥匙串里，它的 ACL 是宽松的**（`-A`）。
  只影响这台开发机上的空库，发布构建的 ACL 仍是默认的（只有签了名的薪邮能读）。
- 负面：**发布后的用户体验仍然是「升级后要重新授权」**——这是 ADR-0002 早就记下的，
  本 ADR 第 6 条的 `KEYCHAIN_MISSING` 分支与 TC-103 标准 15 的两条引导就是为它准备的。
  买 Developer ID 之前无法从根上避免。
- 遗留：**登录钥匙串里还有一条 2026-10-18 的旧条目**（account 叫 `neo`，不是我们的 `default`）。
  它不归本项目管（账号名不是我们写的），但同 service 前缀，审计时容易混。要不要删由使用者定。

## 补充二（2026-11-13 · TC-603 期间的受控实验）：**不是「重编译才变」，是「每次启动都问」**

上面「补充」一节把弹窗归因于「每重编译一次 cdhash 就变 → 每次启动都弹」。
那个归因**只对了一半**：它把「重编译」当成了触发条件，实际触发条件更宽。

**受控实验（2026-11-13）**：拿开发构建的**同一个二进制**（不重新编译，
`codesign -dvvv` 的 CDHash 两次完全相同：`d0fc78db37cdf3c452a445ef01071a3b844db275`）：

1. 启动 → 弹窗 → 自动应答「始终允许」；
2. **不重新编译**，再启动一次 → **又弹了一次**。

所以正确的机制是：**钥匙串对「没有 Apple 锚定签名」的程序无法建立持久信任** ——
macOS 没法验证这个程序是谁，于是每次进程启动都要人亲自授权一次；
点「始终允许」也只对**有 Apple 锚定签名**的程序才可能持久。
「cdhash 变了」是**另一种**触发（重新构建后必然重问），但它**不是必要条件**。

**对文档与产品的直接影响**：

- **给使用者的安装说明**原写「每换版本这个弹窗都会再问一次」——**低估了**。
  实际是「**每次启动都问**」。已改（TC-600 的安装说明「三、弹窗 2」）。
- **买 Developer ID 的价值被重新确认**：签名之后，钥匙串的信任凭据变成
  「证书 + bundle id」，**重新构建不再重置授权**，「每次启动输一次密码」这件事**彻底消失**。
  这比「双击能打开」更实在（后者只是第一次的门槛）。
- **ADR-0028 决策四**（「每换版本这个弹窗都会再问一次」）同样要更正为「每次启动」。
- 上面「补充」一节里「发布后的用户体验仍然是『升级后要重新授权』」这句**偏轻**了：
  不是「升级后」，是「**每次启动**」。

**不变的部分**：ADR-0009 的其余决策、ADR-0002 的 D8 结论、TC-103 标准 15 的两条引导
（重试 / 重新初始化）**都不受影响** —— 它们本来就是为「授权随时可能被重置」准备的，
只是这个「随时」比原先记的更频繁。

## 补充三（2026-11-13 · TC-603）：开发钥匙串那条旧结论也不成立

「补充」一节说「自签证书让 designated requirement 稳定了，但 macOS 评估自签根证书时仍不放行」，
因此只能自动授权。这条**实测依然成立**（开发构建每次启动仍要应答），
但**理由要改**：不是因为「DR 不稳定」，而是因为上面那条 ——
**没有 Apple 锚定的签名，钥匙串一律不持久信任**。DR 稳不稳是另一回事。

`dev-run.sh` 的自动授权逻辑（每次启动轮询 SecurityAgent、填口令、点「始终允许」）**照旧成立**，
只是它现在被理解成「每次启动都要应答」而不是「重编译后要应答」。
