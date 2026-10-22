//! Composite S12 state consumed by the app.
//!
//! `SubagentState` owns the official catalog projection, lifecycle facts, and
//! navigation; the app owns transcript views, drafts, attachments, and host
//! requests. Keeping the display rules here makes them unit-testable without a
//! terminal or a host.

use serde_json::Value;

use nano_conversation::{SessionAddress, SubagentMode};

use crate::composer::ComposerGate;
use crate::lifecycle::{Lamp, Lifecycle, LifecycleEvent};
use crate::model::{AgentMode, AgentRow, Catalog, Diagnostic};
use crate::nav::{MountedChild, Navigation};
use crate::wire::{self, WireError};

/// Maximum direct children shown in the dock.
pub const DOCK_LIMIT: usize = 4;

/// Event kind carrying official subagent lifecycle edges.
pub const EVENT_KIND: &str = "subagents.event";

/// Full S12 display state.
#[derive(Debug, Clone)]
pub struct SubagentState {
    root_session_id: String,
    catalog: Catalog,
    lifecycle: Lifecycle,
    nav: Navigation,
    list_loaded: bool,
    list_truncated: bool,
    last_error: Option<String>,
    /// A lifecycle `changed` event asked for a fresh snapshot.
    pub snapshot_due: bool,
    /// A describe request is in flight for this child id.
    pub describing: Option<String>,
}

impl SubagentState {
    /// Empty state for one root session.
    pub fn new(root_session_id: impl Into<String>) -> Self {
        Self {
            root_session_id: root_session_id.into(),
            catalog: Catalog::new(),
            lifecycle: Lifecycle::new(),
            nav: Navigation::new(),
            list_loaded: false,
            list_truncated: false,
            last_error: None,
            snapshot_due: false,
            describing: None,
        }
    }

    /// Root session identity.
    pub fn root_session_id(&self) -> &str {
        &self.root_session_id
    }

    /// Official catalog projection.
    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    /// Lifecycle facts.
    pub fn lifecycle(&self) -> &Lifecycle {
        &self.lifecycle
    }

    /// Navigation state.
    pub fn nav(&self) -> &Navigation {
        &self.nav
    }

    /// Navigation state, mutably.
    pub fn nav_mut(&mut self) -> &mut Navigation {
        &mut self.nav
    }

    /// Last wire or request error worth showing.
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Clear the last error after it has been surfaced.
    pub fn clear_error(&mut self) {
        self.last_error = None;
    }

    /// Whether the recursive list has been read at least once.
    pub fn list_loaded(&self) -> bool {
        self.list_loaded
    }

    /// Whether the last list was truncated by transport limits.
    pub fn list_truncated(&self) -> bool {
        self.list_truncated
    }

    /// Replace the direct-child snapshot.
    pub fn apply_snapshot(&mut self, value: &Value) -> Result<bool, String> {
        let (rows, diagnostics, parent_live) =
            wire::parse_snapshot(value).map_err(|error| self.remember(error))?;
        self.catalog.apply_snapshot(rows, diagnostics, parent_live);
        // Keep the mounted child's activity current without re-keying its
        // address; mode and liveness still come from `describe`.
        let mounted_id = self.nav.mounted().and_then(|mounted| {
            mounted
                .address
                .child
                .as_ref()
                .map(|child| child.child_session_id.clone())
        });
        if let Some(id) = mounted_id {
            if let Some(row) = self.catalog.row(&id).cloned() {
                if let Some(mounted) = self.nav.mounted_mut() {
                    mounted.activity = row.activity;
                }
            }
        }
        self.snapshot_due = false;
        Ok(true)
    }

    /// Replace the recursive list.
    pub fn apply_list(&mut self, value: &Value) -> Result<bool, String> {
        let (rows, diagnostics) = wire::parse_list(value).map_err(|error| self.remember(error))?;
        self.catalog.apply_list(rows, diagnostics);
        self.list_loaded = true;
        self.list_truncated = value
            .get("truncated")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        Ok(true)
    }

    /// Apply one `describe` row and update the mounted child when it matches.
    pub fn apply_description(&mut self, value: &Value) -> Result<AgentRow, String> {
        let row = wire::parse_description(value).map_err(|error| self.remember(error))?;
        self.catalog.update_row(&row);
        if self.nav.mounted().is_some_and(|mounted| {
            mounted
                .address
                .child
                .as_ref()
                .is_some_and(|child| child.child_session_id == row.id)
        }) {
            let address = SessionAddress::child(
                self.root_session_id.clone(),
                row.parent_id.clone(),
                row.id.clone(),
                match row.mode {
                    AgentMode::Continuable => SubagentMode::Continuable,
                    AgentMode::OneShot => SubagentMode::OneShot,
                    AgentMode::Unknown | AgentMode::External => SubagentMode::Unknown,
                },
            );
            self.nav.resolve_mounted(
                row.mode,
                row.label.clone(),
                row.parent_live
                    .unwrap_or_else(|| self.parent_live(row.parent_id.as_str())),
                row.activity,
                Some(address),
            );
        }
        self.describing = None;
        Ok(row)
    }

