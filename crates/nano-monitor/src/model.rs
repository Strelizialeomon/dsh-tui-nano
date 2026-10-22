//! Bounded monitor projection state for the Nano TUI.
//!
//! The bridge owns every offset, budget, and delivery fact. This model only
//! keeps the latest absolute projections, rejects stale snapshots by the
//! bridge sequence, and surfaces explicit notices for exhaustion, terminal
//! streams, and delivery failures.

use serde_json::Value;

use crate::view::{DeliveryState, MonitorStream, MonitorView};

/// Maximum monitors retained for display; the bridge itself caps at 32.
pub const MAX_MONITORS: usize = 32;

/// Monitor list and selection for one root session.
#[derive(Debug, Clone)]
pub struct MonitorModel {
    root_session_id: String,
    monitors: Vec<MonitorView>,
    selected: usize,
    last_event_seq: u64,
    last_notice: Option<String>,
    last_error: Option<String>,
    max_monitors: usize,
}

impl MonitorModel {
    /// Build for one root session.
    pub fn new(root_session_id: impl Into<String>) -> Self {
        Self {
            root_session_id: root_session_id.into(),
            monitors: Vec::new(),
            selected: 0,
            last_event_seq: 0,
            last_notice: None,
            last_error: None,
            max_monitors: MAX_MONITORS,
        }
    }

    /// Use a smaller display bound (tests and narrow terminals).
    pub fn with_bound(mut self, max_monitors: usize) -> Self {
        self.max_monitors = max_monitors.max(1);
        self
    }

    /// Current projections in creation order.
    pub fn monitors(&self) -> &[MonitorView] {
        &self.monitors
    }

    /// Number of monitors.
    pub fn len(&self) -> usize {
        self.monitors.len()
    }

    /// Whether no monitor exists.
    pub fn is_empty(&self) -> bool {
        self.monitors.is_empty()
    }

    /// Monitors that still accept line delivery.
    pub fn listening_count(&self) -> usize {
        self.monitors
            .iter()
            .filter(|monitor| monitor.listening)
            .count()
    }

    /// Monitors whose budget is exhausted.
    pub fn exhausted_count(&self) -> usize {
        self.monitors
            .iter()
            .filter(|monitor| monitor.exhausted)
            .count()
    }

    /// Selected index.
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Selected projection.
    pub fn selected_monitor(&self) -> Option<&MonitorView> {
        self.monitors.get(self.selected)
    }

