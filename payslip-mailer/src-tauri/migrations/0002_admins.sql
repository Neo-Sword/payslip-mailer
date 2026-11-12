-- 0002_admins —— 管理员口令与失败锁定（TC-102）
--
-- 设计依据：设计文档 ch.9 表结构 461-464（`admins`）、ch.4 安全模型 179-183
-- （「Argon2id + 随机盐」「绝不明文或 MD5 保存」）、ch.6 S02 1007-1026（失败锁定）。
-- 决策依据：ADR-0011 §3（PHC 单列）、§5（时间列 INTEGER 存 epoch 秒）、§6（计数进库不进内存）。
--
-- 三处与设计文档字面不同，都在 ADR-0011 里记了理由，这里再钉一遍免得后人以为是写错：
--
-- 1. **没有 `salt` 列**（设计文档 463 有）。Argon2id 的 PHC 串本身就长成
--    `$argon2id$v=19$m=19456,t=2,p=1$<盐>$<哈希>`，盐和参数都在串里；
--    spike 实测过「存进去再解析回来」参数一字不差（`/tmp/argon2-run.log`）。
--    单独存一列 salt 只会造出第二个真相源——两处不一致时没人知道该信哪个。
--
-- 2. **没有独立的 `admin_lockout` 表**（0001 的文件头预告过它）。v1 写死单管理员
--    （设计文档 ch.9 的 `admins` 就是单人语义），锁定计数与口令哈希属于同一行的同一次判定，
--    拆两张表要么两次写要么靠人记得开事务。`CHECK (id = 1)` 把「单人」钉在库层面，
--    插入第二条会直接约束失败，而不是靠 Rust 代码里一个没人调的断言。
--
-- 3. 时间列是 `INTEGER` 存 epoch 秒，不是设计文档常见的 TEXT。锁定判定要做减法与比较，
--    `app_meta` 的「value 一律 TEXT」是给「一次性事实的 KV」定的口径，不适用于要参与
--    算术的列（ADR-0011 §5）。
--
-- `failed_attempts` / `locked_until` 放在库里而不是内存里，是为了设计文档 1023 那句
-- 「无法通过重装应用绕过」：⌘Q 重开、甚至重启电脑，计数都还在（spec 标准 8）。

CREATE TABLE IF NOT EXISTS admins (
    id              INTEGER PRIMARY KEY CHECK (id = 1),
    -- 显示用的登录标识。设计文档 S01 的表单里叫「管理员邮箱」。
    email           TEXT    NOT NULL,
    -- Argon2id 的 PHC 串，含盐与参数。**绝不**往里写明文口令（spec 标准 3 会 grep 库文件）。
    pwd_hash        TEXT    NOT NULL,
    created_at      INTEGER NOT NULL,
    last_login      INTEGER,
    -- 当前失败窗口内已错的次数；上限 5 由 `domain/auth.rs` 判定，不在 SQL 里写死，
    -- 因为「上限是 5」和「锁 60 秒」是一组要一起改的策略参数，散到 SQL 里就改漏。
    failed_attempts INTEGER NOT NULL DEFAULT 0,
    -- 锁定到什么时候（epoch 秒）。NULL = 未锁定。
    -- 锁定期内的失败**不延长**它（spec 标准 6），否则乱按的人会永久锁死。
    locked_until    INTEGER
);

-- 空闲自动锁定的阈值（分钟）存在 `app_meta` 的 `idle_lock_minutes` 键里，
-- 这里**不预插行**：读不到键就用代码里的默认 10 分钟（设计文档 795）。
-- 预插一行的话，「用户从没改过设置」和「用户改成了 10 分钟」这两种状态在库里长得一样，
-- 以后想加「跟随系统设置」之类的第三态就分不出来了。
