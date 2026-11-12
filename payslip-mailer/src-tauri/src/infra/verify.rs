//! 发送前的三重核对（TC-401 · ADR-0024）
//!
//! 设计文档 ch.5「防错机制」（「本产品最重要的差异化」）的发送半：
//!
//! 1. **身份一致**：`identity_verified = 0` 的行、停用的成员、校验列有问题的行
//!    —— 全部**拦在发送范围外**（不是报错，是从范围里排除并让人看见）。
//! 2. **内容一致**：`row_checksum` 现算，与 `send_jobs.checksum`（建批次时的）
//!    比对 —— TC-402 的编排循环在**每一封发送前**跑它。
//! 3. **变量完整**：逐人渲染，required 内置变量空 / required 字段缺 /
//!    渲染失败 → 拦；可选变量空 → 放行（「组名」空着不构成发错工资）。
//!
//! 渲染上下文（[`context_for`])同时服务核对与预览 ——
//! 「核对看到的」与「发出去的」必然是同一份渲染（spec 标准 9）。

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{OptionalExtension, params};
use serde::Serialize;

use crate::domain::field::FieldType;
use crate::domain::record::{self, FieldSpec};
use crate::domain::template::RenderContext;
use crate::domain::template::{format_amount, salary_table_html};
use crate::error::{AppError, AppResult};
use crate::infra::db::DbState;
use crate::infra::periods;
use crate::infra::records;
use crate::infra::templates::{self, Rendered};

/// 一个成员的核对结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberVerification {
    pub member_id: i64,
    pub name: String,
    pub email: String,
    /// 被拦的原因（身份 / 停用 / 校验列）。空 = 身份这一关过了。
    pub blocked_reasons: Vec<String>,
    /// 变量完整性的问题（required 空 / 渲染失败）。
    pub variable_problems: Vec<String>,
    /// 这个人整体能不能发（身份 + 变量两关都过）。
    pub sendable: bool,
    /// 单人预览（渲染结果）。身份被拦的人也有预览 —— 让人看见要改什么。
    pub preview: Option<PreviewPayload>,
}

/// 单人预览的载荷（S11 的 HTML 视图）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewPayload {
    pub subject: String,
    pub html: String,
    pub text: String,
    /// 渲染时这一行的内容 checksum（内容一致的「预览侧」值）。
    pub checksum: String,
}

/// 整个发送范围的核对报告（S11 屏的数据源）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopeVerification {
    /// 全体都能发吗？
    pub ok: bool,
    pub total: usize,
    pub sendable_count: usize,
    pub blocked: Vec<MemberVerification>,
    /// 被拦的原因分布（「身份待核对 ×2 · 个税缺失 ×1」那类汇总由前端算）。
    pub members: Vec<MemberVerification>,
}

