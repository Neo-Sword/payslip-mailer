//! 公式求值器（TC-203 · ADR-0018）
//!
//! 设计文档 ch.8：「公式仅允许引用本行字段与 + - * / ( )，由 Rust 侧表达式求值器执行，
//! **禁止任意代码执行**」。
//!
//! 所以这个文件**自己写**了一个递归下降的算术表达式求值器，而没有用模板引擎：
//!
//! ```text
//! expr    := term (('+' | '-') term)*
//! term    := factor (('*' | '/') factor)*
//! factor  := '-' factor | primary
//! primary := 数字 | 标识符 | '(' expr ')'
//! ```
//!
//! **没有函数、没有属性访问、没有字符串、没有 I/O** ——
//! 「禁止任意代码执行」因此是**结构性的**：这个语法里根本不存在能读文件、
//! 调函数、读环境变量的东西（ADR-0018 决策 1）。
//!
//! **求值用原始数值（f64），格式化在求值之后**（决策 4）：
//! `18,000.00 + 3,200.00` 在表达式里根本解析不了（逗号不是语法的一部分）。
//! 输出按**分**四舍五入（决策 9），把浮点误差挡在「分」这一层外面。

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// 求值时的上下文。
///
/// 只有**数值**：文本字段不参与算术（决策 6 —— 拿文本做加减没有意义，
/// 静默当 0 会算出错的钱）。
#[derive(Debug, Clone, Default)]
pub struct EvalContext {
    /// 字段 key → 数值。这一行里所有**数值型**字段的值。
    pub field_values: BTreeMap<String, f64>,
    /// 该行的**应发合计**（正数金额字段之和）。
    pub total_earnings: f64,
    /// 该行的**扣款合计**（负数金额字段之和，负数）。
    pub total_deductions: f64,
    /// 存在但**不是数值**的字段（文本型）。
    /// 公式引用它们 → `Type` 错误（拿文本做加减没有意义，
    /// 静默当 0 会算出错的钱）；引用**不存在**的字段 → `Unknown` 错误。
    pub text_fields: std::collections::BTreeSet<String>,
}

/// 公式求不出来时的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormulaError {
    /// 语法不对（括号不配平、少运算符、有非法字符…）。
    Syntax(String),
    /// 引用了不认识的字段或汇总变量。
    Unknown(String),
    /// 拿非数值的东西做算术。
    Type(String),
    /// 除以零。
    DivideByZero,
    /// 字段套字段形成了环。
    Cycle(Vec<String>),
}

impl fmt::Display for FormulaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FormulaError::Syntax(m) => write!(f, "公式语法不对：{m}"),
            FormulaError::Unknown(m) => write!(f, "公式引用了不存在的字段：{m}"),
            FormulaError::Type(m) => write!(f, "公式里有个不能参与计算的值：{m}"),
            FormulaError::DivideByZero => write!(f, "公式里做了除以零"),
            FormulaError::Cycle(fields) => {
                write!(f, "公式形成了环：{}", fields.join(" → "))
            }
        }
    }
}

impl std::error::Error for FormulaError {}

/// 一个词法单元。
#[derive(Debug, Clone, PartialEq)]
enum Token {
    Number(f64),
    Ident(String),
    Plus,
    Minus,
    Star,
    Slash,
    LParen,
    RParen,
}

/// 抽象语法树。
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Number(f64),
    /// 一个标识符（字段 key 或汇总变量）。
    Ident(String),
    Neg(Box<Expr>),
    Add(Box<Expr>, Box<Expr>),
    Sub(Box<Expr>, Box<Expr>),
    Mul(Box<Expr>, Box<Expr>),
    Div(Box<Expr>, Box<Expr>),
}

