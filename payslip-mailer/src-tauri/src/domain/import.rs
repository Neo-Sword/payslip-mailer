//! Excel 导入的纯逻辑（TC-302 · ADR-0021）
//!
//! 三件事，全都不碰文件、不碰数据库：
//!
//! 1. **表头映射**：Excel 的列名 → 目标（固定列 / 工资字段 / 跳过），
//!    分「精确 / 模糊 / 未匹配」三档（ADR-0021 决策 1、2）。
//! 2. **行级校验**：每行是「成功 / 警告 / 错误」（决策 4）。
//! 3. **冲突策略**：覆盖 / 仅补空 / 取消（决策 8）。
//!
//! 数值解析的判据在 [`parse_amount`]：**空 = 没有这一项**（不是 0、不是错误），
//! 带千分位与货币符号要能解析（决策 6）—— 与 TC-301 导出的说明 Sheet 说同一套话。

use std::collections::BTreeMap;

use serde::Serialize;

use crate::domain::field::FieldType;
use crate::domain::member::check_email;

/// 固定列的键（设计文档 S08 的三个固定列：姓名 / 邮箱 / 工号）。
///
/// 它们**不是** `salary_fields` 的行 —— 固定列来自成员表，工资字段来自工资条项目配置，
/// 两者的来源不同（ADR-0021 洞一）。
pub const FIXED_NAME: &str = "name";
pub const FIXED_EMAIL: &str = "email";
pub const FIXED_EMPLOYEE_NO: &str = "employee_no";

/// 固定列的显示名（Excel 表头里写的就是这三个词）。
pub const FIXED_LABELS: [(&str, &str); 3] = [
    (FIXED_NAME, "姓名"),
    (FIXED_EMAIL, "邮箱"),
    (FIXED_EMPLOYEE_NO, "工号"),
];

/// 公式列在导出的表里写的占位文字（TC-301 的 `FORMULA_PLACEHOLDER`）。
///
/// 导入时看到它 → **静默忽略**；看到别的 → 错误（ADR-0021 洞四 / 决策 7）。
pub const FORMULA_PLACEHOLDER: &str = "公式自动计算";

/// 一列的目标是什么。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ColumnTarget {
    /// 固定列（姓名 / 邮箱 / 工号）。
    Fixed { key: String },
    /// 工资字段（按 field key）。
    Field { key: String },
    /// 这一列被跳过（不导入）。
    Skip,
}

/// 匹配的可信度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MatchKind {
    /// 表头与目标名完全相等 —— 直接采用，不需要用户确认。
    Exact,
    /// 靠包含关系命中 —— **需要用户确认**。
    Fuzzy,
    /// 没命中。
    Unmatched,
}

/// 一个可被映射的目标（一列）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappingTarget {
    /// 稳定键（固定列的 key 或字段的 field key）。
    pub key: String,
    /// 显示名（匹配时比的就是它）。
    pub label: String,
    /// 是固定列吗？
    pub fixed: bool,
    /// 字段类型（固定列是 `text`）。
    pub data_type: FieldType,
    /// 是公式列吗？
    pub formula: bool,
}

/// 一列的映射结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnMapping {
    /// Excel 里的列序（0 基）。
    pub index: usize,
    /// Excel 里的表头文字。
    pub header: String,
    /// 映射到哪儿。
    pub target: ColumnTarget,
    /// 匹配的可信度。
    pub match_kind: MatchKind,
    /// 模糊命中时，这一条要用户确认才能入库。
    pub needs_confirmation: bool,
}

/// 归一化表头：去空格、去全角空格、去掉 `*` 标记、大小写无关。
///
/// 为什么去 `*`：TC-301 导出的表头必填列写的是「姓名 *」，
/// 导回来时不该因为那个星号就变成「模糊匹配」。
fn normalize_header(text: &str) -> String {
    text.trim()
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '*' && *c != '＊')
        .collect::<String>()
        .to_lowercase()
}

/// 一列的匹配结果（内部用）。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Hit {
    Exact(MappingTarget),
    Fuzzy(MappingTarget),
}