/// 渲染一个人的上下文（ADR-0024 决策 6 / spec 标准 10）。
///
/// 口径：
/// - 姓名/工号/邮箱来自**成员表**（不是 Excel 里写的 —— ADR-0021 洞二）；
/// - 年月/年份/月份来自账套；
/// - 公司名称来自 `app_meta.org_name`（没配置 → 空 → required 拦）；
/// - 组名来自成员的组（逗号连接；不属于任何组 → 空 → 可选放行）；
/// - 回信邮箱在 TC-500 之前恒为空（可选放行）；
/// - 字段值来自该行的 values（千分位格式化在取值层）；
/// - 汇总变量来自 [`record::evaluate_row`] 的该行合计；
/// - 工资条表格是全部**启用**字段的 HTML 表。
pub fn context_for(db: &DbState, period_id: i64, member_id: i64) -> AppResult<RenderContext> {
    let period = periods::get(db, period_id)?;
    let fields = display_specs(db, &period)?;
    let member = member_info(db, member_id)?;
    let org_name = crate::infra::admins::get_org_name(db)?.unwrap_or_default();

    // 这一行已有的记录（没有记录的人给空值袋 —— 核对会拦 required）
    let rows = records::list_records(db, period_id)?;
    let row = rows.iter().find(|r| r.member_id == member_id);

    let mut field_values: BTreeMap<String, String> =
        row.map(|r| r.values.clone()).unwrap_or_default();

    // 汇总变量：该行求值（公式列 + 应发/扣款合计）
    let mut earnings = 0.0f64;
    let mut deductions = 0.0f64;
    for spec in &fields {
        if spec.is_formula() {
            continue;
        }
        if let Some(v) = field_values
            .get(&spec.key)
            .and_then(|s| s.parse::<f64>().ok())
        {
            if v >= 0.0 {
                earnings += v;
            } else {
                deductions += v;
            }
        }
    }
    // 公式列的值（该行库里存的；没有记录的人算不出来 → 空 → required 拦）
    let evaluated = record::evaluate_row(&fields, &field_values).ok();
    if let Some(ev) = &evaluated {
        for (k, v) in &ev.computed {
            field_values.insert(k.clone(), v.clone());
        }
    }

    // 格式化在取值层（TC-202 的口径：模板不做格式化）
    let fields_fmt: BTreeMap<String, String> = fields
        .iter()
        .map(|spec| {
            let raw = field_values.get(&spec.key).cloned().unwrap_or_default();
            let shown = if spec.is_formula() || spec.data_type != FieldType::Text {
                if raw.is_empty() {
                    String::new()
                } else {
                    format_amount(&raw)
                }
            } else {
                raw
            };
            (spec.key.clone(), shown)
        })
        .collect();

    // 工资条表格：全部启用字段（空值的行显示「—」—— 财务对账时看得见「没有这项」）
    let table_rows: Vec<(String, String)> = fields
        .iter()
        .map(|spec| {
            let v = fields_fmt.get(&spec.key).cloned().unwrap_or_default();
            let shown = if v.is_empty() { "—".to_string() } else { v };
            (spec.label.clone(), shown)
        })
        .collect();

    let (year, month) = period.year_month.split_once('-').unwrap_or(("", ""));
    let mut builtins = BTreeMap::new();
    builtins.insert("姓名".into(), member.name.clone());
    builtins.insert(
        "工号".into(),
        member.employee_no.clone().unwrap_or_default(),
    );
    builtins.insert("邮箱".into(), member.email.clone());
    builtins.insert(
        "年月".into(),
        format!("{} 年 {} 月", year, month.parse::<u32>().unwrap_or(0)),
    );
    builtins.insert("年份".into(), year.to_string());
    builtins.insert("月份".into(), month.trim_start_matches('0').to_string());
    builtins.insert("公司名称".into(), org_name);
    builtins.insert("组名".into(), member.groups.join("、"));
    // 回信邮箱：SMTP 设置 TC-500 才建 —— 可选变量，先恒空
    builtins.insert("回信邮箱".into(), String::new());
    builtins.insert("应发合计".into(), format_amount(&format!("{earnings:.2}")));
    builtins.insert(
        "扣款合计".into(),
        format_amount(&format!("{deductions:.2}")),
    );
    builtins.insert(
        "实发工资".into(),
        format_amount(&format!("{:.2}", earnings + deductions)),
    );

    Ok(RenderContext {
        builtins,
        fields: fields_fmt,
        required_fields: fields
            .iter()
            .filter(|s| s.required)
            .map(|s| s.key.clone())
            .collect::<BTreeSet<_>>(),
        salary_table_html: salary_table_html(&table_rows),
    })
}

/// 某个月显示的字段（与 TC-301/302/303 同一口径：draft 用当前配置、ready 起用快照）。
fn display_specs(db: &DbState, period: &periods::Period) -> AppResult<Vec<FieldSpec>> {
    let fields = periods::display_fields(db, period)?;
    Ok(fields
        .iter()
        .filter(|f| f.visible)
        .map(|f| FieldSpec {
            key: f.key.clone(),
            label: f.label.clone(),
            data_type: FieldType::parse(&f.data_type).unwrap_or(FieldType::Text),
            formula: f.formula.clone(),
            required: f.required,
        })
        .collect())
}

