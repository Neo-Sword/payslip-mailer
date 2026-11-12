//! 模板的纯逻辑：变量白名单、变量解析、未知变量检查、纯文本兜底（TC-202 · ADR-0017）
//!
//! **变量白名单是这一卡的全部安全性所在**：模板里能用的变量只有两组 ——
//! 内置变量（固定的一组）与字段变量（`{{字段.<key>}}`，key 来自 S04 的配置）。
//! 别的名字一律是「未知变量」→ 报错 → 不能设为默认 → 绝不会发出去
//! （ch.10 防错机制③：「绝不发出「{{实发工资}}」字面量」）。
//!
//! 渲染本身由 minijinja 做（`infra::templates` 那一层），
//! 这一层只负责**「哪些变量合法」「模板里用了哪些变量」「纯文本怎么生成」**。

use std::collections::BTreeMap;

use serde::Serialize;

/// 一个可用的变量（界面上的变量面板就是它）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplateVariable {
    /// 模板里写什么（如 `姓名`、`字段.base_salary`）。
    pub name: String,
    /// 界面上显示什么（如「姓名」「基本工资」）。
    pub label: String,
    /// 这个变量为空时是否要拦下（ADR-0017 决策 5）。
    pub required: bool,
    /// 内置变量还是字段变量。
    pub kind: VariableKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum VariableKind {
    /// 内置变量（姓名、年月…）。
    Builtin,
    /// 字段变量（`字段.<key>`）。
    Field,
}

/// 内置变量的定义。
///
/// `required` 的口径（ADR-0017 洞三）：**是否必填**，不是「是否非空」。
/// 工号在成员表里就是可选的（ch.6：`employee_no 可选`），
/// 照字面「非空」拦的话，没填工号的成员会永远收不到信。
/// 内置变量的 `(变量名, 显示名, 是否必填)` 清单（只读视图）。
///
/// TC-401 的空值核对拿它判「required 内置变量渲染后是不是空的」——
/// 「哪些变量空了该拦」是**白名单自己的知识**，不该在核对层再抄一份。
pub fn builtin_vars() -> &'static [(&'static str, &'static str, bool)] {
    BUILTINS
}

const BUILTINS: &[(&str, &str, bool)] = &[
    ("姓名", "姓名", true),
    ("工号", "工号", false),
    // 下面三个是设计文档 ch.8「变量白名单」里列的，TC-202 落地时漏了，
    // 本卡补齐（见 ADR-0018 的「附带」一节与 TC-202 评审记录的补充）。
    ("邮箱", "邮箱", false),
    ("年份", "年份", true),
    ("月份", "月份", true),
    ("年月", "年月", true),
    ("公司名称", "公司名称", true),
    ("组名", "组名", false),
    ("回信邮箱", "回信邮箱", false),
    ("应发合计", "应发合计", true),
    ("扣款合计", "扣款合计", true),
    ("实发工资", "实发工资", true),
    ("工资条表格", "工资条表格", true),
];

/// 字段变量的前缀（设计文档的 `{{字段.基本工资}}`）。
pub const FIELD_PREFIX: &str = "字段.";

/// 渲染时用到的变量集合。
///
/// 它是**渲染上下文**：minijinja 拿它当全局变量。
/// `字段` 是一个嵌套 map（`字段.base_salary`），其余是平铺的全局。
#[derive(Debug, Clone, Default)]
pub struct RenderContext {
    /// 平铺的内置变量（`姓名` → 「张三」）。
    pub builtins: BTreeMap<String, String>,
    /// 字段变量（`base_salary` → 「18000.00」）。
    pub fields: BTreeMap<String, String>,
    /// 哪些字段是**必填**的（就是 S04 的 `required` 列）。
    /// 放在上下文里而不是全局 —— 必填与否是**这一次渲染的数据**，不是进程状态。
    pub required_fields: std::collections::BTreeSet<String>,
    /// `{{工资条表格}}` 的值 —— **已渲染好的 HTML**，不能被转义。
    pub salary_table_html: String,
}

/// 一条模板里的问题。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplateProblem {
    /// 出问题的变量名（如 `字段.nosuchfield`）。
    pub variable: String,
    /// 一句人话。
    pub message: String,
}

