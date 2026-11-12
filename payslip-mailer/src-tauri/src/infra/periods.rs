//! 月份账套的读写（TC-300 · ADR-0019）
//!
//! 三件核心的事：
//!
//! 1. **状态迁移**：合法才动，且非法迁移**不留任何痕迹**（标准 8）。
//! 2. **进入 ready 时冻结字段快照**：草稿期间字段配置还能随便改，
//!    进了 ready 才把当时的定义钉住（ADR-0019 洞一）。
//! 3. **解锁必留修订记录**：`sent → draft` 不是普通迁移，
//!    它要在 `audit_log` 里留下一条（ADR-0019 洞四）。

use rusqlite::params;

use crate::domain::period::PeriodStatus;
use crate::error::{AppError, AppResult};
use crate::infra::db::DbState;

/// 一个月份账套。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Period {
    pub id: i64,
    /// `YYYY-MM`。
    pub year_month: String,
    pub status: PeriodStatus,
    /// 冻结的字段定义（JSON）。**draft 状态下是空的**（还没冻结）。
    pub field_snapshot: Option<String>,
    /// 进入 sending 的时刻（epoch 秒）。
    pub locked_at: Option<i64>,
    /// 现在能不能编辑工资数值（**前端靠它禁用编辑**，不靠前端自己判断状态）。
    pub editable: bool,
    /// 是否只读。
    pub read_only: bool,
}

/// 快照里的一个字段（冻结当时的定义）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldSnapshot {
    pub key: String,
    pub label: String,
    pub data_type: String,
    /// **公式源码**（不是算出来的值）。
    pub formula: Option<String>,
    pub required: bool,
    pub visible: bool,
    pub sort_order: i64,
}

/// 审计日志的一条。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditEntry {
    pub id: i64,
    pub action: String,
    pub target: String,
    /// 脱敏后的细节。**不含工资数值**（红线 2 的同族）。
    pub detail_masked: Option<String>,
    pub at: i64,
}

/// 列出全部账套（按年月倒序）。
pub fn list(db: &DbState) -> AppResult<Vec<Period>> {
    db.read(|conn| {
        let mut stmt = conn
            .prepare(
                "SELECT id, year_month, status, field_snapshot, locked_at \
                 FROM salary_periods ORDER BY year_month DESC, id DESC",
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                ))
            })
            .map_err(|e| AppError::Database(e.to_string()))?;
        let mut out = Vec::new();
        for row in rows {
            let (id, year_month, status, field_snapshot, locked_at) =
                row.map_err(|e| AppError::Database(e.to_string()))?;
            // 库里的状态认不出来（可能被手工改过）→ 当作只读的终态，
            // **不猜**：猜错会让界面显示一个假的可编辑状态。
            let status = PeriodStatus::parse(&status).unwrap_or(PeriodStatus::Archived);
            out.push(Period {
                id,
                year_month,
                editable: status.is_editable(),
                read_only: status.is_read_only(),
                status,
                field_snapshot,
                locked_at,
            });
        }
        Ok(out)
    })
}

/// 按 id 取一个账套。
pub fn get(db: &DbState, id: i64) -> AppResult<Period> {
    list(db)?
        .into_iter()
        .find(|p| p.id == id)
        .ok_or_else(|| AppError::NotFound("月份账套".into()))
}

