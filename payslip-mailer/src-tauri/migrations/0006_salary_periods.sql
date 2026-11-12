-- 0006_salary_periods —— 月份账套与审计日志（TC-300）
--
-- 设计依据：设计文档 ch.5 状态机（月份账套 status：草稿 draft → 待发送 ready →
--   发送中 sending → 已发送 sent → 已归档 archived）、「发送中/已发送状态禁止编辑工资数值；
--   如需纠错，走『解锁 → 生成修订记录 → 仅对未发送者补发』）、
--   ch.6 核心表定义（`salary_periods | id, year_month, status, field_snapshot, locked_at |
--   UNIQUE(year_month) | field_snapshot 冻结当月字段定义，防止事后改字段导致历史错位」；
--   `audit_log | id, action, target, detail_masked, at | 记录登录、改配置、导入、发送、解锁、导出`）。
-- 决策依据：ADR-0019（冻结点 = draft → ready、draft 用当前配置而 ready 起用快照、
--   is_editable = draft|ready、解锁必留修订记录、快照存 JSON、locked_at 记进入 sending 的时刻）。
--
-- 两处要说清：
--
-- 1. **`field_snapshot` 存的是 JSON**（当时所有字段定义的数组）。
--    字段定义没有独立的表（它就是 `salary_fields` 的行），而 `salary_records.values_json`
--    也是 JSON —— 同一套「键值袋」的做法。快照存的是**公式源码**不是算出来的值：
--    公式属于字段定义（ADR-0018），值属于工资记录（TC-303）。存成值就无法重算那个月。
--
-- 2. **`year_month` 用 TEXT `'YYYY-MM'`**（如 `2026-10`）：可读，
--    且按文本排序就是按时间排序。设计文档只说 `UNIQUE(year_month)`，没定格式。

CREATE TABLE IF NOT EXISTS salary_periods (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    -- 年月，`YYYY-MM`。一个年月只有一个账套（ch.6：UNIQUE(year_month)）。
    year_month    TEXT    NOT NULL UNIQUE,
    -- 状态：draft / ready / sending / sent / archived（ch.5 状态机）。
    -- 合法迁移由 `domain::period.rs` 判定，不在 SQL 里写死 —— 状态机是**一组要一起改的策略**。
    status        TEXT    NOT NULL DEFAULT 'draft',
    -- 冻结的字段定义（JSON 数组）。**进入 ready 时才写**（ADR-0019 洞一）：
    -- 草稿期间字段配置还能随便改，HR 还在配；进了 ready 才把当时的定义钉住。
    -- 之后 S04 怎么改字段配置，这个月的表头与列都不动。
    field_snapshot TEXT,
    -- 什么时候进入 sending（epoch 秒）。那一刻开始数据被锁住等发送结果。
    -- 只在 sending 及其之后有值。
    locked_at     INTEGER
);

-- 审计日志（ch.6：记录登录、改配置、导入、发送、解锁、导出）。
-- `detail_masked` 是**脱敏后**的细节：只写「解锁了哪个账套、当时什么状态」，
-- 不写工资数值（红线 2 的同族）。
CREATE TABLE IF NOT EXISTS audit_log (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    -- 动作：unlock / import / export / send / config / login …
    action        TEXT    NOT NULL,
    -- 动作的对象（如账套 id、成员 id）。
    target        TEXT    NOT NULL,
    -- 脱敏后的细节（JSON 或短文本）。
    detail_masked TEXT,
    -- 什么时候（epoch 秒）。
    at            INTEGER NOT NULL
);

-- 审计日志按时间倒序读（「这个月发生过什么」）。
CREATE INDEX IF NOT EXISTS idx_audit_log_at ON audit_log(at);
