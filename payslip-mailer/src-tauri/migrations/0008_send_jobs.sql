-- 0008_send_jobs —— 发送批次与逐封任务（TC-400）
--
-- 设计依据：设计文档 ch.6 核心表定义（`send_batches | id, period_id, template_id,
--   scope_json, total, success, failed, started_at, finished_at |
--   scope_json 记录本次范围（组/成员 id 列表）`；
--   `send_jobs | id, batch_id, member_id, to_email, subject, checksum, status,
--   attempts, smtp_code, error_code, idempotency_key, sent_at |
--   UNIQUE(idempotency_key)；不存渲染后的正文（脱敏），仅存摘要与结果`）。
-- 决策依据：ADR-0023（job 状态机无 failed 存储态、降速是批次级的、
--   每封完成即持久化、批次的 status 与 rate_per_min、retrying 的排定时间）。
--
-- 四处要说清：
--
-- 1. **`send_jobs` 没有正文列**（ch.6 明写「不存渲染后的正文（脱敏），仅存摘要与结果」）。
--    工资数值只活在 `salary_records.values_json`（加密库）里；
--    这张表存的是「谁、发到哪、什么时候、结果是什么」—— 证据链本身，
--    不是第二份数据副本。`subject` 列按 ch.6 建了但 v1 不落渲染值
--    （渲染归 TC-401/402；建列只为避免再迁移一次）。
--
-- 2. **没有 cc / bcc 列**（红线 1：「绝不把多个收件人放进同一封邮件」）。
--    表结构上就没有这两个字段 —— 比「约定不填」硬得多；
--    想群发的人改不了表结构。
--
-- 3. **`status` 没有 `failed` 存储态**（ADR-0023 决策 4）：失败的那一刻
--    立即变成 `retrying`（排定了重试时间）或 `dead`（不重试），
--    中间不存在「失败了但还没决定怎么办」的记录。
--    `queued → sending → success | retrying | dead`；`retrying → sending`。
--
-- 4. **`idempotency_key` 的 UNIQUE 现在就建**（ch.6 / ch.5）：
--    `hash(batch_id + member_id + period_id + content_checksum)`，
--    重复点发送不会产生第二封（TC-402 的中断恢复靠它识别「已发的」）。
--    本卡按设计文档的公式生成键；TC-402 消费它。

CREATE TABLE IF NOT EXISTS send_batches (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    -- 哪个月份账套。
    period_id     INTEGER NOT NULL REFERENCES salary_periods(id) ON DELETE RESTRICT,
    -- 用哪套模板（TC-401/402 消费）。
    template_id   INTEGER REFERENCES email_templates(id) ON DELETE SET NULL,
    -- 本次范围（成员 id 列表的 JSON）。含 id，不含姓名邮箱数值。
    scope_json    TEXT    NOT NULL,
    -- running / paused / aborted / finished（ADR-0023 决策 7）。
    status        TEXT    NOT NULL DEFAULT 'running',
    -- 批次开始时的速率；临时失败把**这个批次**的桶减半（不写回用户配置）。
    rate_per_min  INTEGER NOT NULL DEFAULT 30,
    total         INTEGER NOT NULL DEFAULT 0,
    success       INTEGER NOT NULL DEFAULT 0,
    failed        INTEGER NOT NULL DEFAULT 0,
    started_at    INTEGER,
    finished_at   INTEGER
);

-- 批次按时间倒序（发送历史那屏从最近的看起）。
CREATE INDEX IF NOT EXISTS idx_send_batches_started ON send_batches(started_at);

CREATE TABLE IF NOT EXISTS send_jobs (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    batch_id        INTEGER NOT NULL REFERENCES send_batches(id) ON DELETE CASCADE,
    -- 这封发给谁。
    member_id       INTEGER NOT NULL REFERENCES members(id) ON DELETE RESTRICT,
    -- 收件地址。**发送时刻的地址**（成员后来改了邮箱，历史日志仍是当时的地址 —— S06）。
    to_email        TEXT    NOT NULL,
    -- 渲染后的主题。v1 不落（渲染归 TC-401/402），列按 ch.6 建着。
    subject         TEXT,
    -- 这封对应工资记录的 checksum（TC-303 存的）。
    -- TC-401 的「内容一致」核对拿它比对渲染时的内容。
    checksum        TEXT    NOT NULL,
    -- queued / sending / success / retrying / dead（ADR-0023 决策 4，无 failed）。
    status          TEXT    NOT NULL DEFAULT 'queued',
    -- 已尝试次数（含失败的重试）。
    attempts        INTEGER NOT NULL DEFAULT 0,
    -- SMTP 回码（成功是 2xx、失败是对应错误码；网络错误为 NULL）。
    smtp_code       INTEGER,
    -- 失败分类：transient / permanent / auth / network / unknown。
    error_code      TEXT,
    -- 幂等键 = hash(batch_id + member_id + period_id + content_checksum)（ch.5）。
    -- 重复点发送不会产生第二封；中断恢复靠它识别「已发的」。
    idempotency_key TEXT    NOT NULL,
    -- retrying 状态的下一次重试时刻（epoch 毫秒）。
    next_attempt_at INTEGER,
    -- 发送成功时刻（epoch 秒）。
    sent_at         INTEGER,
    -- 幂等键唯一：同一批里同一个人同一份内容只有一封（ch.6）。
    UNIQUE (idempotency_key)
);

-- 「下一个该发的」：批次内按入队顺序，且 queued 优先。
CREATE INDEX IF NOT EXISTS idx_send_jobs_queue
    ON send_jobs(batch_id, status, id);
-- 到点的重试。
CREATE INDEX IF NOT EXISTS idx_send_jobs_retry
    ON send_jobs(batch_id, next_attempt_at);
