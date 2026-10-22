//! Dock selection, the all-agents list, and child navigation.
//!
//! Entering a child only changes the mounted view; it never stops the child or
//! discards the parent transcript, draft, or reading position. The origin is
//! kept so Esc returns to the exact entry (dock row or selected list row).

use nano_conversation::{SessionAddress, SubagentMode};

use crate::model::{Activity, AgentMode};

/// Where a child view was opened from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryOrigin {
    /// The dock row at this index.
    Dock {
        /// Index in the visible dock at entry time.
        index: usize,
        /// Durable child id, used to find the row if the dock shifted.
        child_id: String,
    },
    /// The full list row with this child id.
    List {
        /// Durable child id whose row was selected.
        child_id: String,
    },
}

/// One mounted child conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountedChild {
    /// S02 address for drafts, transcript, and requests.
    pub address: SessionAddress,
    /// Display label; never identity.
    pub label: String,
    /// Official mode; unknown until the child history confirms it.
    pub mode: AgentMode,
    /// Whether the exact direct parent Agent is live.
    pub parent_live: bool,
    /// Official live activity.
    pub activity: Activity,
    /// Where this view was opened from.
    pub origin: EntryOrigin,
}

impl MountedChild {
    /// The S02 address mode, when this row has a local child session.
    pub fn address_mode(&self) -> Option<SubagentMode> {
        match self.mode {
            AgentMode::Continuable => Some(SubagentMode::Continuable),
            AgentMode::OneShot => Some(SubagentMode::OneShot),
            AgentMode::Unknown => Some(SubagentMode::Unknown),
            AgentMode::External => None,
        }
    }

    /// Whether the child can be navigated to at all.
    pub fn navigable(&self) -> bool {
        self.mode.is_navigable()
    }
}

/// Where Esc should land after leaving a child view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExitTarget {
    /// Select this dock row.
    Dock {
        /// Dock index at entry time.
        index: usize,
        /// Durable child id.
        child_id: String,
    },
    /// Reopen the list and select this child row.
    List {
        /// Child id.
        child_id: String,
    },
}

/// Navigation state for the dock, the full list, and one mounted child.
#[derive(Debug, Clone)]
pub struct Navigation {
    /// Selected visible dock row.
    pub dock_selected: usize,
    /// Whether unmodified Up/Down route to the dock.
    pub dock_focus: bool,
    /// Whether the all-agents list is open.
    pub list_open: bool,
    /// Selected row in the all-agents list.
    pub list_selected: usize,
    mounted: Option<MountedChild>,
}

impl Default for Navigation {
    fn default() -> Self {
        Self {
            dock_selected: 0,
            dock_focus: false,
            list_open: false,
            list_selected: 0,
            mounted: None,
        }
    }
}

impl Navigation {
    /// Empty navigation.
    pub fn new() -> Self {
        Self::default()
    }

    /// The mounted child, when inside a child view.
    pub fn mounted(&self) -> Option<&MountedChild> {
        self.mounted.as_ref()
    }

    /// The mounted child, mutably.
    pub fn mounted_mut(&mut self) -> Option<&mut MountedChild> {
        self.mounted.as_mut()
    }

    /// Whether a child view is active.
    pub fn is_mounted(&self) -> bool {
        self.mounted.is_some()
    }

    /// Keep the dock selection inside the visible row count (including the
    /// overflow entry when `overflow` is true).
    pub fn clamp_dock(&mut self, visible: usize, overflow: bool) {
        let rows = visible + usize::from(overflow);
        if rows == 0 {
            self.dock_selected = 0;
            self.dock_focus = false;
        } else if self.dock_selected >= rows {
            self.dock_selected = rows - 1;
        }
    }

    /// Move the dock selection by one row, wrapping nowhere.
    pub fn move_dock(&mut self, delta: isize, visible: usize, overflow: bool) -> bool {
        let rows = visible + usize::from(overflow);
        if rows == 0 {
            return false;
        }
        let next = (self.dock_selected as isize + delta).clamp(0, rows as isize - 1) as usize;
        let changed = next != self.dock_selected;
        self.dock_selected = next;
        self.dock_focus = true;
        changed
    }

    /// Open one child from the dock.
    pub fn enter_from_dock(
        &mut self,
        address: SessionAddress,
        label: String,
        mode: AgentMode,
        parent_live: bool,
        activity: Activity,
        index: usize,
        child_id: String,
    ) -> bool {
        if !mode.is_navigable() {
            return false;
        }
        self.mounted = Some(MountedChild {
            address,
            label,
            mode,
            parent_live,
            activity,
            origin: EntryOrigin::Dock { index, child_id },
        });
        self.list_open = false;
        self.dock_focus = false;
        true
    }

    /// Open one child from the full list.
    pub fn enter_from_list(
        &mut self,
        address: SessionAddress,
        label: String,
        mode: AgentMode,
        parent_live: bool,
        activity: Activity,
        child_id: String,
    ) -> bool {
        if !mode.is_navigable() {
            return false;
        }
        self.mounted = Some(MountedChild {
            address,
            label,
            mode,
            parent_live,
            activity,
            origin: EntryOrigin::List { child_id },
        });
        self.list_open = false;
        true
    }

    /// Leave the child view and report where to return.
    pub fn exit(&mut self) -> Option<ExitTarget> {
        let mounted = self.mounted.take()?;
        match mounted.origin {
            EntryOrigin::Dock { index, child_id } => {
                self.dock_selected = index;
                Some(ExitTarget::Dock { index, child_id })
            }
            EntryOrigin::List { child_id } => {
                self.list_open = true;
                Some(ExitTarget::List { child_id })
            }
        }
    }

    /// Open the all-agents list.
    pub fn open_list(&mut self) {
        self.list_open = true;
        self.list_selected = 0;
        self.dock_focus = false;
    }

    /// Close the all-agents list without entering anything.
    pub fn close_list(&mut self) {
        self.list_open = false;
        self.list_selected = 0;
    }

    /// Move the list selection inside `len` rows.
    pub fn move_list(&mut self, delta: isize, len: usize) -> bool {
        if len == 0 {
            self.list_selected = 0;
            return false;
        }
        let next = (self.list_selected as isize + delta).clamp(0, len as isize - 1) as usize;
        let changed = next != self.list_selected;
        self.list_selected = next;
        changed
    }

    /// Apply a `describe` result to the mounted child. A mode confirmed from
    /// the child history re-keys the address; `None` means the address mode is
    /// unchanged.
    pub fn resolve_mounted(
        &mut self,
        mode: AgentMode,
        label: Option<String>,
        parent_live: bool,
        activity: Activity,
        address: Option<SessionAddress>,
    ) -> bool {
        let Some(mounted) = self.mounted.as_mut() else {
            return false;
        };
        let changed = mounted.mode != mode
            || mounted.parent_live != parent_live
            || mounted.activity != activity
            || label.as_deref().is_some_and(|value| value != mounted.label);
        mounted.mode = mode;
        mounted.parent_live = parent_live;
        mounted.activity = activity;
        if let Some(label) = label {
            mounted.label = label;
        }
        if let Some(address) = address {
            mounted.address = address;
        }
        changed
    }
}
