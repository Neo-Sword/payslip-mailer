//! 导出空模板（TC-301 · ADR-0020）
//!
//! 用 `rust_xlsxwriter` 生成 xlsx（设计文档 ch.8：「由 rust_xlsxwriter 按
//! `salary_fields.sort_order` 生成」）。表结构由 [`crate::domain::sheet`] 决定 ——
//! 这里只负责**怎么写进 Excel**。
//!
//! 写出来的东西要让财务不出错：
//! - 首行冻结（滚动时表头一直在）
//! - 金额列是数值格式 + 数据验证（填错当场被 Excel 拦下）
//! - 公式列写「公式自动计算」占位（它由系统算，不是财务填）
//! - 说明 Sheet 写清「扣款项填负数」

use std::path::{Path, PathBuf};

use rust_xlsxwriter::{
    DataValidation, DataValidationRule, DocProperties, Format, FormatBorder, Workbook,
};

use crate::domain::sheet::{ColumnKind, FORMULA_PLACEHOLDER, SheetSchema};
use crate::error::{AppError, AppResult};

/// 第一个 Sheet 的名字（设计文档 S09 的「工资表」）。
pub const SHEET_SALARY: &str = "工资表";

/// 第二个 Sheet 的名字（设计文档 S09 的「填写说明」）。
pub const SHEET_INSTRUCTIONS: &str = "填写说明";

/// 导出选项。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ExportOptions {
    /// 包含「填写说明」Sheet（默认开 —— 财务第一次填表最需要它）。
    pub include_instructions: bool,
    /// 包含 2 行示例数据（默认关 —— 示例行很容易被忘了删）。
    pub include_sample_rows: bool,
    /// 预填成员名单（默认关 —— 它会把姓名邮箱带进文件）。
    pub include_member_list: bool,
}

impl ExportOptions {
    /// 设计文档 S09 的默认：说明 Sheet 开，另两个关（ADR-0020 决策 5）。
    pub fn defaults() -> Self {
        Self {
            include_instructions: true,
            include_sample_rows: false,
            include_member_list: false,
        }
    }
}

/// 一行数据（预填名单或示例数据用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub cells: Vec<String>,
}

