-- 0005_email_templates —— 邮件模板（TC-202）
--
-- 设计依据：设计文档 ch.6 核心表定义（`email_templates | id, name, subject_tpl, body_html,
--   body_text, is_default | 主题也支持变量`）、ch.12 S10「中文邮件模板编辑器」、
--   ch.10「中文模板示例」。
-- 决策依据：ADR-0017（minijinja 且不开 loader、未知变量报错、字段变量用 key、空值按必填判定）。
--
-- 三处与设计文档字面不同，理由都在 ADR-0017：
--
-- 1. **正文里存的是模板源码，不是渲染结果**。渲染是发送那一刻的事
--    （每个收件人的值不同）。存渲染结果就没法给第二个人用同一份模板。
--
-- 2. **`body_text` 是自动生成的纯文本兜底**，不是手写的。
--    设计文档：「同时自动生成 text/plain 兜底版本（表格转纯文本对齐），
--    避免部分客户端把纯 HTML 邮件判为垃圾邮件」。
--
-- 3. **主题也支持变量**（设计文档原文「主题也支持变量」），所以主题列存的是源码。
--
-- 「默认模板」用 `is_default` 标记，不单独建表：
-- 设计文档 ch.6 的表定义里就是这一列。唯一性（同一时刻只有一个默认模板）
-- 由应用层保证 —— 库层面没法用 CHECK 表达「恰好一行是 1」。

CREATE TABLE IF NOT EXISTS email_templates (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    -- 模板名（给使用者看的）。唯一。
    name        TEXT    NOT NULL UNIQUE,
    -- 邮件主题的**源码**（可含变量，如「{{年月}} 工资条」）。
    subject_tpl TEXT    NOT NULL,
    -- 正文 HTML 的源码（可含变量）。
    body_html   TEXT    NOT NULL,
    -- 纯文本兜底版本。**由应用自动生成**（把 HTML 里的变量替换掉、表格转对齐的纯文本）。
    body_text   TEXT    NOT NULL,
    -- 是不是默认模板。1 = 是。
    is_default  INTEGER NOT NULL DEFAULT 0
);

-- 「取默认模板」是最常用的一条查询。
CREATE INDEX IF NOT EXISTS idx_email_templates_default ON email_templates(is_default);