    /// Move the selection up.
    pub fn select_previous(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    /// Move the selection down.
    pub fn select_next(&mut self) {
        if self.selected + 1 < self.monitors.len() {
            self.selected += 1;
        }
    }

    /// Select the monitor attached to one job.
    pub fn select_job(&mut self, job_id: &str) -> bool {
        if let Some(index) = self
            .monitors
            .iter()
            .position(|monitor| monitor.job_id == job_id)
        {
            self.selected = index;
            true
        } else {
            false
        }
    }

    /// Projection for one observed job.
    pub fn monitor_for_job(&self, job_id: &str) -> Option<&MonitorView> {
        self.monitors
            .iter()
            .find(|monitor| monitor.job_id == job_id)
    }

    /// Last UI notice.
    pub fn last_notice(&self) -> Option<&str> {
        self.last_notice.as_deref()
    }

    /// Last parse or application error.
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Clear transient diagnostics after the app consumed them.
    pub fn clear_diagnostics(&mut self) {
        self.last_notice = None;
        self.last_error = None;
    }

    /// Last bridge sequence applied.
    pub fn last_event_seq(&self) -> u64 {
        self.last_event_seq
    }

    /// Drop every projection and display state because the host is gone.
    pub fn clear(&mut self) {
        self.monitors.clear();
        self.selected = 0;
        self.last_event_seq = 0;
        self.last_notice = None;
        self.last_error = None;
    }

    /// Apply one `nano.monitor.list` result.
    ///
    /// The bridge sequence is captured before the list; a response older than
    /// the last applied live event must not roll a monitor back.
    pub fn apply_list(&mut self, value: &Value) -> Result<bool, String> {
        let list = value
            .get("monitors")
            .and_then(Value::as_array)
            .ok_or_else(|| "monitor list is missing the monitors array".to_string())?;
        let seq = value.get("seq").and_then(Value::as_u64).unwrap_or(0);
        if seq < self.last_event_seq {
            return Ok(false);
        }
        let mut monitors = Vec::with_capacity(list.len());
        for item in list {
            let monitor: MonitorView = serde_json::from_value(item.clone())
                .map_err(|error| format!("invalid monitor projection: {error}"))?;
            monitors.push(monitor);
        }
        self.replace(monitors, seq);
        Ok(true)
    }

    /// Apply one `monitor.updated` event. The payload is a full replacement.
    pub fn apply_event(&mut self, kind: &str, payload: &Value) -> bool {
        if kind != "monitor.updated" || !self.address_matches(payload) {
            return false;
        }
        let seq = payload.get("seq").and_then(Value::as_u64).unwrap_or(0);
        let Some(item) = payload.get("monitor") else {
            self.last_error = Some("监听事件缺少 monitor 投影".to_string());
            return true;
        };
        let Ok(monitor) = serde_json::from_value::<MonitorView>(item.clone()) else {
            self.last_error = Some("监听投影不是可解析的官方事件".to_string());
            return true;
        };
        if seq < self.last_event_seq {
            return false;
        }
        self.last_event_seq = self.last_event_seq.max(seq);
        let previous = self
            .monitors
            .iter()
            .find(|existing| existing.id == monitor.id)
            .cloned();
        let index = self
            .monitors
            .iter()
            .position(|existing| existing.id == monitor.id);
        match index {
            Some(index) => self.monitors[index] = monitor.clone(),
            None => {
                self.monitors.push(monitor.clone());
                self.trim();
            }
        }
        self.observe_transition(previous.as_ref(), &monitor);
        self.clamp_selection();
        true
    }

    /// Replace the whole list from a snapshot.
    fn replace(&mut self, mut monitors: Vec<MonitorView>, seq: u64) {
        self.trim_to(&mut monitors);
        self.monitors = monitors;
        self.last_event_seq = self.last_event_seq.max(seq);
        self.clamp_selection();
    }

    /// Keep the display list bounded; creation order is the bridge order.
    fn trim(&mut self) {
        while self.monitors.len() > self.max_monitors {
            self.monitors.remove(0);
        }
    }

    fn trim_to(&self, monitors: &mut Vec<MonitorView>) {
        while monitors.len() > self.max_monitors {
            monitors.remove(0);
        }
    }

    fn clamp_selection(&mut self) {
        if self.monitors.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.monitors.len() {
            self.selected = self.monitors.len() - 1;
        }
    }

    /// Surface only meaningful transitions, not every offset advancement.
    fn observe_transition(&mut self, previous: Option<&MonitorView>, current: &MonitorView) {
        if current.exhausted && previous.map(|old| !old.exhausted).unwrap_or(true) {
            self.last_notice = Some(match current.exhaust_reason.as_deref() {
                Some("expired") => "监听有效期已过，普通行投递已停止".to_string(),
                _ => "监听预算已用尽，普通行投递已停止；可显式重置".to_string(),
            });
        }
        if current.stream.is_terminal()
            && previous
                .map(|old| !old.stream.is_terminal())
                .unwrap_or(true)
        {
            self.last_notice = Some(format!(
                "监听流：{}{}",
                current.stream.label(),
                current
                    .stream_detail
                    .as_deref()
                    .map(|detail| format!("（{detail}）"))
                    .unwrap_or_default()
            ));
        }
        if let Some(delivery) = current.last_delivery() {
            let previous_state = previous.and_then(MonitorView::last_delivery);
            let changed = previous_state
                .map(|old| old.message_id != delivery.message_id || old.state != delivery.state)
                .unwrap_or(true);
            if changed && delivery.state.is_failure() {
                self.last_error = Some(format!(
                    "监听投递{}：{}",
                    delivery.state.label(),
                    delivery.detail.as_deref().unwrap_or("无详细原因")
                ));
            }
            if changed && delivery.state == DeliveryState::Unknown {
                self.last_notice = Some(delivery.state.label().to_string());
            }
        }
    }

    fn address_matches(&self, payload: &Value) -> bool {
        payload
            .get("address")
            .and_then(|address| address.get("rootSessionId"))
            .and_then(Value::as_str)
            == Some(self.root_session_id.as_str())
    }
}

impl Default for MonitorModel {
    fn default() -> Self {
        Self::new(String::new())
    }
}

/// Whether a projection's stream is terminal (helper for callers without a view).
pub fn stream_is_terminal(stream: MonitorStream) -> bool {
    stream.is_terminal()
}