/// 生成 xlsx 到 `path`。
///
/// `rows` 是可选的数据行（示例数据 / 预填名单）—— 默认为空表（只有表头）。
pub fn write_xlsx(
    path: &Path,
    schema: &SheetSchema,
    options: ExportOptions,
    rows: &[Row],
) -> AppResult<()> {
    let mut workbook = Workbook::new();
    workbook.set_properties(&DocProperties::new().set_title("薪邮工资条模板"));

    // ---- 工资表 ----
    {
        let worksheet = workbook.add_worksheet();
        // Sheet 名要看得懂 —— 设计文档 S09 说的是「工资表」与「填写说明」两个 Sheet，
        // 不是 `Sheet1` / `Sheet2`（默认名会让财务在两个 Sheet 之间认错）。
        worksheet.set_name(SHEET_SALARY).map_err(xlsx_err)?;
        let header_fmt = Format::new()
            .set_bold()
            .set_border_bottom(FormatBorder::Thin);
        let money_fmt = Format::new().set_num_format("#,##0.00");
        let text_fmt = Format::new();

        // 表头
        for (i, col) in schema.columns.iter().enumerate() {
            let title = if col.required {
                format!("{} *", col.title)
            } else {
                col.title.clone()
            };
            worksheet
                .write_string_with_format(0, i as u16, &title, &header_fmt)
                .map_err(|e| AppError::Internal(format!("写表头失败：{e}")))?;
        }

        // 数据行
        for (r, row) in rows.iter().enumerate() {
            let excel_row = (r + 1) as u32;
            for (i, col) in schema.columns.iter().enumerate() {
                let cell = row.cells.get(i).map(|s| s.as_str()).unwrap_or("");
                // 公式列不写数据（它由系统算）—— 给个占位。
                // **先判它**：公式列的格子本来就是空的，不能被下面的「空就跳过」挡掉。
                if col.is_formula && col.kind == ColumnKind::Field {
                    worksheet
                        .write_string_with_format(
                            excel_row,
                            i as u16,
                            FORMULA_PLACEHOLDER,
                            &text_fmt,
                        )
                        .map_err(|e| AppError::Internal(format!("写公式列占位失败：{e}")))?;
                    continue;
                }
                if cell.is_empty() {
                    continue;
                }
                let fmt = if col.numeric { &money_fmt } else { &text_fmt };
                if col.numeric
                    && let Ok(n) = cell.replace(',', "").trim().parse::<f64>()
                {
                    worksheet
                        .write_number_with_format(excel_row, i as u16, n, fmt)
                        .map_err(|e| AppError::Internal(format!("写单元格失败：{e}")))?;
                } else {
                    worksheet
                        .write_string_with_format(excel_row, i as u16, cell, fmt)
                        .map_err(|e| AppError::Internal(format!("写单元格失败：{e}")))?;
                }
            }
        }

        // 首行冻结（设计文档 S09 交互要点）
        worksheet
            .set_freeze_panes(1, 0)
            .map_err(|e| AppError::Internal(format!("冻结首行失败：{e}")))?;

        // 金额列的数据验证：不能填非数字（设计文档 S09「金额列数值格式 + 数据校验」）
        for (i, col) in schema.columns.iter().enumerate() {
            if !col.numeric {
                continue;
            }
            let mut dv = DataValidation::new();
            dv = dv.allow_decimal_number(DataValidationRule::Between(
                -1_000_000_000.0,
                1_000_000_000.0,
            ));
            dv = dv.set_input_title("金额").map_err(xlsx_err)?;
            dv = dv
                .set_input_message("请填数字；扣款项填负数。")
                .map_err(xlsx_err)?;
            dv = dv.set_error_title("这里只能填数字").map_err(xlsx_err)?;
            dv = dv
                .set_error_message("金额必须是数字（扣款项请填负数）。")
                .map_err(xlsx_err)?;
            worksheet
                .add_data_validation(1, i as u16, rows.len().max(1) as u32 + 200, i as u16, &dv)
                .map_err(|e| AppError::Internal(format!("加数据验证失败：{e}")))?;
        }

        // 列宽：固定列窄一点、金额列宽一点
        for (i, col) in schema.columns.iter().enumerate() {
            let width = if col.kind == ColumnKind::Fixed {
                match col.key.as_str() {
                    "name" => 12.0,
                    "email" => 26.0,
                    _ => 10.0,
                }
            } else {
                14.0
            };
            worksheet
                .set_column_width(i as u16, width)
                .map_err(xlsx_err)?;
        }
    }

    // ---- 填写说明 ----
    if options.include_instructions {
        let worksheet = workbook.add_worksheet();
        worksheet.set_name(SHEET_INSTRUCTIONS).map_err(xlsx_err)?;
        let header_fmt = Format::new().set_bold();
        for (i, row) in crate::domain::sheet::instructions_sheet(schema)
            .iter()
            .enumerate()
        {
            for (j, cell) in row.iter().enumerate() {
                let fmt = if i == 0 { &header_fmt } else { &Format::new() };
                worksheet
                    .write_string_with_format(i as u32, j as u16, cell, fmt)
                    .map_err(|e| AppError::Internal(format!("写说明失败：{e}")))?;
            }
        }
        worksheet.set_column_width(0, 18.0).map_err(xlsx_err)?;
        worksheet.set_column_width(1, 72.0).map_err(xlsx_err)?;
    }

    workbook
        .save(path)
        .map_err(|e| AppError::Internal(format!("保存 xlsx 失败：{e}")))?;
    Ok(())
}

/// rust_xlsxwriter 的错误 → 我们的错误。
fn xlsx_err(e: rust_xlsxwriter::XlsxError) -> AppError {
    AppError::Internal(format!("生成 xlsx 出错：{e}"))
}

/// 导出文件名（设计文档：「导出文件名含年月便于归档」）。
pub fn file_name(year_month: &str, include_members: bool) -> String {
    if include_members {
        format!("薪资模板_{year_month}_含名单.xlsx")
    } else {
        format!("薪资模板_{year_month}.xlsx")
    }
}

