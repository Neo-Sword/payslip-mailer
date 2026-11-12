//! 工资条项目的读写（TC-200 · ADR-0015）
//!
//! 这一层是 `salary_fields` 表的**唯一入口**：界面与 Command 层都走它，
//! 所以「表头长什么样」这件事只有一个算法（[`preview_header`]），
//! TC-301 导出表头时直接复用它，不另算一遍。
//!
//! **没有删除操作**（ADR-0015 决策 2）：字段要么在、要么停用。
//! 「有没有被历史月份引用」的判据要到 TC-300 才有，那时才谈得上删除。

use rusqlite::params;

use crate::domain::field::{FieldType, check_field_input};
use crate::error::{AppError, AppResult};
use crate::infra::db::DbState;

/// 一个字段（界面看到的那一行）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Field {
    pub id: i64,
    /// 变量键（`{{key}}` 里的那个）。
    pub key: String,
    /// 显示名称。
    pub label: String,
    pub data_type: FieldType,
    /// 排序（小在前）。
    pub sort_order: i64,
    pub required: bool,
    /// `false` = 已停用（不进新表头、不在新建模板的白名单里）。
    pub visible: bool,
    pub formula: Option<String>,
    pub align: String,
}

/// 列出全部字段，按 `sort_order` 排。
///
/// **含已停用的** —— 界面要把它们显示出来（灰的），用户才知道「我停用过一个」。
pub fn list(db: &DbState) -> AppResult<Vec<Field>> {
    db.read(|conn| {
        let mut stmt = conn
            .prepare(
                "SELECT id, key, label, data_type, sort_order, required, visible, formula, align \
                 FROM salary_fields ORDER BY sort_order, id",
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, String>(8)?,
                ))
            })
            .map_err(|e| AppError::Database(e.to_string()))?;
        let mut out = Vec::new();
        for row in rows {
            let (id, key, label, data_type, sort_order, required, visible, formula, align) =
                row.map_err(|e| AppError::Database(e.to_string()))?;
            // 库里被手工改成非法类型时，不让整个列表挂掉：退化成 Text 并记日志。
            let parsed_type = match FieldType::parse(&data_type) {
                Ok(t) => t,
                Err(_) => {
                    tracing::warn!(%data_type, %key, "库里的字段类型不认识，按文本处理");
                    FieldType::Text
                }
            };
            out.push(Field {
                id,
                key,
                label,
                data_type: parsed_type,
                sort_order,
                required: required != 0,
                visible: visible != 0,
                formula,
                align,
            });
        }
        Ok(out)
    })
}

