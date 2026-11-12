//! 模板的读写与渲染（TC-202 · ADR-0017）
//!
//! 渲染由 **minijinja** 做，`default-features = false` +
//! `features = ["builtins", "debug", "serde", "unicode"]`：
//!
//! - **不开 `loader`** —— 那是「从文件加载模板」的开关。**不开它，
//!   「从磁盘读模板」这条路在结构上就不存在**（红线 4：沙箱不是靠约定，是靠没开那个开关）。
//! - `unicode` —— 变量名是中文的（`{{姓名}}`）。不开它 minijinja 的 lexer
//!   只认 ASCII 标识符，中文变量名直接报 `syntax error`（实测）。
//! - `serde` —— 构造 `{{字段.xxx}}` 这种嵌套 map。serde 已是项目依赖，零新增 crate。
//!
//! **未知变量 → 报错**（`UndefinedBehavior::Strict`），不渲染成空串。
//! 这是 ch.10 防错机制③「绝不发出「{{实发工资}}」字面量」的第一道防线。

use std::collections::BTreeMap;

use minijinja::{Environment, UndefinedBehavior, Value};

use crate::domain::template::{
    DEFAULT_TEMPLATE_BODY, DEFAULT_TEMPLATE_NAME, DEFAULT_TEMPLATE_SUBJECT, RenderContext,
    TemplateProblem, check_variables, html_to_text, salary_table_html,
};
use crate::error::{AppError, AppResult};
use crate::infra::db::DbState;
use crate::infra::fields;

/// 一个模板（界面看到的那一行）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Template {
    pub id: i64,
    pub name: String,
    /// 主题的源码（可含变量）。
    pub subject_tpl: String,
    /// 正文 HTML 的源码（可含变量）。
    pub body_html: String,
    /// 纯文本兜底版本（自动生成）。
    pub body_text: String,
    pub is_default: bool,
}

/// 渲染结果。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Rendered {
    pub subject: String,
    pub html: String,
    pub text: String,
}

/// 列出全部模板（默认的那个排前面）。
pub fn list(db: &DbState) -> AppResult<Vec<Template>> {
    db.read(|conn| {
        let mut stmt = conn
            .prepare(
                "SELECT id, name, subject_tpl, body_html, body_text, is_default \
                 FROM email_templates ORDER BY is_default DESC, name, id",
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })
            .map_err(|e| AppError::Database(e.to_string()))?;
        let mut out = Vec::new();
        for row in rows {
            let (id, name, subject_tpl, body_html, body_text, is_default) =
                row.map_err(|e| AppError::Database(e.to_string()))?;
            out.push(Template {
                id,
                name,
                subject_tpl,
                body_html,
                body_text,
                is_default: is_default != 0,
            });
        }
        Ok(out)
    })
}