/// 新建一个账套（草稿）。
pub fn create(db: &DbState, year_month: &str) -> AppResult<Period> {
    let ym = normalize_year_month(year_month)?;
    let id = db.write(|conn| {
        let exists: i64 = conn
            .query_row(
                "SELECT count(*) FROM salary_periods WHERE year_month = ?1",
                params![ym],
                |row| row.get(0),
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        if exists > 0 {
            return Err(AppError::Duplicate("月份账套".into()));
        }
        conn.execute(
            "INSERT INTO salary_periods (year_month, status, field_snapshot, locked_at) \
             VALUES (?1, 'draft', NULL, NULL)",
            params![ym],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(conn.last_insert_rowid())
    })?;
    Ok(Period {
        id,
        year_month: ym,
        status: PeriodStatus::Draft,
        editable: true,
        read_only: false,
        field_snapshot: None,
        locked_at: None,
    })
}

/// 年月的格式规整：接受 `2026-10`、`2026/10`、`202610`，统一成 `2026-10`。
fn normalize_year_month(text: &str) -> AppResult<String> {
    let cleaned: String = text
        .trim()
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '-' || *c == '/')
        .collect();
    let digits: String = cleaned.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() != 6 {
        return Err(AppError::ValidationError(
            "月份要写成 YYYY-MM（比如 2026-10）".into(),
        ));
    }
    let year: u32 = digits[..4]
        .parse()
        .map_err(|_| AppError::ValidationError("月份要写成 YYYY-MM（比如 2026-10）".into()))?;
    let month: u32 = digits[4..]
        .parse()
        .map_err(|_| AppError::ValidationError("月份要写成 YYYY-MM（比如 2026-10）".into()))?;
    if !(1..=12).contains(&month) {
        return Err(AppError::ValidationError(format!(
            "月份要在 1–12 之间，写的是 {month}"
        )));
    }
    if !(2000..=2100).contains(&year) {
        return Err(AppError::ValidationError(format!("年份 {year} 不合理")));
    }
    Ok(format!("{year:04}-{month:02}"))
}

/// 迁移状态。**非法迁移不留任何痕迹**（标准 8）。
///
/// 进入 `ready` 的那一刻**冻结字段快照**（ADR-0019 洞一）；
/// 进入 `sending` 的那一刻记 `locked_at`。
pub fn transition(db: &DbState, id: i64, target: PeriodStatus) -> AppResult<Period> {
    let current = get(db, id)?;
    if !current.status.can_transition_to(target) {
        return Err(AppError::ValidationError(format!(
            "不能从「{}」直接变成「{}」",
            current.status.label(),
            target.label()
        )));
    }

    // 快照：进入 ready 时冻结当时的字段配置
    let snapshot = if target == PeriodStatus::Ready {
        Some(freeze_snapshot(db)?)
    } else {
        None
    };
    let locked_at = if target == PeriodStatus::Sending {
        Some(now_secs())
    } else {
        None
    };

    db.write(|conn| {
        match (&snapshot, locked_at) {
            (Some(s), Some(t)) => conn.execute(
                "UPDATE salary_periods SET status = ?1, field_snapshot = ?2, locked_at = ?3 \
                     WHERE id = ?4",
                params![target.as_str(), s, t, id],
            ),
            (Some(s), None) => conn.execute(
                "UPDATE salary_periods SET status = ?1, field_snapshot = ?2 WHERE id = ?3",
                params![target.as_str(), s, id],
            ),
            (None, Some(t)) => conn.execute(
                "UPDATE salary_periods SET status = ?1, locked_at = ?2 WHERE id = ?3",
                params![target.as_str(), t, id],
            ),
            (None, None) => conn.execute(
                "UPDATE salary_periods SET status = ?1 WHERE id = ?2",
                params![target.as_str(), id],
            ),
        }
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    })?;
    get(db, id)
}

/// **解锁**一个已发送的账套：回到草稿，并且**必定**留下一条修订记录。
///
/// 设计文档 ch.5：「解锁 → 生成修订记录 → 仅对未发送者补发」。
/// 第三步（仅对未发送者补发）归 TC-402（它才有 `send_jobs`）。
pub fn unlock(db: &DbState, id: i64) -> AppResult<Period> {
    let current = get(db, id)?;
    if !current.status.is_unlock() {
        return Err(AppError::ValidationError(format!(
            "只有「已发送」的账套才能解锁，当前是「{}」",
            current.status.label()
        )));
    }
    // 修订记录先写：万一后面那步失败，至少「有人动过它」这件事留下了痕迹。
    write_audit(
        db,
        "unlock",
        &current.year_month,
        Some(format!(
            "{{\"from\":\"{}\",\"to\":\"{}\"}}",
            current.status.as_str(),
            PeriodStatus::Draft.as_str()
        )),
    )?;
    db.write(|conn| {
        conn.execute(
            "UPDATE salary_periods SET status = ?1, field_snapshot = NULL WHERE id = ?2",
            params![PeriodStatus::Draft.as_str(), id],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    })?;
    get(db, id)
}

/// 写一条审计日志。
pub fn write_audit(
    db: &DbState,
    action: &str,
    target: &str,
    detail_masked: Option<String>,
) -> AppResult<i64> {
    db.write(|conn| {
        conn.execute(
            "INSERT INTO audit_log (action, target, detail_masked, at) VALUES (?1, ?2, ?3, ?4)",
            params![action, target, detail_masked, now_secs()],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(conn.last_insert_rowid())
    })
}

/// 读审计日志（最近的在前）。
pub fn list_audit(db: &DbState, limit: i64) -> AppResult<Vec<AuditEntry>> {
    db.read(|conn| {
        let mut stmt = conn
            .prepare(
                "SELECT id, action, target, detail_masked, at FROM audit_log \
                 ORDER BY at DESC, id DESC LIMIT ?1",
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        let rows = stmt
            .query_map(params![limit], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })
            .map_err(|e| AppError::Database(e.to_string()))?;
        let mut out = Vec::new();
        for row in rows {
            let (id, action, target, detail_masked, at) =
                row.map_err(|e| AppError::Database(e.to_string()))?;
            out.push(AuditEntry {
                id,
                action,
                target,
                detail_masked,
                at,
            });
        }
        Ok(out)
    })
}

/// 冻结当前的字段配置（进入 ready 时调用）。
///
/// 快照里存的是**公式源码**不是算出来的值（ADR-0018）：
/// 值属于工资记录（TC-303），公式属于字段定义。
pub fn freeze_snapshot(db: &DbState) -> AppResult<String> {
    let fields = crate::infra::fields::list(db)?;
    let snapshot: Vec<FieldSnapshot> = fields
        .into_iter()
        .map(|f| FieldSnapshot {
            key: f.key,
            label: f.label,
            data_type: f.data_type.as_str().to_string(),
            formula: f.formula,
            required: f.required,
            visible: f.visible,
            sort_order: f.sort_order,
        })
        .collect();
    serde_json::to_string(&snapshot)
        .map_err(|e| AppError::Internal(format!("字段快照序列化失败：{e}")))
}

/// 读一个账套的快照（解析成结构）。
pub fn read_snapshot(period: &Period) -> AppResult<Vec<FieldSnapshot>> {
    let Some(raw) = &period.field_snapshot else {
        return Ok(Vec::new());
    };
    serde_json::from_str(raw).map_err(|e| AppError::Internal(format!("字段快照读不懂：{e}")))
}

/// 界面上这个月显示的列：**draft 用当前配置，其余用快照**（ADR-0019 洞二）。
pub fn display_fields(db: &DbState, period: &Period) -> AppResult<Vec<FieldSnapshot>> {
    if period.status == PeriodStatus::Draft {
        let fields = crate::infra::fields::list(db)?;
        return Ok(fields
            .into_iter()
            .map(|f| FieldSnapshot {
                key: f.key,
                label: f.label,
                data_type: f.data_type.as_str().to_string(),
                formula: f.formula,
                required: f.required,
                visible: f.visible,
                sort_order: f.sort_order,
            })
            .collect());
    }
    read_snapshot(period)
}

/// 墙钟 epoch 秒。
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::field::FieldType;
    use crate::infra::db::CIPHER_PARAMS;
    use crate::infra::fields;
    use crate::infra::keychain::{TempKeychain, no_user_interaction};
    use std::fs;

    struct Fx {
        dir: std::path::PathBuf,
        kc: TempKeychain,
        db: DbState,
    }

    impl Fx {
        fn new(label: &str) -> Self {
            no_user_interaction();
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default();
            let dir = std::env::temp_dir().join(format!(
                "xinyou-periods-test-{}-{nanos}-{label}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).expect("建临时目录失败");
            Self {
                dir,
                kc: TempKeychain::new(label),
                db: DbState::new(),
            }
        }

        fn open(&self) {
            self.db
                .open(&self.kc.store, &self.dir, &CIPHER_PARAMS)
                .expect("开库失败");
        }
    }

    impl Drop for Fx {
        fn drop(&mut self) {
            self.db.close();
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    /// 标准 1/2：状态机与「一个年月一个账套」。
    #[test]
    fn periods_follow_the_state_machine_and_one_per_month() {
        let fx = Fx::new("std01");
        fx.open();
        let p = create(&fx.db, "2026-10").expect("建账套失败");
        assert_eq!(p.status, PeriodStatus::Draft);

        // 同一个月再建一个 → 拒绝
        let dup = create(&fx.db, "2026-10").expect_err("同月不该能建两个");
        assert!(matches!(dup, AppError::Duplicate(_)), "{dup:?}");

        // 跳级被拒
        let err = transition(&fx.db, p.id, PeriodStatus::Sent).expect_err("不该能跳级");
        assert!(
            err.user_message().contains("草稿"),
            "{}",
            err.user_message()
        );
        // 状态没变
        assert_eq!(
            get(&fx.db, p.id).expect("读账套失败").status,
            PeriodStatus::Draft
        );

        // 正常推进
        let p = transition(&fx.db, p.id, PeriodStatus::Ready).expect("进 ready 失败");
        assert_eq!(p.status, PeriodStatus::Ready);
        let p = transition(&fx.db, p.id, PeriodStatus::Sending).expect("进 sending 失败");
        assert_eq!(p.status, PeriodStatus::Sending);
        assert!(p.locked_at.is_some(), "进 sending 要记 locked_at");
        let p = transition(&fx.db, p.id, PeriodStatus::Sent).expect("进 sent 失败");
        assert_eq!(p.status, PeriodStatus::Sent);
        let p = transition(&fx.db, p.id, PeriodStatus::Archived).expect("归档失败");
        assert_eq!(p.status, PeriodStatus::Archived);
        // 终态不能再走
        assert!(transition(&fx.db, p.id, PeriodStatus::Draft).is_err());
    }

    /// 标准 3/4/5：**快照在进入 ready 时冻结**，之后改字段配置不动这个月。
    #[test]
    fn the_snapshot_freezes_at_ready_and_survives_later_field_changes() {
        let fx = Fx::new("std03");
        fx.open();
        fields::ensure_defaults(&fx.db).expect("写默认字段失败");
        let p = create(&fx.db, "2026-10").expect("建账套失败");

        // draft 期间：显示的是**当前**配置
        let before =
            display_fields(&fx.db, &get(&fx.db, p.id).expect("读账套失败")).expect("取列失败");
        assert!(before.iter().any(|f| f.key == "base_salary"));

        // 进 ready → 冻结
        let p = transition(&fx.db, p.id, PeriodStatus::Ready).expect("进 ready 失败");
        let snap = read_snapshot(&p).expect("读快照失败");
        assert!(snap.iter().any(|f| f.key == "base_salary"));
        // 快照里存的是**公式源码**
        let net = snap
            .iter()
            .find(|f| f.key == "net_pay")
            .expect("快照里应有 net_pay");
        assert!(net.formula.is_some(), "快照要存公式源码：{net:?}");

        // 之后改字段配置：加一个字段、改一个显示名
        fields::create(
            &fx.db,
            "quarter_bonus",
            "季度补贴",
            FieldType::Money,
            false,
            None,
        )
        .expect("加字段失败");
        let base = fields::list(&fx.db)
            .expect("列字段失败")
            .into_iter()
            .find(|f| f.key == "base_salary")
            .unwrap();
        fields::update(
            &fx.db,
            base.id,
            "底薪",
            FieldType::Money,
            base.required,
            base.formula.as_deref(),
        )
        .expect("改显示名失败");

        // 这个月的列**一个字都没变**
        let after =
            display_fields(&fx.db, &get(&fx.db, p.id).expect("读账套失败")).expect("取列失败");
        assert_eq!(before.len(), after.len(), "冻结后不该多出列");
        assert!(
            after.iter().any(|f| f.label == "基本工资"),
            "冻结后显示名不该跟着改：{:?}",
            after.iter().map(|f| &f.label).collect::<Vec<_>>()
        );
        assert!(
            !after.iter().any(|f| f.key == "quarter_bonus"),
            "冻结后不该出现新加的字段"
        );
    }

    /// 标准 6：ready 仍可编辑；sending / sent / archived 不能。
    #[test]
    fn ready_is_still_editable_but_sending_is_not() {
        assert!(PeriodStatus::Draft.is_editable());
        assert!(PeriodStatus::Ready.is_editable());
        assert!(!PeriodStatus::Sending.is_editable());
        assert!(!PeriodStatus::Sent.is_editable());
        assert!(!PeriodStatus::Archived.is_editable());
    }

    /// 标准 7：解锁回到草稿，并且**必定**留下一条修订记录。
    #[test]
    fn unlocking_a_sent_period_leaves_an_audit_record() {
        let fx = Fx::new("std07");
        fx.open();
        let p = create(&fx.db, "2026-10").expect("建账套失败");
        transition(&fx.db, p.id, PeriodStatus::Ready).expect("进 ready 失败");
        transition(&fx.db, p.id, PeriodStatus::Sending).expect("进 sending 失败");
        let p = transition(&fx.db, p.id, PeriodStatus::Sent).expect("进 sent 失败");

        let before = list_audit(&fx.db, 50).expect("读审计失败").len();
        let p = unlock(&fx.db, p.id).expect("解锁失败");
        assert_eq!(p.status, PeriodStatus::Draft);
        // 快照被清了（回到草稿 = 未冻结）
        assert!(p.field_snapshot.is_none(), "解锁回草稿后快照应清掉");

        let audit = list_audit(&fx.db, 50).expect("读审计失败");
        assert_eq!(audit.len(), before + 1, "解锁要留一条修订记录");
        let entry = audit
            .iter()
            .find(|e| e.action == "unlock")
            .expect("要有 unlock 记录");
        assert_eq!(entry.target, "2026-10");
        assert!(
            entry
                .detail_masked
                .as_deref()
                .unwrap_or("")
                .contains("sent"),
            "记录里要写清是从什么状态解锁的：{:?}",
            entry.detail_masked
        );
    }

    /// 标准 8：非法迁移**不留任何痕迹**（状态不变，也没有审计记录）。
    #[test]
    fn a_rejected_transition_changes_nothing() {
        let fx = Fx::new("std08");
        fx.open();
        let p = create(&fx.db, "2026-10").expect("建账套失败");
        transition(&fx.db, p.id, PeriodStatus::Ready).expect("进 ready 失败");
        let before = get(&fx.db, p.id).expect("读账套失败");
        let audit_before = list_audit(&fx.db, 50).expect("读审计失败").len();

        assert!(
            transition(&fx.db, p.id, PeriodStatus::Sent).is_err(),
            "不该能跳级"
        );
        assert!(
            transition(&fx.db, p.id, PeriodStatus::Draft).is_err(),
            "不该能回退"
        );

        let after = get(&fx.db, p.id).expect("读账套失败");
        assert_eq!(before, after, "被拒的迁移不该改变任何东西");
        assert_eq!(
            list_audit(&fx.db, 50).expect("读审计失败").len(),
            audit_before,
            "被拒的迁移不该留下审计记录"
        );
    }

    /// 标准 9：locked_at 只在 sending 及其之后有值。
    #[test]
    fn locked_at_is_set_when_entering_sending() {
        let fx = Fx::new("std09");
        fx.open();
        let p = create(&fx.db, "2026-10").expect("建账套失败");
        assert!(p.locked_at.is_none(), "草稿阶段不该有 locked_at");
        let p = transition(&fx.db, p.id, PeriodStatus::Ready).expect("进 ready 失败");
        assert!(p.locked_at.is_none(), "ready 阶段不该有 locked_at");
        let p = transition(&fx.db, p.id, PeriodStatus::Sending).expect("进 sending 失败");
        assert!(p.locked_at.is_some(), "进 sending 要有 locked_at");
    }

    /// 年月的格式要规整（接受几种写法，统一成 YYYY-MM）。
    #[test]
    fn year_month_is_normalized() {
        assert_eq!(normalize_year_month("2026-10").unwrap(), "2026-10");
        assert_eq!(normalize_year_month("2026/10").unwrap(), "2026-10");
        assert_eq!(normalize_year_month("202610").unwrap(), "2026-10");
        assert!(normalize_year_month("2026-13").is_err(), "月份不能是 13");
        assert!(normalize_year_month("2026").is_err(), "缺月份");
    }
}