/// 新建一个字段。
pub fn create(
    db: &DbState,
    key: &str,
    label: &str,
    data_type: FieldType,
    required: bool,
    formula: Option<&str>,
) -> AppResult<Field> {
    let existing = list(db)?;
    let known: Vec<String> = existing.iter().map(|f| f.key.clone()).collect();
    // 唯一性在这里先挡一道（库里还有 UNIQUE 兜底）
    if known.iter().any(|k| k == key) {
        return Err(AppError::Duplicate(format!("变量键 {key}")));
    }
    check_field_input(key, label, data_type, formula, &known)?;

    // 新字段排在最后
    let next_order = existing.last().map(|f| f.sort_order + 1).unwrap_or(0);
    let align = data_type.default_align();

    let id = db.write(|conn| {
        conn.execute(
            "INSERT INTO salary_fields (key, label, data_type, sort_order, required, visible, formula, align) \
             VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?7)",
            params![
                key,
                label,
                data_type.as_str(),
                next_order,
                i64::from(required),
                formula,
                align
            ],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(conn.last_insert_rowid())
    })?;

    Ok(Field {
        id,
        key: key.to_string(),
        label: label.to_string(),
        data_type,
        sort_order: next_order,
        required,
        visible: true,
        formula: formula.map(|s| s.to_string()),
        align: align.to_string(),
    })
}

/// 编辑一个字段（显示名、类型、必填、公式）。
///
/// **key 不可改**：key 已经进过 Excel 表头与模板变量，改它等于换了一个字段，
/// 而旧数据里的那个 key 会变成「引用了不存在的字段」。
pub fn update(
    db: &DbState,
    id: i64,
    label: &str,
    data_type: FieldType,
    required: bool,
    formula: Option<&str>,
) -> AppResult<()> {
    let existing = list(db)?;
    let known: Vec<String> = existing.iter().map(|f| f.key.clone()).collect();
    check_field_input(
        existing
            .iter()
            .find(|f| f.id == id)
            .map(|f| f.key.as_str())
            .unwrap_or_default(),
        label,
        data_type,
        formula,
        &known,
    )?;
    let align = data_type.default_align();
    db.write(|conn| {
        conn.execute(
            "UPDATE salary_fields SET label = ?1, data_type = ?2, required = ?3, formula = ?4, align = ?5 \
             WHERE id = ?6",
            params![label, data_type.as_str(), i64::from(required), formula, align, id],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    })
}

/// 停用 / 启用一个字段。
///
/// 停用 = `visible = 0`：**不进新导出的 Excel 表头、不在新建模板的变量白名单里**。
/// 数据一行不动（历史数据里那个字段的值仍然可读）。
pub fn set_visible(db: &DbState, id: i64, visible: bool) -> AppResult<()> {
    db.write(|conn| {
        conn.execute(
            "UPDATE salary_fields SET visible = ?1 WHERE id = ?2",
            params![i64::from(visible), id],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    })
}

/// 按给定的 id 顺序重排（拖拽排序的结果）。
///
/// `order` 必须是全部字段的 id，一个不多一个不少 —— 少一个就等于偷偷把某个字段
/// 排到末尾去了。
pub fn reorder(db: &DbState, order: &[i64]) -> AppResult<()> {
    let existing = list(db)?;
    if order.len() != existing.len() {
        return Err(AppError::ValidationError(format!(
            "排序要覆盖全部 {} 个字段，给了 {} 个",
            existing.len(),
            order.len()
        )));
    }
    for id in order {
        if !existing.iter().any(|f| f.id == *id) {
            return Err(AppError::ValidationError(format!(
                "排序里有不存在的字段 id {id}"
            )));
        }
    }
    db.write(|conn| {
        for (i, id) in order.iter().enumerate() {
            conn.execute(
                "UPDATE salary_fields SET sort_order = ?1 WHERE id = ?2",
                params![i as i64, id],
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        }
        Ok(())
    })
}

/// **下一份空模板会出现的列** —— 设计文档 S04 的「预览表头」。
///
/// 这是导出表头的**唯一真相源**：TC-301 直接用它，不另算一遍
/// （两处各算一遍必然漂移）。
///
/// 只含 `visible` 的字段（停用的不进新表头）。
pub fn preview_header(db: &DbState) -> AppResult<Vec<String>> {
    Ok(list(db)?
        .into_iter()
        .filter(|f| f.visible)
        .map(|f| f.label)
        .collect())
}

/// **新建模板时可选的变量** —— 给 TC-202 用。
///
/// 只含 `visible` 的字段：停用的字段不能被选进新建的模板。
///
/// ⚠️ 渲染**历史**模板时要用的是**全部** key（含已停用的）——
/// 那个字段的历史值还在，老模板要能渲染出来。两条名单不是一回事，
/// 别把这里的结果拿去当渲染期的白名单。
pub fn selectable_variables(db: &DbState) -> AppResult<Vec<(String, String)>> {
    Ok(list(db)?
        .into_iter()
        .filter(|f| f.visible)
        .map(|f| (f.key.clone(), f.label.clone()))
        .collect())
}

/// 全部字段的 key（**含已停用的**）—— 公式校验时用。
pub fn all_keys(db: &DbState) -> AppResult<Vec<String>> {
    Ok(list(db)?.into_iter().map(|f| f.key).collect())
}

/// 设计文档 S04 原型里的那组默认字段。
///
/// **这是本卡唯一的一处「猜」**（ADR-0015 决策 3）：它来自设计文档的原型示例集，
/// 不是贵司的真实清单。S04 的界面上有一句话点明这一点，用户照着改即可。
/// 此刻改它零成本（P2/P3 都没开始，没有任何月份引用过字段）。
fn default_fields() -> Vec<(
    &'static str,
    &'static str,
    FieldType,
    bool,
    Option<&'static str>,
)> {
    vec![
        ("base_salary", "基本工资", FieldType::Money, true, None),
        ("performance", "绩效奖金", FieldType::Money, false, None),
        ("meal_allowance", "餐补", FieldType::Money, false, None),
        (
            "social_insurance",
            "社保个人",
            FieldType::Money,
            false,
            None,
        ),
        ("housing_fund", "公积金", FieldType::Money, false, None),
        (
            "net_pay",
            "实发工资",
            FieldType::Formula,
            true,
            // 原型写的是「应发合计 + 扣款合计」；扣款项按设计文档是**负数**
            // （「填负数，如 -2100」），所以它就是全部金额字段之和。
            Some("base_salary + performance + meal_allowance + social_insurance + housing_fund"),
        ),
        ("remark", "备注", FieldType::Text, false, None),
    ]
}

/// 写入默认字段集 —— **只在表为空时写**。
///
/// 「只在表为空时写」是硬要求：初始化会被「恢复当前库」「重新初始化」这些路径碰到，
/// 覆盖掉用户配好的字段是灾难。
pub fn ensure_defaults(db: &DbState) -> AppResult<usize> {
    if !list(db)?.is_empty() {
        return Ok(0);
    }
    let mut written = 0;
    for (key, label, ty, required, formula) in default_fields() {
        create(db, key, label, ty, required, formula)?;
        written += 1;
    }
    tracing::info!(written, "已写入默认工资条项目（设计文档 S04 的示例集）");
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::db::CIPHER_PARAMS;
    use crate::infra::keychain::{TempKeychain, no_user_interaction};

    /// 一个临时库。
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
                "xinyou-fields-test-{}-{nanos}-{label}",
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
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// 标准 1：默认字段集是设计文档 S04 原型的那七项。
    #[test]
    fn default_fields_match_the_design_doc_prototype() {
        let fx = Fx::new("std01");
        fx.open();
        assert_eq!(ensure_defaults(&fx.db).expect("写默认字段失败"), 7);

        let fields = list(&fx.db).expect("列字段失败");
        let keys: Vec<&str> = fields.iter().map(|f| f.key.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "base_salary",
                "performance",
                "meal_allowance",
                "social_insurance",
                "housing_fund",
                "net_pay",
                "remark"
            ]
        );
        // 实发工资是公式类型且带公式
        let net = fields.iter().find(|f| f.key == "net_pay").unwrap();
        assert_eq!(net.data_type, FieldType::Formula);
        assert!(net.formula.is_some(), "实发工资要有公式");
        // 备注是文本且不显示在工资条上
        let remark = fields.iter().find(|f| f.key == "remark").unwrap();
        assert_eq!(remark.data_type, FieldType::Text);
    }

    /// 标准 11：默认字段集只在表为空时写。
    #[test]
    fn defaults_are_only_written_when_the_table_is_empty() {
        let fx = Fx::new("std11");
        fx.open();
        ensure_defaults(&fx.db).expect("写默认字段失败");

        // 用户自己加了一个字段
        create(&fx.db, "custom", "自定义项", FieldType::Money, false, None).expect("新建失败");
        // 再走一遍初始化 → 不该覆盖
        assert_eq!(ensure_defaults(&fx.db).expect("再次初始化失败"), 0);
        let fields = list(&fx.db).expect("列字段失败");
        assert_eq!(fields.len(), 8, "不该覆盖用户已有的字段");
        assert!(fields.iter().any(|f| f.key == "custom"), "用户的字段还在");
    }

    /// 标准 2：key 唯一 + 格式受限。
    #[test]
    fn key_must_be_unique_and_well_formed() {
        let fx = Fx::new("std02");
        fx.open();
        ensure_defaults(&fx.db).expect("写默认字段失败");

        let dup = create(
            &fx.db,
            "base_salary",
            "又一个基本工资",
            FieldType::Money,
            false,
            None,
        )
        .expect_err("重复 key 应被拒");
        assert!(matches!(dup, AppError::Duplicate(_)), "实际是 {dup:?}");

        assert!(
            create(
                &fx.db,
                "基本工资",
                "中文 key",
                FieldType::Money,
                false,
                None
            )
            .is_err(),
            "中文 key 应被拒"
        );
        assert!(
            create(
                &fx.db,
                "Base Salary",
                "带空格",
                FieldType::Money,
                false,
                None
            )
            .is_err(),
            "带空格的 key 应被拒"
        );
    }

    /// 标准 4：停用后不在新表头里、不在新建模板的白名单里。
    #[test]
    fn a_deactivated_field_leaves_the_header_and_the_whitelist() {
        let fx = Fx::new("std04");
        fx.open();
        ensure_defaults(&fx.db).expect("写默认字段失败");

        let before = preview_header(&fx.db).expect("预览表头失败");
        assert!(before.contains(&"餐补".to_string()), "停用前表头里有餐补");
        assert!(
            selectable_variables(&fx.db)
                .expect("白名单失败")
                .iter()
                .any(|(k, _)| k == "meal_allowance")
        );

        let meal = list(&fx.db)
            .expect("列字段失败")
            .into_iter()
            .find(|f| f.key == "meal_allowance")
            .unwrap();
        set_visible(&fx.db, meal.id, false).expect("停用失败");

        let after = preview_header(&fx.db).expect("预览表头失败");
        assert!(
            !after.contains(&"餐补".to_string()),
            "停用后表头里不该有餐补"
        );
        assert!(
            !selectable_variables(&fx.db)
                .expect("白名单失败")
                .iter()
                .any(|(k, _)| k == "meal_allowance"),
            "停用后不该能选进新建模板"
        );
        // 但字段行还在、key 还在（历史数据要能读）
        let fields = list(&fx.db).expect("列字段失败");
        let still = fields.iter().find(|f| f.key == "meal_allowance").unwrap();
        assert!(!still.visible, "应当是停用态");
        assert!(
            all_keys(&fx.db)
                .expect("取 key 失败")
                .contains(&"meal_allowance".to_string()),
            "停用的字段的 key 仍然要能被公式引用（历史数据要能渲染）"
        );
    }

    /// 标准 6/7：排序落库，预览表头跟着变。
    #[test]
    fn reordering_persists_and_moves_the_header_columns() {
        let fx = Fx::new("std06");
        fx.open();
        ensure_defaults(&fx.db).expect("写默认字段失败");

        let before = preview_header(&fx.db).expect("预览表头失败");
        assert_eq!(before[0], "基本工资");

        let fields = list(&fx.db).expect("列字段失败");
        let mut ids: Vec<i64> = fields.iter().map(|f| f.id).collect();
        ids.swap(0, 1);
        reorder(&fx.db, &ids).expect("排序失败");

        // 落库了（不是只在内存里排）
        let after_fields = list(&fx.db).expect("列字段失败");
        assert_eq!(after_fields[0].key, "performance");
        assert_eq!(after_fields[1].key, "base_salary");
        assert_eq!(preview_header(&fx.db).expect("预览表头失败")[0], "绩效奖金");

        // 排序必须覆盖全部字段
        assert!(
            reorder(&fx.db, &ids[..2]).is_err(),
            "少给 id 应当被拒（不能偷偷把字段排到末尾）"
        );
    }

    /// 标准 8/9/10：公式校验与类型一致性。
    #[test]
    fn formula_rules_are_enforced_on_save() {
        let fx = Fx::new("std08");
        fx.open();
        ensure_defaults(&fx.db).expect("写默认字段失败");

        let known = all_keys(&fx.db).expect("取 key 失败");
        // 引用不存在的 key
        assert!(
            create(
                &fx.db,
                "net2",
                "净发",
                FieldType::Formula,
                true,
                Some("base_salary + nope")
            )
            .is_err(),
            "引用不存在的 key 应被拒"
        );
        // 括号不配平
        assert!(
            create(
                &fx.db,
                "net2",
                "净发",
                FieldType::Formula,
                true,
                Some("base_salary + (")
            )
            .is_err(),
            "括号不配平应被拒"
        );
        // 非法字符
        assert!(
            create(
                &fx.db,
                "net2",
                "净发",
                FieldType::Formula,
                true,
                Some("base_salary & x")
            )
            .is_err(),
            "非法字符应被拒"
        );
        // 非公式类型不许有公式
        assert!(
            create(
                &fx.db,
                "net2",
                "净发",
                FieldType::Money,
                true,
                Some("base_salary")
            )
            .is_err(),
            "金额类型不该有公式"
        );
        // 公式类型必须填公式
        assert!(
            create(&fx.db, "net2", "净发", FieldType::Formula, true, None).is_err(),
            "公式类型必须填公式"
        );
        // 合法公式通过
        let ok = create(
            &fx.db,
            "net2",
            "净发",
            FieldType::Formula,
            true,
            Some("base_salary + performance"),
        )
        .expect("合法公式应通过");
        assert_eq!(ok.formula.as_deref(), Some("base_salary + performance"));
        assert_eq!(known.len(), 7);
    }

    /// 标准 3：没有删除操作。
    #[test]
    fn there_is_no_delete_operation() {
        // 这条是「结构上没有」而不是「行为上没有」：
        // 模块里根本没有 delete / remove 这样的函数。
        // 只扫**测试模块之前**的那部分 —— 否则这条测试会匹配到它自己
        // （禁用词表里就写着 "fn delete"）。
        let src = include_str!("fields.rs");
        let production = src.split("#[cfg(test)]").next().unwrap_or_default();
        for forbidden in ["fn delete", "fn remove", "DELETE FROM salary_fields"] {
            assert!(
                !production.contains(forbidden),
                "字段模块里不该有 {forbidden}（红线 8：只能停用不能删除）"
            );
        }
    }
}

#[cfg(test)]
mod formula_tests {
    use super::*;
    use crate::domain::field::FieldType;
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
                "xinyou-fields-formula-{}-{nanos}-{label}",
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

    /// 按行求值：汇总变量按「该行」算，公式字段算出自己的值。
    #[test]
    fn a_row_evaluates_its_formula_fields() {
        let fx = Fx::new("row");
        fx.open();
        create(
            &fx.db,
            "base_salary",
            "基本工资",
            FieldType::Money,
            true,
            None,
        )
        .expect("建字段失败");
        create(
            &fx.db,
            "performance",
            "绩效奖金",
            FieldType::Money,
            false,
            None,
        )
        .expect("建字段失败");
        create(
            &fx.db,
            "social_insurance",
            "社保个人",
            FieldType::Money,
            false,
            None,
        )
        .expect("建字段失败");
        create(
            &fx.db,
            "net_pay",
            "实发工资",
            FieldType::Formula,
            true,
            Some("应发合计 + 扣款合计"),
        )
        .expect("建字段失败");

        let mut values = std::collections::BTreeMap::new();
        values.insert("base_salary".to_string(), "18000".to_string());
        values.insert("performance".to_string(), "3200".to_string());
        values.insert("social_insurance".to_string(), "-2100".to_string());

        let mut formulas = std::collections::BTreeMap::new();
        formulas.insert("net_pay".to_string(), "应发合计 + 扣款合计".to_string());

        // 汇总：正数 21200、负数 -2100（公式字段不参与汇总）
        let mut field_values = std::collections::BTreeMap::new();
        field_values.insert("base_salary".to_string(), 18000.0);
        field_values.insert("performance".to_string(), 3200.0);
        field_values.insert("social_insurance".to_string(), -2100.0);

        let computed = crate::domain::formula::evaluate_formula_fields(
            &formulas,
            &field_values,
            21200.0,
            -2100.0,
            &std::collections::BTreeSet::new(),
        )
        .expect("求值失败");
        assert_eq!(computed.get("net_pay"), Some(&19100.0), "{computed:?}");
    }

    /// 循环引用在生产路径上被抓住（不是栈溢出）。
    #[test]
    fn a_cyclic_formula_is_caught_in_the_row_evaluation() {
        let mut formulas = std::collections::BTreeMap::new();
        formulas.insert("a".to_string(), "b + 1".to_string());
        formulas.insert("b".to_string(), "a + 1".to_string());
        let err = crate::domain::formula::evaluate_formula_fields(
            &formulas,
            &std::collections::BTreeMap::new(),
            0.0,
            0.0,
            &std::collections::BTreeSet::new(),
        )
        .expect_err("应报错");
        assert!(
            matches!(err, crate::domain::formula::FormulaError::Cycle(_)),
            "{err:?}"
        );
    }
}