/// 模板里用到的变量名。
///
/// 扫 `{{ ... }}` 里的内容，去掉空白后就是变量名。
/// **不做表达式解析**（minijinja 那边会解析）；这一层只负责把「用了哪些名字」收集出来
/// 交给白名单比对。
pub fn used_variables(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            break;
        };
        let raw = after[..end].trim();
        // 去掉可能的过滤器（`x | upper`）——我们不用过滤器，但别被它带偏。
        let name = raw.split('|').next().unwrap_or("").trim().to_string();
        if !name.is_empty() && !out.contains(&name) {
            out.push(name);
        }
        rest = &after[end + 2..];
    }
    out
}

/// 可用的变量面板内容。
///
/// `fields` 的每一项是 `(key, 显示名, 是否必填, 是否已停用)`。
///
/// **只含未停用的字段**（新建模板时可选的那些）——
/// 停用的字段不进新模板，但**已存在的模板仍然能渲染它**
/// （见 [`check_variables`] 的 `known_for_rendering`）。
pub fn variable_panel(fields: &[(String, String, bool, bool)]) -> Vec<TemplateVariable> {
    let mut out: Vec<TemplateVariable> = BUILTINS
        .iter()
        .map(|(name, label, required)| TemplateVariable {
            name: (*name).to_string(),
            label: (*label).to_string(),
            required: *required,
            kind: VariableKind::Builtin,
        })
        .collect();
    for (key, label, required, visible) in fields {
        if !visible {
            // 停用的字段**不进新建模板的变量面板**（红线 8：停用只影响新数据）。
            // 但已存在的模板仍然能渲染它 —— 那是渲染期的另一份白名单。
            continue;
        }
        out.push(TemplateVariable {
            name: format!("{FIELD_PREFIX}{key}"),
            label: label.clone(),
            required: *required,
            kind: VariableKind::Field,
        });
    }
    out
}

/// 检查模板里用到的变量。
///
/// `allowed_for_new` = 新建模板时能用的（未停用的字段 + 全部内置变量）；
/// `known_for_rendering` = 渲染历史模板时要认的（**含已停用的**字段 + 全部内置变量）。
///
/// 返回**两个**结果：
/// - `unknown`（不在 `allowed_for_new` 里）：模板级问题 → 不能设为默认；
/// - `empty_required`（在必填变量里但值为空）：发送级问题 → 拦下这一个收件人。
///
/// 一个已停用的字段落在 `known_for_rendering` 里但不在 `allowed_for_new` 里 ⇒
/// **不是错误**，只是「这个变量不能用在新建的模板里」（红线 8 的另一面）。
pub fn check_variables(
    text: &str,
    ctx: &RenderContext,
    allowed_for_new: &[String],
    known_for_rendering: &[String],
) -> (Vec<TemplateProblem>, Vec<TemplateProblem>) {
    let mut unknown = Vec::new();
    let mut empty_required = Vec::new();

    for name in used_variables(text) {
        let allowed = allowed_for_new.iter().any(|a| a == &name);
        let known = known_for_rendering.iter().any(|k| k == &name);
        if !allowed {
            if known {
                // 已停用的字段：老模板能用，但新模板不该用 → 提示但不算「未知」
                continue;
            }
            unknown.push(TemplateProblem {
                variable: name.clone(),
                message: format!("未解析的变量：{name}"),
            });
            continue;
        }
        // 必填变量为空 → 发送前要拦
        if let Some(problem) = empty_if_required(&name, ctx) {
            empty_required.push(problem);
        }
    }
    (unknown, empty_required)
}

/// 某个变量在上下文里的值（空字符串表示空）。
fn value_of(name: &str, ctx: &RenderContext) -> Option<String> {
    if let Some(key) = name.strip_prefix(FIELD_PREFIX) {
        return ctx.fields.get(key).cloned();
    }
    if name == "工资条表格" {
        return Some(ctx.salary_table_html.clone());
    }
    ctx.builtins.get(name).cloned()
}

/// 变量是否必填（内置表 + 字段的 required）。
fn is_required(name: &str, ctx: &RenderContext) -> bool {
    if let Some(key) = name.strip_prefix(FIELD_PREFIX) {
        // 字段的必填与否就是 S04 的 `required`（同一份数据，不另设一套）
        return ctx.required_fields.contains(key);
    }
    BUILTINS
        .iter()
        .find(|(n, _, _)| *n == name)
        .map(|(_, _, r)| *r)
        .unwrap_or(false)
}

