//! Wire projections owned by the S10 Monitor bridge.
//!
//! The bridge owns the listening offsets, budgets, and delivery receipts. This
//! crate only displays those official facts: it never reads job output, never
//! sends a follow-up, and never decides that a process ended.

use serde::Deserialize;

/// Delivery mode chosen at creation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MonitorMode {
    /// Official `followup`; wakes the Agent and can create a model request.
    Notify,
    /// Official `inject`; never wakes the Agent.
    Quiet,
    /// A mode this build does not know; displayed raw, never guessed.
    #[serde(other)]
    Unknown,
}

impl MonitorMode {
    /// Display label including the wake fact.
    pub fn label(self) -> &'static str {
        match self {
            Self::Notify => "notify（唤醒 Agent，会产生模型调用和费用）",
            Self::Quiet => "quiet（不唤醒；空闲时可能一直等待）",
            Self::Unknown => "未知模式",
        }
    }

    /// Short mode name for list rows and titles.
    pub fn short_label(self) -> &'static str {
        match self {
            Self::Notify => "notify",
            Self::Quiet => "quiet",
            Self::Unknown => "未知模式",
        }
    }

    /// Whether this mode wakes the driver.
    pub fn wakes(self) -> bool {
        matches!(self, Self::Notify)
    }
}

/// Where the monitor's initial offset came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MonitorFrom {
    /// Only output written after attach.
    New,
    /// Replay the retained head from `earliest`.
    Earliest,
    /// A value this build does not know.
    #[serde(other)]
    Unknown,
}

impl MonitorFrom {
    /// Display label.
    pub fn label(self) -> &'static str {
        match self {
            Self::New => "只看新输出",
            Self::Earliest => "从开头（retained head）",
            Self::Unknown => "未知起点",
        }
    }
}

/// UI-only stream fact. It never means "the monitor condition was met".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MonitorStream {
    /// Output can still arrive.
    Watching,
    /// The observed job completed; this is not a condition match.
    StreamEnded,
    /// The observed job failed.
    Failed,
    /// The observed job was killed.
    Stopped,
    /// A fact this build does not know.
    #[serde(other)]
    Unknown,
}

impl MonitorStream {
    /// Exact display token plus a plain-language label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Watching => "监听中",
            Self::StreamEnded => "stream-ended（流结束；不代表条件满足）",
            Self::Failed => "failed（被观察任务失败）",
            Self::Stopped => "stopped（被观察任务被终止）",
            Self::Unknown => "未知流状态",
        }
    }

    /// Whether no further output is expected.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::StreamEnded | Self::Failed | Self::Stopped)
    }
}

/// Official delivery receipt state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeliveryState {
    /// The delivery call was made; no inbox event observed yet.
    Submitted,
    /// The official inbox holds the message. Queueing is not receipt.
    Inserted,
    /// A step offered the message. A rejected step ends here.
    Claimed,
    /// A durable `user/message` proved the official session accepted it.
    Accepted,
    /// The official inbox discarded the message; the model will not see it.
    Discarded,
    /// The delivery call itself failed.
    Failed,
    /// No official receipt arrived before the timeout.
    Unknown,
}

impl DeliveryState {
    /// Display label that never overstates `inserted`/`claimed`.
    pub fn label(self) -> &'static str {
        match self {
            Self::Submitted => "已提交调用（尚无回执）",
            Self::Inserted => "已入队（不证明模型收到）",
            Self::Claimed => "已交给步骤（不证明模型收到）",
            Self::Accepted => "已收件（durable user/message）",
            Self::Discarded => "已丢弃（模型不会收到）",
            Self::Failed => "投递失败",
            Self::Unknown => "收件未知（未观察到接受的步骤）",
        }
    }

    /// Short state name for list rows.
    pub fn short_label(self) -> &'static str {
        match self {
            Self::Submitted => "提交",
            Self::Inserted => "入队",
            Self::Claimed => "步骤",
            Self::Accepted => "已收件",
            Self::Discarded => "丢弃",
            Self::Failed => "失败",
            Self::Unknown => "未知",
        }
    }

    /// Whether this state is a proven receipt.
    pub fn is_proven(self) -> bool {
        matches!(self, Self::Accepted)
    }

    /// Whether this state is a failure.
    pub fn is_failure(self) -> bool {
        matches!(self, Self::Discarded | Self::Failed)
    }
}

