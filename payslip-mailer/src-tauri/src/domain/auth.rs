//! 失败计数与 60 秒锁定：**纯状态机**（TC-102 · 设计文档 ch.4 795、ch.6 S02 1007-1026）
//!
//! 时钟是参数，不是 `SystemTime::now()`；库里那一行是参数，不是数据库连接。
//! 这样 spec 标准 6/7 那两条「错 5 次锁 60 秒、窗口过期后重新给 5 次」
//! 可以在一毫秒内跑完，不用 sleep，也不用真等到 60 秒后再点一次。
//! 落库与读库在 `infra::admins`，那里只做「读一行 → 调这里 → 写回一行」。

use serde::Serialize;

/// 连续失败几次上锁。设计文档 1023 写死 5。
pub const MAX_FAILED_ATTEMPTS: u32 = 5;

/// 锁多久（秒）。设计文档 1023 写死 60。
pub const LOCK_SECONDS: i64 = 60;

/// 判定要看的那两列。字段名与 `admins` 表一致，读一行就能填上。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttemptRecord {
    /// 当前窗口内已错次数
    pub failed_attempts: u32,
    /// 锁定到哪个时刻（epoch 秒）；`None` = 未锁定
    pub locked_until: Option<i64>,
}

impl AttemptRecord {
    /// 全新管理员的初始状态。
    ///
    /// 标 `cfg(test)` 而不是 `allow(dead_code)`：生产路径不引用它
    /// （`create_admin` 的 INSERT 直接写 `0, NULL`），只有测试拿它当起点。
    /// 用 allow 会把「哪天测试也不用它了」一起静默掉。
    #[cfg(test)]
    pub const CLEAN: Self = Self {
        failed_attempts: 0,
        locked_until: None,
    };
}

/// 一次解锁尝试的判定结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attempt {
    /// 口令正确，可以进工作台
    Unlocked,
    /// 口令错。`remaining` 是**还能再试几次**（0 = 下次就锁上）；
    /// `locked_until` 非 `None` 表示这一次正好把锁按下了（第 5 次）。
    ///
    /// 第 5 次失败仍然算 `Failed` 而不是 `Locked`：设计文档 1023 的文案是
    /// 「连续错误 5 次**将**锁定 60 秒」——第 5 次那一刻使用者看到的应该是
    /// 「口令错了，已经锁上」，而不是凭空多一次「锁定」提示（spec 标准 6）。
    Failed {
        remaining: u32,
        locked_until: Option<i64>,
    },
    /// 已经在锁定期内：本次**不判定口令**、不动计数、不延长锁定。
    Locked { seconds_left: u64 },
}

/// 判定要求写回的两列。两个字段都是「显式的新值」，`None` 表示写 NULL，
/// 不是「这一列不动」——所以 SQL 两条占位符都要给值，免得留下上一窗口的残值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Writeback {
    pub failed_attempts: u32,
    pub locked_until: Option<i64>,
}

/// 锁屏**进入时**要显示的状态（spec 标准 6 的「剩余次数与倒计时」）。
///
/// 单独有一个查询函数而不是只靠尝试的返回值，是因为锁定计数跨进程存活：
/// 应用重启后锁屏要显示「已锁定，还剩 37 秒」，那时没人尝试过口令，
/// 手上没有任何错误码可依据（ADR-0011 §6）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LockStatus {
    pub locked: bool,
    pub seconds_left: u64,
    pub remaining_attempts: u32,
}

/// 锁窗口现在是否还生效。返回 `Some(until)` 表示仍在锁定期内。
///
/// `now == until` 算**已过期**：60 秒整点就该能解锁（spec 标准 7 特意推到 60 秒整）。
fn active_window(record: &AttemptRecord, now: i64) -> Option<i64> {
    record.locked_until.filter(|until| *until > now)
}

/// 本次失败应当累加到的基数。
///
/// 锁窗口过期后基数归零：否则第 6 次失败会立刻再锁，「锁定 60 秒」实际变成
/// 「从此每次只剩 4 次」，与设计文档 1023 的「**连续**错误 5 次」不符（spec 标准 7）。
fn base_attempts(record: &AttemptRecord, now: i64) -> u32 {
    if record.locked_until.is_some_and(|until| until <= now) {
        0
    } else {
        record.failed_attempts
    }
}

