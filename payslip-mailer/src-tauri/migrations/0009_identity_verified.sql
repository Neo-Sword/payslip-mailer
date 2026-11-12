-- 0009_identity_verified —— 工资记录的身份核对标记（TC-401）
--
-- 设计依据：设计文档 ch.5 防错机制 ①身份一致（「导入行的姓名+邮箱必须与成员表
--   精确匹配；不匹配的行标红且不允许进入发送范围」）。
-- 决策依据：ADR-0024 决策 1、2。
--
-- **这是 TC-302 的持久化缺口**：导入时已经判出「Excel 姓名与成员表不一致」
-- （警告 + identity_unverified 标记），但 commit 只写了 values_json ——
-- 那个事实在入库那一刻就丢了，TC-401 想拦也无从拦起。
--
-- 为什么是**新迁移**而不是改 0007：迁移的规则是「已应用的条目不许改 SQL」
-- （改了 checksum 对不上 → MIGRATION_DRIFT）。0007 已经在测试与真实库上应用过。
--
-- DEFAULT 1 的含义：修正之前导入的行没有标记，一律按「已验证」放行 ——
-- 它们的身份问题只能靠 TC-303 的校验列与人工；下次导入覆盖时会带上真值。

ALTER TABLE salary_records
    ADD COLUMN identity_verified INTEGER NOT NULL DEFAULT 1;
