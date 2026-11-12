//! 工资条项目的纯逻辑（TC-200 · ADR-0015）
//!
//! 这一层不碰库、不碰文件：只回答「这个 key 合法吗」「这个公式能不能存」。
//! 规则放在这里是为了**只有一处**：S04 的表单与 Command 层的校验共用同一批函数，
//! 不会出现「界面放过了、接口也放过了」而库里躺着一个脏字段。
//!
//! 两个边界要说清：
//!
//! - **key 的格式**（[`check_key`]）：key 会进 Excel 表头、也会进模板变量 `{{key}}`，
//!   必须跨工具安全 —— 只允许小写字母、数字、下划线。
//! - **公式的校验是轻量的**（[`check_formula`]）：只挡「明显坏掉的」——
//!   引用的 key 不存在、括号不配平、出现非法字符。
//!   **完整的语法解析与求值是 TC-203 的事**，那才知道公式语言的确切形态。

use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};

/// 字段类型（设计文档 ch.6 的枚举：`text/number/money/percent/formula`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FieldType {
    /// 文本（如「备注」）。
    Text,
    /// 数字。
    Number,
    /// 金额。
    Money,
    /// 百分比。
    Percent,
    /// 由公式算出来的值（如「实发工资」）。
    Formula,
}

impl FieldType {
    /// 从库里的字符串解析。
    pub fn parse(text: &str) -> AppResult<Self> {
        match text {
            "text" => Ok(Self::Text),
            "number" => Ok(Self::Number),
            "money" => Ok(Self::Money),
            "percent" => Ok(Self::Percent),
            "formula" => Ok(Self::Formula),
            other => Err(AppError::ValidationError(format!(
                "字段类型不对：{other}（只能是 text / number / money / percent / formula）"
            ))),
        }
    }

    /// 存进库里的字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Number => "number",
            Self::Money => "money",
            Self::Percent => "percent",
            Self::Formula => "formula",
        }
    }

    /// 界面上显示的名字。
    pub fn label(self) -> &'static str {
        match self {
            Self::Text => "文本",
            Self::Number => "数字",
            Self::Money => "金额",
            Self::Percent => "百分比",
            Self::Formula => "公式",
        }
    }

    /// 这一类型允许有公式吗？
    ///
    /// **只有 `formula` 类型有**。设计文档的 `data_type` 里 `formula` 是一种**类型**，
    /// 不是附加开关 —— 类型与公式不一致会让下游（TC-203 求值、TC-301 导表头）
    /// 无从判断这个字段是「手填的」还是「算出来的」。
    pub fn allows_formula(self) -> bool {
        matches!(self, Self::Formula)
    }

    /// 这一类型的值在工资条表格里靠右对齐吗？（金额与百分比靠右，文本靠左）
    pub fn default_align(self) -> &'static str {
        match self {
            Self::Number | Self::Money | Self::Percent | Self::Formula => "right",
            Self::Text => "left",
        }
    }
}

/// 变量键的格式规则：小写字母、数字、下划线；不能中文、不能空格、不能以数字开头。
///
/// 为什么这么严：key 是 Excel 的列名，也是模板变量 `{{key}}` 里的名字。
/// 带中文或空格的 key 在 Excel 里就是灾难（列名引用要转义），
/// 在模板里也会和渲染语法打架。
pub fn check_key(key: &str) -> AppResult<()> {
    if key.is_empty() {
        return Err(AppError::ValidationError("变量键不能为空".into()));
    }
    if key.len() > 40 {
        return Err(AppError::ValidationError(
            "变量键最长 40 个字符（它要当 Excel 列名用）".into(),
        ));
    }
    let mut chars = key.chars();
    let first = chars.next().expect("长度已判过");
    if !(first.is_ascii_lowercase() || first == '_') {
        return Err(AppError::ValidationError(format!(
            "变量键要以小写字母或下划线开头：{key}"
        )));
    }
    for ch in key.chars() {
        let ok = ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_';
        if !ok {
            return Err(AppError::ValidationError(format!(
                "变量键只能用小写字母、数字和下划线：{key}"
            )));
        }
    }
    Ok(())
}

/// 公式的轻量校验。
///
/// 三件事：
/// 1. 引用的 key 必须都存在（拼错 key 是最常见的错，而且是**静默**的错）；
/// 2. 括号必须配平；
/// 3. 只能出现「标识符 / 数字 / + − × ÷ ( ) / 空格」这些字符。
///
/// **不算语法解析**：表达式是否合法、优先级对不对，是 TC-203 的事。
/// 这里的价值是「明显坏掉的公式不该被存下来」。
///
/// `known_keys` 是当前所有字段的变量键集合（**含已停用的** ——
/// 公式里引用一个已停用的字段是合法的：那个字段的历史值还在，
/// 老模板要能渲染它）。
pub fn check_formula(formula: &str, known_keys: &[String]) -> AppResult<()> {
    // **语法判定交给解析器**（ADR-0018 决策 7）：本文件原先那套手写的
    // 「括号配平 + 字符集 + 引用的 key 存在」检查就是解析器的雏形，
    // 两份规则并存早晚漂移 —— 到那时会出现「界面放过了、引擎报错」。
    let expr = crate::domain::formula::parse_formula(formula)
        .map_err(|e| AppError::ValidationError(e.to_string()))?;

    // 引用了不存在的字段 → 拒（拼错 key 是最常见的错，而且是静默的错）
    for name in referenced_idents(&expr) {
        let known = known_keys.iter().any(|k| k == &name)
            || matches!(
                name.as_str(),
                crate::domain::formula::TOTAL_EARNINGS
                    | crate::domain::formula::TOTAL_DEDUCTIONS
                    | crate::domain::formula::NET_PAY
            );
        if !known {
            return Err(AppError::ValidationError(format!(
                "公式引用了不存在的字段「{name}」"
            )));
        }
    }
    Ok(())
}

