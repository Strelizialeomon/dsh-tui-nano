//! Read-only task list and output-follow state.
//!
//! Every projection and byte comes from the official registry; this model only
//! keeps a bounded display tail. Reads are non-consuming by construction: the
//! only read the model issues is `readAt`, and offsets only move forward when
//! the bridge answers.

use std::collections::HashMap;

use serde_json::Value;

use crate::view::{JobOutputRead, JobView};

/// Maximum jobs retained for display.
pub const MAX_JOBS: usize = 200;

/// Maximum output bytes retained per followed job.
pub const MAX_OUTPUT_BYTES: usize = 256 * 1024;

/// One followed job's bounded display tail.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OutputBuffer {
    /// Absolute offset to resume from.
    pub next: u64,
    /// Retained display text.
    pub text: String,
    /// Older local bytes were dropped to stay within the display budget.
    pub head_truncated: bool,
    /// The official ring reported that bytes before the read were lost.
    pub lossy: bool,
    /// Number of discontinuity markers shown.
    pub gaps: usize,
}

/// Observable job state for one root session.
#[derive(Debug, Clone)]
pub struct JobsModel {
    root_session_id: String,
    jobs: Vec<JobView>,
    selected: usize,
    detail: Option<String>,
    buffers: HashMap<String, OutputBuffer>,
    last_notice: Option<String>,
    last_error: Option<String>,
    max_jobs: usize,
    max_output_bytes: usize,
}

impl JobsModel {
    /// Build for one root session.
    pub fn new(root_session_id: impl Into<String>) -> Self {
        Self {
            root_session_id: root_session_id.into(),
            jobs: Vec::new(),
            selected: 0,
            detail: None,
            buffers: HashMap::new(),
            last_notice: None,
            last_error: None,
            max_jobs: MAX_JOBS,
            max_output_bytes: MAX_OUTPUT_BYTES,
        }
    }

    /// Use smaller display bounds (tests and narrow terminals).
    pub fn with_bounds(mut self, max_jobs: usize, max_output_bytes: usize) -> Self {
        self.max_jobs = max_jobs.max(1);
        self.max_output_bytes = max_output_bytes.max(1);
        self
    }

    /// Current official projections in registration order.
    pub fn jobs(&self) -> &[JobView] {
        &self.jobs
    }

