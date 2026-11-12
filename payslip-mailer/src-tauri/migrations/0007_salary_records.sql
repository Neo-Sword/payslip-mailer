-- 0007_salary_records —— 月份工资记录（TC-302）
--
-- 设计依据：设计文档 ch.6 核心表定义（`salary_records | id, period_id, member_id,
--   values_json, checksum, source | UNIQUE(period_id, member_id) |
--   values_json 存键值；checksum 用于防错校验`）、ch.5 核心流程 B 第 3 步
--   （Excel 导入 → 上传 → 表头映射 → 校验 → 预览 → 入库）。
-- 决策依据：ADR-0021（冲突策略三选一、覆盖前同事务写 audit_log、K>0 禁止入库、
--   空金额 = 没有这一项、公式列由系统算）。
--
-- 三处要说清：
--
-- 1. **`values_json` 存的是「文本」而不是「数字」。**
--    理由一：**空 ≠ 0**。ADR-0021 决策 6 说「空 = 没有这一项」——
--    要表达「没有」，就得有一种表示法与「0」不同。用文本就能靠「键不存在」
--    表达「没有」，而数字类型做不到（0 就是一个值）。
--    理由二：金额按**分**四舍五入后存成字符串（`18000.00`），
--    避免浮点尾数漂移（`0.1 + 0.2` 那种）。
--    键是**字段 key**（`base_salary`），不是显示名 —— 显示名用户可改，key 不可改。
--
-- 2. **`checksum` 是这一行内容的摘要**，用于「内容一致」防错核对
--    （设计文档 ch.5 防错机制 ②：渲染前算 checksum 与预览时比对，不一致即中止）。
--    它是**十六进制串**，不含任何工资数值的明文。
--
-- 3. **`source` 记这一行是怎么来的**：`import`（Excel 导入）/ `manual`（TC-303 逐格编辑）。
--    归档与追责时要能说清「这行是人填的还是导进来的」。
--
-- 外键都用 `ON DELETE RESTRICT`（ch.6：「外键 ON DELETE RESTRICT 防误删历史」）：
-- 有工资记录的人不能被删、月份账套不能被删。

CREATE TABLE IF NOT EXISTS salary_records (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    -- 属于哪个月份账套。
    period_id   INTEGER NOT NULL REFERENCES salary_periods(id) ON DELETE RESTRICT,
    -- 这一行是谁的。
    member_id   INTEGER NOT NULL REFERENCES members(id) ON DELETE RESTRICT,
    -- 字段 key → 值（JSON 对象）。**键不存在 = 这一项没有**（见文件头第 1 条）。
    -- 公式字段的值也存进来（由 TC-203 的求值器算出的结果）。
    values_json TEXT    NOT NULL,
    -- 这一行内容的摘要（十六进制）。用于「内容一致」核对。
    checksum    TEXT    NOT NULL,
    -- 这一行怎么来的：import / manual。
    source      TEXT    NOT NULL DEFAULT 'import',
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL,
    -- 同一个成员在同一个月只能有一行（ch.6：UNIQUE(period_id, member_id)）。
    -- 这一条约束同时是「同一成员同月不可重复」的**库层兜底**：
    -- 即使应用层漏了判重，数据库也不会让两条记录并存。
    UNIQUE (period_id, member_id)
);

-- 按月份读记录（TC-303 的表格、TC-401 的发送范围都按月份查）。
CREATE INDEX IF NOT EXISTS idx_salary_records_period ON salary_records(period_id);

-- 按成员读（一个成员这个月有没有数据）。
CREATE INDEX IF NOT EXISTS idx_salary_records_member ON salary_records(member_id);
