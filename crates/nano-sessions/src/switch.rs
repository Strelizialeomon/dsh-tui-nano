//! Controlled main-session switching.
//!
//! A window drives exactly one main session tree. Switching therefore stops the
//! old host, releases its mount claim, starts a new host generation, and only
//! then reports the new session. The states are explicit so the UI can never
//! display "switched" while the old host is still alive or while the new claim
//! is still being acquired.

/// How the new root session must be obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetMode {
    /// Create a fresh root session under a Nano-generated identity.
    Create,
    /// Resume an existing official session.
    Resume,
}

impl TargetMode {
    /// Stable label for diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Resume => "resume",
        }
    }
}

/// One requested main session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchTarget {
    /// Opaque official session identity.
    pub session_id: String,
    /// Create or resume.
    pub mode: TargetMode,
    /// Official title for display, when the list had one.
    pub title: Option<String>,
}

impl SwitchTarget {
    /// Build one target.
    pub fn new(session_id: impl Into<String>, mode: TargetMode) -> Self {
        Self {
            session_id: session_id.into(),
            mode,
            title: None,
        }
    }

    /// Attach the display title from the list row.
    pub fn with_title(mut self, title: Option<String>) -> Self {
        self.title = title;
        self
    }

    /// Short display label; never used as identity.
    pub fn label(&self) -> String {
        self.title
            .clone()
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| self.session_id.clone())
    }
}

/// Explicit switching phase.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SwitchState {
    /// A normal, connected session.
    #[default]
    Idle,
    /// The old host is shutting down; the new host has not started.
    Stopping {
        /// Requested session.
        target: SwitchTarget,
        /// Session to return to when the switch fails.
        previous: Option<SwitchTarget>,
    },
    /// The new host is spawned and handshaking.
    Starting {
        /// Requested session.
        target: SwitchTarget,
        /// Session to return to when the switch fails.
        previous: Option<SwitchTarget>,
    },
    /// The switch failed; there is no usable host in this window.
    Failed {
        /// Requested session.
        target: SwitchTarget,
        /// Session to return to.
        previous: Option<SwitchTarget>,
        /// Startup code, when the host reported one (`occupied`, for example).
        code: Option<String>,
        /// Holder pid for a ledger `occupied` rejection.
        holder_pid: Option<u32>,
        /// Sanitized diagnostic.
        message: String,
    },
}

impl SwitchState {
    /// Whether a switch is in progress (and new user actions must wait).
    pub fn busy(&self) -> bool {
        matches!(self, Self::Stopping { .. } | Self::Starting { .. })
    }

    /// Whether the window currently has no usable host after a failed switch.
    pub fn failed(&self) -> bool {
        matches!(self, Self::Failed { .. })
    }

    /// The requested target, when one exists.
    pub fn target(&self) -> Option<&SwitchTarget> {
        match self {
            Self::Stopping { target, .. }
            | Self::Starting { target, .. }
            | Self::Failed { target, .. } => Some(target),
            Self::Idle => None,
        }
    }

    /// The session to return to, when one exists.
    pub fn previous(&self) -> Option<&SwitchTarget> {
        match self {
            Self::Stopping { previous, .. }
            | Self::Starting { previous, .. }
            | Self::Failed { previous, .. } => previous.as_ref(),
            Self::Idle => None,
        }
    }

    /// User-facing state label.
    pub fn label(&self) -> String {
        match self {
            Self::Idle => "已连接".to_string(),
            Self::Stopping { target, .. } => format!("正在停止旧宿主（切换到 {}）", target.label()),
            Self::Starting { target, .. } => format!("正在启动 {}…", target.label()),
            Self::Failed {
                code, holder_pid, ..
            } => match (code.as_deref(), holder_pid) {
                (Some("occupied"), Some(pid)) => format!("会话被 pid {pid} 占用"),
                (Some("occupied"), None) => "会话已被其他进程占用".to_string(),
                (Some(code), _) => format!("切换失败（{code}）"),
                (None, _) => "切换失败".to_string(),
            },
        }
    }
}

/// Transition rules for one window's main-session switch.
#[derive(Debug, Clone, Default)]
pub struct SwitchMachine {
    state: SwitchState,
}

impl SwitchMachine {
    /// Empty machine in the connected state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Current phase.
    pub fn state(&self) -> &SwitchState {
        &self.state
    }