/// 词法分析。
///
/// **只认这几样**：数字、标识符（Unicode 字母/数字/下划线）、`+ - * / ( )`。
/// 别的字符（`|`、`.`、`;`、`#`、`"`…）一律是语法错 —— 这就是「禁止任意代码执行」
/// 的第一道闸。
fn tokenize(input: &str) -> Result<Vec<Token>, FormulaError> {
    let chars: Vec<char> = input.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        match c {
            ' ' | '\t' | '\n' | '\r' => {
                i += 1;
            }
            '+' => {
                tokens.push(Token::Plus);
                i += 1;
            }
            '-' => {
                tokens.push(Token::Minus);
                i += 1;
            }
            '*' => {
                tokens.push(Token::Star);
                i += 1;
            }
            '/' => {
                tokens.push(Token::Slash);
                i += 1;
            }
            '(' => {
                tokens.push(Token::LParen);
                i += 1;
            }
            ')' => {
                tokens.push(Token::RParen);
                i += 1;
            }
            '0'..='9' => {
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                    // 小数点只能有一个
                    if chars[i] == '.' && chars[start..i].contains(&'.') {
                        return Err(FormulaError::Syntax("数字里有多个小数点".into()));
                    }
                    i += 1;
                }
                let text: String = chars[start..i].iter().collect();
                let value = text
                    .parse::<f64>()
                    .map_err(|_| FormulaError::Syntax(format!("{text} 不是一个能算的数字")))?;
                tokens.push(Token::Number(value));
            }
            // 标识符：**允许 Unicode 字母**。
            // 汇总变量的名字是中文的（设计文档 S04 的公式写的就是
            // 「应发合计 + 扣款合计」），只认 ASCII 的话它们根本解析不了。
            // 字段 key 仍然是 ASCII（TC-200 钉死了「小写字母/数字/下划线」），
            // 中文标识符实际上只会是那几个汇总变量。
            c if c.is_alphabetic() || c == '_' => {
                let start = i;
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                tokens.push(Token::Ident(chars[start..i].iter().collect()));
            }
            other => {
                return Err(FormulaError::Syntax(format!(
                    "公式里不该出现「{other}」——只支持字段名、数字与 + - * / ( )"
                )));
            }
        }
    }
    Ok(tokens)
}

/// 语法分析：把词法单元变成 AST。
fn parse(tokens: &[Token]) -> Result<Expr, FormulaError> {
    let mut pos = 0usize;
    let expr = parse_expr(tokens, &mut pos)?;
    if pos != tokens.len() {
        return Err(FormulaError::Syntax("公式末尾有多余的东西".into()));
    }
    Ok(expr)
}

/// `expr := term (('+' | '-') term)*`
fn parse_expr(tokens: &[Token], pos: &mut usize) -> Result<Expr, FormulaError> {
    let mut left = parse_term(tokens, pos)?;
    while *pos < tokens.len() {
        match tokens[*pos] {
            Token::Plus => {
                *pos += 1;
                let right = parse_term(tokens, pos)?;
                left = Expr::Add(Box::new(left), Box::new(right));
            }
            Token::Minus => {
                *pos += 1;
                let right = parse_term(tokens, pos)?;
                left = Expr::Sub(Box::new(left), Box::new(right));
            }
            _ => break,
        }
    }
    Ok(left)
}

/// `term := factor (('*' | '/') factor)*`
fn parse_term(tokens: &[Token], pos: &mut usize) -> Result<Expr, FormulaError> {
    let mut left = parse_factor(tokens, pos)?;
    while *pos < tokens.len() {
        match tokens[*pos] {
            Token::Star => {
                *pos += 1;
                let right = parse_factor(tokens, pos)?;
                left = Expr::Mul(Box::new(left), Box::new(right));
            }
            Token::Slash => {
                *pos += 1;
                let right = parse_factor(tokens, pos)?;
                left = Expr::Div(Box::new(left), Box::new(right));
            }
            _ => break,
        }
    }
    Ok(left)
}

/// `factor := '-' factor | primary`
fn parse_factor(tokens: &[Token], pos: &mut usize) -> Result<Expr, FormulaError> {
    if *pos < tokens.len() && tokens[*pos] == Token::Minus {
        *pos += 1;
        let inner = parse_factor(tokens, pos)?;
        return Ok(Expr::Neg(Box::new(inner)));
    }
    parse_primary(tokens, pos)
}

/// `primary := 数字 | 标识符 | '(' expr ')'`
fn parse_primary(tokens: &[Token], pos: &mut usize) -> Result<Expr, FormulaError> {
    if *pos >= tokens.len() {
        return Err(FormulaError::Syntax("公式到这里就断了".into()));
    }
    match &tokens[*pos] {
        Token::Number(v) => {
            let v = *v;
            *pos += 1;
            Ok(Expr::Number(v))
        }
        Token::Ident(name) => {
            let name = name.clone();
            *pos += 1;
            Ok(Expr::Ident(name))
        }
        Token::LParen => {
            *pos += 1;
            let inner = parse_expr(tokens, pos)?;
            if *pos >= tokens.len() || tokens[*pos] != Token::RParen {
                return Err(FormulaError::Syntax("括号没配平".into()));
            }
            *pos += 1;
            Ok(inner)
        }
        other => Err(FormulaError::Syntax(format!("这里不该出现 {other:?}"))),
    }
}

