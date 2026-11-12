//! 成员的纯逻辑：邮箱格式校验 + 批量粘贴的解析与去重（TC-201 · ADR-0016）
//!
//! **批量粘贴是这一卡最容易出错的地方**，所以解析全部放在这一层（纯函数，好测）：
//!
//! - 格式判定：含制表符 → TSV，含逗号 → CSV（ADR-0016 决策 6）
//! - 表头识别：第一行的第二列**不像邮箱** → 当表头跳过
//!   （HR 从 Excel 连表头一起复制过来是常态）
//! - 去重：同一邮箱出现多次 → **合并成一个人**（ADR-0016 洞二：
//!   批量粘贴是便利功能，整批因为两行重复就进不去是很糟的体验）
//! - 坏行：格式非法的行**整行跳过并报错**，错误里只说**第几行、哪个字段**，
//!   **不回显整行内容**（红线 2：名单是个人数据）
//!
//! 另外提供**可发送名单**的纯逻辑：停用的人不进发送范围（设计文档 S05 原话）。

use std::collections::BTreeMap;

use crate::error::{AppError, AppResult};

/// 一个待写入的成员（已经过格式校验与去重）。
///
/// 它是 Command 的入参（前端把预览确认过的那几项传回来），
/// 所以要能序列化与反序列化。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberDraft {
    pub name: String,
    pub email: String,
}

/// 粘贴结果里**一行**的问题。
///
/// 只带行号与字段名，**不带那一行的内容** —— 报错回显整行等于把别人的
/// 姓名与邮箱写进了错误信息（红线 2）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RowProblem {
    /// 第几行（从 1 开始，按**用户看到的那一行**算，含被跳过的表头）。
    pub line: usize,
    /// 哪个字段不对：`邮箱` / `姓名` / `分隔符`。
    pub field: String,
    /// 一句人话。
    pub message: String,
}

/// 批量粘贴的预览结果。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PastePreview {
    /// 通过校验、会写进去的成员（已按邮箱去重）。
    pub accepted: Vec<MemberDraft>,
    /// 被跳过的行及其原因。
    pub problems: Vec<RowProblem>,
    /// 因邮箱重复被合并掉的行数。
    pub deduped: usize,
    /// 是不是把第一行当表头跳过了。
    pub skipped_header: bool,
    /// 用的哪种分隔符。
    pub delimiter: String,
}

/// 邮箱格式校验。
///
/// 门槛刻意不高：能挡住「明显不是邮箱」的东西（没有 @、@ 前后为空、含空白），
/// 但**不追求 RFC 5322 全量合规** —— 真正的可送达性只有真发一封才知道，
/// 那是 TC-500「测试连接」的事（与 `domain::admin::check_email_format` 同一口径）。
///
/// 失败用 `INVALID_EMAIL` —— TC-102 的文件头早就把这个码划给
/// 「TC-201 成员邮箱批量粘贴」了，本卡是它的正主。
pub fn check_email(email: &str) -> AppResult<()> {
    let trimmed = email.trim();
    if trimmed.is_empty() {
        return Err(AppError::InvalidEmail("邮箱不能为空".into()));
    }
    if trimmed.chars().any(char::is_whitespace) {
        return Err(AppError::InvalidEmail("邮箱里有空格".into()));
    }
    let mut parts = trimmed.split('@');
    let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err(AppError::InvalidEmail("邮箱必须恰好有一个 @".into()));
    };
    if local.is_empty() {
        return Err(AppError::InvalidEmail("@ 前面是空的".into()));
    }
    if domain.is_empty() {
        return Err(AppError::InvalidEmail("@ 后面是空的".into()));
    }
    if !domain.contains('.') {
        return Err(AppError::InvalidEmail("域名部分没有点".into()));
    }
    Ok(())
}

/// 判断一个字符串「像不像邮箱」—— 只用于**表头识别**，不用于最终校验。
///
/// 刻意比 [`check_email`] 宽松：表头那一列可能是「邮箱」「email」「E-mail」，
/// 也可能被 Excel 改成「电子邮箱」。太严就会把真数据当表头跳过。
fn looks_like_email(text: &str) -> bool {
    let t = text.trim();
    let mut parts = t.split('@');
    let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    !local.is_empty() && !domain.is_empty() && domain.contains('.')
}

