//! 月份账套状态机（设计文档 ch.5 状态机 · TC-300 · ADR-0019）
//!
//! 关键约束：发送中 / 已发送状态下禁止编辑工资数值；
//! 如需纠错，必须走「解锁 → 生成修订记录 → 仅对未发送者补发」。
//!
//! **状态机是「一组要一起改的策略」**，所以判定放在这一层，不散进 SQL 或 Command 层。
//! 「ready 能不能编辑」这种问题也在这里回答一次（ADR-0019 洞三）：
//! 设计文档只禁 **sending / sent**（外加终态 archived），
//! **ready 仍然可编辑** —— 它是「校验通过、等待发送」，还没发出去。

use serde::{Deserialize, Serialize};

/// 月份账套的状态（设计文档 ch.5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeriodStatus {
    /// 草稿：可自由编辑
    Draft,
    /// 待发送：校验通过，等待发送。**仍可编辑**（还没发出去）。
    Ready,
    /// 发送中：禁止编辑
    Sending,
    /// 已发送：只读（要改得先解锁）
    Sent,
    /// 已归档：只读终态
    Archived,
}

impl PeriodStatus {
    /// 存进库里的字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Ready => "ready",
            Self::Sending => "sending",
            Self::Sent => "sent",
            Self::Archived => "archived",
        }
    }

    /// 从库里的字符串解析。认不出来 → `None`（库文件可能被手工改过）。
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "draft" => Some(Self::Draft),
            "ready" => Some(Self::Ready),
            "sending" => Some(Self::Sending),
            "sent" => Some(Self::Sent),
            "archived" => Some(Self::Archived),
            _ => None,
        }
    }

    /// 界面上显示的中文名。
    pub fn label(self) -> &'static str {
        match self {
            Self::Draft => "草稿",
            Self::Ready => "待发送",
            Self::Sending => "发送中",
            Self::Sent => "已发送",
            Self::Archived => "已归档",
        }
    }

    /// 是否允许编辑工资数值。
    ///
    /// **draft 与 ready 都允许**（ADR-0019 洞三）——
    /// 设计文档 ch.5 只禁「发送中/已发送」。
    pub fn is_editable(self) -> bool {
        matches!(self, Self::Draft | Self::Ready)
    }

    /// 是否为只读状态（sending / sent / archived）。
    pub fn is_read_only(self) -> bool {
        !self.is_editable()
    }

    /// 状态机允许的下一状态。
    pub fn next(self) -> Option<Self> {
        match self {
            Self::Draft => Some(Self::Ready),
            Self::Ready => Some(Self::Sending),
            Self::Sending => Some(Self::Sent),
            Self::Sent => Some(Self::Archived),
            Self::Archived => None,
        }
    }

    /// 校验一次状态迁移是否合法。
    pub fn can_transition_to(self, target: Self) -> bool {
        self.next() == Some(target)
    }

    /// 「解锁」这条**特殊迁移**：已发送 → 草稿（为了纠错）。
    ///
    /// 设计文档 ch.5：「如需纠错，走『解锁 → 生成修订记录 → 仅对未发送者补发』」。
    /// 它**不是**普通迁移 —— 必须留修订记录，所以由调用方（`infra::periods`）负责写审计。
    pub fn is_unlock(self) -> bool {
        matches!(self, Self::Sent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_and_ready_are_editable_but_sending_and_sent_are_not() {
        assert!(PeriodStatus::Draft.is_editable());
        // ready 仍可编辑：设计文档只禁「发送中/已发送」
        assert!(PeriodStatus::Ready.is_editable());
        assert!(PeriodStatus::Sending.is_read_only());
        assert!(PeriodStatus::Sent.is_read_only());
        assert!(PeriodStatus::Archived.is_read_only());
    }

    #[test]
    fn forward_transitions_are_allowed() {
        assert!(PeriodStatus::Draft.can_transition_to(PeriodStatus::Ready));
        assert!(PeriodStatus::Ready.can_transition_to(PeriodStatus::Sending));
        assert!(PeriodStatus::Sending.can_transition_to(PeriodStatus::Sent));
        assert!(PeriodStatus::Sent.can_transition_to(PeriodStatus::Archived));
    }

    #[test]
    fn skipping_states_is_rejected() {
        // 不能从草稿直接跳到已发送
        assert!(!PeriodStatus::Draft.can_transition_to(PeriodStatus::Sent));
        // 不能回退
        assert!(!PeriodStatus::Sent.can_transition_to(PeriodStatus::Ready));
        // 已归档是终态
        assert!(!PeriodStatus::Archived.can_transition_to(PeriodStatus::Draft));
        assert_eq!(PeriodStatus::Archived.next(), None);
    }

    #[test]
    fn unlock_is_the_only_backward_move_and_only_from_sent() {
        assert!(PeriodStatus::Sent.is_unlock());
        assert!(!PeriodStatus::Draft.is_unlock());
        assert!(!PeriodStatus::Archived.is_unlock());
        assert!(!PeriodStatus::Sending.is_unlock());
    }

    #[test]
    fn status_strings_round_trip() {
        for s in [
            PeriodStatus::Draft,
            PeriodStatus::Ready,
            PeriodStatus::Sending,
            PeriodStatus::Sent,
            PeriodStatus::Archived,
        ] {
            assert_eq!(PeriodStatus::parse(s.as_str()), Some(s));
        }
        assert_eq!(PeriodStatus::parse("nonsense"), None);
    }
}