    /// Whether a switch is in progress.
    pub fn busy(&self) -> bool {
        self.state.busy()
    }

    /// Request a switch. Refused while another switch is in progress.
    pub fn begin(&mut self, target: SwitchTarget, previous: Option<SwitchTarget>) -> bool {
        if self.state.busy() {
            return false;
        }
        self.state = SwitchState::Stopping { target, previous };
        true
    }

    /// The new host process was spawned; it is waiting for the handshake.
    pub fn host_started(&mut self) -> bool {
        let SwitchState::Stopping { target, previous } = &self.state else {
            return false;
        };
        self.state = SwitchState::Starting {
            target: target.clone(),
            previous: previous.clone(),
        };
        true
    }

    /// The handshake completed: the switch is done.
    pub fn succeed(&mut self) -> Option<SwitchTarget> {
        let target = match &self.state {
            SwitchState::Starting { target, .. } | SwitchState::Stopping { target, .. } => {
                target.clone()
            }
            _ => return None,
        };
        self.state = SwitchState::Idle;
        Some(target)
    }

    /// The switch failed; the window has no usable host.
    pub fn fail(
        &mut self,
        code: Option<String>,
        holder_pid: Option<u32>,
        message: impl Into<String>,
    ) {
        let (target, previous) = match &self.state {
            SwitchState::Stopping { target, previous }
            | SwitchState::Starting { target, previous } => (target.clone(), previous.clone()),
            SwitchState::Failed {
                target, previous, ..
            } => (target.clone(), previous.clone()),
            SwitchState::Idle => return,
        };
        self.state = SwitchState::Failed {
            target,
            previous,
            code,
            holder_pid,
            message: message.into(),
        };
    }

    /// Target for the retry entry after a failure.
    pub fn retry_target(&self) -> Option<SwitchTarget> {
        match &self.state {
            SwitchState::Failed { target, .. } => Some(target.clone()),
            _ => None,
        }
    }

    /// Target for the "back to the previous session" entry after a failure.
    pub fn back_target(&self) -> Option<SwitchTarget> {
        match &self.state {
            SwitchState::Failed { previous, .. } => previous.clone(),
            _ => None,
        }
    }

    /// Reset after the failure was handled by another action.
    pub fn clear_failed(&mut self) {
        if self.state.failed() {
            self.state = SwitchState::Idle;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(id: &str) -> SwitchTarget {
        SwitchTarget::new(id, TargetMode::Resume)
    }

    #[test]
    fn a_switch_is_never_reported_as_switched_before_the_handshake() {
        let mut machine = SwitchMachine::new();
        assert!(machine.begin(target("b"), Some(target("a"))));
        assert!(machine.busy());
        assert_eq!(machine.state().label(), "正在停止旧宿主（切换到 b）");
        assert!(machine.host_started());
        assert!(machine.busy());
        assert_eq!(machine.state().label(), "正在启动 b…");
        assert!(machine.succeed().is_some());
        assert!(!machine.busy());
        assert_eq!(machine.state(), &SwitchState::Idle);
    }

    #[test]
    fn a_second_switch_is_refused_while_one_is_in_progress() {
        let mut machine = SwitchMachine::new();
        assert!(machine.begin(target("b"), None));
        assert!(!machine.begin(target("c"), None));
    }

    #[test]
    fn occupancy_failure_carries_the_holder_pid_and_keeps_both_targets() {
        let mut machine = SwitchMachine::new();
        machine.begin(target("b"), Some(target("a")));
        machine.host_started();
        machine.fail(
            Some("occupied".into()),
            Some(4242),
            "owner holds the session",
        );
        assert!(machine.state().failed());
        assert_eq!(machine.state().label(), "会话被 pid 4242 占用");
        assert_eq!(machine.retry_target().unwrap().session_id, "b");
        assert_eq!(machine.back_target().unwrap().session_id, "a");
    }

    #[test]
    fn a_failure_before_the_new_host_exists_still_offers_retry_and_back() {
        let mut machine = SwitchMachine::new();
        machine.begin(target("b"), Some(target("a")));
        machine.fail(None, None, "cleanup failed");
        assert!(machine.state().failed());
        assert_eq!(machine.retry_target().unwrap().session_id, "b");
        assert_eq!(machine.back_target().unwrap().session_id, "a");
        machine.clear_failed();
        assert_eq!(machine.state(), &SwitchState::Idle);
    }
}