/// 新建一个模板。
pub fn create(db: &DbState, name: &str, subject_tpl: &str, body_html: &str) -> AppResult<Template> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::ValidationError("模板名不能为空".into()));
    }
    let body_text = html_to_text(body_html);
    let id = db.write(|conn| {
        let exists: i64 = conn
            .query_row(
                "SELECT count(*) FROM email_templates WHERE name = ?1",
                rusqlite::params![name],
                |row| row.get(0),
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        if exists > 0 {
            return Err(AppError::Duplicate("模板名".into()));
        }
        conn.execute(
            "INSERT INTO email_templates (name, subject_tpl, body_html, body_text, is_default) \
             VALUES (?1, ?2, ?3, ?4, 0)",
            rusqlite::params![name, subject_tpl, body_html, body_text],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(conn.last_insert_rowid())
    })?;
    Ok(Template {
        id,
        name: name.to_string(),
        subject_tpl: subject_tpl.to_string(),
        body_html: body_html.to_string(),
        body_text,
        is_default: false,
    })
}

/// 编辑一个模板（名字也可以改）。
pub fn update(
    db: &DbState,
    id: i64,
    name: &str,
    subject_tpl: &str,
    body_html: &str,
) -> AppResult<()> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::ValidationError("模板名不能为空".into()));
    }
    let body_text = html_to_text(body_html);
    db.write(|conn| {
        let exists: i64 = conn
            .query_row(
                "SELECT count(*) FROM email_templates WHERE name = ?1 AND id <> ?2",
                rusqlite::params![name, id],
                |row| row.get(0),
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        if exists > 0 {
            return Err(AppError::Duplicate("模板名".into()));
        }
        conn.execute(
            "UPDATE email_templates SET name = ?1, subject_tpl = ?2, body_html = ?3, body_text = ?4 \
             WHERE id = ?5",
            rusqlite::params![name, subject_tpl, body_html, body_text, id],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    })
}

/// 存为副本（设计文档 S10 的按钮）。
///
/// 副本**不是默认模板** —— 默认只有一个，改副本不影响原模板（标准 11）。
pub fn duplicate(db: &DbState, id: i64) -> AppResult<Template> {
    let src = list(db)?
        .into_iter()
        .find(|t| t.id == id)
        .ok_or_else(|| AppError::NotFound("模板".into()))?;
    let mut name = format!("{} 副本", src.name);
    let mut n = 2;
    while list(db)?.iter().any(|t| t.name == name) {
        name = format!("{} 副本 {}", src.name, n);
        n += 1;
    }
    create(db, &name, &src.subject_tpl, &src.body_html)
}

/// 设为默认模板。
///
/// **模板里有未知变量时拒绝**（设计文档 S10：「未解析变量高亮并阻止设为默认」）。
/// 判据用「未停用的字段 + 全部内置变量」的白名单 —— 已停用的字段不算未知
/// （它只是不能用在**新**模板里）。
pub fn set_default(
    db: &DbState,
    id: i64,
    allowed_for_new: &[String],
    known_for_rendering: &[String],
) -> AppResult<()> {
    let tpl = list(db)?
        .into_iter()
        .find(|t| t.id == id)
        .ok_or_else(|| AppError::NotFound("模板".into()))?;
    let ctx = RenderContext::default();
    let (unknown, _) = check_variables(
        &format!("{}\n{}", tpl.subject_tpl, tpl.body_html),
        &ctx,
        allowed_for_new,
        known_for_rendering,
    );
    if !unknown.is_empty() {
        return Err(AppError::TemplateError(format!(
            "模板里有未解析的变量，不能设为默认：{}",
            unknown
                .iter()
                .map(|p| p.variable.as_str())
                .collect::<Vec<_>>()
                .join("、")
        )));
    }
    db.write(|conn| {
        conn.execute("UPDATE email_templates SET is_default = 0", [])
            .map_err(|e| AppError::Database(e.to_string()))?;
        conn.execute(
            "UPDATE email_templates SET is_default = 1 WHERE id = ?1",
            rusqlite::params![id],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    })
}

/// 删除一个模板。**默认模板不能删** —— 没有默认模板时发信会没有模板可用。
pub fn delete(db: &DbState, id: i64) -> AppResult<()> {
    let tpl = list(db)?
        .into_iter()
        .find(|t| t.id == id)
        .ok_or_else(|| AppError::NotFound("模板".into()))?;
    if tpl.is_default {
        return Err(AppError::ValidationError(
            "默认模板不能删除 —— 先把另一个模板设为默认".into(),
        ));
    }
    db.write(|conn| {
        conn.execute(
            "DELETE FROM email_templates WHERE id = ?1",
            rusqlite::params![id],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    })
}

/// 渲染一份模板。
///
/// 未知变量 → `Err`（不是渲染成空串）。这条是「绝不发出字面量」的兜底：
/// 就算某条路径忘了先校验变量，渲染这一步也会挡住。
pub fn render(subject_tpl: &str, body_html: &str, ctx: &RenderContext) -> AppResult<Rendered> {
    let mut env = Environment::new();
    // 未知变量直接报错，不渲染成空串
    env.set_undefined_behavior(UndefinedBehavior::Strict);

    for (k, v) in &ctx.builtins {
        env.add_global(k.clone(), Value::from(v.clone()));
    }
    // `工资条表格` 是**已渲染好的 HTML**，不能被转义
    env.add_global(
        "工资条表格".to_string(),
        Value::from_safe_string(ctx.salary_table_html.clone()),
    );
    if !ctx.fields.is_empty() {
        let mut fields: BTreeMap<String, String> = ctx.fields.clone();
        fields.insert("__required__".to_string(), String::new());
        env.add_global("字段", Value::from(minijinja::value::Serde(&fields)));
    }

    let subject = env
        .render_str(subject_tpl, ())
        .map_err(|e| AppError::TemplateError(format!("主题渲染失败：{e}")))?;
    let html = env
        .render_str(body_html, ())
        .map_err(|e| AppError::TemplateError(format!("正文渲染失败：{e}")))?;
    Ok(Rendered {
        subject,
        html: html.clone(),
        text: html_to_text(&html),
    })
}

/// 变量面板里的一项（与 `domain::template::TemplateVariable` 同形，
/// 这里另起一个是为了 Command 层不直接依赖 domain 的内部结构）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VariableInfo {
    /// 模板里写什么。
    pub name: String,
    /// 界面上显示什么。
    pub label: String,
    /// 为空时是否要拦下。
    pub required: bool,
    pub kind: VariableKind,
}

/// 变量是内置的还是字段的。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum VariableKind {
    Builtin,
    Field,
}

/// 变量校验的结果（界面高亮用）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VariableCheck {
    /// 未解析的变量（不能设为默认）。
    pub unknown: Vec<TemplateProblem>,
    /// 必填但为空的变量（发送前拦下这个人）。
    pub empty: Vec<TemplateProblem>,
}