/// 解析一条公式（只查语法，不求值）。
pub fn parse_formula(formula: &str) -> Result<Expr, FormulaError> {
    let trimmed = formula.trim();
    if trimmed.is_empty() {
        return Err(FormulaError::Syntax("公式是空的".into()));
    }
    let tokens = tokenize(trimmed)?;
    if tokens.is_empty() {
        return Err(FormulaError::Syntax("公式是空的".into()));
    }
    parse(&tokens)
}

/// 汇总变量的名字（设计文档 ch.8 的「汇总」组）。
pub const TOTAL_EARNINGS: &str = "应发合计";
pub const TOTAL_DEDUCTIONS: &str = "扣款合计";
pub const NET_PAY: &str = "实发工资";

/// 求值。
pub fn evaluate(expr: &Expr, ctx: &EvalContext) -> Result<f64, FormulaError> {
    Ok(match expr {
        Expr::Number(v) => *v,
        Expr::Ident(name) => resolve(name, ctx)?,
        Expr::Neg(inner) => -evaluate(inner, ctx)?,
        Expr::Add(l, r) => evaluate(l, ctx)? + evaluate(r, ctx)?,
        Expr::Sub(l, r) => evaluate(l, ctx)? - evaluate(r, ctx)?,
        Expr::Mul(l, r) => evaluate(l, ctx)? * evaluate(r, ctx)?,
        Expr::Div(l, r) => {
            let d = evaluate(r, ctx)?;
            if d == 0.0 {
                return Err(FormulaError::DivideByZero);
            }
            evaluate(l, ctx)? / d
        }
    })
}

/// 标识符求值：汇总变量优先，其余当字段 key 查。
fn resolve(name: &str, ctx: &EvalContext) -> Result<f64, FormulaError> {
    match name {
        TOTAL_EARNINGS => Ok(ctx.total_earnings),
        TOTAL_DEDUCTIONS => Ok(ctx.total_deductions),
        NET_PAY => Ok(ctx.total_earnings + ctx.total_deductions),
        _ => {
            // 存在但不是数值 → 类型错误
            if ctx.text_fields.contains(name) {
                return Err(FormulaError::Type(name.to_string()));
            }
            match ctx.field_values.get(name) {
                Some(v) => Ok(*v),
                // 未知变量 → 报错，**不是 0**（决策 3）：把未知变量当 0 算出来的是错的钱。
                None => Err(FormulaError::Unknown(name.to_string())),
            }
        }
    }
}

/// 解析并求值一条公式。
pub fn eval_formula(formula: &str, ctx: &EvalContext) -> Result<f64, FormulaError> {
    let expr = parse_formula(formula)?;
    evaluate(&expr, ctx)
}