    /// Whether no visible job exists.
    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }

    /// Number of visible jobs.
    pub fn len(&self) -> usize {
        self.jobs.len()
    }

    /// Number of live (running or stopping) jobs.
    pub fn active_count(&self) -> usize {
        self.jobs.iter().filter(|job| job.is_active()).count()
    }

    /// Selected index, clamped to the visible list.
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Selected projection.
    pub fn selected_job(&self) -> Option<&JobView> {
        self.jobs.get(self.selected)
    }

    /// Move the selection up.
    pub fn select_previous(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    /// Move the selection down.
    pub fn select_next(&mut self) {
        if self.selected + 1 < self.jobs.len() {
            self.selected += 1;
        }
    }

    /// Select one job by id, when it is visible.
    pub fn select(&mut self, job_id: &str) {
        if let Some(index) = self.jobs.iter().position(|job| job.id == job_id) {
            self.selected = index;
        }
    }

    /// The job whose output panel is open, if any.
    pub fn detail_id(&self) -> Option<&str> {
        self.detail.as_deref()
    }

    /// Whether a detail panel is open.
    pub fn detail_open(&self) -> bool {
        self.detail.is_some()
    }

    /// Open the detail panel for one visible job.
    pub fn open_detail(&mut self, job_id: &str) -> bool {
        if self.jobs.iter().any(|job| job.id == job_id) {
            self.detail = Some(job_id.to_string());
            // Only one detail is shown at a time; keep the display cache
            // bounded instead of retaining a tail for every opened job.
            self.buffers.retain(|id, _| id.as_str() == job_id);
            self.select(job_id);
            true
        } else {
            false
        }
    }

    /// Close the detail panel.
    pub fn close_detail(&mut self) {
        self.detail = None;
    }

    /// Projection of the open detail job.
    pub fn detail_job(&self) -> Option<&JobView> {
        let id = self.detail.as_deref()?;
        self.jobs.iter().find(|job| job.id == id)
    }

    /// Find one visible job by id.
    pub fn job(&self, job_id: &str) -> Option<&JobView> {
        self.jobs.iter().find(|job| job.id == job_id)
    }

    /// Last settlement notice.
    pub fn last_notice(&self) -> Option<&str> {
        self.last_notice.as_deref()
    }

    /// Last parse or read error.
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Clear transient diagnostics.
    pub fn clear_diagnostics(&mut self) {
        self.last_notice = None;
        self.last_error = None;
    }

    /// Apply one `nano.jobs.snapshot` result. Invalid payloads leave the
    /// current projection untouched.
    pub fn apply_snapshot(&mut self, value: &Value) -> Result<(), String> {
        let list = value
            .get("jobs")
            .and_then(Value::as_array)
            .ok_or_else(|| "snapshot is missing the jobs array".to_string())?;
        let mut jobs = Vec::with_capacity(list.len());
        for item in list {
            let job: JobView = serde_json::from_value(item.clone())
                .map_err(|error| format!("invalid job projection: {error}"))?;
            jobs.push(job);
        }
        self.jobs = jobs;
        self.trim_jobs();
        self.selected = self.selected.min(self.jobs.len().saturating_sub(1));
        if let Some(detail) = self.detail.clone() {
            if !self.jobs.iter().any(|job| job.id == detail) {
                self.detail = None;
            }
        }
        let visible: Vec<&String> = self.jobs.iter().map(|job| &job.id).collect();
        self.buffers.retain(|id, _| visible.contains(&id));
        Ok(())
    }

    /// Apply one `jobs.event` or `jobs.output` payload.
    ///
    /// `jobs.output` only announces an advancement; the caller schedules the
    /// non-consuming follow. It never changes projection or cursor state.
    pub fn apply_event(&mut self, kind: &str, payload: &Value) -> bool {
        if !self.address_matches(payload) {
            return false;
        }
        match kind {
            "jobs.event" => self.apply_job_event(payload),
            "jobs.output" => false,
            _ => false,
        }
    }

    /// The job id carried by a `jobs.output` event, when it belongs here.
    pub fn output_job_id(&self, payload: &Value) -> Option<String> {
        if !self.address_matches(payload) {
            return None;
        }
        payload
            .get("jobId")
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    fn apply_job_event(&mut self, payload: &Value) -> bool {
        let event_type = payload.get("type").and_then(Value::as_str).unwrap_or("");
        let Some(item) = payload.get("job") else {
            return false;
        };
        let Ok(job) = serde_json::from_value::<JobView>(item.clone()) else {
            self.last_error = Some("官方任务事件不是可解析的投影".to_string());
            return true;
        };
        if event_type == "removed" {
            return self.remove(&job.id);
        }
        let became_terminal = event_type == "settled" && job.status().is_terminal();
        let index = self.jobs.iter().position(|existing| existing.id == job.id);
        match index {
            Some(index) => self.jobs[index] = job.clone(),
            None => {
                self.jobs.push(job.clone());
                self.trim_jobs();
            }
        }
        self.clamp_selection();
        if became_terminal {
            self.last_notice = Some(format!(
                "任务 {} {}（{}）",
                job.id,
                job.status().label(),
                job.detail.as_deref().unwrap_or("无可用的结束原因")
            ));
            // A settled ring is final; keep the buffer for the open detail but
            // drop the cursor so a reopen reads the retained tail again.
            if self.detail.as_deref() != Some(job.id.as_str()) {
                self.buffers.remove(&job.id);
            }
        }
        true
    }

    fn remove(&mut self, job_id: &str) -> bool {
        let before = self.jobs.len();
        self.jobs.retain(|job| job.id != job_id);
        self.buffers.remove(job_id);
        if self.detail.as_deref() == Some(job_id) {
            self.detail = None;
        }
        self.clamp_selection();
        self.jobs.len() != before
    }

    fn trim_jobs(&mut self) {
        while self.jobs.len() > self.max_jobs {
            if let Some(index) = self.jobs.iter().position(|job| !job.is_active()) {
                self.jobs.remove(index);
            } else {
                self.jobs.remove(0);
            }
        }
    }

    fn clamp_selection(&mut self) {
        if self.jobs.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.jobs.len() {
            self.selected = self.jobs.len() - 1;
        }
    }

    fn address_matches(&self, payload: &Value) -> bool {
        payload
            .get("address")
            .and_then(|address| address.get("rootSessionId"))
            .and_then(Value::as_str)
            == Some(self.root_session_id.as_str())
    }

    /// Next absolute offset for a followed job. The first read starts at zero
    /// so the official `lossy` fact is reported rather than guessed.
    pub fn next_offset(&self, job_id: &str) -> u64 {
        self.buffers
            .get(job_id)
            .map(|buffer| buffer.next)
            .unwrap_or(0)
    }

    /// Whether this job has ever been followed.
    pub fn is_followed(&self, job_id: &str) -> bool {
        self.buffers.contains_key(job_id)
    }

    /// Bounded display tail for one job.
    pub fn output(&self, job_id: &str) -> Option<&OutputBuffer> {
        self.buffers.get(job_id)
    }

    /// Append one non-consuming read. Returns whether anything changed.
    pub fn apply_follow(&mut self, job_id: &str, value: &Value) -> Result<bool, String> {
        let read: JobOutputRead = serde_json::from_value(value.clone())
            .map_err(|error| format!("invalid job output: {error}"))?;
        let mut changed = false;
        let max_bytes = self.max_output_bytes;
        let buffer = self
            .buffers
            .entry(job_id.to_string())
            .or_insert_with(|| OutputBuffer {
                next: 0,
                ..OutputBuffer::default()
            });
        if read.lossy && !buffer.lossy {
            buffer.lossy = true;
            push_marker(buffer, "[输出缺口（已裁剪）：请求位置早于官方 ring 保留范围]");
            changed = true;
        }
        for chunk in &read.chunks {
            if chunk.gap_before {
                push_marker(buffer, "[输出缺口：中间有未显示内容]");
                buffer.gaps += 1;
                changed = true;
            }
            if chunk.text.is_empty() {
                continue;
            }
            match chunk.channel.as_deref() {
                Some("stderr") => {
                    push_marker(buffer, "[stderr]");
                    buffer.text.push_str(&chunk.text);
                }
                Some("log") => {
                    push_marker(buffer, "[log]");
                    buffer.text.push_str(&chunk.text);
                }
                _ => buffer.text.push_str(&chunk.text),
            }
            changed = true;
        }
        if read.next > buffer.next {
            buffer.next = read.next;
        }
        if buffer.text.len() > max_bytes {
            buffer.text = tail_on_char_boundary(&buffer.text, max_bytes);
            buffer.head_truncated = true;
            changed = true;
        }
        Ok(changed)
    }

    /// Record a read failure without moving the cursor.
    pub fn record_error(&mut self, message: impl Into<String>) {
        self.last_error = Some(message.into());
    }
}

/// Append a marker line, avoiding adjacent duplicates.
fn push_marker(buffer: &mut OutputBuffer, marker: &str) {
    if buffer.text.ends_with(marker) {
        return;
    }
    if !buffer.text.is_empty() && !buffer.text.ends_with('\n') {
        buffer.text.push('\n');
    }
    buffer.text.push_str(marker);
    buffer.text.push('\n');
}

/// Keep the last `max_bytes` bytes without splitting a UTF-8 character.
fn tail_on_char_boundary(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut start = text.len() - max_bytes;
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    text[start..].to_string()
}