/// 判定分隔符：含制表符 → TSV；否则含逗号 → CSV；都没有 → 单列（报错）。
fn detect_delimiter(text: &str) -> Option<char> {
    if text.contains('\t') {
        Some('\t')
    } else if text.contains(',') {
        Some(',')
    } else {
        None
    }
}

/// 解析一段批量粘贴的文本，给出预览（**不写库**）。
///
/// 设计文档 S05：「『批量粘贴』接受 TSV/CSV 两列（姓名 邮箱）并预览去重结果」。
/// 所以这个函数只解析与预览，**写不写由界面决定**（用户看着预览点确认）。
///
/// `existing_emails` 是**库里已有**的邮箱：粘进来的邮箱若已在库里，
/// 那不是「重复行」而是「这个人已经在名单里了」—— 单独报出来，
/// 不与「同一批里重复」混为一谈（两者的处理不一样）。
pub fn preview_paste(text: &str, existing_emails: &[String]) -> PastePreview {
    let mut problems: Vec<RowProblem> = Vec::new();
    let mut accepted: Vec<MemberDraft> = Vec::new();
    // 邮箱（小写） → 第一次出现的行号。用来判重复。
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut deduped = 0usize;
    let mut skipped_header = false;

    let Some(delimiter) = detect_delimiter(text) else {
        problems.push(RowProblem {
            line: 1,
            field: "分隔符".into(),
            message: "看不出是表格还是列表：制表符或逗号分隔的两列（姓名、邮箱）才行".into(),
        });
        return PastePreview {
            accepted,
            problems,
            deduped,
            skipped_header,
            delimiter: String::new(),
        };
    };

    let delimiter_name = if delimiter == '\t' {
        "制表符"
    } else {
        "逗号"
    };
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();

    for (idx, raw) in lines.iter().enumerate() {
        // 行号按**用户看到的那一行**算（含空行），从 1 开始
        let line_no = text
            .lines()
            .enumerate()
            .filter(|(_, l)| !l.trim().is_empty())
            .nth(idx)
            .map(|(i, _)| i + 1)
            .unwrap_or(idx + 1);

        let cols: Vec<&str> = raw.split(delimiter).map(|s| s.trim()).collect();
        if cols.len() < 2 {
            problems.push(RowProblem {
                line: line_no,
                field: "列数".into(),
                message: format!("这一行只有一列；要两列（姓名、邮箱），用{delimiter_name}分隔"),
            });
            continue;
        }
        let name = cols[0];
        let email = cols[1];

        // 表头识别：第一行的第二列不像邮箱 → 当表头
        if idx == 0 && !looks_like_email(email) {
            skipped_header = true;
            continue;
        }

        if name.is_empty() {
            problems.push(RowProblem {
                line: line_no,
                field: "姓名".into(),
                message: "姓名是空的".into(),
            });
            continue;
        }
        if let Err(e) = check_email(email) {
            problems.push(RowProblem {
                line: line_no,
                field: "邮箱".into(),
                message: e.user_message(),
            });
            continue;
        }

        let key = email.to_lowercase();
        if seen.contains_key(&key) {
            // 同一批里的重复 → 自动去重（合并成一个人）
            deduped += 1;
            continue;
        }

        if existing_emails.iter().any(|e| e.to_lowercase() == key) {
            // 库里已经有人用这个邮箱了 —— 这不是「重复行」，是「这个人已经在名单里」
            problems.push(RowProblem {
                line: line_no,
                field: "邮箱".into(),
                message: "这个邮箱已经在名单里了".into(),
            });
            continue;
        }

        seen.insert(key, line_no);
        accepted.push(MemberDraft {
            name: name.to_string(),
            email: email.to_string(),
        });
    }

    PastePreview {
        accepted,
        problems,
        deduped,
        skipped_header,
        delimiter: delimiter_name.to_string(),
    }
}

