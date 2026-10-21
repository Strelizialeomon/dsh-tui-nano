//! Schedule screen state: list, create/edit form, delete confirmation, and
//! bounded delivery history. Key presses become wire actions; responses only
//! replace state the official service confirmed.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use serde_json::Value;

use crate::form::ScheduleForm;
use crate::wire::{
    parse_catalog, parse_delete, parse_history, parse_update, HistoryOutcome, HistoryPage,
    ScheduleEntry, UpdateOutcome,
};

/// Which surface the overlay shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleMode {
    /// Task list.
    List,
    /// Create/edit form.
    Form,
    /// Delete confirmation.
    Delete,
    /// Delivery history.
    History,
}

/// Visual role of one rendered row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    /// Screen title.
    Title,
    /// Ordinary text.
    Plain,
    /// Secondary explanation.
    Dim,
    /// Error.
    Error,
    /// Selected list row.
    Selected,
    /// Secondary text on the selected row.
    SelectedDim,
    /// Confirmation or result notice.
    Notice,
}

/// One rendered row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Row text.
    pub text: String,
    /// Visual role.
    pub kind: RowKind,
}

impl Row {
    fn new(text: impl Into<String>, kind: RowKind) -> Self {
        Self {
            text: text.into(),
            kind,
        }
    }
}

/// One bounded history view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryState {
    /// Task identity.
    pub id: String,
    /// Task title.
    pub title: String,
    /// Newest-first saved records.
    pub records: Vec<crate::wire::DeliveryRecord>,
    /// Missing receipts were not saved.
    pub earlier_unavailable: bool,
    /// Earlier records were confirmed pruned.
    pub earlier_pruned: bool,
    /// Retention window in days.
    pub retention_days: u64,
    /// Retention record cap.
    pub retention_records: u64,
    /// Cursor for the next older page.
    pub next_before: Option<String>,
    /// An older page is in flight.
    pub loading_older: bool,
    /// Page failure.
    pub error: Option<String>,
}

/// Request one key press asks the app to send.
#[derive(Debug, Clone, PartialEq)]
pub enum ScheduleAction {
    /// Nothing to do.
    None,
    /// Redraw only.
    Redraw,
    /// Close the overlay.
    Close,
    /// Refetch the catalog.
    Refresh,
    /// Create with the official request object.
    Create(Value),
    /// Update one entry: `id`, the complete observed record, and update fields.
    Update {
        /// Task identity.
        id: String,
        /// Complete record captured before editing.
        expected: Value,
        /// `title`, `prompt`, and optional `change`.
        fields: Value,
    },
    /// Delete one task.
    Delete {
        /// Task identity.
        id: String,
    },
    /// Read one history page.
    History {
        /// Task identity.
        id: String,
        /// Exclusive cursor for an older page.
        before: Option<String>,
    },
}

/// The schedule overlay state.
#[derive(Debug, Clone)]
pub struct ScheduleModel {
    /// Whether the overlay is visible.
    pub open: bool,
    /// Missing capability detail; when set the overlay only displays it.
    pub unsupported: Option<String>,
    /// Catalog entries in display order (active first, then ended).
    pub entries: Vec<ScheduleEntry>,
    /// Whether the ended group is expanded.
    pub show_ended: bool,
    /// Selected entry index in the visible list.
    pub selected: usize,
    /// Current surface.
    pub mode: ScheduleMode,
    /// Form, when the mode is Form.
    pub form: Option<ScheduleForm>,
    /// Pending delete target.
    pub delete_target: Option<ScheduleEntry>,
    /// History view, when the mode is History.
    pub history: Option<HistoryState>,
    /// Last error, kept until the next action.
    pub error: Option<String>,
    /// Last notice, kept until the next action.
    pub notice: Option<String>,
    /// In-flight operation description.
    pub busy: Option<String>,
    /// Whether a catalog refresh should follow the current result.
    pub wants_refresh: bool,
    /// A conflict kept the form open and asked for a refresh.
    pub conflict: bool,
}

impl Default for ScheduleModel {
    fn default() -> Self {
        Self::new()
    }
}

impl ScheduleModel {
    /// Hidden overlay.
    pub fn new() -> Self {
        Self {
            open: false,
            unsupported: None,
            entries: Vec::new(),
            show_ended: false,
            selected: 0,
            mode: ScheduleMode::List,
            form: None,
            delete_target: None,
            history: None,
            error: None,
            notice: None,
            busy: None,
            wants_refresh: false,
            conflict: false,
        }
    }

