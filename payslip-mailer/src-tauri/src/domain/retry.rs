//! 发送结果的分类与退避表（TC-400 · ADR-0023 决策 3、4）
//!
//! 设计文档 ch.7 的错误处置表是发送引擎的判断中枢：
//!
//! | 类别 | 典型码 | 动作 |
//! | --- | --- | --- |
//! | 临时失败 | 421 / 451 / 452 / 4xx 限流 | 退避重试，自动降速 50% |
//! | 永久失败 | 550 / 553 / 收件人不存在 | 标记 dead，不重试 |
//! | 认证失败 | 535 / 534 | 立即中止整批，不重发 |
//! | 网络中断 | 连接超时 / DNS | 暂停队列，保留现场，可「继续」 |
//!
//! 分类是一个**纯函数** —— TC-500 接 lettre 时只需把驱动错误映射成
//! [`SendResult`]，处置逻辑一行不改。

/// 一次发送尝试的结果（发送器吐出来的，SMTP 码在里面的才算到了协议层）。
// 未接线：TC-402 发送编排 / TC-500 真 SMTP 接上时消费（TC-400 只钉住策略与测试）。
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendResult {
    /// 2xx：服务器收下了。
    Accepted,
    /// 服务器给了个 SMTP 错误码（4xx / 5xx，理论上 3xx 也会走到）。
    Rejected(u16),
    /// 连接层失败（超时 / DNS / TLS）—— 没有 SMTP 码。
    Network,
}

/// 处置：这一封接下来怎么办。
// 未接线：TC-402 发送编排 / TC-500 真 SMTP 接上时消费（TC-400 只钉住策略与测试）。
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// 发成功了。
    Success,
    /// 退避重试（临时失败）。
    Retry,
    /// 标记 dead，不重试（永久失败）。
    Dead,
    /// 暂停批次保留现场（网络中断），用户可「继续」。
    Pause,
    /// 立即中止整批（认证失败）。
    Abort,
}

/// 分类（ADR-0023 决策 3）。
///
/// 未知码的归法：**4xx → Retry、5xx → Dead**。
/// 4xx 的本义是「稍后再试」（服务器忙）；5xx 是服务器**拒绝**，
/// 拒绝的原因多半不会自己好 —— 保守判死，不浪费重试机会。
/// 3xx 是 SMTP 会话的中间态（继续输入），客户端不该把它交给应用层；
/// 真收到了归 `Retry`（保守，不死也不当成功）。
#[allow(dead_code)]
pub fn classify(result: SendResult) -> Disposition {
    match result {
        SendResult::Accepted => Disposition::Success,
        SendResult::Network => Disposition::Pause,
        SendResult::Rejected(code) => match code {
            // 认证失败：凭据坏了，重试只会累计失败计数触发风控
            534 | 535 => Disposition::Abort,
            // 永久失败（ch.7 点名的 550/553 + **全部未知 5xx** —— 服务器拒绝
            // 的原因多半不会自己好，保守判死，不浪费重试机会）
            500..=599 => Disposition::Dead,
            // 临时失败（421/451/452 是 ch.7 点名的；其余 4xx 同类）
            400..=499 => Disposition::Retry,
            // 中间态保守重试
            _ => Disposition::Retry,
        },
    }
}

/// 最大重试次数（ch.7：「3 —— 最大重试次数」）。
///
/// 注意语义：**失败 3 次后判死**。第 1 次发送 + 3 次重试 = 最多 4 次尝试。
// 未接线：record_result 消费它（TC-402 编排接线）。
#[allow(dead_code)]
pub const MAX_RETRIES: u32 = 3;

/// 退避表：第 n 次失败后等多少毫秒（ch.7：「指数退避 30s / 2min / 10min」）。
// 未接线：backoff_after 消费它（TC-402 编排接线）。
#[allow(dead_code)]
pub const BACKOFF_MS: [i64; MAX_RETRIES as usize] = [30_000, 120_000, 600_000];

/// 第 `failed_attempts` 次失败后的退避毫秒数。
///
/// 超出表长（不该发生，次数上限在前）按最后一档。
#[allow(dead_code)]
pub fn backoff_after(failed_attempts: u32) -> i64 {
    let idx = (failed_attempts.max(1) - 1) as usize;
    BACKOFF_MS[idx.min(BACKOFF_MS.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 标准 3：错误分类全表。
    #[test]
    fn the_classification_table_is_complete() {
        // 认证失败 → 中止整批
        assert_eq!(classify(SendResult::Rejected(535)), Disposition::Abort);
        assert_eq!(classify(SendResult::Rejected(534)), Disposition::Abort);
        // 永久失败 → 判死
        assert_eq!(classify(SendResult::Rejected(550)), Disposition::Dead);
        assert_eq!(classify(SendResult::Rejected(553)), Disposition::Dead);
        // 临时失败 → 重试
        assert_eq!(classify(SendResult::Rejected(421)), Disposition::Retry);
        assert_eq!(classify(SendResult::Rejected(451)), Disposition::Retry);
        assert_eq!(classify(SendResult::Rejected(452)), Disposition::Retry);
        // 未知 5xx → Dead（保守）；未知 4xx → Retry（保守）
        assert_eq!(classify(SendResult::Rejected(531)), Disposition::Dead);
        assert_eq!(classify(SendResult::Rejected(441)), Disposition::Retry);
        // 网络中断 → 暂停
        assert_eq!(classify(SendResult::Network), Disposition::Pause);
        // 成功
        assert_eq!(classify(SendResult::Accepted), Disposition::Success);
    }

    /// 标准 4：退避表 —— 30s / 2min / 10min，第 3 次失败后判死。
    #[test]
    fn the_backoff_table_matches_the_design_doc() {
        assert_eq!(backoff_after(1), 30_000);
        assert_eq!(backoff_after(2), 120_000);
        assert_eq!(backoff_after(3), 600_000);
        assert_eq!(MAX_RETRIES, 3);
    }
}