/// 必填变量为空 → 一条问题。
fn empty_if_required(name: &str, ctx: &RenderContext) -> Option<TemplateProblem> {
    if !is_required(name, ctx) {
        return None;
    }
    let value = value_of(name, ctx).unwrap_or_default();
    if value.trim().is_empty() {
        Some(TemplateProblem {
            variable: name.to_string(),
            message: format!("必填变量「{name}」没有值"),
        })
    } else {
        None
    }
}

/// HTML → 纯文本兜底版本。
///
/// 设计文档 ch.10：「同时自动生成 text/plain 兜底版本（**表格转纯文本对齐**）」。
///
/// 做法：把标签换成换行或空、表格的每一行变成一行、列之间用空格对齐。
/// 不追求完美 —— 它的作用是「部分客户端把纯 HTML 邮件判为垃圾邮件」时的兜底。
pub fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut tag = String::new();
    let mut in_table = false;
    let mut cell = String::new();
    let mut row = Vec::<String>::new();

    for ch in html.chars() {
        if in_tag {
            tag.push(ch);
            if ch == '>' {
                in_tag = false;
                let name = tag
                    .trim_start_matches('/')
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .to_lowercase();
                match name.as_str() {
                    "br" | "p" | "div" | "tr" | "hr" | "h1" | "h2" | "h3" => out.push('\n'),
                    "td" | "th" => {
                        if in_table {
                            row.push(cell.trim().to_string());
                            cell.clear();
                        }
                    }
                    "table" => {
                        in_table = true;
                    }
                    "/table" => {
                        in_table = false;
                        if !row.is_empty() {
                            out.push_str(&pad_row(&row));
                            row.clear();
                        }
                    }
                    _ => {}
                }
                tag.clear();
            }
            continue;
        }
        match ch {
            '<' => {
                in_tag = true;
                tag.clear();
            }
            '>' => {
                in_tag = true;
                tag.push('>');
            }
            _ => {
                if in_table {
                    cell.push(ch);
                } else {
                    out.push(ch);
                }
            }
        }
    }
    // 收尾：最后一行
    if in_table && !cell.trim().is_empty() {
        row.push(cell.trim().to_string());
    }
    if !row.is_empty() {
        out.push_str(&pad_row(&row));
    }
    // 压掉多余空行
    let mut lines: Vec<&str> = out.lines().map(str::trim_end).collect();
    lines.dedup();
    lines.join("\n").trim().to_string()
}

/// 一行表格转成对齐的纯文本。
fn pad_row(cells: &[String]) -> String {
    let width = cells.iter().map(|c| c.chars().count()).max().unwrap_or(0);
    let mut parts: Vec<String> = Vec::new();
    for c in cells {
        let pad = width.saturating_sub(c.chars().count());
        parts.push(format!("{c}{}", " ".repeat(pad)));
    }
    format!("{}\n", parts.join("  ").trim_end())
}

/// 工资条表格的 HTML（`{{工资条表格}}` 的值）。
///
/// 设计文档 ch.10：「自动渲染为带样式的 HTML 表格，含全部启用字段」，
/// 「HTML 内联 CSS，中文字体栈 PingFang SC, Microsoft YaHei, sans-serif」。
///
/// **内联 CSS 是必须的**：邮件客户端普遍屏蔽外部样式表，`<style>` 块也常被剥掉。
pub fn salary_table_html(rows: &[(String, String)]) -> String {
    let mut out = String::with_capacity(512 + rows.len() * 120);
    out.push_str(
        "<table cellpadding=\"6\" cellspacing=\"0\" border=\"0\" style=\"border-collapse:collapse;font-family:'PingFang SC','Microsoft YaHei',sans-serif;font-size:13px;color:#222;\">",
    );
    out.push_str(
        "<tr style=\"background:#f5f6f8;\"><th align=\"left\" style=\"text-align:left;border-bottom:1px solid #d9dce1;\">项目</th><th align=\"right\" style=\"text-align:right;border-bottom:1px solid #d9dce1;\">金额</th></tr>",
    );
    for (label, value) in rows {
        out.push_str("<tr>");
        out.push_str(&format!(
            "<td style=\"padding:4px 0;border-bottom:1px solid #eef0f3;\">{}</td>",
            escape_html(label)
        ));
        out.push_str(&format!(
            "<td align=\"right\" style=\"text-align:right;padding:4px 0;border-bottom:1px solid #eef0f3;font-variant-numeric:tabular-nums;\">{}</td>",
            escape_html(value)
        ));
        out.push_str("</tr>");
    }
    out.push_str("</table>");
    out
}

