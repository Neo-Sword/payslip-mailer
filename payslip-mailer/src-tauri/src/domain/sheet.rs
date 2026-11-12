//! 工资表的结构（TC-301 · ADR-0020）
//!
//! 一张工资条模板的表头 = **3 个固定列**（姓名 / 邮箱 / 工号，来自成员表）
//! + **启用字段**（来自工资条项目配置，按 `sort_order`）。
//!
//! 固定列在前，是因为它们是**导入映射的锚点**（TC-302 的表头映射规则
//! 「优先按字段 key 精确匹配，其次按显示名模糊匹配」需要一个确定的起点）。
//! 固定列的来源是成员表，启用字段的来源是工资条项目 —— 两者不是一回事。
//!
//! 纯逻辑：这里只回答「这张表该有哪些列、每一列是什么性质」，
//! 不碰 xlsx、不碰文件。

use serde::Serialize;

/// 固定列的键（设计文档 S09 的「3 个固定列」）。
pub const FIXED_COLUMNS: [(&str, &str); 3] =
    [("姓名", "name"), ("邮箱", "email"), ("工号", "employee_no")];

/// 公式列在导出的表里写的占位文字（设计文档 S09 的原型里就是这么画的）。
pub const FORMULA_PLACEHOLDER: &str = "公式自动计算";

/// 一列的种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ColumnKind {
    /// 固定列（姓名 / 邮箱 / 工号）。
    Fixed,
    /// 工资项字段。
    Field,
}

/// 一列。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SheetColumn {
    /// 表头文字（显示名）。
    pub title: String,
    /// 稳定键：固定列是 `name`/`email`/`employee_no`，字段列是字段的 key。
    /// 导入映射按它精确匹配（TC-302）。
    pub key: String,
    pub kind: ColumnKind,
    /// 这一列是不是必填。
    pub required: bool,
    /// 这一列是不是公式列（只读、导入时忽略）。
    pub is_formula: bool,
    /// 这一列是不是金额/数值（决定用不用数值格式与数据验证）。
    pub numeric: bool,
    /// 这一列的数据类型（`money` / `number` / `percent` / `text`）。
    /// 固定列没有类型 → `None`。
    pub data_type: Option<String>,
}

impl SheetColumn {
    /// 这一列的数据类型（固定列 → `None`）。
    pub fn data_type_hint(&self) -> Option<&str> {
        self.data_type.as_deref()
    }
}

/// 一张工资条模板的结构。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SheetSchema {
    pub columns: Vec<SheetColumn>,
}

/// 字段类型 → 是不是数值列。
///
/// `money` / `number` / `percent` / `formula` 都是数值；`text` 不是
/// （设计文档 ch.8 的「字段类型与行为」表：text 原样，其余按数字渲染）。
pub fn is_numeric_type(data_type: &str) -> bool {
    matches!(data_type, "money" | "number" | "percent" | "formula")
}

/// 由固定列 + 字段列拼出表结构。
///
/// `fields` 的每一项是 `(key, 显示名, 类型, 是否必填, 是否公式列, 是否启用)`。
/// **未启用的字段不进表头**（ADR-0020 决策 3）。
pub fn build_schema(fields: &[(String, String, String, bool, bool, bool)]) -> SheetSchema {
    let mut columns: Vec<SheetColumn> = FIXED_COLUMNS
        .iter()
        .map(|(title, key)| SheetColumn {
            title: (*title).to_string(),
            key: (*key).to_string(),
            kind: ColumnKind::Fixed,
            // 姓名与邮箱必填（设计文档 S08 的行级校验：「姓名/邮箱必填」）；工号可选。
            required: *key != "employee_no",
            is_formula: false,
            numeric: false,
            data_type: None,
        })
        .collect();

    for (key, label, data_type, required, is_formula, visible) in fields {
        if !visible {
            continue;
        }
        columns.push(SheetColumn {
            title: label.clone(),
            key: key.clone(),
            kind: ColumnKind::Field,
            required: *required,
            is_formula: *is_formula,
            numeric: is_numeric_type(data_type),
            data_type: Some(data_type.clone()),
        });
    }

    SheetSchema { columns }
}