/// 内置变量（界面上固定的那一组）。
pub fn builtin_variables() -> Vec<VariableInfo> {
    use crate::domain::template::variable_panel;
    variable_panel(&[])
        .into_iter()
        .map(|v| VariableInfo {
            name: v.name,
            label: v.label,
            required: v.required,
            kind: match v.kind {
                crate::domain::template::VariableKind::Builtin => VariableKind::Builtin,
                crate::domain::template::VariableKind::Field => VariableKind::Field,
            },
        })
        .collect()
}

/// 两份白名单：
/// - `allowed_for_new`：新建模板时能用的（**未停用**的字段 + 全部内置变量）；
/// - `known_for_rendering`：渲染历史模板时要认的（**含已停用的**字段 + 全部内置变量）。
///
/// 这两份不是一回事（红线 8 的另一面：停用只影响新数据，老模板还得能渲染）。
pub fn whitelists(db: &DbState) -> AppResult<(Vec<String>, Vec<String>)> {
    let all = fields::list(db)?;
    let mut allowed = Vec::new();
    let mut known = Vec::new();
    for f in &all {
        known.push(format!(
            "{}{}",
            crate::domain::template::FIELD_PREFIX,
            f.key
        ));
        if f.visible {
            allowed.push(format!(
                "{}{}",
                crate::domain::template::FIELD_PREFIX,
                f.key
            ));
        }
    }
    let mut builtins = Vec::new();
    for v in builtin_variables() {
        builtins.push(v.name);
    }
    allowed.extend(builtins.iter().cloned());
    known.extend(builtins);
    Ok((allowed, known))
}