/// One literal or regex line filter.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorFilter {
    /// Literal substring or regular-expression source.
    pub pattern: String,
    /// Whether both sides apply a regular expression.
    #[serde(default)]
    pub regex: bool,
}

impl MonitorFilter {
    /// Display label.
    pub fn label(&self) -> String {
        if self.regex {
            format!("正则 /{}/", self.pattern)
        } else {
            format!("字面「{}」", self.pattern)
        }
    }
}

/// Per-monitor budgets fixed by the bridge.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorBudgets {
    /// Maximum delivered line notifications before the budget is exhausted.
    #[serde(default)]
    pub max_notifications: u64,
    /// Minimum delay between two delivered notifications.
    #[serde(default)]
    pub min_interval_ms: u64,
    /// Maximum line events merged into one notification.
    #[serde(default)]
    pub max_lines_per_notification: u64,
    /// Maximum UTF-8 bytes in one notification.
    #[serde(default)]
    pub max_notification_bytes: u64,
    /// Maximum estimated tokens in one notification.
    #[serde(default)]
    pub max_notification_tokens: u64,
    /// Unix epoch milliseconds when the monitor stops delivering.
    #[serde(default)]
    pub expires_at: u64,
}

/// Counters consumed by this monitor.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorUsage {
    /// Notifications delivered.
    #[serde(default)]
    pub notifications: u64,
    /// Line events covered by those notifications.
    #[serde(default)]
    pub lines: u64,
    /// UTF-8 notification bytes.
    #[serde(default)]
    pub bytes: u64,
    /// Estimated notification tokens.
    #[serde(default)]
    pub estimated_tokens: u64,
}

/// One delivery attempt and its official receipt state.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorDelivery {
    /// Official message id used by inbox and durable events.
    pub message_id: String,
    /// Delivery mode at attempt time.
    pub mode: MonitorMode,
    /// Receipt state.
    pub state: DeliveryState,
    /// Unix epoch milliseconds.
    #[serde(default)]
    pub at: u64,
    /// Line events covered.
    #[serde(default)]
    pub lines: u64,
    /// UTF-8 bytes.
    #[serde(default)]
    pub bytes: u64,
    /// Estimated tokens.
    #[serde(default)]
    pub estimated_tokens: u64,
    /// Absolute byte offset of the first line.
    #[serde(default)]
    pub first_offset: u64,
    /// Absolute byte offset of the last line.
    #[serde(default)]
    pub last_offset: u64,
    /// Middle lines omitted from the notification text.
    #[serde(default)]
    pub omitted_lines: u64,
    /// Bounded text preview.
    #[serde(default)]
    pub text_preview: String,
    /// Honest explanation of the state.
    #[serde(default)]
    pub detail: Option<String>,
}

