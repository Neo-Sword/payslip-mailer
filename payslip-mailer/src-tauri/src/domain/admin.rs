//! 管理员口令策略与邮箱格式门槛（TC-102 口令 · TC-103 邮箱 · 设计文档 ch.6 S01）
//!
//! 纯函数：不查库、不哈希、不碰系统时间。策略改动只可能出现在这个文件里。
//! 前端的实时校验（S01 表单）与后端的兜底校验（`create_admin`）用同一组函数，
//! 同一条规则不会在前端和后端各写一份、各漂移各的。

use crate::error::{AppError, AppResult};

/// 最少位数。设计文档写的是「至少 10 位」。
pub const MIN_PASSWORD_CHARS: usize = 10;

/// 校验口令是否够长。
///
/// **按字符数，不按字节数**（spec 标准 2）。设计文档的「位」是使用者眼里的「位」：
/// 一个汉字算一位。按字节算的话，「工资条2026年11月发」这种 11 个汉字的口令
/// （33 字节，比 10 个 ASCII 强得多）会被拒掉，而报错文案还写着「至少 10 位」，
/// 使用者根本不知道被拒的原因。
///
/// 不做 trim：口令两端的空格是使用者故意输的还是有意的，程序猜不了。
/// 首尾空格算进长度，也照样参与哈希。
pub fn check_password_policy(password: &str) -> AppResult<()> {
    let chars = password.chars().count();
    if chars < MIN_PASSWORD_CHARS {
        // 错误里只带**计数**，绝不带口令本身（红线 2 的同族，spec 标准 5）。
        return Err(AppError::ValidationError(format!(
            "管理员口令至少 {MIN_PASSWORD_CHARS} 位，当前 {chars} 位"
        )));
    }
    Ok(())
}

/// 管理员邮箱的最低格式门槛（TC-103 · ADR-0012 第 3 条）。
///
/// 只拦「明显不可能是邮箱」的输入：没有 `@`、`@` 两边有一边是空的、中间夹着空白。
/// **不是** RFC 5322 全量校验——那种校验是假精确，真正能不能收信只有真发一封才知道，
/// 那是 TC-500「测试连接」的事。错误码走 `VALIDATION_ERROR`：
/// `INVALID_EMAIL` 是 TC-201 给成员批量粘贴留的（两个场景语义不同，别混用）。
///
/// 前端的实时校验与后端的兜底用同一个函数（spec 标准 4 的「底线」）：
/// 界面会在提交前 trim 并拦住大多数坏输入，但绕过界面直接喂 Command 也必须被拒。
/// 这里同样**不做 trim**：后端看到的字符串是什么就判什么，trim 是界面的职责。
fn invalid_email(why: &str) -> AppError {
    AppError::ValidationError(format!("邮箱格式不对：{why}。要形如 name@company.com"))
}

pub fn check_email_format(email: &str) -> AppResult<()> {
    if email.chars().any(char::is_whitespace) {
        return Err(invalid_email("不能含空格或换行"));
    }
    let mut parts = email.split('@');
    let (local, domain) = match (parts.next(), parts.next(), parts.next()) {
        (Some(l), Some(d), None) => (l, d),
        // 没有 @ 或有两个以上 @，都落在这里
        _ => return Err(invalid_email("必须有且只有一个 @")),
    };
    if local.is_empty() {
        return Err(invalid_email("@ 前面不能是空的"));
    }
    if domain.is_empty() {
        return Err(invalid_email("@ 后面不能是空的"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验收标准 2：策略按字符数，不按字节数。
    #[test]
    fn policy_counts_characters_not_bytes() {
        // ASCII：字节数 == 字符数，两个口径都给同一个答案
        assert!(
            check_password_policy("123456789").is_err(),
            "9 个 ASCII 应被拒"
        );
        check_password_policy("1234567890").expect("10 个 ASCII 应通过");

        // 汉字：字节数是字符数的三倍，按字节算就会误拒
        let han_10 = "工资条群发应用真好用"; // 10 个汉字 = 30 字节
        let han_9 = "工资条群发应用真好"; // 9 个汉字 = 27 字节
        assert_eq!(han_10.chars().count(), 10, "测试自己得先数对");
        assert_eq!(han_10.len(), 30, "测试自己得先数对");
        check_password_policy(han_10).expect("10 个汉字（30 字节）应通过");
        assert!(check_password_policy(han_9).is_err(), "9 个汉字应被拒");

        // 边界：正好 10 位通过，少一位就拒
        assert_eq!(MIN_PASSWORD_CHARS, 10);
    }

    /// 错误文案要说清规则，但不能回显口令（spec 标准 5 同族）。
    #[test]
    fn error_names_the_rule_but_never_the_password() {
        // 8 个汉字：够短被拒，又不是 ASCII，顺带确认「当前几位」报的是字符数而不是字节数
        // （按字节报的话这里会是 24 位，正是 spec 标准 2 要防的误拒）。
        let secret = "欧阳傲天欧阳傲天";
        assert_eq!(secret.chars().count(), 8, "测试自己得先数对");
        let err = check_password_policy(secret).expect_err("8 位应被拒");
        let msg = err.user_message();
        assert!(msg.contains("至少 10 位"), "缺：规则 · {msg}");
        assert!(msg.contains("8 位"), "缺：当前长度 · {msg}");
        assert!(
            !msg.contains("24 位"),
            "报的是字节数而不是字符数，中文口令会被误拒 · {msg}"
        );
        assert!(!msg.contains(secret), "错误里回显了口令明文 · {msg}");
        assert!(
            !serde_json::to_string(&err).unwrap().contains(secret),
            "序列化后的 payload 里也不该有口令"
        );
    }

    /// 验收标准 4 的规则面：明显坏掉的输入都拒，普通地址放行。
    #[test]
    fn email_format_rejects_the_obviously_broken_and_passes_the_ordinary() {
        check_email_format("hr@company.com").expect("普通地址应通过");
        check_email_format("欧阳傲天@工资条.cn")
            .expect("非 ASCII 地址不在拦截范围（能不能收信归 TC-500）");
        assert!(check_email_format("hr").is_err(), "没有 @ 应被拒");
        assert!(check_email_format("@company.com").is_err(), "@ 前空应被拒");
        assert!(check_email_format("hr@").is_err(), "@ 后空应被拒");
        assert!(check_email_format("a@b@c").is_err(), "两个 @ 应被拒");
        assert!(
            check_email_format("hr @company.com").is_err(),
            "夹空格应被拒"
        );
        assert!(
            check_email_format("hr@company.com\n").is_err(),
            "尾部换行应被拒"
        );
    }

    /// 错误文案说规则、不回显输入：绕过界面喂进来的字符串是任意的，
    /// 回显它等于把注入内容放进界面文案（与口令策略同一口径）。
    #[test]
    fn email_error_names_the_rule_not_the_input() {
        let err = check_email_format("not-an-email").expect_err("应被拒");
        let msg = err.user_message();
        assert!(msg.contains("@"), "缺：规则 · {msg}");
        assert!(!msg.contains("not-an-email"), "错误里回显了输入 · {msg}");
        assert!(
            serde_json::to_string(&err)
                .unwrap()
                .starts_with("{\"code\":\"VALIDATION_ERROR\""),
            "code 必须是 VALIDATION_ERROR"
        );
    }
}
