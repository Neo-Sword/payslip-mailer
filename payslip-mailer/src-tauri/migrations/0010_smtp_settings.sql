-- 0010_smtp_settings —— 发信邮箱配置（TC-500）
--
-- 设计依据：设计文档 ch.6 核心表定义（`smtp_settings | host, port, security,
--   username, from_name, from_email, reply_to, rate_per_min, credential_ref |
--   INT / TEXT CHECK | credential_ref 只是 Keychain 条目名，不含秘密`）、
--   ch.7 邮箱配置矩阵（飞书 smtp.feishu.cn/465/SSL 为主项目实际接入，
--   其余为 S14 的取值依据）。
-- 决策依据：ADR-0027（专用密码进 Keychain、rustls+ring 的 TLS 后端、
--   测试连接发自检信）。
--
-- 三处要说清：
--
-- 1. **单行表**（管理员只有一位，发信配置也只有一个）。
--    用 `id = 1` 的固定行表达「只有一个配置」；重复 INSERT 由应用层拦截。
--
-- 2. **`security` 三态**：`ssl`（465，implicit TLS）/ `starttls`（587，
--    明文开始后升级）/ `none`（仅内网，ch.7「自建/其他：无（仅内网）」）。
--    v1 的 S14 提供三选一；飞书主项目用 `ssl`。
--
-- 3. **`credential_ref` 不含秘密**（ch.6 原话）：它只是 Keychain 条目的
--    account 名（`SecretKind::SmtpCredential`，service = cn.payslipmailer.smtp-credential）。
--    专用密码本体在 Keychain；删配置时条目一起删（TC-105 管销毁）。

CREATE TABLE IF NOT EXISTS smtp_settings (
    id             INTEGER PRIMARY KEY CHECK (id = 1),
    -- SMTP 主机（smtp.feishu.cn / smtp.qq.com / ...）。
    host           TEXT    NOT NULL,
    -- 端口（465 / 587 / 25）。
    port           INTEGER NOT NULL CHECK (port BETWEEN 1 AND 65535),
    -- ssl / starttls / none（见文件头第 2 条）。
    security       TEXT    NOT NULL CHECK (security IN ('ssl', 'starttls', 'none')),
    -- SMTP 用户名（飞书/腾讯 = 完整邮箱地址）。
    username       TEXT    NOT NULL,
    -- 发件人显示名（「人力资源部」—— 收件人看到的名字）。
    from_name      TEXT    NOT NULL,
    -- 发件地址（信封 From；一般 = username）。
    from_email     TEXT    NOT NULL,
    -- 回信地址（模板变量 {{回信邮箱}}）。可空 = 不设 Reply-To。
    reply_to       TEXT,
    -- 发信速率（封/分钟）—— TC-400 的令牌桶读它（app_meta.rate_per_min 的正主）。
    rate_per_min   INTEGER NOT NULL DEFAULT 30 CHECK (rate_per_min BETWEEN 1 AND 600),
    -- Keychain 条目名。**不含密码本体**（文件头第 3 条）。
    credential_ref TEXT    NOT NULL,
    created_at     INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL
);

-- 发送速率的设置页直接写 rate_per_min；TC-400 的 rate_from_settings 读 app_meta
-- 是当时的过渡，本卡起改为读这里（两处合一）。