    /// Apply one lifecycle event payload.
    pub fn apply_event(&mut self, value: &Value) -> Result<bool, String> {
        let event = wire::parse_event(value).map_err(|error| self.remember(error))?;
        let changed = self.lifecycle.apply(&event);
        if matches!(event, LifecycleEvent::Changed)
            || matches!(
                event,
                LifecycleEvent::Start { .. } | LifecycleEvent::End { .. }
            )
        {
            self.snapshot_due = true;
        }
        Ok(changed)
    }

    /// Hide dock entries whose official terminal edge has aged out. Returns
    /// whether anything changed. Records stay in the catalog.
    pub fn cleanup(&mut self, now: std::time::Instant) -> bool {
        // The lifecycle keeps the hidden set; the catalog is not mutated, so
        // the all-agents list and a later snapshot can still find the record.
        let changed = self.lifecycle.take_due(now);
        if changed {
            let (rows, overflow) = self.dock();
            self.nav.clamp_dock(rows.len(), overflow > 0);
        }
        changed
    }

    /// Dock rows and overflow count.
    pub fn dock(&self) -> (Vec<AgentRow>, usize) {
        let hidden = self.lifecycle.hidden();
        (
            self.catalog.dock_rows(DOCK_LIMIT, &hidden),
            self.catalog.dock_overflow(DOCK_LIMIT, &hidden),
        )
    }

    /// `N local agents` pill count. External executions never count.
    pub fn local_count(&self) -> usize {
        let hidden = self.lifecycle.hidden();
        self.catalog
            .dock_rows(usize::MAX, &hidden)
            .iter()
            .filter(|row| row.mode.is_local())
            .count()
    }

    /// Lamp for one row.
    pub fn lamp(&self, row: &AgentRow) -> Lamp {
        self.lifecycle.lamp(row)
    }

    /// Gate for the mounted child, or one row.
    pub fn gate(&self, row: &AgentRow) -> ComposerGate {
        ComposerGate::resolve(
            row.mode,
            row.parent_live
                .unwrap_or_else(|| self.parent_live(row.parent_id.as_str())),
            row.activity,
        )
    }

    /// Gate for the mounted child.
    pub fn mounted_gate(&self) -> Option<ComposerGate> {
        self.nav.mounted().map(|mounted| {
            ComposerGate::resolve(mounted.mode, mounted.parent_live, mounted.activity)
        })
    }

    /// Whether any visible dock row is running.
    pub fn any_running(&self) -> bool {
        let (rows, _) = self.dock();
        rows.iter()
            .any(|row| self.lifecycle.lamp(row) == Lamp::Running)
    }

    /// Enter one dock row. External rows refuse.
    pub fn enter_dock(&mut self, index: usize) -> bool {
        let (rows, overflow) = self.dock();
        let Some(row) = rows.get(index) else {
            return false;
        };
        let address = self.address_for(row);
        let entered = self.nav.enter_from_dock(
            address,
            row.display_label().to_string(),
            row.mode,
            row.parent_live
                .unwrap_or_else(|| self.parent_live(row.parent_id.as_str())),
            row.activity,
            index,
            row.id.clone(),
        );
        if entered {
            self.nav.clamp_dock(rows.len(), overflow > 0);
        }
        entered
    }

    /// Open the dock's overflow row (the entry after the visible rows).
    pub fn enter_overflow(&mut self) {
        self.nav.open_list();
    }

    /// Enter one row from the recursive list.
    pub fn enter_list(&mut self, index: usize) -> bool {
        let rows = self.catalog.list_rows();
        let Some(row) = rows.get(index) else {
            return false;
        };
        let address = self.address_for(row);
        self.nav.enter_from_list(
            address,
            row.display_label().to_string(),
            row.mode,
            self.parent_live(row.parent_id.as_str()),
            row.activity,
            row.id.clone(),
        )
    }

    /// Leave the mounted child.
    pub fn exit(&mut self) -> Option<crate::nav::ExitTarget> {
        self.nav.exit()
    }

    /// Mounted child.
    pub fn mounted(&self) -> Option<&MountedChild> {
        self.nav.mounted()
    }

    /// Build the S02 address for one row.
    pub fn address_for(&self, row: &AgentRow) -> SessionAddress {
        SessionAddress::child(
            self.root_session_id.clone(),
            row.parent_id.clone(),
            row.id.clone(),
            match row.mode {
                AgentMode::Continuable => SubagentMode::Continuable,
                AgentMode::OneShot => SubagentMode::OneShot,
                AgentMode::Unknown | AgentMode::External => SubagentMode::Unknown,
            },
        )
    }

    /// Whether the exact parent Agent is live. The host root is live whenever
    /// the UI has a ready host. For a nested parent the catalog activity is
    /// not liveness (an idle resident parent reports inactive), so the answer
    /// stays unproven until the host's `describe` supplies `parentLive`.
    pub fn parent_live(&self, parent_id: &str) -> bool {
        parent_id == self.root_session_id
    }

    /// Diagnostics for the all-agents list.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        self.catalog.diagnostics()
    }

    /// Rows for the all-agents list.
    pub fn list_rows(&self) -> &[AgentRow] {
        self.catalog.list_rows()
    }

    fn remember(&mut self, error: WireError) -> String {
        self.last_error = Some(error.0.clone());
        error.0
    }
}
