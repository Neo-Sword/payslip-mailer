//! 令牌桶限速（TC-400 · ADR-0023 决策 1、2）
//!
//! 设计文档 ch.7：「20–30 封 / 分钟（默认 30）—— 飞书邮箱保守值，可在设置页调整」。
//!
//! **只判定，不等待**：[`TokenBucket::try_acquire`] 返回「现在能不能发 +
//! 下一封要等多久」，**谁 sleep、睡多久由调用方决定**（编排器睡，UI 永不阻塞）。
//! 时钟由调用方注入 —— 测试用假时钟完整验证节奏，一行 `sleep` 都不用真等。

/// 每分钟多少封（`app_meta.rate_per_min` 的默认值，ch.7）。
// 未接线：commands/batch.rs 已消费；TC-500 的设置页写入它。
#[allow(dead_code)]
pub const DEFAULT_RATE_PER_MIN: u32 = 30;

/// 判定结果。
// 未接线：TC-402 发送编排 / TC-500 真 SMTP 接上时消费（TC-400 只钉住策略与测试）。
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TryAcquire {
    /// 现在能不能发。
    pub allowed: bool,
    /// 不能发的话，下一封要等多少毫秒。
    pub retry_after_ms: i64,
}

/// 令牌桶（容量 1 —— 匀速出信，不允许突发）。
///
/// 容量 1 的理由：工资条不是营销邮件，**匀速**比「攒一波突发」更不像垃圾行为；
/// 飞书的风控盯的是突刺。
// 未接线：TC-402 发送编排 / TC-500 真 SMTP 接上时消费（TC-400 只钉住策略与测试）。
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TokenBucket {
    /// 速率：每分钟几封。
    rate_per_min: u32,
    /// 上次补充的时刻（注入的毫秒时钟）。
    last: i64,
    /// 当前令牌数（0..=1）。
    tokens: f64,
    /// 本桶已经降过速吗？（**批内只降一次** —— ADR-0023 决策 2 的「不叠加」：
    /// 连续临时失败不再减，减到 0 就永远发不完了。）
    halved: bool,
}

#[allow(dead_code)]
impl TokenBucket {
    /// 造一个满桶。
    pub fn new(rate_per_min: u32, now_ms: i64) -> Self {
        Self {
            rate_per_min: rate_per_min.max(1),
            last: now_ms,
            tokens: 1.0,
            halved: false,
        }
    }

    /// 当前速率。
    pub fn rate_per_min(&self) -> u32 {
        self.rate_per_min
    }

    /// **降速 50%**（ADR-0023 决策 2：批次级、不叠加、不写回配置）。
    ///
    /// 「不叠加」由这里保证：已经是半速（或更慢）就不再减 ——
    /// 减到 0 就永远发不完了。
    pub fn halve_rate(&mut self) {
        if self.halved {
            return; // 已经降过 —— 不叠加
        }
        self.rate_per_min = self.rate_per_min.div_ceil(2).max(1);
        self.halved = true;
    }

    /// 现在能不能发一封？
    ///
    /// `now_ms` 是**注入的毫秒时钟**（测试假时钟 / 生产真时钟）。
    pub fn try_acquire(&mut self, now_ms: i64) -> TryAcquire {
        let elapsed = (now_ms - self.last).max(0);
        let per_token_ms = 60_000.0 / f64::from(self.rate_per_min);
        // 补充令牌：经过的时间按速率换算成令牌，封顶 1
        self.tokens = (self.tokens + elapsed as f64 / per_token_ms).min(1.0);
        self.last = now_ms;

        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            TryAcquire {
                allowed: true,
                retry_after_ms: 0,
            }
        } else {
            TryAcquire {
                allowed: false,
                retry_after_ms: ((1.0 - self.tokens) * per_token_ms).ceil() as i64,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 标准 1：按速率放行 —— 第 1 封立即、第 2 封要等一个间隔。
    #[test]
    fn the_bucket_paces_at_the_configured_rate() {
        let mut bucket = TokenBucket::new(30, 0); // 30 封/分 = 2000ms 一封
        let first = bucket.try_acquire(0);
        assert!(first.allowed, "第 1 封该立即放行");

        let second = bucket.try_acquire(1); // 紧接着再来
        assert!(!second.allowed, "第 2 封该被压住");
        assert!(
            (second.retry_after_ms - 1999).abs() <= 1,
            "retry_after 应 ≈ 2000ms：{}",
            second.retry_after_ms
        );

        let third = bucket.try_acquire(2000); // 时钟前进一个间隔
        assert!(third.allowed, "等满间隔后该放行");
    }

    /// 标准 2：速率从设置读出来 —— 改成 20 就按 20 跑。
    #[test]
    fn the_rate_follows_the_setting() {
        let mut bucket = TokenBucket::new(20, 0); // 20 封/分 = 3000ms 一封
        assert!(bucket.try_acquire(0).allowed);
        let next = bucket.try_acquire(1);
        assert!(!next.allowed);
        assert!(
            (next.retry_after_ms - 2999).abs() <= 1,
            "{}",
            next.retry_after_ms
        );
    }

    /// 标准 9：降速 50% 且不叠加（批内只降一次）。
    #[test]
    fn halving_is_batch_level_and_never_stacks() {
        let mut bucket = TokenBucket::new(30, 0);
        bucket.halve_rate();
        assert_eq!(bucket.rate_per_min(), 15);
        bucket.halve_rate();
        bucket.halve_rate();
        assert_eq!(
            bucket.rate_per_min(),
            15,
            "连续临时失败不该一路减到 0 —— 批内只降一次"
        );

        // 奇数速率的半速向下取整但不低于 1
        let mut odd = TokenBucket::new(5, 0);
        odd.halve_rate();
        assert_eq!(odd.rate_per_min(), 3); // (5+1)/2 = 3
        // 极端小速率：1 封/分 → 降到 1（不会到 0）
        let mut tiny = TokenBucket::new(1, 0);
        tiny.halve_rate();
        assert_eq!(tiny.rate_per_min(), 1, "最低 1 封/分，不会到 0");
    }

    /// 长时间空闲后桶不会攒出突发（容量 1）。
    #[test]
    fn idle_time_never_accumulates_a_burst() {
        let mut bucket = TokenBucket::new(30, 0);
        // 空闲 10 分钟 —— 容量 1，回来也只能立刻发 1 封
        let back = bucket.try_acquire(600_000);
        assert!(back.allowed);
        let again = bucket.try_acquire(600_001);
        assert!(!again.allowed, "攒了很久也只能发 1 封（容量 1）");
        assert!((again.retry_after_ms - 1999).abs() <= 1);
    }
}