/// One full monitor projection.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorView {
    /// Monitor identity; one per observed job.
    pub id: String,
    /// Observed official job id.
    pub job_id: String,
    /// Delivery mode.
    pub mode: MonitorMode,
    /// Whether the monitor still accepts normal line delivery.
    #[serde(default)]
    pub listening: bool,
    /// Initial offset choice.
    pub from: MonitorFrom,
    /// Offset at attach.
    #[serde(default)]
    pub initial_offset: u64,
    /// Last processed absolute byte offset.
    #[serde(default)]
    pub offset: u64,
    /// Bytes already retained when the monitor attached.
    #[serde(default)]
    pub unread_bytes: u64,
    /// Bytes skipped by choosing `new`.
    #[serde(default)]
    pub skipped_bytes: u64,
    /// Optional line filter.
    #[serde(default)]
    pub filter: Option<MonitorFilter>,
    /// Merge window in milliseconds.
    #[serde(default)]
    pub merge_window_ms: u64,
    /// Budgets.
    #[serde(default)]
    pub budgets: MonitorBudgets,
    /// Counters.
    #[serde(default)]
    pub used: MonitorUsage,
    /// Whether normal delivery stopped for a budget reason.
    #[serde(default)]
    pub exhausted: bool,
    /// `notifications` or `expired`.
    #[serde(default)]
    pub exhaust_reason: Option<String>,
    /// Whether the user stopped this monitor explicitly.
    #[serde(default)]
    pub stopped_by_user: bool,
    /// UI-only stream fact.
    pub stream: MonitorStream,
    /// Terminal detail copied from the official job projection.
    #[serde(default)]
    pub stream_detail: Option<String>,
    /// Last observed official job status.
    #[serde(default)]
    pub job_status: String,
    /// The requested offset fell below the official ring's retained window.
    #[serde(default)]
    pub lossy: bool,
    /// Number of output discontinuities.
    #[serde(default)]
    pub gaps: u64,
    /// Matching lines held after exhaustion.
    #[serde(default)]
    pub held_matches: u64,
    /// Bytes in the held first/last summary.
    #[serde(default)]
    pub held_bytes: u64,
    /// Matching lines dropped by an explicit stop.
    #[serde(default)]
    pub dropped_matches: u64,
    /// Last read or parse error.
    #[serde(default)]
    pub last_error: Option<String>,
    /// Bounded delivery history.
    #[serde(default)]
    pub deliveries: Vec<MonitorDelivery>,
    /// The delivered message role; always `user`.
    #[serde(default)]
    pub role: String,
    /// The delivered message source kind; always `monitor`.
    #[serde(default)]
    pub source: String,
    /// Creation time.
    #[serde(default)]
    pub created_at: u64,
    /// Last projection change.
    #[serde(default)]
    pub updated_at: u64,
}

impl MonitorView {
    /// The newest delivery attempt.
    pub fn last_delivery(&self) -> Option<&MonitorDelivery> {
        self.deliveries.last()
    }

    /// Whether the monitor is no longer delivering normal line notifications.
    pub fn delivery_stopped(&self) -> bool {
        !self.listening || self.exhausted
    }

    /// Budget line for the panel.
    pub fn budget_label(&self) -> String {
        let mut line = format!(
            "通知 {}/{} · 行 {} · 字节 {} · 估算 token {}",
            self.used.notifications,
            self.budgets.max_notifications,
            self.used.lines,
            self.used.bytes,
            self.used.estimated_tokens
        );
        if self.exhausted {
            line.push_str(match self.exhaust_reason.as_deref() {
                Some("expired") => " · 已过期",
                _ => " · 预算已用尽",
            });
        }
        if self.held_matches > 0 {
            line.push_str(&format!(" · 暂存未投递 {} 行", self.held_matches));
        }
        if self.dropped_matches > 0 {
            line.push_str(&format!(" · 停止时丢弃 {} 行", self.dropped_matches));
        }
        line
    }

    /// Role/source line required by the specification.
    pub fn identity_label(&self) -> String {
        let role = if self.role.is_empty() {
            "user"
        } else {
            &self.role
        };
        let source = if self.source.is_empty() {
            "monitor"
        } else {
            &self.source
        };
        format!("role={role} / source={source}（user-role 消息，不是 Agent 授权）")
    }

    /// Offset line for the panel.
    pub fn offset_label(&self) -> String {
        format!(
            "初始偏移 {} · 当前 {} · 附着时已有 {} 字节未读 · 选择跳过的字节 {}",
            self.initial_offset, self.offset, self.unread_bytes, self.skipped_bytes
        )
    }
}