/// 成员信息（姓名邮箱工号 + 组名）。
struct MemberInfo {
    name: String,
    email: String,
    employee_no: Option<String>,
    active: bool,
    groups: Vec<String>,
}

fn member_info(db: &DbState, member_id: i64) -> AppResult<MemberInfo> {
    let members = crate::infra::members::list_members(db)?;
    let m = members
        .into_iter()
        .find(|m| m.id == member_id)
        .ok_or_else(|| AppError::NotFound("成员".into()))?;
    let groups = db.read(|conn| {
        let mut stmt = conn
            .prepare(
                "SELECT g.name FROM mail_groups g \
                 JOIN group_members gm ON gm.group_id = g.id \
                 WHERE gm.member_id = ?1 ORDER BY g.sort_order, g.name",
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        let rows = stmt
            .query_map(params![member_id], |row| row.get::<_, String>(0))
            .map_err(|e| AppError::Database(e.to_string()))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| AppError::Database(e.to_string()))
    })?;
    Ok(MemberInfo {
        name: m.name,
        email: m.email,
        employee_no: m.employee_no,
        active: m.active,
        groups,
    })
}

/// 渲染一封（预览与核对共用）。
// 未接线：TC-402 的编排循环渲染真邮件时消费（核对走 verify_member 内部的 render）。
#[allow(dead_code)]
pub fn render_for(
    db: &DbState,
    period_id: i64,
    member_id: i64,
    subject_tpl: &str,
    body_html: &str,
) -> AppResult<Rendered> {
    let ctx = context_for(db, period_id, member_id)?;
    templates::render(subject_tpl, body_html, &ctx)
}

/// 一行的内容 checksum（内容一致的「现在侧」）。
///
/// 与 TC-400 建批次时写的 `send_jobs.checksum` 同一公式
/// （`record::checksum(邮箱, values_json)`）—— 两侧可比。
pub fn row_checksum(db: &DbState, period_id: i64, member_id: i64) -> AppResult<String> {
    let rows = records::list_records(db, period_id)?;
    let row = rows
        .iter()
        .find(|r| r.member_id == member_id)
        .ok_or_else(|| AppError::NotFound("工资记录".into()))?;
    let json = record::values_to_json(&row.values);
    Ok(record::checksum(&row.email, &json))
}

/// 内容一致的比对（ADR-0024 决策 4）。
///
/// `expected` 是建批次那一刻的 checksum（`send_jobs.checksum`）。
/// 不一致 → 发送前判死（TC-402 的编排循环消费它）。
// 未接线：TC-402 的编排循环在每封发送前消费它（ADR-0024 决策 4）。
#[allow(dead_code)]
pub fn checksum_matches(
    db: &DbState,
    period_id: i64,
    member_id: i64,
    expected: &str,
) -> AppResult<bool> {
    Ok(row_checksum(db, period_id, member_id)? == expected)
}

