//! Create/edit form state and request construction.
//!
//! The form chooses one of the six official selectors and builds the exact
//! official request shape. It performs only structural checks that would
//! otherwise send the wrong JSON type (numbers and weekday lists); title,
//! prompt, zone, cron, and target validation stay with the official service so
//! the UI can show its stable codes and readable reasons.

use serde_json::{json, Value};

use crate::wire::ScheduleEntry;

/// One editable text field with a character-indexed cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextField {
    text: String,
    cursor: usize,
}

impl TextField {
    /// Empty field.
    pub fn new() -> Self {
        Self {
            text: String::new(),
            cursor: 0,
        }
    }

    /// Field prefilled with `text` and the cursor at the end.
    pub fn with_text(text: impl Into<String>) -> Self {
        let text = text.into();
        let cursor = text.chars().count();
        Self { text, cursor }
    }

    /// Current text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Replace the text and put the cursor at the end.
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = self.text.chars().count();
    }

    /// Whether the field holds no characters.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Insert one character at the cursor.
    pub fn insert(&mut self, character: char) {
        let byte = byte_index(&self.text, self.cursor);
        self.text.insert(byte, character);
        self.cursor += 1;
    }

    /// Insert a pasted string at the cursor, ignoring control characters.
    pub fn insert_str(&mut self, value: &str) {
        for character in value.chars().filter(|character| !character.is_control()) {
            self.insert(character);
        }
    }

    /// Remove the character before the cursor.
    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = byte_index(&self.text, self.cursor - 1);
        let end = byte_index(&self.text, self.cursor);
        self.text.replace_range(start..end, "");
        self.cursor -= 1;
    }

    /// Remove the character under the cursor.
    pub fn delete(&mut self) {
        if self.cursor >= self.text.chars().count() {
            return;
        }
        let start = byte_index(&self.text, self.cursor);
        let end = byte_index(&self.text, self.cursor + 1);
        self.text.replace_range(start..end, "");
    }

    /// Move the cursor left.
    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Move the cursor right.
    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.text.chars().count());
    }
}

impl Default for TextField {
    fn default() -> Self {
        Self::new()
    }
}

fn byte_index(text: &str, character_index: usize) -> usize {
    text.char_indices()
        .nth(character_index)
        .map(|(index, _)| index)
        .unwrap_or(text.len())
}

/// The timing selector currently chosen by the form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selector {
    /// Keep the stored rule (edit only).
    Keep,
    /// One-shot relative delay (create only).
    After,
    /// One-shot absolute instant.
    At,
    /// Fixed interval.
    Every,
    /// Daily wall clock.
    Daily,
    /// Weekly wall clock.
    Weekly,
    /// Five-field cron.
    Cron,
}

impl Selector {
    /// Short label for the form row.
    pub fn label(self) -> &'static str {
        match self {
            Self::Keep => "保持原时间",
            Self::After => "一次性延迟",
            Self::At => "绝对时间",
            Self::Every => "固定间隔",
            Self::Daily => "每日",
            Self::Weekly => "每周",
            Self::Cron => "cron",
        }
    }

    /// Next selector in the cycle. Creating never offers `Keep`; editing always does.
    pub fn next(self, editing: bool) -> Self {
        if editing {
            match self {
                Self::Keep => Self::At,
                Self::At => Self::Every,
                Self::Every => Self::Daily,
                Self::Daily => Self::Weekly,
                Self::Weekly => Self::Cron,
                Self::Cron => Self::Keep,
                Self::After => Self::Keep,
            }
        } else {
            match self {
                Self::After => Self::At,
                Self::At => Self::Every,
                Self::Every => Self::Daily,
                Self::Daily => Self::Weekly,
                Self::Weekly => Self::Cron,
                Self::Cron | Self::Keep => Self::After,
            }
        }
    }

    /// Previous selector in the cycle.
    pub fn previous(self, editing: bool) -> Self {
        if editing {
            match self {
                Self::Keep => Self::Cron,
                Self::At => Self::Keep,
                Self::Every => Self::At,
                Self::Daily => Self::Every,
                Self::Weekly => Self::Daily,
                Self::Cron => Self::Weekly,
                Self::After => Self::Keep,
            }
        } else {
            match self {
                Self::After | Self::Keep => Self::Cron,
                Self::At => Self::After,
                Self::Every => Self::At,
                Self::Daily => Self::Every,
                Self::Weekly => Self::Daily,
                Self::Cron => Self::Weekly,
            }
        }
    }
}

