-- 0003_salary_fields —— 工资条项目（TC-200）
--
-- 设计依据：设计文档 ch.6 核心表定义（`salary_fields` 九列、
--   `data_type ∈ text/number/money/percent/formula`、「停用不删；影响模板表头与变量白名单」）、
--   ch.12 S04「单一事实源：决定 Excel 表头 / 表格列 / 模板变量」。
-- 决策依据：ADR-0015（停用 = `visible = 0`、只给停用没有删除、默认字段集用设计文档的示例集）。
--
-- 与设计文档字面不同的地方只有一处，已在 ADR-0015 记了理由：
-- **九列里没有独立的 `active`**（设计文档的 `members` 表有 `salary_fields` 没有）。
-- 「停用」由 `visible = 0` 承担 —— 它同时决定「不进新导出的 Excel 表头」与
-- 「不在新建模板的变量白名单里」，正是设计文档说的「影响模板表头与变量白名单」。
--
-- 停用**不是**删除：字段行永远留在库里，因为历史月份的数据里还有它的值，
-- 旧模板也要能渲染它。红线 8：「字段一旦被历史数据引用只能停用不能删除」。
--
-- `key` 是模板变量 `{{key}}` 里的那个 key，也是 Excel 表头的一列，
-- 所以它必须跨工具安全：小写字母 / 数字 / 下划线。格式规则在 `domain/field.rs` 里
-- （唯一真相源，SQL 只靠 UNIQUE 挡重复）。

CREATE TABLE IF NOT EXISTS salary_fields (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    -- 变量键。模板里写 `{{key}}`，Excel 里是列名。唯一。
    key         TEXT    NOT NULL UNIQUE,
    -- 显示名称（中文，给 HR 看的那一列）。
    label       TEXT    NOT NULL,
    -- text / number / money / percent / formula（设计文档 ch.6 的枚举）
    data_type   TEXT    NOT NULL,
    -- 排序。越小越靠前；S04 里拖动手柄改的就是它。
    sort_order  INTEGER NOT NULL,
    -- 是否必填。
    required    INTEGER NOT NULL DEFAULT 0,
    -- 是否出现在新导出的 Excel 表头与新建模板的变量白名单里。
    -- 0 = 停用（设计文档：「停用后不再出现在新导出的 Excel 模板中，历史数据保持原样」）。
    visible     INTEGER NOT NULL DEFAULT 1,
    -- 公式表达式。**只有 `data_type = 'formula'` 的行才有**；
    -- 其它类型必须为 NULL（存了就报错，见 `domain/field.rs`）。
    formula     TEXT,
    -- 对齐方式：left / right / center。金额类一般是 right。
    align       TEXT    NOT NULL DEFAULT 'left',
    -- 只允许这五种类型。放在库里而不是只在代码里，是为了让「从库读出来的脏数据」
    -- 也有一道兜底 —— 库文件是可以被手工改的。
    CHECK (data_type IN ('text', 'number', 'money', 'percent', 'formula'))
);

-- 注意：这里**故意没有**「停用时必须清掉公式」的约束。
-- 停用只是 `visible = 0`；公式留着是为了「停用 → 又启用」时不用重填，
-- 真要删公式是用户自己编辑的事。
