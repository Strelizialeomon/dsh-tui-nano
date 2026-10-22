//! Terminal lifecycle facts, dock status lamps, blink timing, and cleanup.
//!
//! The only terminal authority is the official `subagent/end` stop reason.
//! Unknown reasons are reported as `unknown`; a missing reason is never
//! promoted to success. `subagentTiming.lastTurnCompleted` is a turn fact, not
//! an activation outcome.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use crate::model::{Activity, AgentRow};

/// Dock entries linger after an official terminal event for about this long.
/// This mirrors Claude Code's `PANEL_GRACE_MS = 30_000`.
pub const CLEANUP_DELAY: Duration = Duration::from_millis(30_000);

/// Running lamps blink with this full period.
pub const BLINK_PERIOD: Duration = Duration::from_millis(800);

/// Official terminal reasons this build understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// The child finished normally.
    Completed,
    /// The child was interrupted.
    Aborted,
    /// The child failed.
    Error,
    /// The child hit its token ceiling.
    MaxTokens,
    /// The model refused.
    Refusal,
    /// The official union carried a value this build does not know.
    Unknown,
}

impl StopReason {
    /// Parse one official stop reason. Unknown values stay unknown.
    pub fn parse(value: &str) -> Self {
        match value {
            "completed" => Self::Completed,
            "aborted" => Self::Aborted,
            "error" => Self::Error,
            "max-tokens" => Self::MaxTokens,
            "refusal" => Self::Refusal,
            _ => Self::Unknown,
        }
    }

    /// Wire spelling for diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Aborted => "aborted",
            Self::Error => "error",
            Self::MaxTokens => "max-tokens",
            Self::Refusal => "refusal",
            Self::Unknown => "unknown",
        }
    }
}

/// Dock status lamp semantics. Rendering chooses the actual colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lamp {
    /// Running: accent colour with a discrete blink.
    Running,
    /// Official stop reason completed.
    Completed,
    /// Official stop reason aborted, refusal, or max-tokens.
    Warned,
    /// Official stop reason error.
    Error,
    /// No proven outcome and not running.
    Inactive,
}

/// One observed terminal fact for a child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalFact {
    /// Official stop reason.
    pub reason: StopReason,
    /// When this window observed the terminal edge.
    pub at: Instant,
}

/// One lifecycle event parsed from `subagents.event`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleEvent {
    /// A child became resident.
    Start {
        /// Child identity.
        child_id: String,
    },
    /// A child settled with an official stop reason.
    End {
        /// Child identity.
        child_id: String,
        /// Official stop reason.
        reason: StopReason,
    },
    /// The catalog may have changed.
    Changed,
}

/// Lifecycle facts observed by this window.
#[derive(Debug, Clone)]
pub struct Lifecycle {
    terminal: BTreeMap<String, TerminalFact>,
    hidden: BTreeSet<String>,
    pending_cleanup: BTreeMap<String, Instant>,
    blink_origin: Instant,
}

impl Default for Lifecycle {
    fn default() -> Self {
        Self {
            terminal: BTreeMap::new(),
            hidden: BTreeSet::new(),
            pending_cleanup: BTreeMap::new(),
            blink_origin: Instant::now(),
        }
    }
}

impl Lifecycle {
    /// Empty lifecycle state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one lifecycle event. Returns whether the screen needs a redraw.
    pub fn apply(&mut self, event: &LifecycleEvent) -> bool {
        match event {
            LifecycleEvent::Start { child_id } => {
                let was_hidden = self.hidden.remove(child_id);
                let was_pending = self.pending_cleanup.remove(child_id).is_some();
                // A new activation is a new run: the previous terminal reason
                // must not keep the lamp green before this run ends.
                let was_terminal = self.terminal.remove(child_id).is_some();
                was_hidden || was_pending || was_terminal
            }
            LifecycleEvent::End { child_id, reason } => {
                self.apply_end(child_id, *reason);
                true
            }
            LifecycleEvent::Changed => true,
        }
    }

    /// Record one terminal edge. The dock entry ages out after
    /// [`CLEANUP_DELAY`]; the catalog record is never deleted here.
    pub fn apply_end(&mut self, child_id: &str, reason: StopReason) {
        self.terminal.insert(
            child_id.to_string(),
            TerminalFact {
                reason,
                at: Instant::now(),
            },
        );
        self.pending_cleanup
            .insert(child_id.to_string(), Instant::now() + CLEANUP_DELAY);
    }

    /// Terminal fact for one child, if this window observed it.
    pub fn terminal(&self, child_id: &str) -> Option<&TerminalFact> {
        self.terminal.get(child_id)
    }

    /// Move every dock entry whose terminal edge has aged out into the hidden
    /// set. Returns whether the dock projection changed.
    pub fn take_due(&mut self, now: Instant) -> bool {
        let due: Vec<String> = self
            .pending_cleanup
            .iter()
            .filter(|(_, deadline)| **deadline <= now)
            .map(|(id, _)| id.clone())
            .collect();
        if due.is_empty() {
            return false;
        }
        for id in due {
            self.pending_cleanup.remove(&id);
            self.hidden.insert(id);
        }
        true
    }

    /// Currently hidden dock entries.
    pub fn hidden(&self) -> BTreeSet<String> {
        self.hidden.clone()
    }

    /// Whether a cleanup is still pending for any child.
    pub fn cleanup_pending(&self) -> bool {
        !self.pending_cleanup.is_empty()
    }

    /// Next cleanup deadline, for the app's tick loop.
    pub fn next_cleanup(&self) -> Option<Instant> {
        self.pending_cleanup.values().copied().min()
    }

    /// Status lamp for one row.
    pub fn lamp(&self, row: &AgentRow) -> Lamp {
        if let Some(fact) = self.terminal.get(&row.id) {
            return match fact.reason {
                StopReason::Completed => Lamp::Completed,
                StopReason::Error => Lamp::Error,
                StopReason::Aborted | StopReason::MaxTokens | StopReason::Refusal => Lamp::Warned,
                StopReason::Unknown => Lamp::Inactive,
            };
        }
        if row.activity == Activity::Running {
            return Lamp::Running;
        }
        // `lastTurnCompleted` is a turn fact, not an activation outcome. Only
        // the official `subagent/end` stop reason may paint a terminal lamp.
        Lamp::Inactive
    }

    /// Whether any visible row is running and should animate.
    pub fn any_running<'a>(&self, rows: impl IntoIterator<Item = &'a AgentRow>) -> bool {
        rows.into_iter().any(|row| self.lamp(row) == Lamp::Running)
    }

    /// Blink phase: true for the first half of each 800ms period.
    pub fn blink_on(&self, now: Instant) -> bool {
        let elapsed = now.duration_since(self.blink_origin).as_millis() as u64;
        let period = BLINK_PERIOD.as_millis() as u64;
        elapsed % period < period / 2
    }

    /// Next instant at which the blink phase changes. Used by the app's tick
    /// loop so a running dock redraws twice per period and sleeps otherwise.
    pub fn next_blink_change(&self, now: Instant) -> Instant {
        let elapsed = now.duration_since(self.blink_origin);
        let half = BLINK_PERIOD / 2;
        let phase = elapsed.as_millis() as u64 % BLINK_PERIOD.as_millis() as u64;
        let remaining = if phase < half.as_millis() as u64 {
            half.as_millis() as u64 - phase
        } else {
            BLINK_PERIOD.as_millis() as u64 - phase
        };
        now + Duration::from_millis(remaining.max(1))
    }
}
