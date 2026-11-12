-- 0004_members —— 邮件组、成员与归属（TC-201）
--
-- 设计依据：设计文档 ch.6 核心表定义（`mail_groups` / `members` / `group_members` 三张表；
--   「email 格式校验 / UNIQUE」「employee_no 可选，用于 Excel 精确匹配」「成员可属多组」）。
-- 决策依据：ADR-0016（一个邮箱一个人、成员只有停用没有删除、组可以删但只解除归属）。
--
-- 三处要说清：
--
-- 1. **`members.email` 是全局 UNIQUE**，不是「组内唯一」。
--    一个邮箱 = 一个人；一个人属于多个组靠 `group_members` 的多行表达。
--    这是 S03「三重核对」里「身份一致」的唯一依据（ADR-0016 洞一）。
--
-- 2. **没有「删除成员」的痕迹，也不需要**：成员用 `active` 停用。
--    停用的人不进入发送范围，但名单里的记录还在（红线 8 的同族，与 `salary_fields` 同一口径）。
--
-- 3. **`members` 没有「状态」列**，S05 原型里的第三态「邮箱待确认」是**从发送历史
--    算出来的**（曾有一次 550 退信），那属于 TC-403。派生态不存成列。

CREATE TABLE IF NOT EXISTS mail_groups (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    -- 组名。唯一（设计文档：「组名唯一」）。
    name        TEXT    NOT NULL UNIQUE,
    -- 备注。组名会进邮件的收件范围说明里，备注不进。
    note        TEXT,
    -- 排序（侧边栏与群发对话框里的组列表顺序）。
    sort_order  INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS members (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    -- 姓名。个人数据，绝不进日志与错误文案（红线 2）。
    name        TEXT    NOT NULL,
    -- 邮箱。**全局唯一**（见文件头第 1 条）。
    email       TEXT    NOT NULL UNIQUE,
    -- 工号。可选，用于 Excel 导入时的精确匹配（TC-302）。
    employee_no TEXT,
    -- 1 = 启用（进入发送范围），0 = 停用（不在发送范围，但记录还在）。
    active      INTEGER NOT NULL DEFAULT 1,
    note        TEXT
);

-- 成员与组的归属。复合主键保证「同一个人在同一组里只出现一次」（ADR-0016 决策 1）。
-- 删组时这一行会被删掉，**成员本身不动**（删组只解除归属）。
CREATE TABLE IF NOT EXISTS group_members (
    group_id  INTEGER NOT NULL REFERENCES mail_groups(id) ON DELETE CASCADE,
    member_id INTEGER NOT NULL REFERENCES members(id) ON DELETE CASCADE,
    PRIMARY KEY (group_id, member_id)
);

-- 按成员列组时要用（一个成员可能属于多个组）。
CREATE INDEX IF NOT EXISTS idx_group_members_member ON group_members(member_id);