/// 逐人核对（ADR-0024 决策 3、5）。
///
/// 身份关：`identity_verified = 0` / 停用 / 校验列有问题 → blocked。
/// 变量关：渲染失败（unknown）→ 拦；required 内置变量空 → 拦；
/// required 字段缺 → 拦；可选空 → 放行。
pub fn verify_member(
    db: &DbState,
    period_id: i64,
    member_id: i64,
    subject_tpl: &str,
    body_html: &str,
) -> AppResult<MemberVerification> {
    let period = periods::get(db, period_id)?;
    let specs = display_specs(db, &period)?;
    let member = member_info(db, member_id)?;
    let mut blocked_reasons: Vec<String> = Vec::new();

    // ---- 身份关 ----
    let identity_ok = db
        .read(|conn| {
            conn.query_row(
                "SELECT identity_verified FROM salary_records \
                 WHERE period_id = ?1 AND member_id = ?2",
                params![period_id, member_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(|e| AppError::Database(e.to_string()))
        })?
        .unwrap_or(1)
        == 1;
    if !identity_ok {
        blocked_reasons.push("身份待核对：导入的姓名与成员名单不一致".into());
    }
    if !member.active {
        blocked_reasons.push("成员已停用，不在发送范围里".into());
    }
    // 校验列（TC-303）：必填缺失等
    let rows = records::list_records(db, period_id)?;
    if let Some(row) = rows.iter().find(|r| r.member_id == member_id)
        && row.has_problem
    {
        blocked_reasons.push(format!("校验未通过：{}", row.validation));
    }
    // 没有记录的人不能发（没有内容）
    if row_of(&rows, member_id).is_none() {
        blocked_reasons.push("这个月没有工资记录".into());
    }

    // ---- 变量关 ----
    let mut variable_problems: Vec<String> = Vec::new();
    let ctx = context_for(db, period_id, member_id)?;

    // required 内置变量空 → 拦。
    // 「工资条表格」单独查（它不在 builtins 里 —— render 时以 safe string 单独注入）；
    // 它的值为空 = 这个月没有启用字段，表都拼不出来，同样该拦。
    for (name, label, required) in crate::domain::template::builtin_vars() {
        if !required || *name == "工资条表格" {
            continue;
        }
        if ctx
            .builtins
            .get(*name)
            .map(|s| s.trim().is_empty())
            .unwrap_or(true)
        {
            variable_problems.push(format!("{label}是空的"));
        }
    }
    if ctx.salary_table_html.trim().is_empty() {
        variable_problems.push("工资条表格是空的".to_string());
    }
    // required 字段缺 → 拦
    for key in &ctx.required_fields {
        let spec = specs.iter().find(|s| &s.key == key);
        let empty = ctx
            .fields
            .get(key)
            .map(|s| s.trim().is_empty())
            .unwrap_or(true);
        if empty && let Some(spec) = spec {
            variable_problems.push(format!("{}是空的", spec.label));
        }
    }
    // 渲染（StrictUndefined）—— unknown 或引擎错误 → 拦
    let rendered = match templates::render(subject_tpl, body_html, &ctx) {
        Ok(r) => Some(r),
        Err(e) => {
            variable_problems.push(e.user_message());
            None
        }
    };

    let preview = rendered.map(|r| {
        let checksum = row_checksum(db, period_id, member_id).unwrap_or_default();
        PreviewPayload {
            subject: r.subject,
            html: r.html,
            text: r.text,
            checksum,
        }
    });

    let sendable = blocked_reasons.is_empty() && variable_problems.is_empty();
    Ok(MemberVerification {
        member_id,
        name: member.name,
        email: member.email,
        blocked_reasons,
        variable_problems,
        sendable,
        preview,
    })
}

fn row_of(rows: &[records::RecordRow], member_id: i64) -> Option<&records::RecordRow> {
    rows.iter().find(|r| r.member_id == member_id)
}

/// 核对整个发送范围（S11 屏的数据源；ADR-0024 决策 6）。
pub fn verify_scope(
    db: &DbState,
    period_id: i64,
    member_ids: &[i64],
    subject_tpl: &str,
    body_html: &str,
) -> AppResult<ScopeVerification> {
    if member_ids.is_empty() {
        return Err(AppError::ValidationError("发送范围是空的".into()));
    }
    let mut members = Vec::new();
    for id in member_ids {
        members.push(verify_member(db, period_id, *id, subject_tpl, body_html)?);
    }
    let sendable_count = members.iter().filter(|m| m.sendable).count();
    Ok(ScopeVerification {
        ok: sendable_count == members.len(),
        total: members.len(),
        sendable_count,
        blocked: members.iter().filter(|m| !m.sendable).cloned().collect(),
        members,
    })
}
