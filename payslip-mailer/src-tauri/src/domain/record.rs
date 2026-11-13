//! 工资记录这一行的口径（TC-302 / TC-303 / TC-401 共用）
//!
//! 三件事放在这里，因为它们是**一套规则的三处用法**，各有各的实现就会漂：
//!
//! 1. **行求值**：给一行原始值 → 算出公式列的值（TC-203 的求值器）。
//!    汇总变量（应发合计 / 扣款合计）的口径也在�� ——
//!    导入时（TC-302）、逐格编辑时（TC-303）、发送前渲染时（TC-401）算出来的
//!    必须**是同一个数**，否则「预览看到的」与「发出去的」会不一样。
//! 2. **值的规范化存法**：金额按分四舍五入后存成字符串，空 = 键不存在
//!    （ADR-0021 决策 6：「空 = 没有这一项」不是 0）。
//! 3. **校验和**：这一行内容的摘要（十六进制），用于「内容一致」防错核对
//!    （设计文档 ch.5 防错机制 ②）。

use std::collections::{BTreeMap, BTreeSet};

use sha2::{Digest, Sha256};

use crate::domain::field::FieldType;
use crate::domain::formula;

/// 一行里一个字段的定义（求值时只需要这几样）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldSpec {
    pub key: String,
    pub label: String,
    pub data_type: FieldType,
    /// 公式源码（只有 `data_type = formula` 的字段有）。
    pub formula: Option<String>,
    /// 是否必填（求值用不着它；S07 的校验列需要 —— TC-303）。
    pub required: bool,
}

impl FieldSpec {
    /// 是公式列吗？
    pub fn is_formula(&self) -> bool {
        matches!(self.data_type, FieldType::Formula)
    }
}

/// 行求值的结果。
#[derive(Debug, Clone, PartialEq)]
pub struct RowEvaluation {
    /// 公式字段的 key → 值（已按分四舍五入，字符串形式）。
    pub computed: BTreeMap<String, String>,
    /// 应发合计（正数金额之和）。
    pub total_earnings: f64,
    /// 扣款合计（负数金额之和）。
    pub total_deductions: f64,
}

/// 给一行算公式列。
///
/// `values` 是这一行的**原始**数值（字段 key → 数值字符串）。
/// 文本字段不参与算术（TC-203 决策 6：拿文本做加减没有意义，
/// 静默当 0 会算出错的钱）。
///
/// 汇总口径：**正数金额之和 = 应发合计、负数金额之和 = 扣款合计**，
/// **公式字段不参与汇总**（它们自己就是算出来的）。
///
/// **「这一行没有这一项」在公式里算 0**（不是报错）。
///
/// 为什么：公式引用的是**字段名**，而一行里某个字段可以没值
/// （TC-301 的说明 Sheet 说「留空表示没有这一项，不要填 0」）。
/// 若把「没值」当成「引用了不存在的字段」，那「绩效奖金」留空的一行
/// 会让整行算不出来 —— 一个没这项奖金的人反而拿不到工资条。
/// 设计文档 S07 的原型正是这么画的：赵六那行「个税」是「—」，
/// 而「实发工资」照样算出 13,100.00。
///
/// 但**名字不在配置里的字段仍然报错**（TC-203 标准 3）：
/// 那是公式写错了字段名，不是「这一项没有」。
/// 两者的区别就在这里：`specs` 是这个月的**字段配置**，
/// 配置里有、本行没值 → 0；配置里根本没有 → 报错。
pub fn evaluate_row(
    specs: &[FieldSpec],
    values: &BTreeMap<String, String>,
) -> Result<RowEvaluation, String> {
    // 拆成「数值的」与「文本的」
    let mut field_values: BTreeMap<String, f64> = BTreeMap::new();
    let mut text_fields: BTreeSet<String> = BTreeSet::new();
    for spec in specs {
        if matches!(spec.data_type, FieldType::Text) {
            text_fields.insert(spec.key.clone());
            continue;
        }
        match values.get(&spec.key) {
            Some(raw) => match crate::util::normalize_fullwidth(raw.trim()).parse::<f64>() {
                Ok(n) => {
                    field_values.insert(spec.key.clone(), n);
                }
                Err(_) => {
                    return Err(format!("{} 的值不是一个能算的数字", spec.label));
                }
            },
            // 配置里有、这一行没填 → 0（见函数注释）
            None => {
                field_values.insert(spec.key.clone(), 0.0);
            }
        }
    }

    // 汇总变量
    let formula_keys: BTreeSet<&str> = specs
        .iter()
        .filter(|s| s.is_formula())
        .map(|s| s.key.as_str())
        .collect();
    let mut total_earnings = 0.0f64;
    let mut total_deductions = 0.0f64;
    for (key, v) in &field_values {
        if formula_keys.contains(key.as_str()) {
            continue;
        }
        if *v >= 0.0 {
            total_earnings += v;
        } else {
            total_deductions += v;
        }
    }

    // 要算的公式字段
    let mut formula_fields: BTreeMap<String, String> = BTreeMap::new();
    for spec in specs {
        if spec.is_formula()
            && let Some(f) = &spec.formula
        {
            formula_fields.insert(spec.key.clone(), f.clone());
        }
    }

    let computed = formula::evaluate_formula_fields(
        &formula_fields,
        &field_values,
        total_earnings,
        total_deductions,
        &text_fields,
    )
    .map_err(|e| e.to_string())?;
    let computed = computed
        .into_iter()
        .map(|(k, v)| (k, format!("{:.2}", formula::round_to_cents(v))))
        .collect();

    Ok(RowEvaluation {
        computed,
        total_earnings,
        total_deductions,
    })
}