    /// Open the overlay. `unsupported` is the hello capability detail when the
    /// host did not mount the scheduler.
    pub fn open_view(&mut self, unsupported: Option<String>) {
        self.open = true;
        self.unsupported = unsupported;
        self.error = None;
        self.notice = None;
        self.mode = ScheduleMode::List;
        if self.unsupported.is_none() {
            self.busy = Some("正在读取官方任务列表…".to_string());
            self.wants_refresh = true;
        }
    }

    /// Hide the overlay. Form edits are kept for the next open only while the
    /// mode stays Form? No: the form is discarded on close, matching Cancel.
    pub fn close_view(&mut self) {
        self.open = false;
        self.mode = ScheduleMode::List;
        self.form = None;
        self.delete_target = None;
        self.history = None;
    }

    /// Selected entry, if any. The ended group is only selectable when expanded.
    pub fn selected_entry(&self) -> Option<&ScheduleEntry> {
        self.visible_entries().get(self.selected).copied()
    }

    fn visible_entries(&self) -> Vec<&ScheduleEntry> {
        self.entries
            .iter()
            .filter(|entry| self.show_ended || !entry.ended())
            .collect()
    }

    fn visible_len(&self) -> usize {
        self.visible_entries().len()
    }

    fn clamp_selection(&mut self) {
        let len = self.visible_len();
        if len == 0 {
            self.selected = 0;
        } else if self.selected >= len {
            self.selected = len - 1;
        }
    }

    /// Replace the catalog, keeping the selected identity when possible.
    pub fn apply_catalog(&mut self, value: &Value) -> Result<(), String> {
        let previous = self.selected_entry().map(|entry| entry.id.clone());
        let mut entries = parse_catalog(value)?;
        entries.sort_by_key(|entry| (entry.ended(), 0));
        self.entries = entries;
        self.reselect(previous.as_deref());
        if self.mode != ScheduleMode::Form {
            self.busy = None;
        }
        self.wants_refresh = false;
        if self.mode == ScheduleMode::History {
            // Keep the history view; the list underneath refreshes quietly.
        }
        Ok(())
    }

    fn reselect(&mut self, previous: Option<&str>) {
        if self.entries.is_empty() {
            self.selected = 0;
            return;
        }
        if let Some(id) = previous {
            if let Some(index) = self
                .visible_entries()
                .iter()
                .position(|entry| entry.id == id)
            {
                self.selected = index;
                return;
            }
        }
        self.clamp_selection();
    }

    /// Start the create form.
    pub fn begin_create(&mut self) {
        self.mode = ScheduleMode::Form;
        self.form = Some(ScheduleForm::new_create());
        self.error = None;
        self.notice = None;
        self.conflict = false;
    }

    /// Start the edit form for the selected entry.
    pub fn begin_edit(&mut self) {
        let Some(entry) = self.selected_entry().cloned() else {
            self.error = Some("没有选中的任务".to_string());
            return;
        };
        if entry.ended() {
            self.error = Some("已结束的任务只能查看或删除".to_string());
            return;
        }
        self.mode = ScheduleMode::Form;
        self.form = Some(ScheduleForm::new_edit(&entry));
        self.error = None;
        self.notice = None;
        self.conflict = false;
    }

    /// Start the delete confirmation for the selected entry.
    pub fn begin_delete(&mut self) {
        let Some(entry) = self.selected_entry().cloned() else {
            self.error = Some("没有选中的任务".to_string());
            return;
        };
        self.mode = ScheduleMode::Delete;
        self.delete_target = Some(entry);
        self.error = None;
        self.notice = None;
    }

    /// Start the history view for the selected entry.
    pub fn begin_history(&mut self) -> ScheduleAction {
        let Some(entry) = self.selected_entry().cloned() else {
            self.error = Some("没有选中的任务".to_string());
            return ScheduleAction::None;
        };
        let id = entry.id.clone();
        self.mode = ScheduleMode::History;
        self.history = Some(HistoryState {
            id: id.clone(),
            title: entry.title.clone(),
            records: Vec::new(),
            earlier_unavailable: false,
            earlier_pruned: false,
            retention_days: 30,
            retention_records: 200,
            next_before: None,
            loading_older: true,
            error: None,
        });
        self.error = None;
        self.notice = None;
        ScheduleAction::History { id, before: None }
    }