/// 找出表头命中了哪个目标。
///
/// **先精确后模糊**（ADR-0021 决策 2）：
/// 精确 = 归一化后完全相等；模糊 = 双向包含。
///
/// 精确命中时**不看别的目标** —— 「基本工资」精确命中 `base_salary`，
/// 就不会又被某个「工资」字段的模糊规则抢走。
fn hit_target(header: &str, targets: &[MappingTarget]) -> Hit {
    let h = normalize_header(header);
    if h.is_empty() {
        return Hit::Fuzzy(MappingTarget {
            key: String::new(),
            label: String::new(),
            fixed: false,
            data_type: FieldType::Text,
            formula: false,
        });
    }

    // 第一轮：精确
    for t in targets {
        if normalize_header(&t.label) == h {
            return Hit::Exact(t.clone());
        }
    }
    // 第二轮：模糊（双向包含）
    for t in targets {
        let l = normalize_header(&t.label);
        if l.is_empty() {
            continue;
        }
        if h.contains(&l) || l.contains(&h) {
            return Hit::Fuzzy(t.clone());
        }
    }
    Hit::Fuzzy(MappingTarget {
        key: String::new(),
        label: String::new(),
        fixed: false,
        data_type: FieldType::Text,
        formula: false,
    })
}

/// 把 Excel 的表头映射到目标列。
///
/// `targets` 里既有固定列也有工资字段 —— 顺序决定**先命中谁**：
/// 固定列放前面（TC-301 导出的表头里固定列在最左）。
pub fn map_headers(headers: &[String], targets: &[MappingTarget]) -> Vec<ColumnMapping> {
    headers
        .iter()
        .enumerate()
        .map(|(index, header)| {
            let hit = hit_target(header, targets);
            match hit {
                Hit::Exact(t) => ColumnMapping {
                    index,
                    header: header.clone(),
                    target: target_of(&t),
                    match_kind: MatchKind::Exact,
                    needs_confirmation: false,
                },
                Hit::Fuzzy(t) if !t.key.is_empty() => ColumnMapping {
                    index,
                    header: header.clone(),
                    target: target_of(&t),
                    match_kind: MatchKind::Fuzzy,
                    needs_confirmation: true,
                },
                Hit::Fuzzy(_) => ColumnMapping {
                    index,
                    header: header.clone(),
                    target: ColumnTarget::Skip,
                    match_kind: MatchKind::Unmatched,
                    needs_confirmation: false,
                },
            }
        })
        .collect()
}

/// 目标 → 映射结果里的目标表示。
fn target_of(t: &MappingTarget) -> ColumnTarget {
    if t.fixed {
        ColumnTarget::Fixed { key: t.key.clone() }
    } else {
        ColumnTarget::Field { key: t.key.clone() }
    }
}

/// 映射里有没有「必须确认」的模糊命中。
pub fn has_unconfirmed(mappings: &[ColumnMapping]) -> bool {
    mappings.iter().any(|m| m.needs_confirmation)
}

/// 三个固定列是不是都映射到了（ADR-0021 决策 3）。
///
/// 返回没对上的固定列 key（空 = 都对上了）。
pub fn missing_fixed_columns(mappings: &[ColumnMapping]) -> Vec<String> {
    FIXED_LABELS
        .iter()
        .filter(|(key, _)| {
            !mappings
                .iter()
                .any(|m| matches!(&m.target, ColumnTarget::Fixed { key: k } if k == key))
        })
        .map(|(key, label)| format!("{label}（{key}）"))
        .collect()
}

/// 一行的问题有多严重。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Severity {
    /// 阻断入库。
    Error,
    /// 入库但带标记。
    Warning,
}

/// 一行上的一个问题。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RowProblem {
    /// Excel 里的行号（1 基，**含表头行** —— 与 Excel 显示的一致）。
    pub row: usize,
    /// 出问题的列名（Excel 的表头文字）。
    pub column: String,
    /// 严重程度。
    pub severity: Severity,
    /// 说清「怎么改」。
    pub message: String,
}

/// 一条校验结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportSummary {
    pub success: usize,
    pub warnings: usize,
    pub errors: usize,
}

/// 合并两个统计。
impl ImportSummary {
    pub fn new() -> Self {
        Self {
            success: 0,
            warnings: 0,
            errors: 0,
        }
    }