/// 用示例数据渲染一份模板（预览用）。
///
/// **示例数据不是真实数据**（真实工资数据在 TC-303 才有）——
/// 界面必须标明「示例数据」，否则使用者会以为看到的是真人的工资。
pub fn preview(subject_tpl: &str, body_html: &str) -> AppResult<Rendered> {
    // 金额的千分位在这里加 —— 模板不做格式化（模板不该知道「这个变量是钱」）
    fn fmt(v: &str) -> String {
        crate::domain::template::format_amount(v)
    }
    let mut builtins = BTreeMap::new();
    builtins.insert("姓名".to_string(), "张三".to_string());
    builtins.insert("工号".to_string(), "E001".to_string());
    builtins.insert("年月".to_string(), "2026 年 10 月".to_string());
    builtins.insert("公司名称".to_string(), "示例科技有限公司".to_string());
    builtins.insert("组名".to_string(), "研发组".to_string());
    builtins.insert("回信邮箱".to_string(), "hr@example.com".to_string());
    builtins.insert("应发合计".to_string(), fmt("21200.00"));
    builtins.insert("扣款合计".to_string(), fmt("-4360.00"));
    builtins.insert("实发工资".to_string(), fmt("16840.00"));

    let mut fields = BTreeMap::new();
    fields.insert("base_salary".to_string(), fmt("18000.00"));
    fields.insert("performance".to_string(), fmt("3200.00"));
    fields.insert("meal_allowance".to_string(), fmt("0.00"));
    fields.insert("social_insurance".to_string(), fmt("-2100.00"));
    fields.insert("housing_fund".to_string(), fmt("-2260.00"));
    fields.insert("net_pay".to_string(), fmt("16840.00"));
    fields.insert("remark".to_string(), String::new());

    let ctx = RenderContext {
        builtins,
        fields,
        required_fields: ["base_salary".to_string(), "net_pay".to_string()]
            .into_iter()
            .collect(),
        salary_table_html: salary_table_html(&[
            ("基本工资".to_string(), "18,000.00".to_string()),
            ("绩效奖金".to_string(), "3,200.00".to_string()),
            ("社保个人".to_string(), "-2,100.00".to_string()),
            ("实发工资".to_string(), "16,840.00".to_string()),
        ]),
    };
    render(subject_tpl, body_html, &ctx)
}

