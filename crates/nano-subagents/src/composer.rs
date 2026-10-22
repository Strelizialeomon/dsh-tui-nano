//! Input boundary for one child conversation.
//!
//! A continuable child keeps ordinary input only while its exact direct parent
//! is live. Parent-unavailable children are read-only when idle and offer only
//! Stop while running. One-shot history is read-only; external executions have
//! no detail view at all; an unproven mode stays read-only until the child's
//! own history confirms a supported mode.

use crate::model::{Activity, AgentMode};

/// What the child composer may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposerGate {
    /// Text and images may be sent; Stop is available while running.
    Enabled,
    /// No input or send; the view is a read-only record.
    ReadOnly,
    /// No input or send, but Stop remains available.
    StopOnly,
    /// External children have no detail view.
    NotNavigable,
    /// The official mode is not proven yet; read history before enabling input.
    Resolving,
}

impl ComposerGate {
    /// Resolve the gate from official facts.
    pub fn resolve(mode: AgentMode, parent_live: bool, activity: Activity) -> Self {
        match mode {
            AgentMode::External => Self::NotNavigable,
            AgentMode::Unknown => Self::Resolving,
            AgentMode::OneShot => Self::ReadOnly,
            AgentMode::Continuable => {
                if parent_live {
                    Self::Enabled
                } else if activity == Activity::Running {
                    Self::StopOnly
                } else {
                    Self::ReadOnly
                }
            }
        }
    }

    /// Whether Enter/Send may submit text or images.
    pub fn can_send(self) -> bool {
        matches!(self, Self::Enabled)
    }

    /// Whether the independent Stop action is available.
    pub fn can_stop(self) -> bool {
        matches!(self, Self::Enabled | Self::StopOnly)
    }

    /// Whether an ordinary composer (with a cursor) is drawn.
    pub fn shows_composer(self) -> bool {
        matches!(self, Self::Enabled | Self::ReadOnly | Self::StopOnly)
    }

    /// Short explanation for the composer title.
    pub fn hint(self) -> &'static str {
        match self {
            Self::Enabled => "Enter 发送 · Shift+Enter 换行 · k 停止本轮 · Esc 返回",
            Self::ReadOnly => "只读（父会话不可用且子代理未运行）· Esc 返回",
            Self::StopOnly => "父会话不可用；输入已禁用，k 停止本轮 · Esc 返回",
            Self::NotNavigable => "外部子代理没有本地详情",
            Self::Resolving => "正在读取子代理历史以确认可续聊性…",
        }
    }
}
