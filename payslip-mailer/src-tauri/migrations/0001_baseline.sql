-- 0001_baseline —— 库结构基线（TC-100）
--
-- 迁移规则（ADR-0010 第 6/7 条）：本文件一旦应用过就**不许再改**。
-- 改动会让 `schema_migrations.checksum` 对不上，启动即报 DATABASE（spec 标准 7）——
-- 这不是刁难，而是堵「库里记的是 v1、跑的是改过的 v1」这种无声漂移。要改结构就新增 0002。
--
-- 本卡只放一条基线迁移证明机制在跑。业务表按卡追加，别在别的卡里顺手建：
--   admins / admin_lockout          → TC-102
--   payslip_projects / salary_fields → TC-200
--   email_groups / members           → TC-201
--   send_jobs / 幂等键               → TC-402
--
-- 字段一旦被历史数据引用只能停用不能删除（红线 8），所以列名要按「十年后还认」的标准起。

-- 本机一次性事实的 KV。key 是稳定的英文标识，value 一律 TEXT（SQLite 动态类型下
-- 混着存数字与字符串，恢复时没人分得清哪个是时间戳哪个是计数）。
--
-- 已知的未来写入方先记在这里，免得后人往库里塞野生表：
--   initialized_at    —— TC-103 首启向导完成时刻
--   last_restored_at  —— TC-104 从备份恢复的时刻
CREATE TABLE IF NOT EXISTS app_meta (
    key   TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
);
