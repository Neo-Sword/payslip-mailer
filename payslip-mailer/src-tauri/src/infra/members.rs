//! 成员与邮件组的读写（TC-201 · ADR-0016）
//!
//! **没有删除成员的操作**（ADR-0016 决策 4）：要让某人不再收到信 = 停用他。
//! 邮件组**可以删**，但只解除归属，成员本身不动。

use rusqlite::params;

use crate::domain::member::{MemberDraft, PastePreview, check_email, preview_paste};
use crate::error::{AppError, AppResult};
use crate::infra::db::DbState;

/// 一个成员（界面看到的那一行）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Member {
    pub id: i64,
    pub name: String,
    pub email: String,
    /// 工号。可选，Excel 导入时用来精确匹配。
    pub employee_no: Option<String>,
    /// 停用的人不在发送范围，但记录还在。
    pub active: bool,
    pub note: Option<String>,
    /// 所属的组（名字）。一个人可以属于多个组。
    pub groups: Vec<String>,
}

/// 一个邮件组。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Group {
    pub id: i64,
    pub name: String,
    pub note: Option<String>,
    pub sort_order: i64,
    /// 组里**启用**的成员数（界面上的数字）。
    pub active_member_count: i64,
    /// 组里全部成员数（含停用的）。
    pub member_count: i64,
}

/// 列出全部成员（含停用的 —— 界面要能看见它们是灰的）。
pub fn list_members(db: &DbState) -> AppResult<Vec<Member>> {
    db.read(|conn| {
        let mut stmt = conn
            .prepare(
                "SELECT id, name, email, employee_no, active, note FROM members ORDER BY name, id",
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            })
            .map_err(|e| AppError::Database(e.to_string()))?;
        let mut out: Vec<Member> = Vec::new();
        for row in rows {
            let (id, name, email, employee_no, active, note) =
                row.map_err(|e| AppError::Database(e.to_string()))?;
            let groups = groups_of(conn, id)?;
            out.push(Member {
                id,
                name,
                email,
                employee_no,
                active: active != 0,
                note,
                groups,
            });
        }
        Ok(out)
    })
}

/// 列出**可发送**的成员（停用的不在其中）。
///
/// 设计文档 S05 原话：「停用成员不进入发送范围」。
/// 发送侧（TC-401/402）拿到的必须是这个名单。
pub fn list_sendable_members(db: &DbState) -> AppResult<Vec<Member>> {
    Ok(list_members(db)?
        .into_iter()
        .filter(|m| crate::domain::member::is_sendable(m.active))
        .collect())
}

/// 某个成员属于哪些组。
fn groups_of(conn: &rusqlite::Connection, member_id: i64) -> AppResult<Vec<String>> {
    let mut stmt = conn
        .prepare(
            "SELECT g.name FROM mail_groups g \
             JOIN group_members gm ON gm.group_id = g.id \
             WHERE gm.member_id = ?1 ORDER BY g.sort_order, g.id",
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
    let rows = stmt
        .query_map(params![member_id], |row| row.get::<_, String>(0))
        .map_err(|e| AppError::Database(e.to_string()))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| AppError::Database(e.to_string()))?);
    }
    Ok(out)
}

/// 列出全部组。
pub fn list_groups(db: &DbState) -> AppResult<Vec<Group>> {
    db.read(|conn| {
        let mut stmt = conn
            .prepare(
                "SELECT g.id, g.name, g.note, g.sort_order, \
                        (SELECT count(*) FROM group_members gm \
                          JOIN members m ON m.id = gm.member_id \
                         WHERE gm.group_id = g.id AND m.active = 1), \
                        (SELECT count(*) FROM group_members gm WHERE gm.group_id = g.id) \
                 FROM mail_groups g ORDER BY g.sort_order, g.id",
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })
            .map_err(|e| AppError::Database(e.to_string()))?;
        let mut out = Vec::new();
        for row in rows {
            let (id, name, note, sort_order, active_count, member_count) =
                row.map_err(|e| AppError::Database(e.to_string()))?;
            out.push(Group {
                id,
                name,
                note,
                sort_order,
                active_member_count: active_count,
                member_count,
            });
        }
        Ok(out)
    })
}