/// Focusable rows of the form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    /// Title.
    Title,
    /// Prompt.
    Prompt,
    /// Selector.
    Selector,
    /// Delay seconds.
    After,
    /// Absolute instant.
    At,
    /// Interval seconds.
    Every,
    /// Daily local time.
    DailyTime,
    /// Daily zone.
    DailyZone,
    /// Weekly local time.
    WeeklyTime,
    /// Weekly zone.
    WeeklyZone,
    /// Weekly weekday set.
    WeeklyDays,
    /// Cron expression.
    CronExpression,
    /// Cron zone.
    CronZone,
}

impl Field {
    /// Field label shown in the form.
    pub fn label(self) -> &'static str {
        match self {
            Self::Title => "标题",
            Self::Prompt => "指令",
            Self::Selector => "时间类型",
            Self::After => "延迟秒数",
            Self::At => "绝对时间（RFC3339）",
            Self::Every => "间隔秒数（≥60）",
            Self::DailyTime => "每日时间 HH:MM:SS",
            Self::DailyZone => "时区（IANA）",
            Self::WeeklyTime => "每周时间 HH:MM:SS",
            Self::WeeklyZone => "时区（IANA）",
            Self::WeeklyDays => "星期（1-7，逗号分隔）",
            Self::CronExpression => "cron 表达式（五字段）",
            Self::CronZone => "时区（IANA）",
        }
    }
}

/// Create/edit form.
#[derive(Debug, Clone, PartialEq)]
pub struct ScheduleForm {
    /// Entry being edited, or `None` when creating.
    pub editing: Option<ScheduleEntry>,
    /// Title field.
    pub title: TextField,
    /// Prompt field.
    pub prompt: TextField,
    /// Chosen selector.
    pub selector: Selector,
    /// Delay seconds.
    pub after: TextField,
    /// Absolute instant.
    pub at: TextField,
    /// Interval seconds.
    pub every: TextField,
    /// Daily time.
    pub daily_time: TextField,
    /// Daily zone.
    pub daily_zone: TextField,
    /// Weekly time.
    pub weekly_time: TextField,
    /// Weekly zone.
    pub weekly_zone: TextField,
    /// Weekly weekdays.
    pub weekly_days: TextField,
    /// Cron expression.
    pub cron_expression: TextField,
    /// Cron zone.
    pub cron_zone: TextField,
    /// Focused row.
    pub field_index: usize,
    /// Whether the user changed the timing selector while editing.
    pub timing_dirty: bool,
    /// Local validation message.
    pub error: Option<String>,
}

impl ScheduleForm {
    /// New one-off form.
    pub fn new_create() -> Self {
        let zone = local_time_zone();
        Self {
            editing: None,
            title: TextField::new(),
            prompt: TextField::new(),
            selector: Selector::After,
            after: TextField::with_text("60"),
            at: TextField::new(),
            every: TextField::with_text("300"),
            daily_time: TextField::with_text("09:00:00"),
            daily_zone: TextField::with_text(zone.clone()),
            weekly_time: TextField::with_text("09:00:00"),
            weekly_zone: TextField::with_text(zone.clone()),
            weekly_days: TextField::with_text("1"),
            cron_expression: TextField::with_text("*/15 * * * *"),
            cron_zone: TextField::with_text(zone),
            field_index: 0,
            timing_dirty: false,
            error: None,
        }
    }