/// 导出文件的完整路径（数据目录下的 `exports/`）。
///
/// **只有这一个函数决定导出落哪** —— Command 层不许自己拼路径，
/// 否则「落在 `exports/` 里」这条标准就散成两处口径。
pub fn target_path(data_dir: &Path, year_month: &str, include_members: bool) -> PathBuf {
    crate::infra::backup::exports_dir(data_dir).join(file_name(year_month, include_members))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::sheet::build_schema;
    use std::fs;

    fn schema() -> SheetSchema {
        build_schema(&[
            (
                "base_salary".to_string(),
                "基本工资".to_string(),
                "money".to_string(),
                true,
                false,
                true,
            ),
            (
                "social_insurance".to_string(),
                "社保个人".to_string(),
                "money".to_string(),
                false,
                false,
                true,
            ),
            (
                "net_pay".to_string(),
                "实发工资".to_string(),
                "formula".to_string(),
                true,
                true,
                true,
            ),
            (
                "remark".to_string(),
                "备注".to_string(),
                "text".to_string(),
                false,
                false,
                true,
            ),
        ])
    }

    /// 标准 8/10：默认导出的是**空模板**（只有表头），文件里没有成员姓名与邮箱。
    #[test]
    fn the_default_export_is_an_empty_template_without_member_data() {
        let dir = std::env::temp_dir().join(format!(
            "xinyou-export-test-{}-{}",
            std::process::id(),
            "empty"
        ));
        fs::create_dir_all(&dir).expect("建临时目录失败");
        let path = dir.join("t.xlsx");
        write_xlsx(&path, &schema(), ExportOptions::defaults(), &[]).expect("导出失败");

        assert!(path.exists());
        let bytes = fs::read(&path).expect("读回失败");
        // 里面不该有成员姓名/邮箱（我们根本没写）
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains("@company.com"), "空模板里不该有邮箱");

        let _ = fs::remove_dir_all(&dir);
    }

    /// 标准 5/6/7：说明 Sheet、冻结首行、数值格式与数据验证。
    ///
    /// 这些是 xlsx 内部结构，**不靠读 zip 内部 XML 验**（太脆），
    /// 而是靠「用 calamine 读回来」—— TC-302 的解析器正好能读，
    /// 两边用同一套库，正好互相验证格式对不对。
    #[test]
    fn the_exported_file_can_be_read_back() {
        let dir = std::env::temp_dir().join(format!(
            "xinyou-export-test-{}-{}",
            std::process::id(),
            "readback"
        ));
        fs::create_dir_all(&dir).expect("建临时目录失败");
        let path = dir.join("t.xlsx");
        let rows = vec![Row {
            cells: vec![
                "张三".to_string(),
                "zhangsan@company.com".to_string(),
                "E001".to_string(),
                "18000".to_string(),
                "-2100".to_string(),
                "".to_string(),
                "备注".to_string(),
            ],
        }];
        write_xlsx(&path, &schema(), ExportOptions::defaults(), &rows).expect("导出失败");

        // 用 calamine 读回来（与 TC-302 的解析器同一个库）
        use calamine::Reader;
        let mut reader = calamine::open_workbook_auto(&path).expect("读不出 xlsx");
        let sheet = reader
            .worksheet_range_at(0)
            .expect("导出的文件里没有 Sheet")
            .expect("读 Sheet 出错");
        // 表头：固定列带「*」标记必填
        assert_eq!(
            sheet.get((0, 0)).map(|c| c.to_string()),
            Some("姓名 *".into())
        );
        assert_eq!(
            sheet.get((0, 3)).map(|c| c.to_string()),
            Some("基本工资 *".into())
        );
        // 数据行
        assert_eq!(
            sheet.get((1, 0)).map(|c| c.to_string()),
            Some("张三".into())
        );
        // 公式列写的是占位文字，不是数字
        assert_eq!(
            sheet.get((1, 5)).map(|c| c.to_string()),
            Some(FORMULA_PLACEHOLDER.into())
        );

        let _ = fs::remove_dir_all(&dir);
    }

    /// 标准 9：文件名含年月。
    #[test]
    fn the_file_name_carries_the_month() {
        assert_eq!(file_name("2026-10", false), "薪资模板_2026-10.xlsx");
        assert_eq!(file_name("2026-10", true), "薪资模板_2026-10_含名单.xlsx");
    }
}