    /// Apply one key press.
    pub fn handle_key(&mut self, key: KeyEvent) -> ScheduleAction {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return ScheduleAction::None;
        }
        if !self.open {
            return ScheduleAction::None;
        }
        if self.unsupported.is_some() {
            if key.code == KeyCode::Esc {
                self.close_view();
                return ScheduleAction::Close;
            }
            return ScheduleAction::None;
        }
        if key.code == KeyCode::Esc && self.busy.is_some() {
            self.close_view();
            return ScheduleAction::Close;
        }
        if self.busy.is_some() {
            return ScheduleAction::None;
        }
        self.error = None;
        self.notice = None;
        match self.mode {
            ScheduleMode::List => self.handle_list_key(key),
            ScheduleMode::Form => self.handle_form_key(key),
            ScheduleMode::Delete => self.handle_delete_key(key),
            ScheduleMode::History => self.handle_history_key(key),
        }
    }

    fn handle_list_key(&mut self, key: KeyEvent) -> ScheduleAction {
        match key.code {
            KeyCode::Esc => {
                self.close_view();
                ScheduleAction::Close
            }
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                ScheduleAction::Redraw
            }
            KeyCode::Down => {
                if self.selected + 1 < self.visible_len() {
                    self.selected += 1;
                }
                ScheduleAction::Redraw
            }
            KeyCode::Enter => {
                self.begin_edit();
                ScheduleAction::Redraw
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                self.begin_create();
                ScheduleAction::Redraw
            }
            KeyCode::Char('d') | KeyCode::Char('D') => {
                self.begin_delete();
                ScheduleAction::Redraw
            }
            KeyCode::Char('h') | KeyCode::Char('H') => self.begin_history(),
            KeyCode::Char('e') | KeyCode::Char('E') => {
                self.show_ended = !self.show_ended;
                self.clamp_selection();
                ScheduleAction::Redraw
            }
            KeyCode::Char('r') | KeyCode::Char('R') => {
                self.busy = Some("正在刷新官方任务列表…".to_string());
                self.wants_refresh = false;
                ScheduleAction::Refresh
            }
            _ => ScheduleAction::None,
        }
    }

    fn handle_form_key(&mut self, key: KeyEvent) -> ScheduleAction {
        let Some(form) = self.form.as_mut() else {
            self.mode = ScheduleMode::List;
            return ScheduleAction::Redraw;
        };
        match key.code {
            KeyCode::Esc => {
                self.form = None;
                self.mode = ScheduleMode::List;
                self.conflict = false;
                ScheduleAction::Redraw
            }
            KeyCode::Tab | KeyCode::Down => {
                form.focus_next();
                ScheduleAction::Redraw
            }
            KeyCode::BackTab | KeyCode::Up => {
                form.focus_previous();
                ScheduleAction::Redraw
            }
            KeyCode::Left => {
                if form.focused() == crate::form::Field::Selector {
                    form.cycle_selector(false);
                } else if let Some(field) = form.focused_text() {
                    field.left();
                }
                ScheduleAction::Redraw
            }
            KeyCode::Right => {
                if form.focused() == crate::form::Field::Selector {
                    form.cycle_selector(true);
                } else if let Some(field) = form.focused_text() {
                    field.right();
                }
                ScheduleAction::Redraw
            }
            KeyCode::Char(' ') if form.focused() == crate::form::Field::Selector => {
                form.cycle_selector(true);
                ScheduleAction::Redraw
            }
            KeyCode::Backspace => {
                if let Some(field) = form.focused_text() {
                    field.backspace();
                }
                ScheduleAction::Redraw
            }
            KeyCode::Delete => {
                if let Some(field) = form.focused_text() {
                    field.delete();
                }
                ScheduleAction::Redraw
            }
            KeyCode::Home => {
                if let Some(field) = form.focused_text() {
                    for _ in 0..field.text().chars().count() {
                        field.left();
                    }
                }
                ScheduleAction::Redraw
            }
            KeyCode::End => {
                if let Some(field) = form.focused_text() {
                    for _ in 0..field.text().chars().count() {
                        field.right();
                    }
                }
                ScheduleAction::Redraw
            }
            KeyCode::Enter => self.submit_form(),
            KeyCode::Char(character)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                if let Some(field) = form.focused_text() {
                    field.insert(character);
                }
                ScheduleAction::Redraw
            }
            _ => ScheduleAction::None,
        }
    }

    fn submit_form(&mut self) -> ScheduleAction {
        let Some(form) = self.form.as_mut() else {
            return ScheduleAction::Redraw;
        };
        form.error = None;
        if let Some(entry) = form.editing.clone() {
            let fields = match form.update_fields() {
                Ok(fields) => fields,
                Err(error) => {
                    form.error = Some(error);
                    return ScheduleAction::Redraw;
                }
            };
            let id = entry.id.clone();
            let expected = entry.record.clone();
            self.busy = Some("正在提交修改…".to_string());
            return ScheduleAction::Update {
                id,
                expected,
                fields,
            };
        }
        let request = match form.create_request() {
            Ok(request) => request,
            Err(error) => {
                form.error = Some(error);
                return ScheduleAction::Redraw;
            }
        };
        self.busy = Some("正在创建任务…".to_string());
        ScheduleAction::Create(request)
    }

    fn handle_delete_key(&mut self, key: KeyEvent) -> ScheduleAction {
        match key.code {
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                self.mode = ScheduleMode::List;
                self.delete_target = None;
                ScheduleAction::Redraw
            }
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                let Some(entry) = self.delete_target.take() else {
                    self.mode = ScheduleMode::List;
                    return ScheduleAction::Redraw;
                };
                self.busy = Some("正在删除任务…".to_string());
                ScheduleAction::Delete { id: entry.id }
            }
            _ => ScheduleAction::None,
        }
    }

    fn handle_history_key(&mut self, key: KeyEvent) -> ScheduleAction {
        let Some(history) = self.history.as_mut() else {
            self.mode = ScheduleMode::List;
            return ScheduleAction::Redraw;
        };
        match key.code {
            KeyCode::Esc => {
                self.history = None;
                self.mode = ScheduleMode::List;
                ScheduleAction::Redraw
            }
            KeyCode::Enter | KeyCode::Char('p') | KeyCode::Char('P') => {
                if history.loading_older || history.next_before.is_none() {
                    return ScheduleAction::Redraw;
                }
                let before = history.next_before.clone();
                let id = history.id.clone();
                history.loading_older = true;
                ScheduleAction::History { id, before }
            }
            KeyCode::Char('r') | KeyCode::Char('R') => {
                let id = history.id.clone();
                history.loading_older = true;
                ScheduleAction::History { id, before: None }
            }
            _ => ScheduleAction::None,
        }
    }

    /// Apply a history response. `append` keeps the current records and adds
    /// the older page.
    pub fn apply_history(&mut self, value: &Value, append: bool) -> Result<(), String> {
        let page = parse_history(value)?;
        let Some(history) = self.history.as_mut() else {
            return Ok(());
        };
        history.loading_older = false;
        match page {
            HistoryOutcome::Missing { code } => {
                history.error = Some(format!("{code}：任务或游标不存在"));
                self.wants_refresh = true;
            }
            HistoryOutcome::Page(page) => {
                merge_history(history, page, append);
            }
        }
        Ok(())
    }

    /// Apply an update result.
    pub fn apply_update(&mut self, value: &Value) -> Result<(), String> {
        self.busy = None;
        match parse_update(value)? {
            UpdateOutcome::Updated => {
                self.form = None;
                self.mode = ScheduleMode::List;
                self.notice = Some("修改已由官方确认".to_string());
                self.wants_refresh = true;
                self.conflict = false;
            }
            UpdateOutcome::Miss { code } if code == "schedule_conflict" => {
                self.error = Some(format!(
                    "{code}：任务在编辑期间被修改。你的编辑已保留；刷新列表后请重新打开并提交。"
                ));
                self.conflict = true;
                self.wants_refresh = true;
            }
            UpdateOutcome::Miss { code } => {
                self.form = None;
                self.mode = ScheduleMode::List;
                self.error = Some(format!("{code}：任务已不存在或已结束"));
                self.wants_refresh = true;
            }
            UpdateOutcome::Invalid { code, message } => {
                if let Some(form) = self.form.as_mut() {
                    form.error = Some(format!("{code}：{message}"));
                } else {
                    self.error = Some(format!("{code}：{message}"));
                }
            }
        }
        Ok(())
    }

    /// Apply a delete result.
    pub fn apply_delete(&mut self, value: &Value) -> Result<(), String> {
        self.busy = None;
        self.mode = ScheduleMode::List;
        self.delete_target = None;
        let (deleted, code) = parse_delete(value)?;
        if deleted {
            self.notice = Some("任务与投递历史已删除；未来投递已停止".to_string());
        } else {
            self.notice = Some(format!(
                "{}：任务已不存在；列表将刷新",
                code.unwrap_or_else(|| "schedule_not_found".to_string())
            ));
        }
        self.wants_refresh = true;
        Ok(())
    }

    /// Report a request failure.
    pub fn fail(&mut self, message: impl Into<String>) {
        self.busy = None;
        if let Some(history) = self.history.as_mut() {
            history.loading_older = false;
        }
        self.error = Some(message.into());
        self.wants_refresh = true;
    }

    /// Report a catalog read failure without immediately requesting another
    /// catalog; a persistent host error must not become a request loop.
    pub fn fail_catalog(&mut self, message: impl Into<String>) {
        self.busy = None;
        self.error = Some(message.into());
        self.wants_refresh = false;
    }

    /// Apply a create result: the record is official, but the list still comes
    /// from a fresh catalog read.
    pub fn apply_create(&mut self, _value: &Value) -> Result<(), String> {
        self.busy = None;
        self.form = None;
        self.mode = ScheduleMode::List;
        self.notice = Some("任务已由官方确认创建".to_string());
        self.wants_refresh = true;
        Ok(())
    }

    /// Render rows for the current width.
    pub fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        if let Some(detail) = &self.unsupported {
            rows.push(Row::new("定时任务不可用", RowKind::Title));
            rows.push(Row::new(detail.clone(), RowKind::Error));
            rows.push(Row::new("Esc 关闭", RowKind::Dim));
            return rows;
        }
        match self.mode {
            ScheduleMode::List => self.render_list(&mut rows),
            ScheduleMode::Form => self.render_form(&mut rows),
            ScheduleMode::Delete => self.render_delete(&mut rows),
            ScheduleMode::History => self.render_history(&mut rows),
        }
        rows
    }

    fn render_list(&self, rows: &mut Vec<Row>) {
        let active = self.entries.iter().filter(|entry| !entry.ended()).count();
        let ended = self.entries.len() - active;
        rows.push(Row::new(
            format!("定时任务 · 官方 schedule · 活跃 {active} · 已结束 {ended}"),
            RowKind::Title,
        ));
        rows.push(Row::new(
            "投递会唤醒一次模型请求并可能产生费用；回执只确认进入 inbox，不代表模型已执行。",
            RowKind::Dim,
        ));
        rows.push(Row::new(
            "官方不保证严格一次：会话回执写入后若任务写入失败或宿主崩溃，同一条提醒可能再次投递。",
            RowKind::Dim,
        ));
        rows.push(Row::new(
            "关闭会话归档时，有活跃任务会被官方拒绝；选择停止会删除全部活跃任务，取消归档不会恢复。",
            RowKind::Dim,
        ));
        self.push_status(rows);
        let active: Vec<&ScheduleEntry> =
            self.entries.iter().filter(|entry| !entry.ended()).collect();
        let ended: Vec<&ScheduleEntry> =
            self.entries.iter().filter(|entry| entry.ended()).collect();
        if self.entries.is_empty() {
            rows.push(Row::new("还没有任务。按 n 新建。", RowKind::Dim));
        }
        if !active.is_empty() {
            rows.push(Row::new("活跃", RowKind::Plain));
        }
        for (index, entry) in active.iter().enumerate() {
            self.push_entry_row(rows, entry, index);
        }
        if !ended.is_empty() {
            if self.show_ended {
                rows.push(Row::new("已结束（仍可查看，直到显式删除）", RowKind::Plain));
                for (offset, entry) in ended.iter().enumerate() {
                    self.push_entry_row(rows, entry, active.len() + offset);
                }
            } else {
                rows.push(Row::new(
                    format!("已结束 {} 项（按 e 展开）", ended.len()),
                    RowKind::Dim,
                ));
            }
        }
        rows.push(Row::new(
            "↑↓ 选择 · Enter 编辑 · n 新建 · d 删除 · h 投递历史 · e 展开/收起已结束 · r 刷新 · Esc 关闭",
            RowKind::Dim,
        ));
    }

    fn push_entry_row(&self, rows: &mut Vec<Row>, entry: &ScheduleEntry, index: usize) {
        let selected = index == self.selected;
        let marker = if selected { "▶" } else { " " };
        let kind = if selected {
            RowKind::Selected
        } else {
            RowKind::Plain
        };
        rows.push(Row::new(
            format!(
                "{marker} {} [{}] {} · {}",
                entry.title,
                if entry.ended() { "已结束" } else { "活跃" },
                entry.rule_label(),
                entry.scheduled_at
            ),
            kind,
        ));
        if selected {
            let receipt = match &entry.last_delivery {
                Some(receipt) => format!(
                    "   最近回执：已进入 inbox · {} · 目标 {}",
                    receipt.delivered_at, receipt.scheduled_at
                ),
                None => "   最近回执：暂无".to_string(),
            };
            rows.push(Row::new(receipt, RowKind::SelectedDim));
            if let Some(zone) = entry.zone_label() {
                rows.push(Row::new(format!("   时区 {zone}"), RowKind::SelectedDim));
            }
        }
    }

    fn render_form(&self, rows: &mut Vec<Row>) {
        let Some(form) = self.form.as_ref() else {
            rows.push(Row::new("表单已关闭", RowKind::Dim));
            return;
        };
        let heading = if form.is_editing() {
            "编辑定时任务 · 时间类型选「保持原时间」时不改动执行时间"
        } else {
            "新建定时任务 · 六种时间类型任选其一"
        };
        rows.push(Row::new(heading, RowKind::Title));
        for (index, field) in form.fields().iter().enumerate() {
            let focused = index == form.field_index;
            let marker = if focused { "▶" } else { " " };
            let value = match field {
                crate::form::Field::Title => form.title.text(),
                crate::form::Field::Prompt => form.prompt.text(),
                crate::form::Field::Selector => form.selector.label(),
                crate::form::Field::After => form.after.text(),
                crate::form::Field::At => form.at.text(),
                crate::form::Field::Every => form.every.text(),
                crate::form::Field::DailyTime => form.daily_time.text(),
                crate::form::Field::DailyZone => form.daily_zone.text(),
                crate::form::Field::WeeklyTime => form.weekly_time.text(),
                crate::form::Field::WeeklyZone => form.weekly_zone.text(),
                crate::form::Field::WeeklyDays => form.weekly_days.text(),
                crate::form::Field::CronExpression => form.cron_expression.text(),
                crate::form::Field::CronZone => form.cron_zone.text(),
            };
            rows.push(Row::new(
                format!("{marker} {}：{value}", field.label()),
                if focused {
                    RowKind::Selected
                } else {
                    RowKind::Plain
                },
            ));
        }
        if let Some(error) = &form.error {
            rows.push(Row::new(format!("错误：{error}"), RowKind::Error));
        }
        rows.push(Row::new(
            "时间类型用 ←/→ 或空格切换；每日/每周/cron 必须写明 IANA 时区。",
            RowKind::Dim,
        ));
        rows.push(Row::new(
            "创建与投递都会产生模型调用和费用；投递只是把提醒放进原会话 inbox。",
            RowKind::Dim,
        ));
        self.push_status(rows);
        rows.push(Row::new(
            "Tab/↑↓ 换行 · ←→ 切时间类型或移光标 · Enter 提交 · Esc 取消",
            RowKind::Dim,
        ));
    }

    fn render_delete(&self, rows: &mut Vec<Row>) {
        rows.push(Row::new("删除任务？", RowKind::Title));
        if let Some(entry) = &self.delete_target {
            rows.push(Row::new(
                format!("{} · {}", entry.title, entry.rule_label()),
                RowKind::Plain,
            ));
        }
        rows.push(Row::new(
            "删除会停止未来投递，并删除任务与全部投递历史；已经排队的消息不受影响。",
            RowKind::Plain,
        ));
        rows.push(Row::new("这是不可逆的官方动作。", RowKind::Error));
        self.push_status(rows);
        rows.push(Row::new("y / Enter 删除 · n / Esc 取消", RowKind::Dim));
    }

    fn render_history(&self, rows: &mut Vec<Row>) {
        let Some(history) = &self.history else {
            rows.push(Row::new("没有投递历史", RowKind::Dim));
            return;
        };
        rows.push(Row::new(
            format!("投递历史 · {}", history.title),
            RowKind::Title,
        ));
        rows.push(Row::new(
            format!(
                "保留窗口：{} 天 / {} 条；历史有界，读取不裁剪。",
                history.retention_days, history.retention_records
            ),
            RowKind::Dim,
        ));
        rows.push(Row::new(
            "回执确认「已进入 inbox」，不代表模型已执行，也不代表任务完成。",
            RowKind::Dim,
        ));
        if history.earlier_unavailable {
            rows.push(Row::new(
                "更早记录未保存（earlierRecordsUnavailable）：缺失的回执不会重建。",
                RowKind::Error,
            ));
        }
        if history.earlier_pruned {
            rows.push(Row::new(
                "更早记录已被确认裁剪（earlierRecordsPruned）：由后续投递追加时移除。",
                RowKind::Notice,
            ));
        }
        if history.records.is_empty() && !history.loading_older {
            rows.push(Row::new("还没有投递记录。", RowKind::Dim));
        }
        for record in &history.records {
            rows.push(Row::new(
                format!(
                    "已进入 inbox · 目标 {} · 回执 {} · 消息 {}",
                    record.scheduled_at, record.delivered_at, record.message_id
                ),
                RowKind::Plain,
            ));
            if let Some(prompt) = &record.prompt {
                rows.push(Row::new(format!("   发送内容：{prompt}"), RowKind::Dim));
            }
        }
        if let Some(error) = &history.error {
            rows.push(Row::new(format!("错误：{error}"), RowKind::Error));
        }
        self.push_status(rows);
        if history.loading_older {
            rows.push(Row::new("正在读取…", RowKind::Dim));
        } else if history.next_before.is_some() {
            rows.push(Row::new(
                "Enter/p 读取更早一页 · r 重新读取 · Esc 返回",
                RowKind::Dim,
            ));
        } else {
            rows.push(Row::new(
                "已经读到最早已保存的记录 · r 重新读取 · Esc 返回",
                RowKind::Dim,
            ));
        }
    }

    fn push_status(&self, rows: &mut Vec<Row>) {
        if let Some(busy) = &self.busy {
            rows.push(Row::new(busy.clone(), RowKind::Dim));
        }
        if let Some(error) = &self.error {
            rows.push(Row::new(format!("错误：{error}"), RowKind::Error));
        }
        if let Some(notice) = &self.notice {
            rows.push(Row::new(notice.clone(), RowKind::Notice));
        }
    }

    /// Take the refresh flag after a response.
    pub fn take_refresh(&mut self) -> bool {
        std::mem::take(&mut self.wants_refresh)
    }
}