/// 把字段值序列化成存库的 JSON 文本。
///
/// **键按字典序**（`BTreeMap` 自带）—— 同样的内容永远序列化成同样的串，
/// 校验和因此是稳定的（ADR-0021 决策 13 的前提）。
///
/// 值的顺序无所谓（JSON 对象无序），但**键的顺序**要稳定。
pub fn values_to_json(values: &BTreeMap<String, String>) -> String {
    let mut out = String::from("{");
    for (i, (k, v)) in values.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&json_string(k));
        out.push(':');
        out.push_str(&json_string(v));
    }
    out.push('}');
    out
}

/// 从存库的 JSON 文本读回字段值。
pub fn values_from_json(text: &str) -> Result<BTreeMap<String, String>, String> {
    let parsed: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("记录里的值读不懂：{e}"))?;
    let obj = parsed
        .as_object()
        .ok_or_else(|| "记录里的值不是一个对象".to_string())?;
    let mut out = BTreeMap::new();
    for (k, v) in obj {
        let s = match v {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::Null => String::new(),
            other => other.to_string(),
        };
        out.insert(k.clone(), s);
    }
    Ok(out)
}

/// JSON 字符串字面量（含转义）。
fn json_string(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// 一行记录的校验和。
///
/// **算的是「收件人 + 这一行的值」**（设计文档 ch.5 防错机制 ②：
/// 「checksum(收件人 + 工资条数据)」）。
///
/// 为什么要带收件人：同一个人的数据被换成了另一个人的数据时，
/// 只算数据是发现不了的（值可能碰巧一样）。
///
/// 输出十六进制小写。**它不是密码学安全哈希的对抗场景**，
/// 但它是 SHA-256 —— 够用来发现「内容被改了」。
pub fn checksum(email: &str, values_json: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(email.trim().to_lowercase().as_bytes());
    hasher.update(b"\n");
    hasher.update(values_json.as_bytes());
    let digest = hasher.finalize();
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn specs() -> Vec<FieldSpec> {
        vec![
            FieldSpec {
                key: "base_salary".into(),
                label: "基本工资".into(),
                data_type: FieldType::Money,
                formula: None,
                required: true,
            },
            FieldSpec {
                key: "performance".into(),
                label: "绩效奖金".into(),
                data_type: FieldType::Money,
                formula: None,
                required: false,
            },
            FieldSpec {
                key: "social_insurance".into(),
                label: "社保个人".into(),
                data_type: FieldType::Money,
                formula: None,
                required: false,
            },
            FieldSpec {
                key: "net_pay".into(),
                label: "实发工资".into(),
                data_type: FieldType::Formula,
                formula: Some("base_salary + performance + social_insurance".into()),
                required: true,
            },
        ]
    }

    /// 行求值：公式列算出来的值 + 两个汇总。
    #[test]
    fn a_row_evaluates_its_formula_fields() {
        let mut values = BTreeMap::new();
        values.insert("base_salary".to_string(), "18000.00".to_string());
        values.insert("performance".to_string(), "3200.00".to_string());
        values.insert("social_insurance".to_string(), "-2100.00".to_string());
        let r = evaluate_row(&specs(), &values).expect("求值失败");
        assert_eq!(r.computed.get("net_pay").unwrap(), "19100.00");
        assert_eq!(r.total_earnings, 21200.0);
        assert_eq!(r.total_deductions, -2100.0);
    }

    /// ch.13 必测边界用例「中文全角数字」在**求值这条路上**也成立：
    /// 逐格编辑里用中文输入法打进去的「１８５００」要能算，不能报「不是一个能算的数字」。
    /// （导入那条路归 `domain::import::parse_amount` 的测试，这里钉住预览/核对/发送
    /// 共用的 `evaluate_row` —— 它们看的是同一份入库后的字符串。）
    #[test]
    fn fullwidth_cell_values_evaluate_like_halfwidth() {
        let mut values = BTreeMap::new();
        values.insert("base_salary".to_string(), "１８５００".to_string());
        values.insert("performance".to_string(), "３２００".to_string());
        values.insert("social_insurance".to_string(), "－２１００".to_string());
        let r = evaluate_row(&specs(), &values).expect("全角金额不该报错");
        assert_eq!(r.computed.get("net_pay").unwrap(), "19600.00");
        assert_eq!(r.total_earnings, 21700.0);
        assert_eq!(r.total_deductions, -2100.0);
    }

    /// 文本字段参与算术 → 报错（不是静默当 0）。
    #[test]
    fn text_in_arithmetic_is_an_error() {
        let mut spec_list = specs();
        spec_list.push(FieldSpec {
            key: "remark".into(),
            label: "备注".into(),
            data_type: FieldType::Text,
            formula: None,
            required: false,
        });
        let mut values = BTreeMap::new();
        values.insert("base_salary".to_string(), "18000.00".to_string());
        values.insert("remark".to_string(), "离职".to_string());
        let mut f = spec_list[3].clone();
        f.formula = Some("base_salary + remark".into());
        spec_list[3] = f;
        let err = evaluate_row(&spec_list, &values).expect_err("文本参与算术应该报错");
        // 报的是「类型错误」而不是「未知变量」——
        // 这正是 TC-203 标准 7 要求的（设计文档 ch.8：「类型错误」单独成一类）。
        assert!(err.contains("不能参与计算"), "{err}");
    }

    /// **这一行没填的字段，在公式里算 0**（不是「引用了不存在的字段」）。
    ///
    /// 这是 TC-302 验收时发现并定的语义：公式引用的是**字段名**，
    /// 而一行里某个字段可以没值（TC-301 的说明 Sheet：「留空表示没有这一项」）。
    /// 若把「没值」当成「字段不存在」，那「绩效奖金」留空的一行会算不出来 ——
    /// 一个没这项奖金的人反而拿不到工资条。
    /// 设计文档 S07 的原型正是这么画的：赵六那行「个税」是「—」，
    /// 而「实发工资」照样算出 13,100.00。
    ///
    /// 但**名字不在配置里的字段仍然报错**（TC-203 标准 3）——那是公式写错了字段名。
    #[test]
    fn a_field_that_is_known_but_blank_counts_as_zero() {
        let mut values = BTreeMap::new();
        values.insert("base_salary".to_string(), "18000.00".to_string());
        // performance 与 social_insurance 都留空 → 这两项当 0
        let r = evaluate_row(&specs(), &values).expect("求值失败");
        assert_eq!(
            r.computed.get("net_pay").unwrap(),
            "18000.00",
            "留空的字段该按 0 算：{:?}",
            r.computed
        );
        assert_eq!(r.total_earnings, 18000.0);
        assert_eq!(r.total_deductions, 0.0);
    }

    /// **名字不在配置里的字段仍然报错**（TC-203 标准 3 不因为上面那条而松）。
    #[test]
    fn a_field_that_does_not_exist_is_still_an_error() {
        let mut specs = specs();
        specs[3].formula = Some("nosuchfield + 1".into());
        let mut values = BTreeMap::new();
        values.insert("base_salary".to_string(), "18000.00".to_string());
        let err = evaluate_row(&specs, &values).expect_err("不存在的字段该报错");
        assert!(err.contains("nosuchfield"), "{err}");
    }

    /// 值的序列化是稳定的（键按字典序）。
    #[test]
    fn values_json_is_stable() {
        let mut a = BTreeMap::new();
        a.insert("b".to_string(), "2".to_string());
        a.insert("a".to_string(), "1".to_string());
        let mut b = BTreeMap::new();
        b.insert("a".to_string(), "1".to_string());
        b.insert("b".to_string(), "2".to_string());
        assert_eq!(values_to_json(&a), values_to_json(&b));
        assert_eq!(values_to_json(&a), "{\"a\":\"1\",\"b\":\"2\"}");
    }

    /// 值的往返：序列化 → 读回，一致。
    #[test]
    fn values_round_trip() {
        let mut v = BTreeMap::new();
        v.insert("base_salary".to_string(), "18000.00".to_string());
        v.insert("remark".to_string(), "带\"引号\"与\\反斜杠".to_string());
        let json = values_to_json(&v);
        assert_eq!(values_from_json(&json).unwrap(), v);
    }

    /// 校验和：内容变了就变；收件人变了也变。
    #[test]
    fn checksum_covers_content_and_recipient() {
        let a = "{\"a\":\"1\"}";
        let b = "{\"a\":\"2\"}";
        assert_ne!(checksum("x@example.com", a), checksum("x@example.com", b));
        assert_ne!(checksum("x@example.com", a), checksum("y@example.com", a));
        assert_eq!(checksum("X@example.com", a), checksum("x@example.com", a));
    }
}