/// 判定一次解锁尝试，并给出要写回库的内容（`None` = 什么都不用写）。
///
/// `password_ok` 只在**不在锁定期内**时才被看：锁定期内即使口令正确也不解锁，
/// 否则「锁定 60 秒」会退化成「锁定 60 秒，除非你恰好猜对」，
/// 而这 60 秒要挡的正是「不停试」这个动作本身。
pub fn decide(record: &AttemptRecord, password_ok: bool, now: i64) -> (Attempt, Option<Writeback>) {
    if let Some(until) = active_window(record, now) {
        return (
            Attempt::Locked {
                seconds_left: (until - now) as u64,
            },
            None,
        );
    }

    if password_ok {
        return (
            Attempt::Unlocked,
            Some(Writeback {
                failed_attempts: 0,
                locked_until: None,
            }),
        );
    }

    let failed = base_attempts(record, now).saturating_add(1);
    if failed >= MAX_FAILED_ATTEMPTS {
        (
            Attempt::Failed {
                remaining: 0,
                locked_until: Some(now + LOCK_SECONDS),
            },
            Some(Writeback {
                failed_attempts: failed,
                locked_until: Some(now + LOCK_SECONDS),
            }),
        )
    } else {
        (
            Attempt::Failed {
                remaining: MAX_FAILED_ATTEMPTS - failed,
                locked_until: None,
            },
            Some(Writeback {
                failed_attempts: failed,
                locked_until: None,
            }),
        )
    }
}

