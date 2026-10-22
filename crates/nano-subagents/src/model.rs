//! Official subagent catalog and the dock projection.
//!
//! The catalog is a display projection of official rows. Membership, mode,
//! activity, labels, and timing all come from the host's official subagent and
//! Session services; nothing here creates, resumes, or guesses a child.

use std::collections::BTreeSet;

/// How one child conversation can be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AgentMode {
    /// A durable child that accepts more human turns.
    Continuable,
    /// A finished single execution with a readable history.
    OneShot,
    /// An external execution without a local child session; never navigable.
    External,
    /// The official catalog did not prove the mode.
    Unknown,
}

impl AgentMode {
    /// Stable spelling used on the wire and in draft keys.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Continuable => "continuable",
            Self::OneShot => "one-shot",
            Self::External => "external",
            Self::Unknown => "unknown",
        }
    }

    /// Whether the row can open a local detail view.
    pub fn is_navigable(self) -> bool {
        !matches!(self, Self::External)
    }

    /// Whether the row is a local child (not an external execution).
    pub fn is_local(self) -> bool {
        !matches!(self, Self::External)
    }

    /// User-facing mode label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Continuable => "可续聊",
            Self::OneShot => "一次性",
            Self::External => "外部",
            Self::Unknown => "模式未知",
        }
    }
}

/// Official live-registry activity for one child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
    /// A live Agent is currently running.
    Running,
    /// No live Agent is running; the durable record may still exist.
    Inactive,
}

impl Activity {
    /// Stable wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Inactive => "inactive",
        }
    }

    /// User-facing label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "运行中",
            Self::Inactive => "未运行",
        }
    }
}

/// One child row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRow {
    /// Durable child session id.
    pub id: String,
    /// Direct parent whose catalog contains this child.
    pub parent_id: String,
    /// Edge distance from the requested root; direct children are 1.
    pub depth: u32,
    /// Mode reported by the catalog or confirmed from the child's projection.
    pub mode: AgentMode,
    /// Durable creation label, when the catalog recorded one.
    pub label: Option<String>,
    /// Official live activity.
    pub activity: Activity,
    /// Whether this child's own catalog has at least one direct child.
    pub has_children: bool,
    /// Latest closed turn completed normally, from `subagentTiming`.
    pub last_turn_completed: Option<bool>,
    /// Accumulated settled milliseconds, from `subagentTiming`.
    pub settled_ms: Option<u64>,
    /// Start of the open turn, from `subagentTiming.active`.
    pub active_since: Option<u64>,
    /// Latest event time folded into the timing projection.
    pub active_through: Option<u64>,
    /// Exact-parent liveness, only known from a `describe` read.
    pub parent_live: Option<bool>,
    /// Next durable sequence, only known from a `describe` read.
    pub seq: Option<u64>,
}

impl AgentRow {
    /// Display label; never used as identity.
    pub fn display_label(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.id)
    }

    /// Short id for narrow layouts.
    pub fn short_id(&self) -> String {
        self.id.chars().take(8).collect()
    }

    /// Total active duration at `now` milliseconds since epoch.
    pub fn duration_ms(&self, now_ms: u64) -> Option<u64> {
        let settled = self.settled_ms?;
        match (self.active_since, self.active_through) {
            (Some(since), _) => {
                let end = if self.activity == Activity::Running {
                    now_ms.max(since)
                } else {
                    self.active_through.unwrap_or(since).max(since)
                };
                Some(settled.saturating_add(end.saturating_sub(since)))
            }
            _ => Some(settled),
        }
    }
}

/// One unreadable catalog branch from the official diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// Candidate child id whose branch could not be read.
    pub id: String,
    /// Parent catalog that contained the candidate, when reported.
    pub parent_id: Option<String>,
    /// Distance from the requested root, when reported.
    pub depth: Option<u32>,
    /// `corrupt`, `unsupported`, or `unavailable`.
    pub reason: String,
}

/// The official child catalog projection.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    direct: Vec<AgentRow>,
    descendants: Vec<AgentRow>,
    diagnostics: Vec<Diagnostic>,
    parent_live: bool,
}

impl Catalog {
    /// Empty catalog.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the direct-child snapshot.
    pub fn apply_snapshot(
        &mut self,
        direct: Vec<AgentRow>,
        diagnostics: Vec<Diagnostic>,
        parent_live: bool,
    ) {
        self.direct = direct;
        self.diagnostics = diagnostics;
        self.parent_live = parent_live;
    }

    /// Replace the recursive list.
    pub fn apply_list(&mut self, descendants: Vec<AgentRow>, diagnostics: Vec<Diagnostic>) {
        self.descendants = descendants;
        self.diagnostics = diagnostics;
    }

    /// Direct children in official parent-catalog order.
    pub fn direct(&self) -> &[AgentRow] {
        &self.direct
    }

    /// Recursive descendants in official pre-order.
    pub fn descendants(&self) -> &[AgentRow] {
        &self.descendants
    }

    /// Unreadable branches.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Whether the direct parent Agent (the host root) is live.
    pub fn parent_live(&self) -> bool {
        self.parent_live
    }

    /// Whether no direct child is known.
    pub fn is_empty(&self) -> bool {
        self.direct.is_empty()
    }

    /// Official row by durable id across the list and direct snapshot.
    pub fn row(&self, id: &str) -> Option<&AgentRow> {
        self.direct
            .iter()
            .chain(self.descendants.iter())
            .find(|row| row.id == id)
    }

    /// Replace one row's timing/identity facts after `describe`.
    pub fn update_row(&mut self, updated: &AgentRow) {
        for row in self
            .direct
            .iter_mut()
            .chain(self.descendants.iter_mut())
            .filter(|row| row.id == updated.id)
        {
            row.mode = updated.mode;
            if updated.label.is_some() {
                row.label = updated.label.clone();
            }
            row.activity = updated.activity;
            row.last_turn_completed = updated.last_turn_completed;
            row.settled_ms = updated.settled_ms;
            row.active_since = updated.active_since;
            row.active_through = updated.active_through;
        }
    }

    /// The local-agent count for the `N local agents` pill. External entries
    /// are not local and never count.
    pub fn local_count(&self) -> usize {
        self.direct.iter().filter(|row| row.mode.is_local()).count()
    }

    /// The dock rows: at most `limit` direct children in official order,
    /// excluding externally hidden entries.
    pub fn dock_rows(&self, limit: usize, hidden: &BTreeSet<String>) -> Vec<AgentRow> {
        self.direct
            .iter()
            .filter(|row| !hidden.contains(&row.id))
            .take(limit)
            .cloned()
            .collect()
    }

    /// Number of direct children beyond the dock limit (plus hidden entries
    /// still readable from the full list).
    pub fn dock_overflow(&self, limit: usize, hidden: &BTreeSet<String>) -> usize {
        self.direct
            .iter()
            .filter(|row| !hidden.contains(&row.id))
            .count()
            .saturating_sub(limit)
    }

    /// Rows for the full list: descendants when the host returned them;
    /// otherwise the direct snapshot so a failed recursive read still shows
    /// everything already proven.
    pub fn list_rows(&self) -> &[AgentRow] {
        if self.descendants.is_empty() {
            &self.direct
        } else {
            &self.descendants
        }
    }
}