    /// 记一行的问题。
    pub fn add(&mut self, severity: Severity) {
        match severity {
            Severity::Error => self.errors += 1,
            Severity::Warning => self.warnings += 1,
        }
    }

    /// 成功行数。
    pub fn add_success(&mut self) {
        self.success += 1;
    }

    /// 能不能入库（K>0 禁止，ADR-0021 决策 9）。
    pub fn can_commit(&self) -> bool {
        self.errors == 0
    }
}

impl Default for ImportSummary {
    fn default() -> Self {
        Self::new()
    }
}

/// 成员的最小信息（导入要用来核对身份）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberRef {
    pub id: i64,
    pub name: String,
    pub email: String,
    pub employee_no: Option<String>,
    pub active: bool,
}

/// 冲突策略（ADR-0021 决策 8）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConflictStrategy {
    /// 覆盖已有记录。
    Overwrite,
    /// 只填已有记录里没有的字段。
    FillEmpty,
    /// 有冲突就整批不做。
    Cancel,
}

/// 解析金额。
///
/// - **空 → `Ok(None)`**：这个人不填这项（不是 0、不是错误）。
/// - 带千分位逗号 / 货币符号 / 空白 → 去掉后解析。
/// - **全角数字 → 先归一到半角**（ch.13 必测边界用例「中文全角数字」：
///   中文输入法打出的「１８５００」必须和「18500」一样被解析）。
/// - 解析不出来 → `Err`（说清「这一格应该填数字」）。
pub fn parse_amount(text: &str) -> Result<Option<f64>, String> {
    let raw = text.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    let raw = crate::util::normalize_fullwidth(raw);
    let cleaned: String = raw
        .chars()
        .filter(|c| {
            !matches!(c, ',' | '，' | '¥' | '￥' | '$' | ' ' | '\u{3000}' | '元')
                && !c.is_whitespace()
        })
        .collect();
    let cleaned = cleaned.trim_end_matches('%').trim();
    if cleaned.is_empty() {
        return Ok(None);
    }
    cleaned
        .parse::<f64>()
        .map(Some)
        .map_err(|_| "这一格应该填数字".to_string())
}

/// 解析百分比（`%` 去掉后必须在 0–100）。
pub fn parse_percent(text: &str) -> Result<Option<f64>, String> {
    match parse_amount(text)? {
        None => Ok(None),
        Some(v) => {
            if !(0.0..=100.0).contains(&v) {
                Err("百分比要在 0–100 之间".to_string())
            } else {
                Ok(Some(v))
            }
        }
    }
}

/// 解析一个数值型字段的值。
pub fn parse_numeric(data_type: FieldType, text: &str) -> Result<Option<f64>, String> {
    match data_type {
        FieldType::Percent => parse_percent(text),
        // money / number / formula 一律按十进制数解析。
        // formula 列到不了这里（用户填写值只被检查是不是占位文字），
        // 但真走到也不会出错。
        _ => parse_amount(text),
    }
}

/// 一行导入的结果（校验通过的那部分）。
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedRow {
    /// Excel 里的行号（1 基，含表头行）。
    pub row: usize,
    /// 对应到的成员 id。
    pub member_id: i64,
    /// 姓名（成员表里的，**不是** Excel 里写的 —— 见 ADR-0021 洞二）。
    pub name: String,
    /// 邮箱。
    pub email: String,
    /// 字段 key → 值（文本）。**没有的 key 表示「这一项不存在」**。
    pub values: BTreeMap<String, String>,
    /// 这一行有没有「身份待核对」的警告。
    pub identity_unverified: bool,
}

/// 一行在表里的样子（校验要用的最小信息）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawRow {
    /// Excel 里的行号（1 基，含表头行）。
    pub row: usize,
    /// 列序 → 单元格文字。
    pub cells: BTreeMap<usize, String>,
}

/// 校验的结果。
#[derive(Debug, Clone, PartialEq)]
pub struct ValidatedRows {
    /// 通过校验的行。
    pub rows: Vec<ParsedRow>,
    /// 所有问题。
    pub problems: Vec<RowProblem>,
    /// 统计。
    pub summary: ImportSummary,
}