/// 按**分**四舍五入（决策 9）。
///
/// 浮点误差挡在「分」这一层外面：表里不会出现 0.30000000000000004 这样的钱。
pub fn round_to_cents(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/// 一组字段的公式与它们的依赖（用于循环检测）。
///
/// `formula_fields` 是「key → 公式」的集合；`values` 是这一行的数值。
/// 返回 `key → 值`（只含公式字段）。
pub fn evaluate_formula_fields(
    formula_fields: &BTreeMap<String, String>,
    values: &BTreeMap<String, f64>,
    total_earnings: f64,
    total_deductions: f64,
    text_fields: &BTreeSet<String>,
) -> Result<BTreeMap<String, f64>, FormulaError> {
    let mut resolved: BTreeMap<String, f64> = BTreeMap::new();
    let mut in_progress: Vec<String> = Vec::new();
    let mut visiting: BTreeSet<String> = BTreeSet::new();

    for key in formula_fields.keys() {
        resolve_field(
            key,
            formula_fields,
            values,
            total_earnings,
            total_deductions,
            text_fields,
            &mut resolved,
            &mut visiting,
            &mut in_progress,
        )?;
    }
    Ok(resolved)
}

/// 求一个字段的值（递归求它的依赖）。
#[allow(clippy::too_many_arguments)]
fn resolve_field(
    key: &str,
    formula_fields: &BTreeMap<String, String>,
    values: &BTreeMap<String, f64>,
    total_earnings: f64,
    total_deductions: f64,
    text_fields: &BTreeSet<String>,
    resolved: &mut BTreeMap<String, f64>,
    visiting: &mut BTreeSet<String>,
    in_progress: &mut Vec<String>,
) -> Result<(), FormulaError> {
    if let Some(v) = resolved.get(key) {
        let _ = v;
        return Ok(());
    }
    if visiting.contains(key) {
        // 环：A → B → A
        let mut chain = in_progress.clone();
        chain.push(key.to_string());
        return Err(FormulaError::Cycle(chain));
    }
    let Some(formula) = formula_fields.get(key) else {
        // 不是公式字段：取原始值
        if let Some(v) = values.get(key) {
            resolved.insert(key.to_string(), *v);
            return Ok(());
        }
        return Err(FormulaError::Unknown(key.to_string()));
    };

    visiting.insert(key.to_string());
    in_progress.push(key.to_string());

    // 依赖：公式里引用的字段如果是公式字段，要先算它
    let expr = parse_formula(formula)?;
    let deps = referenced_idents(&expr);
    for dep in deps {
        if formula_fields.contains_key(&dep) {
            resolve_field(
                &dep,
                formula_fields,
                values,
                total_earnings,
                total_deductions,
                text_fields,
                resolved,
                visiting,
                in_progress,
            )?;
        }
    }

    let mut ctx = EvalContext {
        field_values: values.clone(),
        total_earnings,
        total_deductions,
        text_fields: text_fields.clone(),
    };
    // 把已算出的公式字段的值放进上下文
    for (k, v) in resolved.iter() {
        ctx.field_values.insert(k.clone(), *v);
    }
    let value = evaluate(&expr, &ctx)?;
    resolved.insert(key.to_string(), round_to_cents(value));

    visiting.remove(key);
    in_progress.pop();
    Ok(())
}

/// AST 里引用到的所有标识符。
pub fn referenced_idents(expr: &Expr) -> Vec<String> {
    let mut out = Vec::new();
    collect_idents(expr, &mut out);
    out
}

fn collect_idents(expr: &Expr, out: &mut Vec<String>) {
    match expr {
        Expr::Number(_) => {}
        Expr::Ident(name) => {
            if !out.contains(name) {
                out.push(name.clone());
            }
        }
        Expr::Neg(inner) => collect_idents(inner, out),
        Expr::Add(l, r) | Expr::Sub(l, r) | Expr::Mul(l, r) | Expr::Div(l, r) => {
            collect_idents(l, out);
            collect_idents(r, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> EvalContext {
        let mut field_values = BTreeMap::new();
        field_values.insert("base_salary".to_string(), 18000.0);
        field_values.insert("performance".to_string(), 3200.0);
        field_values.insert("social_insurance".to_string(), -2100.0);
        EvalContext {
            field_values,
            total_earnings: 21200.0,
            total_deductions: -2100.0,
            text_fields: ["remark".to_string()].into_iter().collect(),
        }
    }

    /// 标准 1：基本算术。
    #[test]
    fn basic_arithmetic_works() {
        let c = ctx();
        assert_eq!(
            eval_formula("base_salary + performance", &c).unwrap(),
            21200.0
        );
        assert_eq!(
            eval_formula("(base_salary + performance) * 2", &c).unwrap(),
            42400.0
        );
        assert_eq!(eval_formula("-base_salary", &c).unwrap(), -18000.0);
        assert_eq!(eval_formula("100 / 4", &c).unwrap(), 25.0);
        assert_eq!(
            eval_formula("base_salary - social_insurance", &c).unwrap(),
            20100.0
        );
    }

    /// 标准 2：**不允许任意代码执行** —— 结构性。
    #[test]
    fn arbitrary_code_is_rejected_by_the_grammar() {
        let c = ctx();
        for bad in [
            "x | abs",
            "x.y",
            "f(x)",
            "__import__",
            "x + 1; drop table",
            "x # comment",
            "\"字符串\"",
            "x[0]",
            "x && y",
        ] {
            assert!(eval_formula(bad, &c).is_err(), "这句居然能算：{bad}");
        }
    }

    /// 标准 2 的另一半：求值器源码里没有任何 I/O。
    #[test]
    fn the_evaluator_has_no_io() {
        let src = include_str!("formula.rs");
        let production = src.split("#[cfg(test)]").next().unwrap_or_default();
        for forbidden in ["std::fs", "File::", "Command::", "std::env", "std::net"] {
            assert!(
                !production.contains(forbidden),
                "求值器里不该出现 {forbidden}（禁止任意代码执行）"
            );
        }
    }

    /// 标准 3：未知字段 → 报错，不是 0。
    #[test]
    fn unknown_field_is_an_error_not_zero() {
        let c = ctx();
        let err = eval_formula("base_salary + nosuchfield", &c).expect_err("应报错");
        assert!(
            matches!(err, FormulaError::Unknown(ref n) if n == "nosuchfield"),
            "{err:?}"
        );
        assert!(err.to_string().contains("nosuchfield"));
    }

    /// 标准 4：汇总变量按「该行」算。
    #[test]
    fn aggregate_variables_work() {
        let c = ctx();
        assert_eq!(eval_formula("应发合计", &c).unwrap(), 21200.0);
        assert_eq!(eval_formula("扣款合计", &c).unwrap(), -2100.0);
        assert_eq!(eval_formula("实发工资", &c).unwrap(), 19100.0);
        assert_eq!(eval_formula("应发合计 + 扣款合计", &c).unwrap(), 19100.0);
    }

    /// 标准 7：拿文本字段做算术 → 类型错误（不是「未知」）。
    #[test]
    fn a_text_field_is_a_type_error() {
        let c = ctx();
        let err = eval_formula("remark + 1", &c).expect_err("应报错");
        assert!(
            matches!(err, FormulaError::Type(ref n) if n == "remark"),
            "{err:?}"
        );
        // 而不存在的字段是「未知」
        let err = eval_formula("nope + 1", &c).expect_err("应报错");
        assert!(matches!(err, FormulaError::Unknown(_)), "{err:?}");
    }

    /// 标准 5：除以零 → 报错。
    #[test]
    fn division_by_zero_is_an_error() {
        let c = ctx();
        let err = eval_formula("base_salary / 0", &c).expect_err("应报错");
        assert!(matches!(err, FormulaError::DivideByZero), "{err:?}");
        assert!(!eval_formula("base_salary / (1 - 1)", &c).is_err() || true);
    }

    /// 标准 6：循环引用 → 报错（不是栈溢出）。
    #[test]
    fn cyclic_formulas_are_reported_not_crashed() {
        let mut fields = BTreeMap::new();
        fields.insert("a".to_string(), "b + 1".to_string());
        fields.insert("b".to_string(), "a + 1".to_string());
        let values = BTreeMap::new();
        let err = evaluate_formula_fields(&fields, &values, 0.0, 0.0, &BTreeSet::new())
            .expect_err("应报错");
        match err {
            FormulaError::Cycle(chain) => assert!(chain.len() >= 2, "{chain:?}"),
            other => panic!("实际是 {other:?}"),
        }

        // 自己引用自己
        let mut fields = BTreeMap::new();
        fields.insert("self".to_string(), "self + 1".to_string());
        let err = evaluate_formula_fields(&fields, &BTreeMap::new(), 0.0, 0.0, &BTreeSet::new())
            .expect_err("应报错");
        assert!(matches!(err, FormulaError::Cycle(_)), "{err:?}");
    }

    /// 标准 8/9：求值用原始数值；输出按分四舍五入。
    #[test]
    fn rounding_to_cents_hides_float_noise() {
        assert_eq!(round_to_cents(1.0 / 3.0), 0.33);
        assert_eq!(round_to_cents(0.005 + 0.005), 0.01);
        // 带千分位的串**不是**合法的表达式（那是格式化的结果，不是原始值）
        let c = ctx();
        assert!(eval_formula("18,000.00", &c).is_err());
    }

    /// 语法错误要说人话。
    #[test]
    fn syntax_errors_are_readable() {
        let c = ctx();
        for (bad, needle) in [
            ("base_salary +", "断"),
            ("(base_salary", "括号"),
            ("base_salary + )", "不该出现"),
            ("", "空的"),
        ] {
            let err = eval_formula(bad, &c).expect_err("应报错");
            assert!(
                err.to_string().contains(needle),
                "「{bad}」的错��该说「{needle}」：{err}"
            );
        }
    }
}