/// 新建一个成员。
///
/// 邮箱撞了别人 → `Duplicate`（编辑表单那侧靠这个码做红描边）。
pub fn create_member(
    db: &DbState,
    name: &str,
    email: &str,
    employee_no: Option<&str>,
    group_ids: &[i64],
) -> AppResult<Member> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::ValidationError("姓名不能为空".into()));
    }
    check_email(email)?;

    let email_norm = email.trim().to_lowercase();
    let id = db.write(|conn| {
        let exists: i64 = conn
            .query_row(
                "SELECT count(*) FROM members WHERE lower(email) = ?1",
                params![email_norm],
                |row| row.get(0),
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        if exists > 0 {
            return Err(AppError::Duplicate("邮箱".into()));
        }
        conn.execute(
            "INSERT INTO members (name, email, employee_no, active, note) \
             VALUES (?1, ?2, ?3, 1, NULL)",
            params![
                name,
                email.trim(),
                employee_no.map(str::trim).filter(|s| !s.is_empty())
            ],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(conn.last_insert_rowid())
    })?;
    set_member_groups(db, id, group_ids)?;
    Ok(Member {
        id,
        name: name.to_string(),
        email: email.trim().to_string(),
        employee_no: employee_no
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        active: true,
        note: None,
        groups: groups_of_names(db, id)?,
    })
}

/// 编辑一个成员（**邮箱可改**：设计文档 S06 说「修改邮箱后，历史发送日志仍保留
/// 原收件地址，便于追溯」—— 历史是以地址留痕的，改邮箱不影响它）。
pub fn update_member(
    db: &DbState,
    id: i64,
    name: &str,
    email: &str,
    employee_no: Option<&str>,
    group_ids: &[i64],
) -> AppResult<()> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::ValidationError("姓名不能为空".into()));
    }
    check_email(email)?;
    let email_norm = email.trim().to_lowercase();
    db.write(|conn| {
        let exists: i64 = conn
            .query_row(
                "SELECT count(*) FROM members WHERE lower(email) = ?1 AND id <> ?2",
                params![email_norm, id],
                |row| row.get(0),
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        if exists > 0 {
            return Err(AppError::Duplicate("邮箱".into()));
        }
        conn.execute(
            "UPDATE members SET name = ?1, email = ?2, employee_no = ?3 WHERE id = ?4",
            params![
                name,
                email.trim(),
                employee_no.map(str::trim).filter(|s| !s.is_empty()),
                id
            ],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    })?;
    set_member_groups(db, id, group_ids)
}

/// 停用 / 启用一个成员。
pub fn set_member_active(db: &DbState, id: i64, active: bool) -> AppResult<()> {
    db.write(|conn| {
        conn.execute(
            "UPDATE members SET active = ?1 WHERE id = ?2",
            params![i64::from(active), id],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    })
}

/// 设置某个成员所属的组（整组替换）。
pub fn set_member_groups(db: &DbState, member_id: i64, group_ids: &[i64]) -> AppResult<()> {
    db.write(|conn| {
        conn.execute(
            "DELETE FROM group_members WHERE member_id = ?1",
            params![member_id],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        for gid in group_ids {
            conn.execute(
                "INSERT OR IGNORE INTO group_members (group_id, member_id) VALUES (?1, ?2)",
                params![gid, member_id],
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        }
        Ok(())
    })
}

/// 新建一个邮件组。
pub fn create_group(db: &DbState, name: &str, note: Option<&str>) -> AppResult<Group> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::ValidationError("组名不能为空".into()));
    }
    let next_order = db
        .read(|conn| {
            conn.query_row(
                "SELECT COALESCE(MAX(sort_order), -1) + 1 FROM mail_groups",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|e| AppError::Database(e.to_string()))
        })
        .unwrap_or(0);
    let id = db.write(|conn| {
        let exists: i64 = conn
            .query_row(
                "SELECT count(*) FROM mail_groups WHERE name = ?1",
                params![name],
                |row| row.get(0),
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        if exists > 0 {
            return Err(AppError::Duplicate("邮件组".into()));
        }
        conn.execute(
            "INSERT INTO mail_groups (name, note, sort_order) VALUES (?1, ?2, ?3)",
            params![
                name,
                note.map(str::trim).filter(|s| !s.is_empty()),
                next_order
            ],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(conn.last_insert_rowid())
    })?;
    Ok(Group {
        id,
        name: name.to_string(),
        note: note
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        sort_order: next_order,
        active_member_count: 0,
        member_count: 0,
    })
}

/// 删除一个邮件组 —— **只解除归属，成员本身不动**（ADR-0016 决策 4）。
pub fn delete_group(db: &DbState, id: i64) -> AppResult<()> {
    db.write(|conn| {
        conn.execute("DELETE FROM group_members WHERE group_id = ?1", params![id])
            .map_err(|e| AppError::Database(e.to_string()))?;
        conn.execute("DELETE FROM mail_groups WHERE id = ?1", params![id])
            .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    })
}

/// 预览一段批量粘贴（**不写库**）。
pub fn preview_paste_text(db: &DbState, text: &str) -> AppResult<PastePreview> {
    let existing = existing_emails(db)?;
    Ok(preview_paste(text, &existing))
}

/// 把预览通过的成员写进库。
///
/// 只写**预览通过**的那些（去重与格式校验都已经在预览里做过了）。
/// 写之前再查一次邮箱是否已被占用 —— 预览与写入之间可能隔了一会儿。
pub fn paste_members(db: &DbState, drafts: &[MemberDraft]) -> AppResult<usize> {
    let mut written = 0usize;
    for d in drafts {
        let email_norm = d.email.trim().to_lowercase();
        let exists = db
            .read(|conn| {
                conn.query_row(
                    "SELECT count(*) FROM members WHERE lower(email) = ?1",
                    params![email_norm],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|e| AppError::Database(e.to_string()))
            })
            .unwrap_or(0);
        if exists > 0 {
            continue;
        }
        create_member(db, &d.name, &d.email, None, &[])?;
        written += 1;
    }
    Ok(written)
}

/// 库里已有的邮箱（小写）。
fn existing_emails(db: &DbState) -> AppResult<Vec<String>> {
    db.read(|conn| {
        let mut stmt = conn
            .prepare("SELECT lower(email) FROM members")
            .map_err(|e| AppError::Database(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| AppError::Database(e.to_string()))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| AppError::Database(e.to_string()))?);
        }
        Ok(out)
    })
}

/// 某个成员所属的组名。
fn groups_of_names(db: &DbState, member_id: i64) -> AppResult<Vec<String>> {
    db.read(|conn| groups_of(conn, member_id))
}

/// 名单里的人数（启用 + 停用）。
pub fn member_count(db: &DbState) -> AppResult<i64> {
    db.read(|conn| {
        conn.query_row("SELECT count(*) FROM members", [], |row| row.get(0))
            .map_err(|e| AppError::Database(e.to_string()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::db::CIPHER_PARAMS;
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
                "xinyou-members-test-{}-{nanos}-{label}",
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

    /// 标准 1：三张表都在。
    #[test]
    fn the_three_tables_exist() {
        let fx = Fx::new("std01");
        fx.open();
        let names: Vec<String> = fx
            .db
            .read(|conn| {
                let mut stmt = conn
                    .prepare("SELECT name FROM sqlite_master WHERE type='table'")
                    .expect("查表失败");
                let rows = stmt
                    .query_map([], |row| row.get::<_, String>(0))
                    .expect("查表失败");
                let mut v = Vec::new();
                for r in rows.flatten() {
                    v.push(r);
                }
                Ok(v)
            })
            .expect("读表清单失败");
        for t in ["mail_groups", "members", "group_members"] {
            assert!(names.contains(&t.to_string()), "缺表 {t}：{names:?}");
        }
    }

    /// 标准 2：邮箱全局唯一；一个人可以属于多个组。
    #[test]
    fn email_is_globally_unique_and_a_member_can_join_many_groups() {
        let fx = Fx::new("std02");
        fx.open();
        let g1 = create_group(&fx.db, "研发组", None).expect("建组失败");
        let g2 = create_group(&fx.db, "全员", None).expect("建组失败");

        let m = create_member(
            &fx.db,
            "张三",
            "zhangsan@company.com",
            Some("E001"),
            &[g1.id, g2.id],
        )
        .expect("建成员失败");
        assert_eq!(m.groups.len(), 2, "应属于两个组：{m:?}");

        let dup = create_member(&fx.db, "另一个人", "zhangsan@company.com", None, &[])
            .expect_err("同邮箱应被拒");
        assert!(matches!(dup, AppError::Duplicate(_)), "实际是 {dup:?}");
    }

    /// 标准 4/5：批量粘贴去重；坏行整行跳过。
    #[test]
    fn paste_dedupes_and_skips_bad_rows() {
        let fx = Fx::new("std04");
        fx.open();

        let text = "姓名\t邮箱\n张三\tzhangsan@company.com\n李四\tlisi@company.com\n张三\tzhangsan@company.com\n坏行\tlisi@\n王五\twangwu@company.com";
        let preview = preview_paste_text(&fx.db, text).expect("预览失败");
        assert!(preview.skipped_header, "表头应被跳过：{preview:?}");
        assert_eq!(preview.accepted.len(), 3, "{preview:?}");
        assert_eq!(preview.deduped, 1, "{preview:?}");
        assert_eq!(preview.problems.len(), 1, "{preview:?}");
        assert_eq!(preview.problems[0].line, 5, "要指出第 5 行：{preview:?}");

        let written = paste_members(&fx.db, &preview.accepted).expect("写入失败");
        assert_eq!(written, 3);
        assert_eq!(member_count(&fx.db).expect("计数失败"), 3);
    }

    /// 标准 9：停用的成员不进发送范围，但仍在名单里。
    #[test]
    fn a_deactivated_member_is_listed_but_not_sendable() {
        let fx = Fx::new("std09");
        fx.open();
        let m =
            create_member(&fx.db, "赵六", "zhaoliu@company.com", None, &[]).expect("建成员失败");
        set_member_active(&fx.db, m.id, false).expect("停用失败");

        let all = list_members(&fx.db).expect("列成员失败");
        assert_eq!(all.len(), 1, "停用的人仍在名单里");
        assert!(!all[0].active);

        let sendable = list_sendable_members(&fx.db).expect("列可发送成员失败");
        assert!(sendable.is_empty(), "停用的人不该能收到信");
    }

    /// 标准 10：删组只解除归属，成员不动。
    #[test]
    fn deleting_a_group_only_removes_memberships() {
        let fx = Fx::new("std10");
        fx.open();
        let g = create_group(&fx.db, "研发组", None).expect("建组失败");
        let m = create_member(&fx.db, "张三", "zhangsan@company.com", None, &[g.id])
            .expect("建成员失败");

        delete_group(&fx.db, g.id).expect("删组失败");
        assert!(list_groups(&fx.db).expect("列组失败").is_empty(), "组没了");
        let members = list_members(&fx.db).expect("列成员失败");
        assert_eq!(members.len(), 1, "成员不该被连带删掉");
        assert!(members[0].groups.is_empty(), "归属被解除了");
        assert_eq!(members[0].id, m.id);
    }

    /// 标准 11：同一个人重复加进同一组被拒（复合主键）。
    #[test]
    fn a_member_cannot_join_the_same_group_twice() {
        let fx = Fx::new("std11");
        fx.open();
        let g = create_group(&fx.db, "研发组", None).expect("建组失败");
        let m =
            create_member(&fx.db, "张三", "zhangsan@company.com", None, &[]).expect("建成员失败");

        set_member_groups(&fx.db, m.id, &[g.id, g.id]).expect("设组失败");
        let members = list_members(&fx.db).expect("列成员失败");
        assert_eq!(members[0].groups.len(), 1, "重复加入应只留一条");
    }

    /// 标准 3：批量粘贴的 TSV / CSV 都能落库。
    #[test]
    fn both_tsv_and_csv_paste_into_the_roster() {
        let fx = Fx::new("std03");
        fx.open();
        let tsv = preview_paste_text(&fx.db, "张三\tzhangsan@company.com").expect("预览失败");
        paste_members(&fx.db, &tsv.accepted).expect("写入失败");
        let csv = preview_paste_text(&fx.db, "李四,lisi@company.com").expect("预览失败");
        paste_members(&fx.db, &csv.accepted).expect("写入失败");
        assert_eq!(member_count(&fx.db).expect("计数失败"), 2);
    }

    /// 标准 8：没有删除成员的操作。
    #[test]
    fn there_is_no_delete_member_operation() {
        let src = include_str!("members.rs");
        let production = src.split("#[cfg(test)]").next().unwrap_or_default();
        for forbidden in [
            "fn delete_member",
            "fn remove_member",
            "DELETE FROM members",
        ] {
            assert!(
                !production.contains(forbidden),
                "成员模块里不该有 {forbidden}（成员只能停用）"
            );
        }
    }
}