/// 校验一批行。
///
/// `mappings` 是确认后的映射（列序 → 目标）。
/// `targets` 用来查每个字段的类型与是否公式列。
/// `members` 是成员表（按 id 查）。
///
/// **「同月已有数据」不在这里判** —— 那是**冲突策略**的事
/// （覆盖 / 仅补空 / 取消，ADR-0021 决策 8），在 `infra::import::commit` 里做。
/// 这里判的是「同一份文件里同一个人出现两次」（spec 标准 7）。
pub fn validate_rows(
    rows: &[RawRow],
    mappings: &[ColumnMapping],
    targets: &BTreeMap<String, MappingTarget>,
    members: &BTreeMap<i64, MemberRef>,
) -> ValidatedRows {
    let mut out = ValidatedRows {
        rows: Vec::new(),
        problems: Vec::new(),
        summary: ImportSummary::new(),
    };

    // 先按列序整理映射：固定列各在第几列，字段各在第几列。
    let mut name_col: Option<usize> = None;
    let mut email_col: Option<usize> = None;
    let mut employee_no_col: Option<usize> = None;
    let mut field_cols: BTreeMap<String, usize> = BTreeMap::new();
    for m in mappings {
        match &m.target {
            ColumnTarget::Fixed { key } => match key.as_str() {
                FIXED_NAME => name_col = Some(m.index),
                FIXED_EMAIL => email_col = Some(m.index),
                FIXED_EMPLOYEE_NO => employee_no_col = Some(m.index),
                _ => {}
            },
            ColumnTarget::Field { key } => {
                field_cols.insert(key.clone(), m.index);
            }
            ColumnTarget::Skip => {}
        }
    }

    // 本批里已经出现过的成员 id（用于「同月不可重复」）。
    let mut seen_members: BTreeMap<i64, usize> = BTreeMap::new();
    // 本批里重复出现的成员 id → 出现过的行号。
    let mut dup_members: BTreeMap<i64, Vec<usize>> = BTreeMap::new();

    for raw in rows {
        let mut problems: Vec<RowProblem> = Vec::new();
        let mut values: BTreeMap<String, String> = BTreeMap::new();
        let mut identity_unverified = false;

        let cell = |col: Option<usize>| -> String {
            col.and_then(|c| raw.cells.get(&c))
                .map(|s| s.trim().to_string())
                .unwrap_or_default()
        };

        let name = cell(name_col);
        let email = cell(email_col);
        let employee_no = cell(employee_no_col);

        // ---- 姓名 / 邮箱必填 ----
        if name.is_empty() {
            problems.push(problem(raw.row, "姓名", Severity::Error, "姓名是空的"));
        }
        if email.is_empty() {
            problems.push(problem(raw.row, "邮箱", Severity::Error, "邮箱是空的"));
        } else if let Err(e) = check_email(&email) {
            problems.push(problem(
                raw.row,
                "邮箱",
                Severity::Error,
                format!("邮箱格式不对：{}", e.user_message()),
            ));
        }

        // ---- 成员存在且启用（按邮箱找）----
        let mut member: Option<&MemberRef> = None;
        if !email.is_empty() {
            let key = email.to_lowercase();
            member = members.values().find(|m| m.email.to_lowercase() == key);
            match member {
                None => problems.push(problem(
                    raw.row,
                    "邮箱",
                    Severity::Error,
                    "成员名单里没有这个邮箱，请先在「邮件组与成员」里添加",
                )),
                Some(m) if !m.active => problems.push(problem(
                    raw.row,
                    "邮箱",
                    Severity::Error,
                    "这个成员已停用，不在发送范围里",
                )),
                Some(_) => {}
            }
        }

        // ---- 身份一致：姓名与成员表对不上 → 警告 + 标记 ----
        if let (Some(m), true) = (member, !name.is_empty()) {
            if m.name.trim() != name.trim() {
                identity_unverified = true;
                problems.push(problem(
                    raw.row,
                    "姓名",
                    Severity::Warning,
                    "姓名与成员名单里的不一样，这一行导入后不会进入发送范围，请核对",
                ));
            }
            // 工号是辅助键：不一致只是提醒（ADR-0021 决策 5）。
            if let Some(excel_no) = employee_no_col.map(|_| employee_no.clone()) {
                let excel_no = excel_no.trim().to_string();
                if !excel_no.is_empty() {
                    let member_no = m.employee_no.as_deref().unwrap_or("").trim();
                    if !member_no.is_empty() && member_no != excel_no {
                        problems.push(problem(
                            raw.row,
                            "工号",
                            Severity::Warning,
                            "工号与成员名单里的不一样，请核对",
                        ));
                    }
                }
            }
        }

        // ---- 字段值 ----
        for (key, col) in &field_cols {
            let Some(target) = targets.get(key) else {
                continue;
            };
            let text = raw
                .cells
                .get(col)
                .map(|s| s.trim().to_string())
                .unwrap_or_default();

            if target.formula {
                // 公式列：填了占位文字 → 静默忽略；填了别的 → 错误（ADR-0021 洞四）。
                if !text.is_empty() && text != FORMULA_PLACEHOLDER {
                    problems.push(problem(
                        raw.row,
                        &target.label,
                        Severity::Error,
                        "这一列由系统计算，请清空这一格",
                    ));
                }
                continue;
            }

            match target.data_type {
                FieldType::Text => {
                    if !text.is_empty() {
                        values.insert(key.clone(), text);
                    }
                }
                _ => match parse_numeric(target.data_type, &text) {
                    Ok(Some(v)) => {
                        values.insert(key.clone(), format_number(v));
                    }
                    Ok(None) => {
                        // 空 = 没有这一项（ADR-0021 决策 6）
                    }
                    Err(msg) => {
                        problems.push(problem(raw.row, &target.label, Severity::Error, msg))
                    }
                },
            }
        }

        // ---- 同月不可重复 ----
        if let Some(m) = member {
            seen_members.entry(m.id).or_insert(raw.row);
            dup_members.entry(m.id).or_default().push(raw.row);
        }

        // 记录问题
        let mut has_error = false;
        for p in &problems {
            if p.severity == Severity::Error {
                has_error = true;
            }
            out.summary.add(p.severity);
        }
        if has_error {
            out.problems.extend(problems);
            continue;
        }
        out.problems.extend(problems);

        if let Some(m) = member {
            out.summary.add_success();
            out.rows.push(ParsedRow {
                row: raw.row,
                member_id: m.id,
                name: m.name.clone(),
                email: m.email.clone(),
                values,
                identity_unverified,
            });
        }
    }

    // 重复出现的成员：**两行都算错误**（ADR-0021 / spec 标准 7）。
    for (member_id, row_nos) in dup_members {
        if row_nos.len() < 2 {
            continue;
        }
        let first = row_nos[0];
        let others: Vec<String> = row_nos.iter().skip(1).map(|r| r.to_string()).collect();
        let email = out
            .rows
            .iter()
            .find(|r| r.member_id == member_id)
            .map(|r| r.email.clone())
            .unwrap_or_default();
        let msg = format!(
            "同一个成员在这个文件里出现了 {} 次（第 {} 行、第 {} 行）",
            row_nos.len(),
            first,
            others.join("、")
        );
        for row_no in &row_nos {
            let p = problem(*row_no, "邮箱", Severity::Error, msg.clone());
            out.summary.errors += 1;
            // 原本算成功的那一行要改判成错误：成功数减一、错误数加一。
            if let Some(pos) = out.rows.iter().position(|r| r.row == *row_no) {
                out.rows.remove(pos);
                out.summary.success -= 1;
            }
            out.problems.push(p);
        }
        let _ = email;
    }

    out
}