/// 「填写说明」Sheet 的内容（设计文档 ch.12 S09 的选项之一）。
///
/// 里面必须写清的三件事：
/// 1. **扣款项填负数**（社保、公积金、个税）—— 填正数会让实发工资整个算错；
/// 2. **公式列不用填**（系统会算）；
/// 3. 每列是什么、单位是什么。
pub fn instructions_sheet(schema: &SheetSchema) -> Vec<Vec<String>> {
    let mut rows: Vec<Vec<String>> = vec![vec!["项目".to_string(), "说明".to_string()]];
    rows.push(vec![
        "整张表".to_string(),
        "每行一个人；姓名、邮箱、工号在最左边，后面是工资项。".to_string(),
    ]);
    rows.push(vec![
        "扣款项".to_string(),
        "**填负数**（比如社保填 -2100、个税填 -860）。实发工资是各列相加，填正数会算错。"
            .to_string(),
    ]);
    rows.push(vec![
        "公式列".to_string(),
        "不用填 —— 系统算好（导入时也会忽略你填的值）。".to_string(),
    ]);
    rows.push(vec!["金额单位".to_string(), "元。两位小数。".to_string()]);
    rows.push(vec![
        "空白的格子".to_string(),
        "这个人不填这项。留空即可，不要填 0（0 和「没有」不是一回事）。".to_string(),
    ]);

    for col in &schema.columns {
        let mut note = String::new();
        if col.is_formula {
            note.push_str("公式列，不用填");
        } else if col.numeric {
            note.push_str("金额/数字，留空表示没有这项");
        } else {
            note.push_str("留空表示没有这项");
        }
        if col.required {
            note.push_str("；必填");
        }
        rows.push(vec![col.title.clone(), note]);
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(key: &str, label: &str, ty: &str) -> (String, String, String, bool, bool, bool) {
        (
            key.to_string(),
            label.to_string(),
            ty.to_string(),
            false,
            false,
            true,
        )
    }

    /// 标准 1：3 个固定列在前，启用字段在后。
    #[test]
    fn fixed_columns_come_first() {
        let schema = build_schema(&[
            field("base_salary", "基本工资", "money"),
            field("net_pay", "实发工资", "formula"),
        ]);
        let titles: Vec<&str> = schema.columns.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, vec!["姓名", "邮箱", "工号", "基本工资", "实发工资"]);
        // 固定列的键是成员的键（导入映射的锚点）
        assert_eq!(schema.columns[0].key, "name");
        assert_eq!(schema.columns[1].key, "email");
        assert_eq!(schema.columns[2].key, "employee_no");
    }

    /// 标准 3：停用 / 隐藏的字段不进表头。
    #[test]
    fn disabled_fields_are_not_in_the_header() {
        let schema = build_schema(&[
            field("base_salary", "基本工资", "money"),
            (
                "old".to_string(),
                "旧项".to_string(),
                "money".to_string(),
                false,
                false,
                false, // visible = false
            ),
        ]);
        assert!(
            !schema.columns.iter().any(|c| c.key == "old"),
            "停用的字段不该进表头：{:?}",
            schema.columns
        );
    }

    /// 标准 4：公式列被标出来，数值列被标出来。
    #[test]
    fn formula_and_numeric_columns_are_flagged() {
        let schema = build_schema(&[
            field("base_salary", "基本工资", "money"),
            (
                "net_pay".to_string(),
                "实发工资".to_string(),
                "formula".to_string(),
                true,
                true,
                true,
            ),
            field("remark", "备注", "text"),
        ]);
        let net = schema.columns.iter().find(|c| c.key == "net_pay").unwrap();
        assert!(net.is_formula);
        assert!(net.numeric);
        let remark = schema.columns.iter().find(|c| c.key == "remark").unwrap();
        assert!(!remark.is_formula);
        assert!(!remark.numeric, "文本列不是数值列");
        // 姓名必填、工号可选
        assert!(schema.columns[0].required);
        assert!(!schema.columns[2].required);
    }

    /// 标准 5：说明里必须写清「扣款项填负数」与「公式列不用填」。
    #[test]
    fn the_instructions_say_the_two_critical_things() {
        let schema = build_schema(&[field("base_salary", "基本工资", "money")]);
        let text = instructions_sheet(&schema)
            .iter()
            .map(|r| r.join(" "))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("填负数"), "{text}");
        assert!(text.contains("公式列"), "{text}");
        // 每列都要有一行说明
        assert!(text.contains("基本工资"), "{text}");
    }
}