    /// Edit form prefilled from one catalog entry.
    pub fn new_edit(entry: &ScheduleEntry) -> Self {
        let mut form = Self::new_create();
        form.editing = Some(entry.clone());
        form.title = TextField::with_text(entry.title.clone());
        form.prompt = TextField::with_text(entry.prompt.clone());
        form.selector = Selector::Keep;
        form.timing_dirty = false;
        if let Some(seconds) = entry.after_seconds {
            form.after = TextField::with_text(seconds.to_string());
        }
        if let Some(seconds) = entry.every_seconds {
            form.every = TextField::with_text(seconds.to_string());
        }
        if let Some(time) = &entry.time {
            form.daily_time = TextField::with_text(time.clone());
            form.weekly_time = TextField::with_text(time.clone());
        }
        if let Some(zone) = &entry.time_zone {
            form.daily_zone = TextField::with_text(zone.clone());
            form.weekly_zone = TextField::with_text(zone.clone());
            form.cron_zone = TextField::with_text(zone.clone());
        }
        if !entry.weekdays.is_empty() {
            form.weekly_days = TextField::with_text(
                entry
                    .weekdays
                    .iter()
                    .map(|day| day.to_string())
                    .collect::<Vec<_>>()
                    .join(","),
            );
        }
        if let Some(expression) = &entry.expression {
            form.cron_expression = TextField::with_text(expression.clone());
        }
        form
    }

    /// Whether this form edits an existing entry.
    pub fn is_editing(&self) -> bool {
        self.editing.is_some()
    }

    /// Rows currently visible, in display order.
    pub fn fields(&self) -> Vec<Field> {
        let mut fields = vec![Field::Title, Field::Prompt, Field::Selector];
        match self.selector {
            Selector::Keep => {}
            Selector::After => fields.push(Field::After),
            Selector::At => fields.push(Field::At),
            Selector::Every => fields.push(Field::Every),
            Selector::Daily => fields.extend([Field::DailyTime, Field::DailyZone]),
            Selector::Weekly => {
                fields.extend([Field::WeeklyTime, Field::WeeklyZone, Field::WeeklyDays])
            }
            Selector::Cron => fields.extend([Field::CronExpression, Field::CronZone]),
        }
        fields
    }

    /// Focused row.
    pub fn focused(&self) -> Field {
        let fields = self.fields();
        let index = self.field_index.min(fields.len().saturating_sub(1));
        fields.get(index).copied().unwrap_or(Field::Title)
    }

    /// Move focus down one row.
    pub fn focus_next(&mut self) {
        let len = self.fields().len();
        self.field_index = (self.field_index + 1) % len.max(1);
    }

    /// Move focus up one row.
    pub fn focus_previous(&mut self) {
        let len = self.fields().len();
        self.field_index = self
            .field_index
            .checked_sub(1)
            .unwrap_or(len.saturating_sub(1));
    }

    /// Cycle the selector.
    pub fn cycle_selector(&mut self, forward: bool) {
        if self.focused() != Field::Selector {
            return;
        }
        let editing = self.is_editing();
        self.selector = if forward {
            self.selector.next(editing)
        } else {
            self.selector.previous(editing)
        };
        if editing && self.selector != Selector::Keep {
            self.timing_dirty = true;
        }
        if editing && self.selector == Selector::Keep {
            self.timing_dirty = false;
        }
        let len = self.fields().len();
        if self.field_index >= len {
            self.field_index = len.saturating_sub(1);
        }
    }