/// 造一个问题。
fn problem(row: usize, column: &str, severity: Severity, message: impl Into<String>) -> RowProblem {
    RowProblem {
        row,
        column: column.to_string(),
        severity,
        message: message.into(),
    }
}

/// 数值 → 存进 `values` 的字符串。
///
/// 金额保留两位小数（`18000` → `18000.00`），避免 `18000` 与 `18000.00`
/// 在库里长得不一样（那会让「仅补空」的判据失准）。
fn format_number(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{v:.2}")
    } else {
        let s = format!("{v:.2}");
        s
    }
}

/// 「仅补空」：把导入的值与已有记录合成。
///
/// 已有记录里**不存在**的字段才填；存在的保留旧值（ADR-0021 决策 8 / spec 标准 12）。
pub fn merge_fill_empty(
    existing: &BTreeMap<String, String>,
    incoming: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut merged = existing.clone();
    for (k, v) in incoming {
        merged.entry(k.clone()).or_insert_with(|| v.clone());
    }
    merged
}

/// 「覆盖」：直接用导入的值。
pub fn merge_overwrite(
    _existing: &BTreeMap<String, String>,
    incoming: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    incoming.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(key: &str, label: &str, ty: FieldType) -> MappingTarget {
        MappingTarget {
            key: key.into(),
            label: label.into(),
            fixed: false,
            data_type: ty,
            formula: ty == FieldType::Formula,
        }
    }

    fn fixed(key: &str, label: &str) -> MappingTarget {
        MappingTarget {
            key: key.into(),
            label: label.into(),
            fixed: true,
            data_type: FieldType::Text,
            formula: false,
        }
    }

    fn targets() -> Vec<MappingTarget> {
        vec![
            fixed(FIXED_NAME, "姓名"),
            fixed(FIXED_EMAIL, "邮箱"),
            fixed(FIXED_EMPLOYEE_NO, "工号"),
            field("base_salary", "基本工资", FieldType::Money),
            field("net_pay", "实发工资", FieldType::Formula),
        ]
    }

    /// 标准 1：精确命中（自己导出的表头）。
    #[test]
    fn exported_headers_match_exactly() {
        let headers = vec![
            "姓名 *".to_string(),
            "邮箱 *".to_string(),
            "工号".to_string(),
            "基本工资 *".to_string(),
            "实发工资".to_string(),
        ];
        let maps = map_headers(&headers, &targets());
        assert!(maps.iter().all(|m| m.match_kind == MatchKind::Exact));
        assert!(!maps.iter().any(|m| m.needs_confirmation));
        assert_eq!(maps[0].target, ColumnTarget::Fixed { key: "name".into() });
        assert_eq!(
            maps[3].target,
            ColumnTarget::Field {
                key: "base_salary".into()
            }
        );
    }

    /// 标准 1：模糊命中 → 需要确认。
    #[test]
    fn fuzzy_match_needs_confirmation() {
        let headers = vec!["员工邮箱".to_string()];
        let maps = map_headers(&headers, &targets());
        assert_eq!(maps[0].match_kind, MatchKind::Fuzzy);
        assert!(maps[0].needs_confirmation);
        assert_eq!(
            maps[0].target,
            ColumnTarget::Fixed {
                key: "email".into()
            }
        );
    }

    /// 标准 1：未匹配 → 跳过。
    #[test]
    fn unmatched_column_is_skipped() {
        let headers = vec!["季度补贴".to_string()];
        let maps = map_headers(&headers, &targets());
        assert_eq!(maps[0].match_kind, MatchKind::Unmatched);
        assert_eq!(maps[0].target, ColumnTarget::Skip);
    }

    /// 标准 3：三个固定列缺一个 → 报出来。
    #[test]
    fn missing_fixed_column_is_reported() {
        let headers = vec!["姓名".to_string(), "邮箱".to_string()];
        let maps = map_headers(&headers, &targets());
        let missing = missing_fixed_columns(&maps);
        assert_eq!(missing.len(), 1);
        assert!(missing[0].contains("工号"), "{missing:?}");
    }

    /// 标准 8：金额解析。
    #[test]
    fn amounts_parse_or_fail_clearly() {
        assert_eq!(parse_amount("").unwrap(), None);
        assert_eq!(parse_amount("   ").unwrap(), None);
        assert_eq!(parse_amount("18000").unwrap(), Some(18000.0));
        assert_eq!(parse_amount("18,000.00").unwrap(), Some(18000.0));
        assert_eq!(parse_amount("¥18000").unwrap(), Some(18000.0));
        assert_eq!(parse_amount("-2100").unwrap(), Some(-2100.0));
        assert_eq!(parse_amount("18000 元").unwrap(), Some(18000.0));
        assert!(parse_amount("一千八").is_err());
    }

    /// ch.13 必测边界用例「中文全角数字」：中文输入法打出来的金额
    /// 必须和半角一样被解析，而不是报「这一格应该填数字」。
    #[test]
    fn fullwidth_amounts_parse_like_halfwidth() {
        assert_eq!(parse_amount("１８５００").unwrap(), Some(18500.0));
        assert_eq!(parse_amount("１８，０００．５０").unwrap(), Some(18000.5));
        assert_eq!(parse_amount("－２１００").unwrap(), Some(-2100.0));
        assert_eq!(parse_amount("＋１８０００").unwrap(), Some(18000.0));
        assert_eq!(parse_amount("１８０００元").unwrap(), Some(18000.0));
        assert_eq!(parse_percent("１２．５％").unwrap(), Some(12.5));
        // 不属于金额的全角字符依旧报错，不悄悄转
        assert!(parse_amount("（１８０００）").is_err());
    }

    /// 标准 9：百分比 0–100。
    #[test]
    fn percent_must_be_within_range() {
        assert_eq!(parse_percent("12.50%").unwrap(), Some(12.5));
        assert!(parse_percent("120%").is_err());
        assert!(parse_percent("-1").is_err());
    }

    /// 标准 10：公式列填了占位文字 → 静默忽略；填了别的 → 错误。
    #[test]
    fn formula_column_placeholder_is_ignored_but_other_values_are_errors() {
        let fx = Fixture::new("formula");
        let headers = vec![
            "姓名".to_string(),
            "邮箱".to_string(),
            "实发工资".to_string(),
        ];
        let maps = map_headers(&headers, &targets());
        let tmap: BTreeMap<String, MappingTarget> =
            targets().into_iter().map(|t| (t.key.clone(), t)).collect();

        let rows = vec![
            RawRow {
                row: 2,
                cells: [
                    (0usize, "张三".to_string()),
                    (1, "zhangsan@example.com".to_string()),
                    (2, FORMULA_PLACEHOLDER.to_string()),
                ]
                .into_iter()
                .collect(),
            },
            RawRow {
                row: 3,
                cells: [
                    (0usize, "李四".to_string()),
                    (1, "lisi@example.com".to_string()),
                    (2, "999".to_string()),
                ]
                .into_iter()
                .collect(),
            },
        ];
        let v = validate_rows(&rows, &maps, &tmap, &fx.members);
        assert_eq!(v.summary.errors, 1, "{:?}", v.problems);
        assert_eq!(v.summary.success, 1);
        let msg = &v.problems[0].message;
        assert!(msg.contains("由系统计算"), "{msg}");
    }

    /// 标准 4/5：成员不存在 / 已停用 → 错误。
    #[test]
    fn unknown_or_inactive_member_is_an_error() {
        let fx = Fixture::new("member");
        let headers = vec!["姓名".to_string(), "邮箱".to_string()];
        let maps = map_headers(&headers, &targets());
        let tmap: BTreeMap<String, MappingTarget> =
            targets().into_iter().map(|t| (t.key.clone(), t)).collect();
        let rows = vec![
            RawRow {
                row: 2,
                cells: [
                    (0usize, "张三".to_string()),
                    (1, "nobody@example.com".to_string()),
                ]
                .into_iter()
                .collect(),
            },
            RawRow {
                row: 3,
                cells: [
                    (0usize, "李四".to_string()),
                    (1, "inactive@example.com".to_string()),
                ]
                .into_iter()
                .collect(),
            },
        ];
        let v = validate_rows(&rows, &maps, &tmap, &fx.members);
        assert_eq!(v.summary.errors, 2, "{:?}", v.problems);
        assert!(v.problems[0].message.contains("没有这个邮箱"));
        assert!(v.problems[1].message.contains("已停用"));
    }

    /// 标准 6：姓名与成员表不一致 → 警告 + 标记，且行仍然入库。
    #[test]
    fn name_mismatch_is_a_warning_not_an_error() {
        let fx = Fixture::new("name");
        let headers = vec!["姓名".to_string(), "邮箱".to_string()];
        let maps = map_headers(&headers, &targets());
        let tmap: BTreeMap<String, MappingTarget> =
            targets().into_iter().map(|t| (t.key.clone(), t)).collect();
        let rows = vec![RawRow {
            row: 2,
            cells: [
                (0usize, "张三丰".to_string()),
                (1, "zhangsan@example.com".to_string()),
            ]
            .into_iter()
            .collect(),
        }];
        let v = validate_rows(&rows, &maps, &tmap, &fx.members);
        assert_eq!(v.summary.errors, 0, "{:?}", v.problems);
        assert_eq!(v.summary.warnings, 1);
        assert_eq!(v.summary.success, 1);
        assert!(v.rows[0].identity_unverified);
        // 入库的是**成员表里的**姓名，不是 Excel 里写的
        assert_eq!(v.rows[0].name, "张三");
    }

    /// 标准 7：同一成员出现两次 → 两行都错。
    #[test]
    fn duplicate_member_makes_both_rows_errors() {
        let fx = Fixture::new("dup");
        let headers = vec!["姓名".to_string(), "邮箱".to_string()];
        let maps = map_headers(&headers, &targets());
        let tmap: BTreeMap<String, MappingTarget> =
            targets().into_iter().map(|t| (t.key.clone(), t)).collect();
        let rows = vec![
            RawRow {
                row: 2,
                cells: [
                    (0usize, "张三".to_string()),
                    (1, "zhangsan@example.com".to_string()),
                ]
                .into_iter()
                .collect(),
            },
            RawRow {
                row: 3,
                cells: [
                    (0usize, "张三".to_string()),
                    (1, "zhangsan@example.com".to_string()),
                ]
                .into_iter()
                .collect(),
            },
        ];
        let v = validate_rows(&rows, &maps, &tmap, &fx.members);
        assert_eq!(v.summary.errors, 2, "{:?}", v.problems);
        assert_eq!(v.summary.success, 0);
        assert!(v.rows.is_empty());
    }

    /// 标准 8（冲突）：「仅补空」以字段为粒度。
    #[test]
    fn fill_empty_keeps_existing_field_values() {
        let mut existing = BTreeMap::new();
        existing.insert("base_salary".to_string(), "17000.00".to_string());
        let mut incoming = BTreeMap::new();
        incoming.insert("base_salary".to_string(), "18000.00".to_string());
        incoming.insert("remark".to_string(), "备注".to_string());
        let merged = merge_fill_empty(&existing, &incoming);
        assert_eq!(merged.get("base_salary").unwrap(), "17000.00");
        assert_eq!(merged.get("remark").unwrap(), "备注");
    }

    /// 标准 8（冲突）：「覆盖」用导入的值。
    #[test]
    fn overwrite_uses_incoming_values() {
        let mut existing = BTreeMap::new();
        existing.insert("base_salary".to_string(), "17000.00".to_string());
        let mut incoming = BTreeMap::new();
        incoming.insert("base_salary".to_string(), "18000.00".to_string());
        let merged = merge_overwrite(&existing, &incoming);
        assert_eq!(merged.get("base_salary").unwrap(), "18000.00");
    }

    /// 标准 4：K>0 禁止入库。
    #[test]
    fn errors_block_the_commit() {
        let mut s = ImportSummary::new();
        s.add_success();
        assert!(s.can_commit());
        s.add(Severity::Warning);
        assert!(s.can_commit(), "警告不该拦住入库");
        s.add(Severity::Error);
        assert!(!s.can_commit());
    }

    // ---- 测试基座 ----

    struct Fixture {
        members: BTreeMap<i64, MemberRef>,
    }

    impl Fixture {
        fn new(label: &str) -> Self {
            let mut members = BTreeMap::new();
            members.insert(
                1,
                MemberRef {
                    id: 1,
                    name: "张三".into(),
                    email: "zhangsan@example.com".into(),
                    employee_no: Some("E001".into()),
                    active: true,
                },
            );
            members.insert(
                2,
                MemberRef {
                    id: 2,
                    name: "李四".into(),
                    email: "inactive@example.com".into(),
                    employee_no: Some("E002".into()),
                    active: false,
                },
            );
            members.insert(
                3,
                MemberRef {
                    id: 3,
                    name: "李四".into(),
                    email: "lisi@example.com".into(),
                    employee_no: Some("E003".into()),
                    active: true,
                },
            );
            let _ = label;
            Self { members }
        }
    }
}