/// 当前锁定状态，供锁屏进入时显示。不判定口令，所以可以在任何时候调。
pub fn status(record: &AttemptRecord, now: i64) -> LockStatus {
    if let Some(until) = active_window(record, now) {
        return LockStatus {
            locked: true,
            seconds_left: (until - now) as u64,
            remaining_attempts: 0,
        };
    }
    LockStatus {
        locked: false,
        seconds_left: 0,
        remaining_attempts: MAX_FAILED_ATTEMPTS.saturating_sub(base_attempts(record, now)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 从判定结果里取出「写回后的两列」，锁定期内则原样返回上一状态。
    fn apply(record: &AttemptRecord, writeback: Option<Writeback>) -> AttemptRecord {
        match writeback {
            Some(w) => AttemptRecord {
                failed_attempts: w.failed_attempts,
                locked_until: w.locked_until,
            },
            None => *record,
        }
    }

    /// 验收标准 6：错 1..5 依次剩 4/3/2/1/0，第 6 次是 `Locked(60)`。
    #[test]
    fn five_consecutive_failures_lock_for_sixty_seconds() {
        let now = 1_780_000_000; // 任取的 epoch 秒，纯函数不关心它是哪天
        let mut record = AttemptRecord::CLEAN;
        let mut remaining_seen = Vec::new();

        for _ in 1..=5 {
            let (attempt, writeback) = decide(&record, false, now);
            match attempt {
                Attempt::Failed {
                    remaining,
                    locked_until,
                } => {
                    remaining_seen.push(remaining);
                    // 只有第 5 次才该顺手上锁
                    assert_eq!(
                        locked_until.is_some(),
                        remaining_seen.len() == 5,
                        "第 {} 次失败不该改动锁定时刻",
                        remaining_seen.len()
                    );
                }
                other => panic!(
                    "第 {} 次失败应判成 Failed，实际 {other:?}",
                    remaining_seen.len() + 1
                ),
            }
            record = apply(&record, writeback);
        }

        assert_eq!(
            remaining_seen,
            vec![4, 3, 2, 1, 0],
            "剩余次数必须逐个对上文案"
        );
        assert_eq!(record.failed_attempts, 5);
        assert_eq!(record.locked_until, Some(now + 60));

        // 第 6 次：锁定期内，口令对不对都不判
        let (attempt, writeback) = decide(&record, false, now + 1);
        assert_eq!(attempt, Attempt::Locked { seconds_left: 59 });
        assert!(writeback.is_none(), "锁定期内不该有任何写回");
    }

    /// 验收标准 6 的后半段：锁定期内的尝试**不延长**锁定、不动计数。
    #[test]
    fn attempts_inside_the_window_change_nothing() {
        let now = 1_780_000_000;
        let locked = AttemptRecord {
            failed_attempts: 5,
            locked_until: Some(now + 60),
        };

        let (first, _) = decide(&locked, false, now + 30);
        assert_eq!(first, Attempt::Locked { seconds_left: 30 });

        // 同一时刻再试一次（连点），倒计时与库里两列都不该变
        let (again, writeback) = decide(&locked, true, now + 30);
        assert_eq!(again, Attempt::Locked { seconds_left: 30 });
        assert!(writeback.is_none());
        assert_eq!(
            apply(&locked, writeback),
            locked,
            "锁定期内的尝试必须让两列保持原值"
        );

        // 锁定期内输对口令也不解锁：60 秒要挡的就是「不停试」
        assert_eq!(
            decide(&locked, true, now + 10).0,
            Attempt::Locked { seconds_left: 50 }
        );
    }

    /// 验收标准 7：窗口过期后首次尝试清零计数，给回 5 次预算。
    #[test]
    fn expired_window_gives_back_the_full_budget() {
        let now = 1_780_000_000;
        let locked = AttemptRecord {
            failed_attempts: 5,
            locked_until: Some(now + 60),
        };

        // 60 秒整：算已过期（不是还差一毫秒）
        let (attempt, writeback) = decide(&locked, true, now + 60);
        assert_eq!(attempt, Attempt::Unlocked);
        assert_eq!(
            writeback,
            Some(Writeback {
                failed_attempts: 0,
                locked_until: None,
            }),
            "计数与锁定时刻都必须清空"
        );

        // 过期后如果又输错，是从 0 重新数起，不是接着 5 立刻再锁
        let expired = AttemptRecord {
            failed_attempts: 5,
            locked_until: Some(now + 60),
        };
        let (attempt, writeback) = decide(&expired, false, now + 61);
        assert_eq!(
            attempt,
            Attempt::Failed {
                remaining: 4,
                locked_until: None
            },
            "过期后第一次失败应重新给 5 次预算"
        );
        assert_eq!(apply(&expired, writeback).failed_attempts, 1);
    }

    /// 锁屏进入时读的状态：与尝试判定用的是同一套规则，不会两处算出两个答案。
    #[test]
    fn status_matches_the_lockout_rules() {
        let now = 1_780_000_000;

        assert_eq!(
            status(&AttemptRecord::CLEAN, now),
            LockStatus {
                locked: false,
                seconds_left: 0,
                remaining_attempts: 5
            }
        );
        assert_eq!(
            status(
                &AttemptRecord {
                    failed_attempts: 3,
                    locked_until: None
                },
                now
            ),
            LockStatus {
                locked: false,
                seconds_left: 0,
                remaining_attempts: 2
            },
            "错 3 次剩 2 次 —— spec 标准 8 要的正是这个数跨进程存活"
        );
        assert_eq!(
            status(
                &AttemptRecord {
                    failed_attempts: 5,
                    locked_until: Some(now + 37)
                },
                now
            ),
            LockStatus {
                locked: true,
                seconds_left: 37,
                remaining_attempts: 0
            }
        );
        // 过期后状态回到「未锁定」，与 decide 的口径一致
        assert!(
            !status(
                &AttemptRecord {
                    failed_attempts: 5,
                    locked_until: Some(now)
                },
                now
            )
            .locked
        );
    }

    /// 计数不会因为异常值溢出（库里那一行被人手工改过也要能继续工作）。
    #[test]
    fn absurd_counts_do_not_panic() {
        let now = 1_780_000_000;
        let weird = AttemptRecord {
            failed_attempts: u32::MAX,
            locked_until: None,
        };
        let (attempt, _) = decide(&weird, false, now);
        assert_eq!(
            attempt,
            Attempt::Failed {
                remaining: 0,
                locked_until: Some(now + 60)
            },
            "计数爆表时该上锁而不是溢出成剩 4 次"
        );
        assert_eq!(status(&weird, now).remaining_attempts, 0);
    }
}