    /// Text field under the cursor, if the focused row is textual.
    pub fn focused_text(&mut self) -> Option<&mut TextField> {
        match self.focused() {
            Field::Title => Some(&mut self.title),
            Field::Prompt => Some(&mut self.prompt),
            Field::After => Some(&mut self.after),
            Field::At => Some(&mut self.at),
            Field::Every => Some(&mut self.every),
            Field::DailyTime => Some(&mut self.daily_time),
            Field::DailyZone => Some(&mut self.daily_zone),
            Field::WeeklyTime => Some(&mut self.weekly_time),
            Field::WeeklyZone => Some(&mut self.weekly_zone),
            Field::WeeklyDays => Some(&mut self.weekly_days),
            Field::CronExpression => Some(&mut self.cron_expression),
            Field::CronZone => Some(&mut self.cron_zone),
            Field::Selector => None,
        }
    }

    /// Build the official create request.
    pub fn create_request(&self) -> Result<Value, String> {
        let title = self.title.text().to_string();
        let prompt = self.prompt.text().to_string();
        let mut request = json!({ "title": title, "prompt": prompt });
        let object = request.as_object_mut().expect("request is an object");
        match self.selector {
            Selector::After => {
                object.insert(
                    "after_seconds".into(),
                    json!(parse_seconds(self.after.text(), "延迟秒数")?),
                );
            }
            Selector::At => {
                object.insert("at".into(), json!(self.at.text()));
            }
            Selector::Every => {
                object.insert(
                    "every_seconds".into(),
                    json!(parse_seconds(self.every.text(), "间隔秒数")?),
                );
            }
            Selector::Daily => {
                object.insert(
                    "daily".into(),
                    json!({"time": self.daily_time.text(), "time_zone": self.daily_zone.text()}),
                );
            }
            Selector::Weekly => {
                object.insert(
                    "weekly".into(),
                    json!({
                        "time": self.weekly_time.text(),
                        "time_zone": self.weekly_zone.text(),
                        "weekdays": parse_weekdays(self.weekly_days.text())?,
                    }),
                );
            }
            Selector::Cron => {
                object.insert(
                    "cron".into(),
                    json!({
                        "expression": self.cron_expression.text(),
                        "time_zone": self.cron_zone.text(),
                    }),
                );
            }
            Selector::Keep => return Err("创建任务不能选择「保持原时间」".to_string()),
        }
        Ok(request)
    }

    /// Build the update fields. The caller adds `address`, `id`, and `expected`.
    pub fn update_fields(&self) -> Result<Value, String> {
        let mut fields = json!({
            "title": self.title.text(),
            "prompt": self.prompt.text(),
        });
        if !self.timing_dirty || self.selector == Selector::Keep {
            return Ok(fields);
        }
        let change = match self.selector {
            Selector::Keep => None,
            Selector::After => {
                return Err("已有任务不能改回相对延迟；请选择绝对时间或其他类型".to_string())
            }
            Selector::At => Some(json!({"kind": "at", "at": self.at.text()})),
            Selector::Every => Some(json!({
                "kind": "every",
                "every_seconds": parse_seconds(self.every.text(), "间隔秒数")?,
            })),
            Selector::Daily => Some(json!({
                "kind": "daily",
                "daily": {"time": self.daily_time.text(), "time_zone": self.daily_zone.text()},
            })),
            Selector::Weekly => Some(json!({
                "kind": "weekly",
                "weekly": {
                    "time": self.weekly_time.text(),
                    "time_zone": self.weekly_zone.text(),
                    "weekdays": parse_weekdays(self.weekly_days.text())?,
                },
            })),
            Selector::Cron => Some(json!({
                "kind": "cron",
                "cron": {
                    "expression": self.cron_expression.text(),
                    "time_zone": self.cron_zone.text(),
                },
            })),
        };
        if let Some(change) = change {
            fields
                .as_object_mut()
                .expect("fields is an object")
                .insert("change".into(), change);
        }
        Ok(fields)
    }
}

fn parse_seconds(text: &str, label: &str) -> Result<u64, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(format!("{label}不能为空"));
    }
    trimmed
        .parse::<u64>()
        .map_err(|_| format!("{label}必须是整数：{trimmed}"))
}