fn merge_history(history: &mut HistoryState, page: HistoryPage, append: bool) {
    if !append {
        history.records.clear();
    }
    history.id = page.id;
    history.records.extend(page.records);
    history.earlier_unavailable = page.earlier_records_unavailable;
    history.earlier_pruned = page.earlier_records_pruned;
    history.retention_days = page.retention_days;
    history.retention_records = page.retention_records;
    history.next_before = page.next_before;
    history.error = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn catalog() -> Value {
        json!({
            "address": {"rootSessionId": "root"},
            "entries": [
                {
                    "id": "s-2", "kind": "every", "title": "活跃", "prompt": "p",
                    "scheduledAt": "2099-01-01T00:00:00.000Z", "everySeconds": 60,
                    "sessionId": "root", "status": "active"
                },
                {
                    "id": "s-1", "kind": "at", "title": "结束", "prompt": "p",
                    "scheduledAt": "2020-01-01T00:00:00.000Z",
                    "sessionId": "root", "status": "inactive",
                    "lastDelivery": {
                        "scheduledAt": "2020-01-01T00:00:00.000Z",
                        "deliveredAt": "2020-01-01T00:00:01.000Z",
                        "messageId": "m-1"
                    }
                }
            ]
        })
    }

    #[test]
    fn catalog_orders_active_before_ended_and_strips_wrappers() {
        let mut model = ScheduleModel::new();
        model.open_view(None);
        model.apply_catalog(&catalog()).unwrap();
        assert_eq!(model.entries[0].id, "s-2");
        assert_eq!(model.entries[1].id, "s-1");
        assert!(model.entries[1].record.get("status").is_none());
        assert!(model.entries[1].record.get("lastDelivery").is_none());
        assert_eq!(
            model.entries[1].last_delivery.as_ref().unwrap().message_id,
            "m-1"
        );
    }

    #[test]
    fn selection_survives_a_refresh_by_identity() {
        let mut model = ScheduleModel::new();
        model.open_view(None);
        model.apply_catalog(&catalog()).unwrap();
        model.show_ended = true;
        model.selected = 1;
        let mut value = catalog();
        value["entries"].as_array_mut().unwrap().reverse();
        model.apply_catalog(&value).unwrap();
        assert_eq!(model.selected_entry().unwrap().id, "s-1");
    }

    #[test]
    fn conflict_keeps_the_form_and_asks_for_refresh() {
        let mut model = ScheduleModel::new();
        model.open_view(None);
        model.apply_catalog(&catalog()).unwrap();
        model.begin_edit();
        model.busy = None;
        model
            .apply_update(&json!({"id": "s-2", "updated": false, "code": "schedule_conflict"}))
            .unwrap();
        assert!(model.form.is_some());
        assert!(model.conflict);
        assert!(model
            .error
            .as_deref()
            .unwrap()
            .contains("schedule_conflict"));
        assert!(model.take_refresh());
    }

    #[test]
    fn form_escape_returns_to_the_list() {
        let mut model = ScheduleModel::new();
        model.open_view(None);
        model.busy = None;
        model.begin_create();
        let action = model.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(action, ScheduleAction::Redraw);
        assert_eq!(model.mode, ScheduleMode::List);
    }

    #[test]
    fn unsupported_overlay_refuses_every_action_but_close() {
        let mut model = ScheduleModel::new();
        model.open_view(Some("调度未挂载".into()));
        assert_eq!(
            model.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE)),
            ScheduleAction::None
        );
        assert_eq!(model.mode, ScheduleMode::List);
        assert_eq!(
            model.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            ScheduleAction::Close
        );
        assert!(!model.open);
    }

    #[test]
    fn history_flags_stay_separate() {
        let mut model = ScheduleModel::new();
        model.open_view(None);
        model.apply_catalog(&catalog()).unwrap();
        model.show_ended = true;
        model.selected = 1;
        model.begin_history();
        model
            .apply_history(
                &json!({
                    "id": "s-1",
                    "records": [],
                    "earlierRecordsUnavailable": true,
                    "earlierRecordsPruned": true,
                    "retention": {"days": 30, "records": 200}
                }),
                false,
            )
            .unwrap();
        let text: Vec<String> = model.rows().into_iter().map(|row| row.text).collect();
        assert!(text
            .iter()
            .any(|line| line.contains("earlierRecordsUnavailable")));
        assert!(text
            .iter()
            .any(|line| line.contains("earlierRecordsPruned")));
    }

    #[test]
    fn a_post_flush_write_failure_is_never_shown_as_success() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../bridge/test/fixtures/schedule-post-flush-write-failure.json"
        ))
        .unwrap();
        let mut model = ScheduleModel::new();
        model.open_view(None);
        model.apply_catalog(&fixture["catalog"]).unwrap();
        let rows: Vec<String> = model.rows().into_iter().map(|row| row.text).collect();
        assert!(rows.iter().any(|row| row.contains("可能再次投递")));
        assert!(rows.iter().any(|row| row.contains("最近回执：暂无")));
        assert!(!rows.iter().any(|row| row.contains("已创建")));
        model.selected = 0;
        model.begin_history();
        model.apply_history(&fixture["history"], false).unwrap();
        let rows: Vec<String> = model.rows().into_iter().map(|row| row.text).collect();
        assert!(rows.iter().any(|row| row.contains("还没有投递记录")));
        assert!(rows.iter().any(|row| row.contains("不代表任务完成")));
    }

    #[test]
    fn a_catalog_failure_does_not_retry_itself() {
        let mut model = ScheduleModel::new();
        model.open_view(None);
        model.fail_catalog("nano/closed");
        assert!(!model.take_refresh());
        assert!(model.error.as_deref().unwrap().contains("nano/closed"));
    }

    #[test]
    fn any_failure_clears_a_stuck_history_loading() {
        let mut model = ScheduleModel::new();
        model.open_view(None);
        model.apply_catalog(&catalog()).unwrap();
        model.begin_history();
        model.history.as_mut().unwrap().loading_older = true;
        model.fail("host closed");
        assert!(!model.history.as_ref().unwrap().loading_older);
    }

    #[test]
    fn the_list_states_the_archive_consequence() {
        let mut model = ScheduleModel::new();
        model.open_view(None);
        model.apply_catalog(&catalog()).unwrap();
        let rows: Vec<String> = model.rows().into_iter().map(|row| row.text).collect();
        assert!(rows.iter().any(|row| row.contains("归档")));
    }

    #[test]
    fn ended_tasks_are_collapsed_until_expanded() {
        let mut model = ScheduleModel::new();
        model.open_view(None);
        model.apply_catalog(&catalog()).unwrap();
        let collapsed: Vec<String> = model.rows().into_iter().map(|row| row.text).collect();
        assert!(collapsed.iter().any(|row| row.contains("已结束 1 项")));
        assert!(!collapsed.iter().any(|row| row.contains("结束 [已结束]")));
        model.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        let expanded: Vec<String> = model.rows().into_iter().map(|row| row.text).collect();
        assert!(expanded.iter().any(|row| row.contains("结束 [已结束]")));
    }

    #[test]
    fn ended_task_cannot_be_edited() {
        let mut model = ScheduleModel::new();
        model.open_view(None);
        model.apply_catalog(&catalog()).unwrap();
        model.show_ended = true;
        model.selected = 1;
        model.begin_edit();
        assert_eq!(model.mode, ScheduleMode::List);
        assert!(model.error.as_deref().unwrap().contains("已结束"));
    }

    #[test]
    fn update_action_carries_the_complete_expected_record() {
        let mut model = ScheduleModel::new();
        model.open_view(None);
        model.apply_catalog(&catalog()).unwrap();
        model.selected = 0;
        model.begin_edit();
        match model.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)) {
            ScheduleAction::Update { expected, .. } => {
                assert_eq!(expected["id"], json!("s-2"));
                assert_eq!(expected["everySeconds"], json!(60));
            }
            other => panic!("unexpected action {other:?}"),
        }
    }
}