/// 停用的人不进发送范围。
///
/// 设计文档 S05 原话：「组内成员可重复归属；**停用成员不进入发送范围**」。
/// 发送侧（TC-401/402）拿到的必须是这个筛过的名单。
pub fn is_sendable(active: bool) -> bool {
    active
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_format_is_checked() {
        check_email("zhangsan@company.com").expect("应通过");
        check_email("hr@a.co").expect("应通过");
        assert!(check_email("").is_err(), "空");
        assert!(check_email("hr@").is_err(), "@ 后面空");
        assert!(check_email("@company.com").is_err(), "@ 前面空");
        assert!(check_email("a@b@c").is_err(), "两个 @");
        assert!(check_email("a b@c.com").is_err(), "含空格");
        assert!(check_email("no-at-sign").is_err(), "没有 @");
    }

    /// 标准 3：TSV 与 CSV 都能解析。
    #[test]
    fn tsv_and_csv_are_both_parsed() {
        let tsv = "张三\tzhangsan@company.com\n李四\tlisi@company.com";
        let p = preview_paste(tsv, &[]);
        assert_eq!(p.accepted.len(), 2, "{p:?}");
        assert!(p.problems.is_empty());
        assert_eq!(p.delimiter, "制表符");

        let csv = "张三,zhangsan@company.com\n李四,lisi@company.com";
        let p = preview_paste(csv, &[]);
        assert_eq!(p.accepted.len(), 2, "{p:?}");
        assert_eq!(p.delimiter, "逗号");
    }

    /// 标准 4：同邮箱自动去重。
    #[test]
    fn duplicate_emails_are_deduped_not_rejected() {
        let text =
            "张三\tzhangsan@company.com\n李四\tzhangsan@company.com\n王五\twangwu@company.com";
        let p = preview_paste(text, &[]);
        assert_eq!(p.accepted.len(), 2, "去重后应是两个人：{p:?}");
        assert_eq!(p.deduped, 1, "应报告去掉了 1 条");
        assert!(p.problems.is_empty(), "重复不该报错：{p:?}");
    }

    /// 标准 5：坏行整行跳过并指出行号与字段，且**不回显内容**。
    #[test]
    fn a_bad_row_is_skipped_with_its_line_number_and_no_content() {
        let text = "张三\tzhangsan@company.com\n李四\tlisi@\n王五\twangwu@company.com";
        let p = preview_paste(text, &[]);
        assert_eq!(p.accepted.len(), 2, "另外两行应正常进：{p:?}");
        assert_eq!(p.problems.len(), 1, "{p:?}");
        assert_eq!(p.problems[0].line, 2, "要指出第 2 行：{p:?}");
        assert_eq!(p.problems[0].field, "邮箱");
        // 错误里不能出现那一行的内容
        assert!(
            !p.problems[0].message.contains("李四"),
            "错误里回显了姓名：{}",
            p.problems[0].message
        );
        assert!(
            !p.problems[0].message.contains("lisi@"),
            "错误里回显了邮箱：{}",
            p.problems[0].message
        );
    }

    /// 标准 6：表头自动跳过。
    #[test]
    fn a_header_row_is_skipped() {
        let text = "姓名\t邮箱\n张三\tzhangsan@company.com";
        let p = preview_paste(text, &[]);
        assert!(p.skipped_header, "第一行应被当表头：{p:?}");
        assert_eq!(p.accepted.len(), 1, "{p:?}");
        assert_eq!(p.accepted[0].name, "张三");
    }

    /// 标准 6 的边界：表头那一列写成「email」也要认出来。
    #[test]
    fn english_headers_are_recognised_too() {
        let text = "name\temail\n张三\tzhangsan@company.com";
        let p = preview_paste(text, &[]);
        assert!(p.skipped_header, "{p:?}");
        assert_eq!(p.accepted.len(), 1);
    }

    /// 库里已有的邮箱要单独报（不是「重复行」）。
    #[test]
    fn an_email_already_in_the_roster_is_reported_separately() {
        let text = "张三\tzhangsan@company.com";
        let existing = vec!["zhangsan@company.com".to_string()];
        let p = preview_paste(text, &existing);
        assert!(p.accepted.is_empty(), "已在库里的人不该重复写：{p:?}");
        assert_eq!(p.problems.len(), 1);
        assert!(p.problems[0].message.contains("已经在名单里"));
    }

    /// 分隔符都看不出来时要报错，而不是把整行当姓名。
    #[test]
    fn unparseable_text_is_reported() {
        let p = preview_paste("张三 zhangsan@company.com", &[]);
        assert!(p.accepted.is_empty());
        assert_eq!(p.problems.len(), 1);
        assert_eq!(p.problems[0].field, "分隔符");
    }

    /// 邮箱大小写不同算同一个（去重按小写比较）。
    #[test]
    fn email_case_does_not_create_two_members() {
        let text = "张三\tZhangSan@company.com\n李四\tzhangsan@company.com";
        let p = preview_paste(text, &[]);
        assert_eq!(p.accepted.len(), 1, "{p:?}");
        assert_eq!(p.deduped, 1);
    }
}