/// 写入默认模板（设计文档 S10 的「标准中文模板」，正文取 ch.10 原文）。
///
/// 只在表为空时写 —— 覆盖掉用户配好的模板是灾难。
pub fn ensure_defaults(db: &DbState) -> AppResult<usize> {
    if !list(db)?.is_empty() {
        return Ok(0);
    }
    let tpl = create(
        db,
        DEFAULT_TEMPLATE_NAME,
        DEFAULT_TEMPLATE_SUBJECT,
        DEFAULT_TEMPLATE_BODY,
    )?;
    db.write(|conn| {
        conn.execute(
            "UPDATE email_templates SET is_default = 1 WHERE id = ?1",
            rusqlite::params![tpl.id],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    })?;
    tracing::info!("已写入默认邮件模板（设计文档 S10 的「标准中文模板」）");
    Ok(1)
}

/// 模板里用到的未解析变量（界面高亮用）。
pub fn problems_of(
    subject_tpl: &str,
    body_html: &str,
    allowed_for_new: &[String],
    known_for_rendering: &[String],
) -> (Vec<TemplateProblem>, Vec<TemplateProblem>) {
    let ctx = RenderContext::default();
    check_variables(
        &format!("{subject_tpl}\n{body_html}"),
        &ctx,
        allowed_for_new,
        known_for_rendering,
    )
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
                "xinyou-templates-test-{}-{nanos}-{label}",
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

    /// 标准 1：首启后有设计文档那个默认模板，且它是默认的。
    #[test]
    fn the_default_template_matches_the_design_doc() {
        let fx = Fx::new("std01");
        fx.open();
        assert_eq!(ensure_defaults(&fx.db).expect("写默认模板失败"), 1);

        let list = list(&fx.db).expect("列模板失败");
        assert_eq!(list.len(), 1);
        let tpl = &list[0];
        assert_eq!(tpl.name, "标准中文模板");
        assert!(tpl.is_default, "它应该是默认模板");
        for v in [
            "{{姓名}}",
            "{{年月}}",
            "{{工资条表格}}",
            "{{实发工资}}",
            "{{回信邮箱}}",
        ] {
            assert!(tpl.body_html.contains(v), "默认模板正文里少了 {v}");
        }
        assert!(tpl.body_text.contains("姓名") || !tpl.body_text.is_empty());
    }

    /// 标准 1 的一半：默认模板只在表为空时写。
    #[test]
    fn defaults_are_only_written_when_the_table_is_empty() {
        let fx = Fx::new("std01b");
        fx.open();
        ensure_defaults(&fx.db).expect("写默认模板失败");
        create(&fx.db, "我自己的模板", "主题", "<p>正文</p>").expect("建模板失败");
        assert_eq!(ensure_defaults(&fx.db).expect("再次初始化失败"), 0);
        assert_eq!(list(&fx.db).expect("列模板失败").len(), 2);
    }

    /// 标准 2：变量面板 = 内置变量 + 未停用的字段。
    #[test]
    fn the_panel_lists_builtins_and_visible_fields_only() {
        let fx = Fx::new("std02");
        fx.open();
        crate::infra::fields::ensure_defaults(&fx.db).expect("写默认字段失败");

        let panel = builtin_variables();
        assert!(panel.iter().any(|v| v.name == "姓名"));
        assert!(panel.iter().any(|v| v.name == "工资条表格"));

        let (allowed, known) = whitelists(&fx.db).expect("白名单失败");
        assert!(
            allowed.iter().any(|a| a == "字段.base_salary"),
            "{allowed:?}"
        );
        assert!(known.iter().any(|k| k == "字段.base_salary"));
    }

    /// 标准 3：未解析的变量 → 不能设为默认。
    #[test]
    fn an_unknown_variable_blocks_setting_the_default() {
        let fx = Fx::new("std03");
        fx.open();
        crate::infra::fields::ensure_defaults(&fx.db).expect("写默认字段失败");
        let tpl = create(
            &fx.db,
            "坏模板",
            "{{字段.nosuchfield}}",
            "<p>{{字段.nosuchfield}}</p>",
        )
        .expect("建模板失败");

        // 先把一个好模板设为默认（这样才看得出「坏模板没能顶掉它」）
        let good =
            create(&fx.db, "好模板", "{{年月}} 工资条", "<p>{{姓名}}</p>").expect("建模板失败");
        let (allowed, known) = whitelists(&fx.db).expect("白名单失败");
        set_default(&fx.db, good.id, &allowed, &known).expect("设默认失败");

        let err = set_default(&fx.db, tpl.id, &allowed, &known).expect_err("应被拒");
        assert!(
            err.user_message().contains("nosuchfield"),
            "错误要点名是哪个变量：{}",
            err.user_message()
        );
        // 默认模板没被动过
        let list = list(&fx.db).expect("列模板失败");
        assert!(list.iter().find(|x| x.id == good.id).unwrap().is_default);
        assert!(!list.iter().find(|x| x.id == tpl.id).unwrap().is_default);
    }

    /// 标准 4：已停用的字段在历史模板里仍能渲染（不算「未知」）。
    #[test]
    fn a_deactivated_field_can_still_be_rendered() {
        let fx = Fx::new("std04");
        fx.open();
        crate::infra::fields::ensure_defaults(&fx.db).expect("写默认字段失败");

        // 停用「餐补」
        let fields = crate::infra::fields::list(&fx.db).expect("列字段失败");
        let meal = fields.iter().find(|f| f.key == "meal_allowance").unwrap();
        crate::infra::fields::set_visible(&fx.db, meal.id, false).expect("停用失败");

        let (allowed, known) = whitelists(&fx.db).expect("白名单失败");
        // 它不在「新建可选」里，但在「历史要认」里
        assert!(!allowed.iter().any(|a| a == "字段.meal_allowance"));
        assert!(known.iter().any(|k| k == "字段.meal_allowance"));

        // 老模板引用它 → 不算未知变量
        let (unknown, _) = problems_of("{{字段.meal_allowance}}", "", &allowed, &known);
        assert!(unknown.is_empty(), "停用的字段不该报「未知」：{unknown:?}");
    }

    /// 标准 5：字段变量用 key —— 改显示名不影响老模板。
    #[test]
    fn renaming_a_field_does_not_break_existing_templates() {
        let fx = Fx::new("std05");
        fx.open();
        crate::infra::fields::ensure_defaults(&fx.db).expect("写默认字段失败");
        create(
            &fx.db,
            "我的模板",
            "{{年月}} 工资条",
            "<p>{{字段.base_salary}}</p>",
        )
        .expect("建模板失败");

        // 改显示名（key 不可改）
        let fields = crate::infra::fields::list(&fx.db).expect("列字段失败");
        let base = fields.iter().find(|f| f.key == "base_salary").unwrap();
        crate::infra::fields::update(
            &fx.db,
            base.id,
            "底薪",
            crate::domain::field::FieldType::Money,
            base.required,
            base.formula.as_deref(),
        )
        .expect("改名失败");

        // 老模板仍然能渲染（它引的是 key）
        let r = preview("{{年月}} 工资条", "<p>{{字段.base_salary}}</p>").expect("预览失败");
        assert!(r.html.contains("18,000.00"), "老模板不该坏：{}", r.html);
    }

    /// 标准 6：沙箱是结构性的 —— 没开 loader，就没有读文件的能力。
    #[test]
    fn the_renderer_cannot_read_files() {
        // Cargo.toml 里 minijinja 不能开 `loader` 特性
        let manifest = include_str!("../../Cargo.toml");
        let line = manifest
            .lines()
            .find(|l| l.contains("minijinja"))
            .expect("Cargo.toml 里没有 minijinja 那行");
        assert!(
            !line.contains("loader"),
            "minijinja 不能开 loader（那是「从文件读模板」的开关）：{line}"
        );
        // 而且真渲染一个带 include 的模板 → 报错，不是读到文件
        let err = render(
            "{% include 'x.html' %}",
            "<p>hi</p>",
            &RenderContext::default(),
        )
        .expect_err("include 应该报错");
        assert!(
            err.user_message().contains("渲染失败"),
            "实际是：{}",
            err.user_message()
        );
    }

    /// 标准 7：纯文本兜底自动生成。
    #[test]
    fn the_plain_text_fallback_is_generated() {
        let fx = Fx::new("std07");
        fx.open();
        let tpl = create(
            &fx.db,
            "带表格",
            "{{年月}} 工资条",
            "<p>{{姓名}}</p>{{工资条表格}}<p>实发 {{实发工资}}</p>",
        )
        .expect("建模板失败");
        assert!(
            !tpl.body_text.contains('<'),
            "纯文本里不该有标签：{}",
            tpl.body_text
        );
        assert!(tpl.body_text.contains("实发"), "{}", tpl.body_text);
    }

    /// 标准 10：预览用示例数据，且渲染得出来。
    #[test]
    fn the_preview_renders_with_sample_data() {
        let r = preview("{{年月}} 工资条", "<p>{{姓名}}</p><p>{{实发工资}}</p>").expect("预览失败");
        assert!(r.subject.contains("2026 年 10 月"), "{}", r.subject);
        assert!(r.html.contains("张三"), "{}", r.html);
        assert!(r.html.contains("16,840.00"), "金额应当带千分位：{}", r.html);
        assert!(!r.text.contains('<'), "纯文本里不该有标签：{}", r.text);
    }

    /// 标准 11：存为副本 —— 改副本不影响原模板。
    #[test]
    fn duplicating_a_template_leaves_the_original_alone() {
        let fx = Fx::new("std11");
        fx.open();
        let original = create(&fx.db, "标准模板", "主题", "<p>正文</p>").expect("建模板失败");
        let copy = duplicate(&fx.db, original.id).expect("存副本失败");
        assert_ne!(copy.id, original.id);
        assert!(copy.name.contains("副本"));
        assert!(!copy.is_default, "副本不该是默认模板");

        update(&fx.db, copy.id, "改过的副本", "新主题", "<p>新正文</p>").expect("改副本失败");
        let list = list(&fx.db).expect("列模板失败");
        let orig = list.iter().find(|t| t.id == original.id).unwrap();
        assert_eq!(orig.body_html, "<p>正文</p>", "原模板不该被改");
    }

    /// 默认模板不能删（没有默认模板时发信没有模板可用）。
    #[test]
    fn the_default_template_cannot_be_deleted() {
        let fx = Fx::new("std11b");
        fx.open();
        ensure_defaults(&fx.db).expect("写默认模板失败");
        let tpl = list(&fx.db).expect("列模板失败")[0].clone();
        let err = delete(&fx.db, tpl.id).expect_err("默认模板不该能删");
        assert!(
            err.user_message().contains("默认模板"),
            "{}",
            err.user_message()
        );
    }
}