/// AST 里引用到的所有标识符（从解析器借来，避免两份遍历逻辑）。
fn referenced_idents(expr: &crate::domain::formula::Expr) -> Vec<String> {
    crate::domain::formula::referenced_idents(expr)
}

/// 校验一次「新增 / 编辑字段」的完整输入。
///
/// `existing_keys` 是**除自己以外**的所有 key（唯一性在库里还有一道 UNIQUE 兜底，
/// 但这里先给出人话）。
pub fn check_field_input(
    key: &str,
    label: &str,
    data_type: FieldType,
    formula: Option<&str>,
    known_keys: &[String],
) -> AppResult<()> {
    check_key(key)?;
    if label.trim().is_empty() {
        return Err(AppError::ValidationError("显示名称不能为空".into()));
    }
    if label.chars().count() > 30 {
        return Err(AppError::ValidationError("显示名称最长 30 个字符".into()));
    }
    match (data_type.allows_formula(), formula) {
        (true, None) => Err(AppError::ValidationError("公式类型的字段必须填公式".into())),
        (true, Some(f)) => check_formula(f, known_keys),
        (false, Some(_)) => Err(AppError::ValidationError(format!(
            "{} 类型的字段不能有公式（只有「公式」类型才有）",
            data_type.label()
        ))),
        (false, None) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    /// key 的格式：小写字母 / 数字 / 下划线。
    #[test]
    fn key_format_is_enforced() {
        check_key("base_salary").expect("应通过");
        check_key("net_pay_2").expect("应通过");
        check_key("_private").expect("下划线开头应通过");

        assert!(check_key("BaseSalary").is_err(), "大写不该通过");
        assert!(check_key("基本工资").is_err(), "中文不该通过");
        assert!(check_key("base salary").is_err(), "空格不该通过");
        assert!(check_key("2nd").is_err(), "数字开头不该通过");
        assert!(check_key("").is_err(), "空不该通过");
    }

    /// 公式：引用的 key 必须存在。
    #[test]
    fn formula_rejects_unknown_keys() {
        let known = keys(&["base_salary", "performance"]);
        check_formula("base_salary + performance", &known).expect("应通过");
        check_formula("(base_salary + performance) / 2", &known).expect("应通过");
        check_formula("base_salary * 1.5", &known).expect("数字字面量应通过");

        let err = check_formula("base_salary + nosuch", &known).expect_err("应被拒");
        assert!(
            err.user_message().contains("nosuch"),
            "错误要说清是哪个 key 不存在：{}",
            err.user_message()
        );
    }

    /// 公式：括号配平与字符集。
    #[test]
    fn formula_rejects_unbalanced_parens_and_bad_chars() {
        let known = keys(&["base_salary"]);
        assert!(
            check_formula("base_salary + (", &known).is_err(),
            "括号不配平"
        );
        assert!(
            check_formula("base_salary + )", &known).is_err(),
            "多右括号"
        );
        assert!(
            check_formula("base_salary & foo", &known).is_err(),
            "非法字符"
        );
        assert!(check_formula("", &known).is_err(), "空公式");
    }

    /// 已停用的字段仍可以被公式引用（历史数据要能渲染）。
    #[test]
    fn a_deactivated_field_can_still_be_referenced() {
        // known_keys 里含已停用的字段 —— 调用方传的是「全部 key」
        let known = keys(&["base_salary", "old_field"]);
        check_formula("base_salary + old_field", &known).expect("应通过");
    }

    /// 类型与公式必须一致。
    #[test]
    fn only_formula_type_may_have_a_formula() {
        let known = keys(&["base_salary"]);
        assert!(
            check_field_input("net", "实发", FieldType::Formula, None, &known).is_err(),
            "公式类型必须填公式"
        );
        assert!(
            check_field_input("base", "基本工资", FieldType::Money, Some("1 + 2"), &known).is_err(),
            "金额类型不该有公式"
        );
        assert!(
            check_field_input(
                "net",
                "实发",
                FieldType::Formula,
                Some("base_salary"),
                &known
            )
            .is_ok(),
            "公式类型 + 合法公式应通过"
        );
    }

    /// 类型的枚举与设计文档一致。
    #[test]
    fn field_types_match_the_design_doc() {
        for (text, ty) in [
            ("text", FieldType::Text),
            ("number", FieldType::Number),
            ("money", FieldType::Money),
            ("percent", FieldType::Percent),
            ("formula", FieldType::Formula),
        ] {
            assert_eq!(FieldType::parse(text).unwrap(), ty);
            assert_eq!(ty.as_str(), text);
        }
        assert!(FieldType::parse("date").is_err(), "未知类型要报错");
    }
}