/// 金额格式化：千分位 + 两位小数。
///
/// 设计文档 S10 的预览里写的是「本月实发：**¥16,840.00**」——
/// 也就是说**发出去的钱是带千分位的**。格式化放在**取值的那一层**
/// （渲染前把值变成「16,840.00」），模板里不做格式化：
/// 模板不该有「这个变量是钱所以要加千分位」这种知识。
pub fn format_amount(value: &str) -> String {
    // 先按小数点拆开
    let (int_part, frac_part) = match value.split_once('.') {
        Some((i, f)) => (i.to_string(), Some(f.to_string())),
        None => (value.to_string(), None),
    };
    // 只处理纯数字（带负号）。不是数字就原样返回（宁可少个千分位，也不要改坏数据）。
    let (sign, digits) = match int_part.strip_prefix('-') {
        Some(rest) => ("-", rest.to_string()),
        None => ("", int_part),
    };
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return value.to_string();
    }
    // 每三位加一个逗号
    let mut grouped = String::new();
    let bytes: Vec<char> = digits.chars().collect();
    for (i, c) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(*c);
    }
    let frac = match frac_part {
        Some(f) if !f.is_empty() => format!(".{f}"),
        _ => String::new(),
    };
    format!("{sign}{grouped}{frac}")
}

/// HTML 转义（表格里的标签与数值要转义，否则一个叫 `<script>` 的字段名就带坏了邮件）。
pub fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// 默认模板（设计文档 S10 的「标准中文模板」，正文取 ch.10 的原文）。
///
/// 主题与正文里的公司信息是**变量**（`{{公司名称}}`、`{{回信邮箱}}`），
/// 等业务方给了值就自动渲染出来 —— 换公司不用改模板。
pub const DEFAULT_TEMPLATE_NAME: &str = "标准中文模板";
pub const DEFAULT_TEMPLATE_SUBJECT: &str = "{{年月}} 工资条";
pub const DEFAULT_TEMPLATE_BODY: &str = "<p>{{姓名}} 您好：</p>\n<p>以下是您 <b>{{年月}}</b> 的工资条，请查收并妥善保管。</p>\n{{工资条表格}}\n<!-- 自动渲染为带样式的 HTML 表格，含全部启用字段 -->\n<p>本月实发：<b>{{实发工资}}</b></p>\n<hr>\n<p style=\"color:#888;font-size:12px\">本邮件由系统自动发送，请勿回复。如有疑问请联系人力资源部（{{回信邮箱}}）。</p>";

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> RenderContext {
        let mut builtins = BTreeMap::new();
        builtins.insert("姓名".to_string(), "张三".to_string());
        builtins.insert("年月".to_string(), "2026 年 10 月".to_string());
        builtins.insert("工号".to_string(), "".to_string()); // 工号为空：可选，不该拦
        builtins.insert("实发工资".to_string(), "16840.00".to_string());
        let mut fields = BTreeMap::new();
        fields.insert("base_salary".to_string(), "18000.00".to_string());
        RenderContext {
            builtins,
            fields,
            required_fields: ["base_salary".to_string()].into_iter().collect(),
            salary_table_html: "<table>ok</table>".to_string(),
        }
    }

    /// 变量扫描：取出模板里用到的变量名。
    #[test]
    fn used_variables_are_extracted() {
        assert_eq!(
            used_variables("<p>{{姓名}} {{字段.base_salary}}</p>"),
            vec!["姓名".to_string(), "字段.base_salary".to_string()]
        );
        assert!(used_variables("没有变量").is_empty());
        assert_eq!(used_variables("{{ 姓名 }}"), vec!["姓名".to_string()]);
    }

    /// 未知变量 → 一条问题。
    #[test]
    fn unknown_variables_are_reported() {
        let allowed = vec!["姓名".to_string(), "字段.base_salary".to_string()];
        let known = allowed.clone();
        let (unknown, empty) =
            check_variables("{{姓名}} {{字段.nosuchfield}}", &ctx(), &allowed, &known);
        assert_eq!(unknown.len(), 1, "{unknown:?}");
        assert!(unknown[0].message.contains("nosuchfield"));
        assert!(empty.is_empty(), "{empty:?}");
    }

    /// 标准 9：空值按「是否必填」判定 —— 工号空不拦、必填字段空要拦。
    #[test]
    fn empty_is_judged_by_requiredness() {
        let allowed = vec!["工号".to_string(), "实发工资".to_string()];
        let known = allowed.clone();
        let mut c = ctx();
        c.builtins.insert("实发工资".to_string(), "".to_string());
        let (unknown, empty) = check_variables("{{工号}} {{实发工资}}", &c, &allowed, &known);
        assert!(unknown.is_empty());
        assert_eq!(empty.len(), 1, "只有必填的那个该被拦：{empty:?}");
        assert!(empty[0].message.contains("实发工资"));
    }

    /// 标准 4：已停用的字段在历史模板里仍能渲染（不算「未知」）。
    #[test]
    fn a_deactivated_field_is_not_an_unknown_variable() {
        // allowed_for_new 里没有它（新模板不该用），known_for_rendering 里有它（老模板要认）
        let allowed = vec!["姓名".to_string()];
        let known = vec!["姓名".to_string(), "字段.old".to_string()];
        let (unknown, _) = check_variables("{{姓名}} {{字段.old}}", &ctx(), &allowed, &known);
        assert!(
            unknown.is_empty(),
            "已停用的字段不该报「未知」：{unknown:?}"
        );
    }

    /// 纯文本兜底：表格转对齐的纯文本。
    #[test]
    fn html_becomes_aligned_plain_text() {
        let html = "<table><tr><th>项目</th><th>金额</th></tr><tr><td>基本工资</td><td>18000.00</td></tr></table>";
        let text = html_to_text(html);
        assert!(!text.contains('<'), "纯文本里不该有标签：{text}");
        assert!(text.contains("基本工资"), "{text}");
        assert!(text.contains("18000.00"), "{text}");
        assert!(text.contains("项目"), "{text}");
    }

    /// 表格 HTML：内联 CSS + 中文字体栈 + 转义。
    #[test]
    fn salary_table_has_inline_css_and_escapes() {
        let html = salary_table_html(&[
            ("基本工资".to_string(), "18000.00".to_string()),
            ("<危险>".to_string(), "1 & 2".to_string()),
        ]);
        assert!(html.contains("PingFang SC"), "缺中文字体栈：{html}");
        assert!(html.contains("Microsoft YaHei"));
        assert!(html.contains("border-collapse"), "缺内联 CSS");
        assert!(html.contains("&lt;危险&gt;"), "标签没转义：{html}");
        assert!(html.contains("1 &amp; 2"), "值没转义：{html}");
    }

    /// 变量面板：内置 + 未停用的字段；停用的字段不在面板里。
    #[test]
    fn the_panel_lists_builtins_and_visible_fields_only() {
        let fields = vec![
            (
                "base_salary".to_string(),
                "基本工资".to_string(),
                true,
                true,
            ),
            ("old_field".to_string(), "旧项".to_string(), false, false), // 已停用
        ];
        let panel = variable_panel(&fields);
        assert!(panel.iter().any(|v| v.name == "姓名"));
        assert!(panel.iter().any(|v| v.name == "字段.base_salary"));
        assert!(
            !panel.iter().any(|v| v.name == "字段.old_field"),
            "停用的字段不该进面板"
        );
    }

    /// 默认模板：正文取设计文档原文。
    #[test]
    fn the_default_template_matches_the_design_doc() {
        assert_eq!(DEFAULT_TEMPLATE_NAME, "标准中文模板");
        assert!(DEFAULT_TEMPLATE_BODY.contains("{{姓名}}"));
        assert!(DEFAULT_TEMPLATE_BODY.contains("{{年月}}"));
        assert!(DEFAULT_TEMPLATE_BODY.contains("{{工资条表格}}"));
        assert!(DEFAULT_TEMPLATE_BODY.contains("{{实发工资}}"));
        assert!(DEFAULT_TEMPLATE_BODY.contains("{{回信邮箱}}"));
        // 用到的变量都在白名单里
        let used = used_variables(DEFAULT_TEMPLATE_BODY);
        let allowed: Vec<String> = BUILTINS.iter().map(|(n, _, _)| (*n).to_string()).collect();
        for name in used {
            assert!(
                allowed.contains(&name),
                "默认模板用了不在白名单里的变量：{name}"
            );
        }
    }
}