fn parse_weekdays(text: &str) -> Result<Vec<u8>, String> {
    let mut days = Vec::new();
    for part in text.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let day: u8 = part
            .parse()
            .map_err(|_| format!("星期必须是 1-7 的数字：{part}"))?;
        if !(1..=7).contains(&day) {
            return Err(format!("星期必须在 1-7 之间：{day}"));
        }
        if !days.contains(&day) {
            days.push(day);
        }
    }
    if days.is_empty() {
        return Err("每周任务至少选择一个星期".to_string());
    }
    days.sort_unstable();
    Ok(days)
}

/// The user's IANA zone, shown explicitly and editable in the form.
///
/// This never substitutes an abbreviation such as `CST`: a wall-clock rule
/// needs an explicit zone, and the official service rejects ambiguous ones.
pub fn local_time_zone() -> String {
    if let Ok(value) = std::env::var("TZ") {
        let value = value.trim().trim_start_matches(':');
        if !value.is_empty() {
            return value.to_string();
        }
    }
    if let Ok(target) = std::fs::read_link("/etc/localtime") {
        let target = target.to_string_lossy();
        if let Some(index) = target.find("zoneinfo/") {
            let zone = &target[index + "zoneinfo/".len()..];
            if !zone.is_empty() {
                return zone.to_string();
            }
        }
    }
    "UTC".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(kind: &str) -> ScheduleEntry {
        ScheduleEntry {
            id: "s-1".into(),
            kind: kind.into(),
            title: "T".into(),
            prompt: "P".into(),
            scheduled_at: "2099-01-01T00:00:00.000Z".into(),
            status: "active".into(),
            record: json!({"id": "s-1", "kind": kind, "title": "T", "prompt": "P",
                "scheduledAt": "2099-01-01T00:00:00.000Z"}),
            last_delivery: None,
            after_seconds: None,
            every_seconds: None,
            time: None,
            time_zone: None,
            weekdays: vec![],
            expression: None,
        }
    }

    #[test]
    fn create_builds_one_selector_and_numbers() {
        let mut form = ScheduleForm::new_create();
        form.title.set_text("提醒");
        form.prompt.set_text("做件事");
        form.selector = Selector::Every;
        form.every.set_text("120");
        let request = form.create_request().unwrap();
        assert_eq!(request["every_seconds"], json!(120));
        assert!(request.get("after_seconds").is_none());
    }

    #[test]
    fn create_weekly_parses_sorted_unique_days() {
        let mut form = ScheduleForm::new_create();
        form.selector = Selector::Weekly;
        form.weekly_days.set_text("3,1,3");
        let request = form.create_request().unwrap();
        assert_eq!(request["weekly"]["weekdays"], json!([1, 3]));
    }

    #[test]
    fn edit_keeps_timing_until_changed() {
        let mut form = ScheduleForm::new_edit(&entry("daily"));
        let fields = form.update_fields().unwrap();
        assert!(fields.get("change").is_none());
        form.field_index = 2;
        form.cycle_selector(true);
        form.cycle_selector(true);
        assert_eq!(form.selector, Selector::Every);
        assert!(form.timing_dirty);
        form.every.set_text("90");
        let fields = form.update_fields().unwrap();
        assert_eq!(fields["change"]["kind"], json!("every"));
        assert_eq!(fields["change"]["every_seconds"], json!(90));
    }

    #[test]
    fn edit_never_cycles_to_relative_delay() {
        let mut form = ScheduleForm::new_edit(&entry("every"));
        form.field_index = 2;
        for _ in 0..6 {
            form.cycle_selector(true);
            assert_ne!(form.selector, Selector::After);
        }
    }

    #[test]
    fn invalid_weekday_is_a_local_error() {
        let mut form = ScheduleForm::new_create();
        form.selector = Selector::Weekly;
        form.weekly_days.set_text("8");
        assert!(form.create_request().unwrap_err().contains("1-7"));
    }
}
