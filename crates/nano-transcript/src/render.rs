//! Transcript rendering: visible-line budgets, expansion, and hit targets.
//!
//! Budgets count display rows after terminal wrapping. A collapsed block shows
//! its budget plus an explicit hidden-row count; expanding is an in-place click
//! on the content area, never a new shortcut. The renderer mounts only the
//! viewport plus a bounded overscan window and keeps a small measurement cache.

use std::collections::{HashMap, HashSet, VecDeque};

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::diff::{DiffLineKind, DiffView};
use crate::display::{char_width, display_text, elide, wrap_line};
use crate::model::{Entry, EntryKind, PreviewEntry, TranscriptModel};
use crate::scroll::{EntryOffset, ScrollState};
use crate::tool::{
    ApprovalView, DisplayStatus, ExitStatus, RecordedQuestionState, ToolDisplay, ToolRecord,
};

/// Default visible rows for a long diff.
pub const DIFF_BUDGET: usize = 8;
/// Default visible rows for a command title.
pub const COMMAND_BUDGET: usize = 1;
/// Default visible rows for running/completed output.
pub const OUTPUT_BUDGET: usize = 2;
/// Default visible rows for a failure summary. Failures are never hidden.
pub const FAILURE_BUDGET: usize = 3;
/// Default visible rows for a thinking summary.
pub const THINKING_BUDGET: usize = 1;
/// Overscan rows mounted above and below the viewport.
pub const OVERSCAN_ROWS: usize = 80;
/// Hard cap on mounted rows in one render pass.
pub const MAX_MOUNTED_ROWS: usize = 300;
/// Rendered entries kept in the line cache.
const MAX_CACHED_ENTRIES: usize = 512;
/// Wrapped source rows kept per block, even when expanded.
const MAX_BLOCK_ROWS: usize = 4_000;

/// What a click does on a rendered row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HitKind {
    /// Toggle the entry's expanded state.
    Toggle,
    /// Open the recorded question for this tool call.
    OpenQuestion {
        /// Official call identity.
        call_id: String,
    },
}

/// One clickable row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// Entry identity the row belongs to.
    pub identity: String,
    /// What the click does.
    pub kind: HitKind,
}

/// One frame of rendered transcript.
#[derive(Debug, Clone)]
pub struct TranscriptRender {
    /// Visible rows for the feed region.
    pub lines: Vec<Line<'static>>,
    /// One hit target per visible row.
    pub hits: Vec<Option<Hit>>,
    /// Total display rows the transcript occupies.
    pub total_height: usize,
    /// First row shown.
    pub top: usize,
    /// Mounted row count.
    pub mounted: usize,
    /// Entry offsets used for anchor resolution.
    pub entries: Vec<EntryOffset>,
}

/// Cached measurement and line data.
#[derive(Debug, Clone, Default)]
pub struct RenderCache {
    heights: HashMap<CacheKey, usize>,
    lines: HashMap<CacheKey, RenderedEntry>,
    order: VecDeque<CacheKey>,
}

type CacheKey = (String, u64, usize, bool);

#[derive(Debug, Clone, Default)]
struct RenderedEntry {
    lines: Vec<Line<'static>>,
    hits: Vec<Option<Hit>>,
}

impl RenderedEntry {
    fn height(&self) -> usize {
        self.lines.len()
    }
}

/// One display unit: single entries render one-to-one; adjacent ordinary
/// context injections merge into a single disclosure.
enum Unit<'a> {
    Single(&'a Entry),
    Context(Vec<&'a Entry>),
}

impl<'a> Unit<'a> {
    fn identity(&self) -> &'a str {
        match self {
            Self::Single(entry) => &entry.identity,
            Self::Context(entries) => &entries[0].identity,
        }
    }

    fn fingerprint(&self) -> u64 {
        match self {
            Self::Single(entry) => entry.fingerprint,
            Self::Context(entries) => entries.iter().fold(entries.len() as u64, |hash, entry| {
                hash.wrapping_mul(1_099_511_628_211)
                    .wrapping_add(entry.fingerprint)
            }),
        }
    }
}

/// Group consecutive ordinary injected context into one display unit.
/// Bash and monitor context keep their own labels and never join a group.
fn units(model: &TranscriptModel) -> Vec<Unit<'_>> {
    let mut units: Vec<Unit<'_>> = Vec::new();
    for entry in model.entries() {
        let ordinary = matches!(
            &entry.kind,
            EntryKind::Context {
                bash: false,
                monitor: false,
                ..
            }
        );
        if ordinary {
            if let Some(Unit::Context(entries)) = units.last_mut() {
                entries.push(entry);
                continue;
            }
            units.push(Unit::Context(vec![entry]));
        } else {
            units.push(Unit::Single(entry));
        }
    }
    units
}

fn render_unit(unit: &Unit<'_>, width: usize, expanded: bool) -> RenderedEntry {
    match unit {
        Unit::Single(entry) => render_entry(entry, width, expanded),
        Unit::Context(entries) => {
            let mut output = RenderedEntry::default();
            render_context_group(&mut output, unit.identity(), entries, width, expanded);
            output.lines.push(Line::default());
            output.hits.push(None);
            output
        }
    }
}

impl RenderCache {
    /// Empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    fn ensure<F>(
        &mut self,
        identity: &str,
        fingerprint: u64,
        width: usize,
        expanded: bool,
        render: F,
    ) -> &RenderedEntry
    where
        F: FnOnce() -> RenderedEntry,
    {
        let key = (identity.to_string(), fingerprint, width.max(1), expanded);
        if !self.lines.contains_key(&key) {
            let rendered = render();
            self.heights.insert(key.clone(), rendered.height());
            self.order.push_back(key.clone());
            self.lines.insert(key.clone(), rendered);
            while self.order.len() > MAX_CACHED_ENTRIES {
                if let Some(oldest) = self.order.pop_front() {
                    self.lines.remove(&oldest);
                }
            }
            if self.heights.len() > MAX_CACHED_ENTRIES * 4 {
                self.heights.clear();
                for key in &self.order {
                    if let Some(rendered) = self.lines.get(key) {
                        self.heights.insert(key.clone(), rendered.height());
                    }
                }
            }
        }
        self.lines.get(&key).expect("entry was just cached")
    }

    fn height<F>(
        &mut self,
        identity: &str,
        fingerprint: u64,
        width: usize,
        expanded: bool,
        render: F,
    ) -> usize
    where
        F: FnOnce() -> RenderedEntry,
    {
        let key = (identity.to_string(), fingerprint, width.max(1), expanded);
        if let Some(height) = self.heights.get(&key) {
            return *height;
        }
        self.ensure(identity, fingerprint, width, expanded, render)
            .height()
    }

    /// Drop cache rows for entries that no longer exist.
    pub fn retain(&mut self, identities: &HashSet<&str>) {
        self.lines
            .retain(|(identity, _, _, _), _| identities.contains(identity.as_str()));
        self.order
            .retain(|(identity, _, _, _)| identities.contains(identity.as_str()));
        self.heights
            .retain(|(identity, _, _, _), _| identities.contains(identity.as_str()));
    }
}

/// Render the visible transcript window.
pub fn render(
    model: &TranscriptModel,
    cache: &mut RenderCache,
    scroll: &mut ScrollState,
    width: usize,
    viewport: usize,
) -> TranscriptRender {
    let width = width.max(1);
    let viewport = viewport.max(1);
    let units = units(model);
    let mut entries = Vec::new();
    let mut starts = Vec::new();
    let mut start = 0usize;
    for unit in &units {
        let expanded = model.is_expanded(unit.identity());
        let height = cache.height(unit.identity(), unit.fingerprint(), width, expanded, || {
            render_unit(unit, width, expanded)
        });
        starts.push(start);
        entries.push(EntryOffset {
            identity: unit.identity().to_string(),
            start,
            height,
        });
        start += height;
    }
    let total = start;
    let top = scroll.top(total, viewport, &entries);
    let limit = MAX_MOUNTED_ROWS.max(viewport);
    let mut window_start = top.saturating_sub(OVERSCAN_ROWS);
    let mut window_end = (top + viewport + OVERSCAN_ROWS).min(total);
    if window_end.saturating_sub(window_start) > limit {
        window_start = window_end.saturating_sub(limit);
        if window_start > top {
            window_start = top;
            window_end = (top + limit).min(total);
        }
    }
    let mut lines = Vec::new();
    let mut hits = Vec::new();
    let mut base = None;
    for (index, unit) in units.iter().enumerate() {
        let offset = &entries[index];
        let entry_end = offset.start + offset.height;
        if entry_end <= window_start || offset.start >= window_end {
            continue;
        }
        let expanded = model.is_expanded(unit.identity());
        let rendered = cache
            .ensure(unit.identity(), unit.fingerprint(), width, expanded, || {
                render_unit(unit, width, expanded)
            })
            .clone();
        let first = window_start.saturating_sub(offset.start);
        let last = (window_end - offset.start).min(rendered.lines.len());
        if first >= last {
            continue;
        }
        if base.is_none() {
            base = Some(offset.start + first);
        }
        lines.extend(rendered.lines[first..last].iter().cloned());
        hits.extend(rendered.hits[first..last].iter().cloned());
    }
    let base = base.unwrap_or(0);
    let local_top = top.saturating_sub(base).min(lines.len());
    let local_end = (local_top + viewport).min(lines.len());
    lines = lines[local_top..local_end].to_vec();
    hits = hits[local_top..local_end].to_vec();
    let mounted = lines.len();
    scroll.after_render(total, viewport, &entries, top);
    let identities: HashSet<&str> = model
        .entries()
        .iter()
        .map(|entry| entry.identity.as_str())
        .collect();
    cache.retain(&identities);
    TranscriptRender {
        lines,
        hits,
        total_height: total,
        top,
        mounted,
        entries,
    }
}

fn render_entry(entry: &Entry, width: usize, expanded: bool) -> RenderedEntry {
    let mut output = RenderedEntry::default();
    if let EntryKind::Assistant {
        text, reasoning, ..
    } = &entry.kind
    {
        if text.trim().is_empty() && reasoning.trim().is_empty() {
            // The assistant settled a step that only requested tool calls; the
            // tool cards below carry the visible content.
            return output;
        }
    }
    match &entry.kind {
        EntryKind::User { text } => render_user(&mut output, width, text),
        EntryKind::Assistant {
            text,
            reasoning,
            reasoning_ms,
            interrupted,
        } => {
            if !reasoning.trim().is_empty() {
                render_thinking(
                    &mut output,
                    &entry.identity,
                    reasoning,
                    *reasoning_ms,
                    width,
                    expanded,
                );
            }
            if !text.trim().is_empty() {
                for row in markdown_rows(text, width) {
                    output.lines.push(row);
                    output.hits.push(None);
                }
            }
            if *interrupted {
                output.lines.push(Line::from(Span::styled(
                    "（中断前的片段）",
                    Style::default().fg(Color::DarkGray),
                )));
                output.hits.push(None);
            }
        }
        EntryKind::Notice { text, is_error } => {
            let color = if *is_error {
                Color::Red
            } else {
                Color::DarkGray
            };
            for row in wrap_block(text, width, MAX_BLOCK_ROWS) {
                output
                    .lines
                    .push(Line::from(Span::styled(row, Style::default().fg(color))));
                output.hits.push(None);
            }
        }
        EntryKind::Context {
            text,
            bash,
            monitor,
        } => {
            if *bash || *monitor {
                let (label, color) = if *bash {
                    ("bash", Color::Yellow)
                } else {
                    ("monitor · role=user", Color::Cyan)
                };
                render_labeled_context(
                    &mut output,
                    &entry.identity,
                    width,
                    label,
                    color,
                    text,
                    expanded,
                );
            } else {
                render_context_group(&mut output, &entry.identity, &[entry], width, expanded);
            }
        }
        EntryKind::Tool(record) => render_tool(&mut output, record, width, expanded),
        EntryKind::Preview(preview) => render_preview(&mut output, preview, width),
        EntryKind::Echo { text, state } => render_echo(&mut output, width, text, state),
    }
    // One display row between entries keeps blocks visually separate and is
    // included in the measured height for anchor math.
    output.lines.push(Line::default());
    output.hits.push(None);
    output
}

fn muted() -> Style {
    Style::default().fg(Color::DarkGray)
}

fn render_user(output: &mut RenderedEntry, width: usize, text: &str) {
    let style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let rows = wrap_block(text, width.saturating_sub(2).max(1), MAX_BLOCK_ROWS);
    for (index, row) in rows.into_iter().enumerate() {
        let prefix = if index == 0 { "> " } else { "  " };
        output.lines.push(Line::from(vec![
            Span::styled(prefix, style),
            Span::styled(row, style),
        ]));
        output.hits.push(None);
    }
}

fn render_echo(output: &mut RenderedEntry, width: usize, text: &str, state: &str) {
    let style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let rows = wrap_block(text, width.saturating_sub(2).max(1), MAX_BLOCK_ROWS);
    let suffix = format!("（{state}）");
    let suffix_width = suffix.chars().map(char_width).sum::<usize>();
    let last = rows.len().saturating_sub(1);
    let mut appended = false;
    for (index, row) in rows.into_iter().enumerate() {
        let prefix = if index == 0 { "> " } else { "  " };
        let row_width = row.chars().map(char_width).sum::<usize>();
        let mut spans = vec![Span::styled(prefix, style), Span::styled(row, style)];
        if index == last && 2 + row_width + suffix_width <= width {
            spans.push(Span::styled(suffix.clone(), muted()));
            appended = true;
        }
        output.lines.push(Line::from(spans));
        output.hits.push(None);
    }
    // The state suffix never gets clipped: when the last text row has no
    // room, it becomes its own muted line.
    if !appended {
        output.lines.push(Line::from(Span::styled(suffix, muted())));
        output.hits.push(None);
    }
}

fn render_thinking(
    output: &mut RenderedEntry,
    identity: &str,
    reasoning: &str,
    reasoning_ms: Option<u64>,
    width: usize,
    expanded: bool,
) {
    let marker = if expanded { "▾" } else { "▸" };
    let meta = reasoning_ms
        .map(format_duration)
        .unwrap_or_else(|| "用时未知".to_string());
    let hit = Some(Hit {
        identity: identity.to_string(),
        kind: HitKind::Toggle,
    });
    push_two_column(
        output,
        vec![Span::styled(format!("{marker} 思考 已完成"), muted())],
        vec![Span::styled(meta, muted())],
        width,
        hit.clone(),
    );
    if expanded {
        for row in wrap_block(reasoning, width.saturating_sub(2).max(1), MAX_BLOCK_ROWS) {
            output
                .lines
                .push(Line::from(Span::styled(format!("  {row}"), muted())));
            output.hits.push(hit.clone());
        }
    }
}

fn render_tool(output: &mut RenderedEntry, record: &ToolRecord, width: usize, expanded: bool) {
    let question_hit = question_hit(record);
    match &record.display {
        ToolDisplay::Bash {
            command,
            description,
            output: body,
            exit,
        } => {
            let status = record.display_status;
            let command_rows = command
                .as_deref()
                .map(|command| wrap_block(command, width.saturating_sub(2).max(1), MAX_BLOCK_ROWS))
                .unwrap_or_default();
            let output_rows = wrap_block(body, width.saturating_sub(2).max(1), MAX_BLOCK_ROWS);
            let (shown_command, shown_output): (Vec<String>, Vec<String>) = if expanded {
                (command_rows.clone(), output_rows.clone())
            } else {
                let command = command_rows.iter().take(COMMAND_BUDGET).cloned().collect();
                let output = match status {
                    DisplayStatus::Pending => Vec::new(),
                    DisplayStatus::Failed | DisplayStatus::Interrupted => {
                        output_rows.iter().take(FAILURE_BUDGET).cloned().collect()
                    }
                    _ => output_rows
                        .iter()
                        .rev()
                        .take(OUTPUT_BUDGET)
                        .rev()
                        .cloned()
                        .collect(),
                };
                (command, output)
            };
            let hidden = command_rows
                .len()
                .saturating_sub(shown_command.len())
                .saturating_add(output_rows.len().saturating_sub(shown_output.len()));
            let title = description
                .as_deref()
                .filter(|description| !description.trim().is_empty())
                .map(str::to_string)
                .or_else(|| {
                    command
                        .as_deref()
                        .and_then(|command| command.lines().next())
                        .map(str::to_string)
                })
                .filter(|title| !title.trim().is_empty())
                .unwrap_or_else(|| "Bash".to_string());
            let marker = if expanded { "▾" } else { "▸" };
            let mut left = vec![
                Span::styled(format!("{marker} "), muted()),
                Span::styled("Bash", muted()),
                Span::styled(format!(" {}", title.replace('\r', "")), muted()),
            ];
            if hidden > 0 {
                left.push(Span::styled(format!(" +{hidden} 行"), muted()));
            }
            push_two_column(
                output,
                left,
                vec![Span::styled(status.label(), process_status_style(status))],
                width,
                toggle_hit(record),
            );
            for row in &shown_command {
                output
                    .lines
                    .push(Line::from(Span::styled(format!("  {row}"), muted())));
                output.hits.push(toggle_hit(record));
            }
            if status == DisplayStatus::Pending && !expanded {
                output.lines.push(Line::from(Span::styled(
                    "  （等待结果；没有来源时不显示假输出）",
                    muted(),
                )));
                output.hits.push(toggle_hit(record));
            }
            for row in &shown_output {
                output
                    .lines
                    .push(Line::from(Span::styled(format!("  {row}"), muted())));
                output.hits.push(toggle_hit(record));
            }
            if !matches!(exit, ExitStatus::Code(0)) {
                let exit_style = match exit {
                    ExitStatus::Code(0) => Style::default().fg(Color::Green),
                    ExitStatus::Unknown => Style::default().fg(Color::Yellow),
                    _ => Style::default().fg(Color::Red),
                };
                output.lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(exit.label(), exit_style),
                ]));
                output.hits.push(toggle_hit(record));
            }
        }
        ToolDisplay::File { path, diff } => {
            let name = official_name(&record.name);
            let mut right = Vec::new();
            if let Some(diff) = diff {
                if let Some(added) = diff.added {
                    right.push(Span::styled(
                        format!("+{added}"),
                        Style::default().fg(Color::Green),
                    ));
                }
                if let Some(deleted) = diff.deleted {
                    if !right.is_empty() {
                        right.push(Span::raw(" "));
                    }
                    right.push(Span::styled(
                        format!("-{deleted}"),
                        Style::default().fg(Color::Red),
                    ));
                }
                if right.is_empty() {
                    right.push(Span::styled("±?", muted()));
                }
            } else {
                right.push(Span::styled("±?", muted()));
            }
            let marker = if expanded { "▾" } else { "▸" };
            let path = path.clone().unwrap_or_else(|| "(未知文件)".to_string());
            push_two_column(
                output,
                vec![
                    Span::styled(format!("{marker} "), muted()),
                    Span::styled(format!("{name} "), muted()),
                    Span::styled(path, Style::default().add_modifier(Modifier::BOLD)),
                ],
                right,
                width,
                toggle_hit(record),
            );
            if let Some(diff) = diff {
                output.lines.push(Line::from(Span::styled(
                    format!("  {}", diff.state.label()),
                    muted(),
                )));
                output.hits.push(toggle_hit(record));
                render_diff_body(output, diff, width, expanded, record);
            } else {
                output.lines.push(Line::from(Span::styled(
                    "  （没有可显示的差异数据）",
                    muted(),
                )));
                output.hits.push(toggle_hit(record));
            }
        }
        ToolDisplay::AskUser { record: question } => {
            output.lines.push(Line::from(vec![
                Span::styled("问卷 ", Style::default().fg(Color::Magenta)),
                Span::styled(
                    question.state.label(),
                    Style::default().fg(match question.state {
                        RecordedQuestionState::Settled => Color::Green,
                        RecordedQuestionState::Cancelled => Color::Red,
                        RecordedQuestionState::Submitted => Color::Yellow,
                        _ => Color::Cyan,
                    }),
                ),
                Span::styled(format!(" · {} 题", question.questions.len()), muted()),
                Span::styled(
                    if question_hit.is_some() {
                        " · 点击查看"
                    } else {
                        ""
                    },
                    muted(),
                ),
            ]));
            output.hits.push(question_hit.clone());
            for (index, item) in question.questions.iter().enumerate() {
                if index >= 3 && !expanded {
                    break;
                }
                let answer = question
                    .answers
                    .iter()
                    .find(|answer| answer.id == item.id)
                    .map(answer_label)
                    .unwrap_or_else(|| {
                        if question.state == RecordedQuestionState::Settled {
                            "（跳过）".to_string()
                        } else {
                            question.state.label().to_string()
                        }
                    });
                let text = format!("  {} → {}", one_line(&item.question), answer);
                for (offset, row) in wrap_line(&text, width).into_iter().enumerate() {
                    output.lines.push(Line::from(vec![
                        Span::styled(if offset == 0 { "  " } else { "    " }, Style::default()),
                        Span::styled(row, muted()),
                    ]));
                    output.hits.push(question_hit.clone());
                }
            }
        }
        ToolDisplay::Generic => {
            let name = official_name(&record.name);
            let body = record.result.as_deref().unwrap_or("");
            let rows = wrap_block(body, width.saturating_sub(2).max(1), MAX_BLOCK_ROWS);
            let shown = if expanded {
                rows.len()
            } else {
                rows.len().min(FAILURE_BUDGET.max(OUTPUT_BUDGET))
            };
            let hidden = rows.len().saturating_sub(shown);
            let marker = if expanded { "▾" } else { "▸" };
            let mut left = vec![
                Span::styled(format!("{marker} "), muted()),
                Span::styled(name, muted()),
            ];
            if let Some(target) = tool_target(record) {
                left.push(Span::styled(format!(" {target}"), muted()));
            }
            if hidden > 0 {
                left.push(Span::styled(format!(" +{hidden} 行"), muted()));
            }
            push_two_column(
                output,
                left,
                vec![Span::styled(
                    record.display_status.label(),
                    process_status_style(record.display_status),
                )],
                width,
                toggle_hit(record),
            );
            for row in rows.iter().take(shown) {
                output
                    .lines
                    .push(Line::from(Span::styled(format!("  {row}"), muted())));
                output.hits.push(toggle_hit(record));
            }
        }
    }
    if let Some(approval) = &record.approval {
        render_approval_line(output, approval, record);
    }
}

fn render_approval_line(output: &mut RenderedEntry, approval: &ApprovalView, record: &ToolRecord) {
    let reason = approval
        .display_reason
        .get("zh")
        .or_else(|| approval.display_reason.get("en"))
        .map(String::as_str)
        .or(approval.reason.as_deref())
        .unwrap_or("");
    output.lines.push(Line::from(vec![
        Span::styled("  审批 ", muted()),
        Span::styled(
            approval.outcome_label(),
            Style::default().fg(match approval.outcome {
                Some(nano_decisions::approval::ApprovalOutcome::AllowedOnce) => Color::Green,
                Some(nano_decisions::approval::ApprovalOutcome::Rejected) => Color::Red,
                Some(nano_decisions::approval::ApprovalOutcome::Cancelled) => Color::Yellow,
                Some(nano_decisions::approval::ApprovalOutcome::Unavailable) => Color::Red,
                None => Color::DarkGray,
            }),
        ),
        Span::styled(
            if reason.is_empty() {
                String::new()
            } else {
                format!(" · {reason}")
            },
            muted(),
        ),
    ]));
    output.hits.push(toggle_hit(record));
}

fn render_diff_body(
    output: &mut RenderedEntry,
    diff: &DiffView,
    width: usize,
    expanded: bool,
    record: &ToolRecord,
) {
    if diff.coarse {
        output.lines.push(Line::from(Span::styled(
            "  （差异过大，仅显示统计）",
            Style::default().fg(Color::Yellow),
        )));
        output.hits.push(toggle_hit(record));
        return;
    }
    let mut rows: Vec<DiffRow> = Vec::new();
    for (index, hunk) in diff.hunks.iter().enumerate() {
        if index > 0 {
            rows.push(DiffRow::Separator(format!(
                "… 另一处修改（旧 {} 行 / 新 {} 行）",
                hunk.old_lines, hunk.new_lines
            )));
        }
        for line in &hunk.lines {
            rows.push(DiffRow::Line(
                line.kind,
                line.old_no,
                line.new_no,
                line.text.clone(),
                line.emphasis,
            ));
        }
    }
    let budget = if expanded { rows.len() } else { DIFF_BUDGET };
    let shown = budget.min(rows.len());
    for row in rows.iter().take(shown) {
        match row {
            DiffRow::Separator(label) => {
                output
                    .lines
                    .push(Line::from(Span::styled(format!("  {label}"), muted())));
                output.hits.push(toggle_hit(record));
            }
            DiffRow::Line(kind, old_no, new_no, text, emphasis) => {
                render_diff_line(
                    output, *kind, *old_no, *new_no, text, *emphasis, width, record,
                );
            }
        }
    }
    if shown < rows.len() {
        output.lines.push(Line::from(Span::styled(
            format!("  … 还有 {} 行 · 点击展开", rows.len() - shown),
            muted(),
        )));
        output.hits.push(toggle_hit(record));
    }
}

fn render_diff_line(
    output: &mut RenderedEntry,
    kind: DiffLineKind,
    old_no: Option<u64>,
    new_no: Option<u64>,
    text: &str,
    emphasis: Option<(usize, usize)>,
    width: usize,
    record: &ToolRecord,
) {
    let number = old_no.or(new_no);
    let number = number
        .map(|number| format!("{number:>4}"))
        .unwrap_or_else(|| "    ".to_string());
    // Frozen dark tokens from design-contract §3; the light set is batch 2.
    let (marker, background, emphasis_background, emphasis_foreground, marker_color) = match kind {
        DiffLineKind::Add => (
            '+',
            Some(Color::Rgb(0, 42, 0)),
            Some(Color::Rgb(0, 88, 0)),
            Color::LightGreen,
            Color::Green,
        ),
        DiffLineKind::Delete => (
            '-',
            Some(Color::Rgb(48, 0, 0)),
            Some(Color::Rgb(96, 0, 0)),
            Color::LightRed,
            Color::Red,
        ),
        DiffLineKind::Context => (' ', None, None, Color::Reset, Color::DarkGray),
    };
    let number_style = Style::default().fg(Color::DarkGray);
    let text_style = Style::default();
    let (number_style, text_style) = match background {
        Some(background) => (number_style.bg(background), text_style.bg(background)),
        None => (number_style, text_style),
    };
    let marker_style = Style::default().fg(marker_color);
    let marker_style = match background {
        Some(background) => marker_style.bg(background),
        None => marker_style,
    };
    let blank_style = match background {
        Some(background) => Style::default().bg(background),
        None => Style::default(),
    };
    let text_width = width.saturating_sub(7).max(1);
    let mut text_offset = 0usize;
    for (row_index, row) in wrap_line(text, text_width).into_iter().enumerate() {
        let mut spans = Vec::new();
        if row_index == 0 {
            spans.push(Span::styled(number.clone(), number_style));
            spans.push(Span::styled(" ", blank_style));
            spans.push(Span::styled(marker.to_string(), marker_style));
            spans.push(Span::styled(" ", blank_style));
        } else {
            spans.push(Span::styled(" ".repeat(7), blank_style));
        }
        let mut run = String::new();
        let mut current_bold = emphasis
            .map(|(start, end)| text_offset >= start && text_offset < end)
            .unwrap_or(false);
        for (index, character) in row.chars().enumerate() {
            let bold = emphasis
                .map(|(start, end)| {
                    let index = text_offset + index;
                    index >= start && index < end
                })
                .unwrap_or(false);
            if bold != current_bold && !run.is_empty() {
                let mut style = text_style;
                if current_bold {
                    style = style
                        .add_modifier(Modifier::BOLD)
                        .fg(emphasis_foreground)
                        .bg(emphasis_background.unwrap_or(Color::Reset));
                }
                spans.push(Span::styled(std::mem::take(&mut run), style));
                current_bold = bold;
            }
            run.push(character);
        }
        if !run.is_empty() {
            let mut style = text_style;
            if current_bold {
                style = style
                    .add_modifier(Modifier::BOLD)
                    .fg(emphasis_foreground)
                    .bg(emphasis_background.unwrap_or(Color::Reset));
            }
            spans.push(Span::styled(run, style));
        }
        output.lines.push(Line::from(spans));
        output.hits.push(toggle_hit(record));
        text_offset += row.chars().count();
    }
}

enum DiffRow {
    Line(
        DiffLineKind,
        Option<u64>,
        Option<u64>,
        String,
        Option<(usize, usize)>,
    ),
    Separator(String),
}

fn render_preview(output: &mut RenderedEntry, preview: &PreviewEntry, width: usize) {
    if !preview.reasoning.trim().is_empty() {
        output.lines.push(Line::from(vec![
            Span::styled("▸ 思考 进行中…", muted()),
            Span::styled(
                if preview.reasoning_ms.is_some() {
                    format!(
                        " · {}",
                        preview
                            .reasoning_ms
                            .map(format_duration)
                            .unwrap_or_default()
                    )
                } else {
                    String::new()
                },
                muted(),
            ),
        ]));
        output.hits.push(None);
    }
    let mut suffix = String::new();
    if preview.gap {
        suffix.push_str("（预览不完整）");
    }
    if preview.truncated {
        suffix.push_str("（预览已截断）");
    }
    for row in wrap_block(&preview.text, width, MAX_BLOCK_ROWS) {
        output
            .lines
            .push(Line::from(Span::styled(row, Style::default())));
        output.hits.push(None);
    }
    if !suffix.is_empty() {
        output.lines.push(Line::from(Span::styled(
            suffix,
            Style::default().fg(Color::Yellow),
        )));
        output.hits.push(None);
    }
    for tool in &preview.tools {
        let name = official_name(tool.name.as_deref().unwrap_or("tool"));
        let state = if tool.complete {
            "参数完成"
        } else {
            "参数流入"
        };
        output.lines.push(Line::from(Span::styled(
            format!("▸ {name} {state}"),
            muted(),
        )));
        output.hits.push(None);
    }
}

/// The merged disclosure for consecutive ordinary injected context.
fn render_context_group(
    output: &mut RenderedEntry,
    identity: &str,
    entries: &[&Entry],
    width: usize,
    expanded: bool,
) {
    let count = entries.len();
    let label = if count > 1 {
        format!("上下文 {count} 段")
    } else {
        "上下文".to_string()
    };
    output.lines.push(Line::from(vec![
        Span::styled(label, muted()),
        Span::styled(
            if expanded {
                " · 已展开"
            } else {
                " · 点击展开"
            },
            muted(),
        ),
    ]));
    let hit = Some(Hit {
        identity: identity.to_string(),
        kind: HitKind::Toggle,
    });
    output.hits.push(hit.clone());
    if !expanded {
        return;
    }
    for (index, entry) in entries.iter().enumerate() {
        if index > 0 {
            output.lines.push(Line::default());
            output.hits.push(hit.clone());
        }
        let EntryKind::Context { text, .. } = &entry.kind else {
            continue;
        };
        for row in wrap_block(text, width, MAX_BLOCK_ROWS) {
            output.lines.push(Line::from(Span::styled(row, muted())));
            output.hits.push(hit.clone());
        }
    }
}

/// Bash and monitor context keep their own labels and never merge.
fn render_labeled_context(
    output: &mut RenderedEntry,
    identity: &str,
    width: usize,
    label: &str,
    color: Color,
    text: &str,
    expanded: bool,
) {
    let rows = wrap_block(text, width, MAX_BLOCK_ROWS);
    output.lines.push(Line::from(vec![
        Span::styled(label.to_string(), Style::default().fg(color)),
        Span::styled(
            if expanded {
                " · 已展开"
            } else {
                " · 点击展开"
            },
            muted(),
        ),
    ]));
    let hit = Some(Hit {
        identity: identity.to_string(),
        kind: HitKind::Toggle,
    });
    output.hits.push(hit.clone());
    if rows.is_empty() {
        return;
    }
    let shown = if expanded {
        rows.len()
    } else {
        1.min(rows.len())
    };
    for row in rows.iter().take(shown) {
        output.lines.push(Line::from(Span::styled(
            row.clone(),
            Style::default().fg(color),
        )));
        output.hits.push(hit.clone());
    }
    if shown < rows.len() {
        output.lines.push(Line::from(Span::styled(
            format!("  … 还有 {} 行 · 点击展开", rows.len() - shown),
            muted(),
        )));
        output.hits.push(hit);
    }
}

/// Minimal Markdown: headings lose their marker and turn bold, a leading
/// paragraph acts as the answer's lead, list items use `-`, and `**…**`
/// turns bold inline. No third-party parser is involved.
fn markdown_rows(text: &str, width: usize) -> Vec<Line<'static>> {
    let text = display_text(text);
    let mut blocks: Vec<Vec<&str>> = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for line in text.split('\n') {
        if line.trim().is_empty() {
            if !current.is_empty() {
                blocks.push(std::mem::take(&mut current));
            }
        } else {
            current.push(line);
        }
    }
    if !current.is_empty() {
        blocks.push(current);
    }
    let mut rows = Vec::new();
    for (block_index, block) in blocks.iter().enumerate() {
        if block_index > 0 {
            rows.push(Line::default());
        }
        let lead = block_index == 0;
        for line in block {
            let line = line.trim_end();
            let trimmed = line.trim_start();
            if let Some(heading) = heading_text(trimmed) {
                for row in inline_rows(
                    heading,
                    width,
                    Style::default().add_modifier(Modifier::BOLD),
                ) {
                    rows.push(row);
                }
            } else if let Some(item) = list_item(trimmed) {
                let item_rows = inline_rows(item, width.saturating_sub(2).max(1), Style::default());
                for (index, row) in item_rows.into_iter().enumerate() {
                    let prefix = if index == 0 { "- " } else { "  " };
                    let mut spans = vec![Span::styled(prefix, muted())];
                    spans.extend(row.spans);
                    rows.push(Line::from(spans));
                }
            } else {
                let style = if lead {
                    Style::default().add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                for row in inline_rows(line, width, style) {
                    rows.push(row);
                }
            }
        }
    }
    rows
}

fn heading_text(line: &str) -> Option<&str> {
    let hashes = line
        .chars()
        .take_while(|character| *character == '#')
        .count();
    if hashes == 0 {
        return None;
    }
    let rest = line[hashes..].trim_start();
    if rest.is_empty() {
        None
    } else {
        Some(rest)
    }
}

fn list_item(line: &str) -> Option<&str> {
    let mut characters = line.chars();
    let marker = characters.next()?;
    if marker != '-' && marker != '*' {
        return None;
    }
    let rest = &line[marker.len_utf8()..];
    rest.strip_prefix(' ')
}

/// Wrap one logical line, styling `**…**` runs bold on top of `base`.
fn inline_rows(text: &str, width: usize, base: Style) -> Vec<Line<'static>> {
    let (plain, bold_flags) = strip_inline_bold(text);
    let mut rows = Vec::new();
    let mut offset = 0usize;
    for row in wrap_line(&plain, width.max(1)) {
        let count = row.chars().count();
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut run = String::new();
        let mut current_bold = bold_flags.get(offset).copied().unwrap_or(false);
        for (index, character) in row.chars().enumerate() {
            let bold = bold_flags.get(offset + index).copied().unwrap_or(false);
            if bold != current_bold && !run.is_empty() {
                let mut style = base;
                if current_bold {
                    style = style.add_modifier(Modifier::BOLD);
                }
                spans.push(Span::styled(std::mem::take(&mut run), style));
                current_bold = bold;
            }
            run.push(character);
        }
        if !run.is_empty() {
            let mut style = base;
            if current_bold {
                style = style.add_modifier(Modifier::BOLD);
            }
            spans.push(Span::styled(run, style));
        }
        if spans.is_empty() {
            spans.push(Span::styled(String::new(), base));
        }
        rows.push(Line::from(spans));
        offset += count;
    }
    if rows.is_empty() {
        rows.push(Line::default());
    }
    rows
}

/// Remove `**` markers and report which characters were emphasised.
fn strip_inline_bold(text: &str) -> (String, Vec<bool>) {
    let mut plain = String::new();
    let mut flags = Vec::new();
    let mut bold = false;
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '*' && characters.peek() == Some(&'*') {
            characters.next();
            bold = !bold;
            continue;
        }
        plain.push(character);
        flags.push(bold);
    }
    (plain, flags)
}

/// Pick the most useful argument for a generic tool's title.
fn tool_target(record: &ToolRecord) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(&record.arguments).ok()?;
    for key in ["file_path", "path", "pattern", "url"] {
        if let Some(target) = value.get(key).and_then(serde_json::Value::as_str) {
            if !target.trim().is_empty() {
                return Some(target.to_string());
            }
        }
    }
    None
}

fn official_name(name: &str) -> String {
    match name.to_ascii_lowercase().as_str() {
        "bash" => "Bash".to_string(),
        "read" => "Read".to_string(),
        "edit" => "Edit".to_string(),
        "write" => "Write".to_string(),
        "grep" => "Grep".to_string(),
        "glob" => "Glob".to_string(),
        "list" => "List".to_string(),
        "ask_user_question" => "AskUserQuestion".to_string(),
        _ => {
            let mut characters = name.chars();
            match characters.next() {
                Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
                None => "Tool".to_string(),
            }
        }
    }
}

fn process_status_style(status: DisplayStatus) -> Style {
    match status {
        DisplayStatus::Failed => Style::default().fg(Color::Red),
        DisplayStatus::Interrupted | DisplayStatus::Unknown => Style::default().fg(Color::Yellow),
        DisplayStatus::Pending | DisplayStatus::Succeeded => muted(),
    }
}

fn spans_width(spans: &[Span<'static>]) -> usize {
    spans.iter().map(Span::width).sum()
}

fn truncate_spans(spans: Vec<Span<'static>>, max: usize) -> Vec<Span<'static>> {
    let mut output = Vec::new();
    let mut used = 0usize;
    for span in spans {
        let style = span.style;
        let text = span.content.to_string();
        let mut kept = String::new();
        for character in text.chars() {
            let character_width = char_width(character);
            if used + character_width > max {
                break;
            }
            used += character_width;
            kept.push(character);
        }
        if !kept.is_empty() {
            output.push(Span::styled(kept, style));
        }
        if used >= max {
            break;
        }
    }
    output
}

/// One left-aligned row with a right-aligned status. The left side gives up
/// width first so the status stays visible at narrow terminals.
fn push_two_column(
    output: &mut RenderedEntry,
    left: Vec<Span<'static>>,
    right: Vec<Span<'static>>,
    width: usize,
    hit: Option<Hit>,
) {
    let width = width.max(1);
    let mut right = right;
    if spans_width(&right) > width {
        right = truncate_spans(right, width);
    }
    let right_width = spans_width(&right);
    let left_room = width.saturating_sub(right_width + 1);
    let mut left = truncate_spans(left, left_room);
    let left_width = spans_width(&left);
    let used = left_width + right_width;
    if used < width {
        left.push(Span::raw(" ".repeat(width - used)));
    }
    left.extend(right);
    output.lines.push(Line::from(left));
    output.hits.push(hit);
}
fn wrap_block(text: &str, width: usize, cap: usize) -> Vec<String> {
    let mut rows = Vec::new();
    for line in text.split('\n') {
        let line = line.replace('\r', "");
        for row in wrap_line(&line, width.max(1)) {
            if rows.len() >= cap {
                rows.push("…（内容已截断）".to_string());
                return rows;
            }
            rows.push(row);
        }
    }
    rows
}

fn toggle_hit(record: &ToolRecord) -> Option<Hit> {
    Some(Hit {
        identity: format!("call-{}", record.call_id),
        kind: HitKind::Toggle,
    })
}

fn question_hit(record: &ToolRecord) -> Option<Hit> {
    let ToolDisplay::AskUser { record: question } = &record.display else {
        return None;
    };
    if question.can_review() || question.answerable() {
        Some(Hit {
            identity: format!("call-{}", record.call_id),
            kind: HitKind::OpenQuestion {
                call_id: record.call_id.clone(),
            },
        })
    } else {
        None
    }
}

fn format_duration(ms: u64) -> String {
    if ms < 1_000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        let seconds = ms / 1000;
        format!("{}m{:02}s", seconds / 60, seconds % 60)
    }
}

fn one_line(text: &str) -> String {
    elide(text.replace('\n', " ").trim(), 80)
}

fn answer_label(answer: &nano_decisions::contract::AnswerItem) -> String {
    let mut parts = answer.selected.clone();
    if let Some(custom) = answer
        .custom
        .as_deref()
        .filter(|text| !text.trim().is_empty())
    {
        parts.push(custom.to_string());
    }
    if parts.is_empty() {
        "（跳过）".to_string()
    } else {
        parts.join("、")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::DiffState;
    use crate::model::TranscriptSource;
    use crate::tool::{QuestionRecord, ToolDisplay, ToolRecord};
    use nano_conversation::{SessionAddress, ToolStatus};
    use nano_decisions::contract::parse_questions;

    fn model(entries: Vec<Entry>) -> TranscriptModel {
        let mut model =
            TranscriptModel::new(SessionAddress::root("root"), TranscriptSource::Interactive);
        model.entries = entries;
        model
    }

    fn bash_entry(status: ToolStatus, output: &str, display: DisplayStatus) -> Entry {
        Entry {
            identity: "call-c-1".into(),
            fingerprint: 1,
            kind: EntryKind::Tool(ToolRecord {
                call_id: "c-1".into(),
                name: "bash".into(),
                status,
                display_status: display,
                turn: None,
                step: None,
                arguments: r#"{"command":"echo hi"}"#.into(),
                result: Some(output.into()),
                result_fact: None,
                approval: None,
                display: ToolDisplay::Bash {
                    command: Some("echo hi".into()),
                    description: None,
                    output: output.into(),
                    exit: ExitStatus::Unknown,
                },
            }),
        }
    }

    fn render_one(entry: Entry, expanded: bool, width: usize) -> RenderedEntry {
        render_entry(&entry, width, expanded)
    }

    #[test]
    fn a_long_transcript_mounts_only_a_bounded_window() {
        let mut model = model(
            (0..100)
                .map(|index| Entry {
                    identity: format!("seq-{index}"),
                    fingerprint: index as u64 + 1,
                    kind: EntryKind::User {
                        text: "line one\nline two\nline three".into(),
                    },
                })
                .collect(),
        );
        let mut cache = RenderCache::new();
        let mut scroll = ScrollState::default();
        let rendered = render(&model, &mut cache, &mut scroll, 80, 20);
        assert!(rendered.total_height > 100);
        assert!(rendered.mounted <= MAX_MOUNTED_ROWS);
        assert_eq!(rendered.lines.len(), 20);
        model.revision += 1;
    }

    #[test]
    fn a_viewport_taller_than_the_mount_cap_still_reaches_the_bottom() {
        let mut model = model(
            (0..100)
                .map(|index| Entry {
                    identity: format!("seq-{index}"),
                    fingerprint: index as u64 + 1,
                    kind: EntryKind::User {
                        text: "line one\nline two\nline three".into(),
                    },
                })
                .collect(),
        );
        let mut cache = RenderCache::new();
        let mut scroll = ScrollState::default();
        let viewport = 250;
        let rendered = render(&model, &mut cache, &mut scroll, 80, viewport);
        assert_eq!(rendered.lines.len(), viewport);
        assert_eq!(rendered.top + rendered.lines.len(), rendered.total_height);
        model.revision += 1;
    }

    #[test]
    fn a_wide_character_never_gets_split_by_the_renderer() {
        let mut model = model(vec![Entry {
            identity: "m-1".into(),
            fingerprint: 1,
            kind: EntryKind::User {
                text: "你好世界".into(),
            },
        }]);
        let mut cache = RenderCache::new();
        let mut scroll = ScrollState::default();
        let rendered = render(&model, &mut cache, &mut scroll, 4, 10);
        assert!(rendered
            .lines
            .iter()
            .all(|line| crate::display::line_width(&line.to_string()) <= 4));
        model.revision = 1;
    }

    #[test]
    fn a_long_diff_shows_at_most_eight_visual_rows_and_a_hidden_count() {
        let old = (0..20)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let new = (0..20)
            .map(|i| format!("changed {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let diff = crate::diff::single_text_diff(
            Some("a.txt".into()),
            Some(old),
            Some(new),
            DiffState::Applied,
        );
        let entry = Entry {
            identity: "call-c".into(),
            fingerprint: 1,
            kind: EntryKind::Tool(ToolRecord {
                call_id: "c".into(),
                name: "edit".into(),
                status: ToolStatus::Succeeded,
                display_status: DisplayStatus::Succeeded,
                turn: None,
                step: None,
                arguments: "{}".into(),
                result: None,
                result_fact: None,
                approval: None,
                display: ToolDisplay::File {
                    path: Some("a.txt".into()),
                    diff: Some(diff),
                },
            }),
        };
        let rendered = render_one(entry, false, 80);
        let body = rendered
            .lines
            .iter()
            .filter(|line| line.to_string().contains('+') || line.to_string().contains('-'))
            .count();
        assert!(body <= DIFF_BUDGET + 1);
        assert!(rendered
            .lines
            .iter()
            .any(|line| line.to_string().contains("还有")));
    }

    #[test]
    fn diff_rows_have_a_background_and_emphasis_style() {
        let diff = crate::diff::single_text_diff(
            Some("a.txt".into()),
            Some("hello world".into()),
            Some("hello there".into()),
            DiffState::Applied,
        );
        let entry = Entry {
            identity: "call-c".into(),
            fingerprint: 1,
            kind: EntryKind::Tool(ToolRecord {
                call_id: "c".into(),
                name: "edit".into(),
                status: ToolStatus::Succeeded,
                display_status: DisplayStatus::Succeeded,
                turn: None,
                step: None,
                arguments: "{}".into(),
                result: None,
                result_fact: None,
                approval: None,
                display: ToolDisplay::File {
                    path: Some("a.txt".into()),
                    diff: Some(diff),
                },
            }),
        };
        let rendered = render_one(entry, true, 80);
        assert!(rendered
            .lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .any(|span| span.style.bg.is_some()));
        assert!(rendered
            .lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .any(|span| span.style.add_modifier.contains(Modifier::BOLD)));
    }

    #[test]
    fn a_missing_exit_code_is_shown_as_unknown() {
        let entry = bash_entry(ToolStatus::Succeeded, "hello", DisplayStatus::Unknown);
        let rendered = render_one(entry, false, 80);
        assert!(rendered
            .lines
            .iter()
            .any(|line| line.to_string().contains("退出码未知")));
    }

    #[test]
    fn a_failure_summary_is_visible_without_expanding() {
        let entry = bash_entry(
            ToolStatus::Failed,
            "bad\nworse\nworst\nextra",
            DisplayStatus::Failed,
        );
        let rendered = render_one(entry, false, 80);
        assert!(rendered
            .lines
            .iter()
            .any(|line| line.to_string().contains("失败")));
        assert!(rendered
            .lines
            .iter()
            .any(|line| line.to_string().contains("worst")));
    }

    #[test]
    fn expanding_a_block_keeps_the_same_identity_and_click_target() {
        let entry = bash_entry(
            ToolStatus::Succeeded,
            "one\ntwo\nthree",
            DisplayStatus::Unknown,
        );
        let collapsed = render_one(entry.clone(), false, 80);
        let expanded = render_one(entry, true, 80);
        assert!(expanded
            .lines
            .iter()
            .any(|line| line.to_string().contains("one")));
        assert!(!collapsed
            .lines
            .iter()
            .any(|line| line.to_string().contains("one")));
        assert!(expanded
            .hits
            .iter()
            .flatten()
            .any(|hit| hit.kind == HitKind::Toggle));
    }

    #[test]
    fn a_monitor_message_is_labelled_as_a_user_role_monitor_notice() {
        let entry = Entry {
            identity: "seq-monitor".into(),
            fingerprint: 2,
            kind: EntryKind::Context {
                text: "[monitor] job=bash-a matched=1".into(),
                bash: false,
                monitor: true,
            },
        };
        let rendered = render_one(entry, false, 80);
        assert!(rendered
            .lines
            .iter()
            .any(|line| line.to_string().contains("monitor · role=user")));
    }

    #[test]
    fn ordinary_context_starts_collapsed_and_expands_to_its_original_text() {
        let entry = Entry {
            identity: "seq-1".into(),
            fingerprint: 1,
            kind: EntryKind::Context {
                text: "line one\nline two\nline three".into(),
                bash: false,
                monitor: false,
            },
        };
        let collapsed = render_one(entry.clone(), false, 80);
        assert!(collapsed
            .lines
            .iter()
            .any(|line| line.to_string().contains("上下文 · 点击展开")));
        assert!(!collapsed
            .lines
            .iter()
            .any(|line| line.to_string().contains("line one")));
        let expanded = render_one(entry, true, 80);
        assert!(expanded
            .lines
            .iter()
            .any(|line| line.to_string().contains("line one")));
        assert!(expanded
            .lines
            .iter()
            .any(|line| line.to_string().contains("line three")));
    }

    #[test]
    fn consecutive_ordinary_context_entries_merge_into_one_disclosure() {
        let entries = (0..3)
            .map(|index| Entry {
                identity: format!("seq-{index}"),
                fingerprint: index + 1,
                kind: EntryKind::Context {
                    text: format!("segment {index}\nsecond row"),
                    bash: false,
                    monitor: false,
                },
            })
            .collect::<Vec<_>>();
        let model = model(entries);
        let mut cache = RenderCache::new();
        let mut scroll = ScrollState::default();
        let rendered = render(&model, &mut cache, &mut scroll, 80, 20);
        let text = rendered
            .lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        assert!(text.iter().any(|line| line.contains("上下文 3 段")));
        assert!(!text.iter().any(|line| line.contains("segment 0")));
        assert_eq!(
            rendered
                .lines
                .iter()
                .filter(|line| line.to_string().contains("上下文 3 段"))
                .count(),
            1
        );
        let mut cache = RenderCache::new();
        let mut scroll = ScrollState::default();
        let expanded = render_expanded(&model, &mut cache, &mut scroll, 80, 20);
        for index in 0..3 {
            assert!(expanded
                .iter()
                .any(|line| line.contains(&format!("segment {index}"))));
        }
    }

    fn render_expanded(
        model: &TranscriptModel,
        cache: &mut RenderCache,
        scroll: &mut ScrollState,
        width: usize,
        viewport: usize,
    ) -> Vec<String> {
        let mut model = model.clone();
        let identities = model
            .entries()
            .iter()
            .map(|entry| entry.identity.clone())
            .collect::<Vec<_>>();
        for identity in identities {
            model.reveal(&identity);
        }
        let rendered = render(&model, cache, scroll, width, viewport);
        rendered.lines.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn an_assistant_message_with_only_tool_calls_has_no_empty_label() {
        let entry = Entry {
            identity: "a-1".into(),
            fingerprint: 1,
            kind: EntryKind::Assistant {
                text: String::new(),
                reasoning: String::new(),
                reasoning_ms: None,
                interrupted: false,
            },
        };
        let rendered = render_one(entry, false, 80);
        assert!(rendered.lines.is_empty());
    }

    #[test]
    fn a_pending_approval_is_not_labeled_as_unknown() {
        let entry = Entry {
            identity: "call-c".into(),
            fingerprint: 1,
            kind: EntryKind::Tool(ToolRecord {
                call_id: "c".into(),
                name: "write".into(),
                status: ToolStatus::Pending,
                display_status: DisplayStatus::Pending,
                turn: None,
                step: None,
                arguments: "{}".into(),
                result: None,
                result_fact: None,
                approval: Some(crate::tool::ApprovalView {
                    tool_name: "write".into(),
                    reason: Some("sandbox".into()),
                    display_reason: Default::default(),
                    outcome: None,
                    asked: true,
                    pending: true,
                }),
                display: ToolDisplay::Generic,
            }),
        };
        let rendered = render_one(entry, false, 80);
        assert!(rendered
            .lines
            .iter()
            .any(|line| line.to_string().contains("审批中")));
        assert!(!rendered
            .lines
            .iter()
            .any(|line| line.to_string().contains("结果未知")));
    }

    #[test]
    fn a_user_message_uses_a_prompt_prefix_instead_of_a_label_row() {
        let entry = Entry {
            identity: "m-1".into(),
            fingerprint: 1,
            kind: EntryKind::User {
                text: "你好世界".into(),
            },
        };
        let rendered = render_one(entry, false, 80);
        assert_eq!(rendered.lines[0].to_string(), "> 你好世界");
        assert!(rendered.lines[0]
            .spans
            .iter()
            .all(|span| span.style.add_modifier.contains(Modifier::BOLD)));
        assert!(!rendered
            .lines
            .iter()
            .any(|line| line.to_string().trim() == "你"));
    }

    #[test]
    fn assistant_markdown_strips_markers_and_styles_lead_heading_and_list() {
        let entry = Entry {
            identity: "a-1".into(),
            fingerprint: 1,
            kind: EntryKind::Assistant {
                text: "# 改动\n完成 **两处** 修改。\n\n- 第一项\n- 第二项".into(),
                reasoning: String::new(),
                reasoning_ms: None,
                interrupted: false,
            },
        };
        let rendered = render_one(entry, false, 40);
        let rows = rendered
            .lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        assert!(rows.iter().any(|row| row == "改动"));
        assert!(!rows.iter().any(|row| row.contains('#')));
        assert!(!rows.iter().any(|row| row.contains("**")));
        let bold = rendered
            .lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .find(|span| span.content.contains("两处"))
            .expect("inline bold run");
        assert!(bold.style.add_modifier.contains(Modifier::BOLD));
        assert!(rendered
            .lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .any(|span| span.content.contains("改动")
                && span.style.add_modifier.contains(Modifier::BOLD)));
        assert!(rows.iter().any(|row| row.starts_with("- 第一项")));
    }

    #[test]
    fn tool_rows_keep_the_status_on_the_right_edge() {
        let entry = bash_entry(ToolStatus::Succeeded, "hello", DisplayStatus::Succeeded);
        let rendered = render_one(entry, false, 40);
        let header = rendered.lines[0].to_string();
        assert!(header.starts_with("▸ Bash "));
        assert!(header.ends_with("完成"));
        assert_eq!(crate::display::line_width(&header), 40);
    }

    #[test]
    fn an_echo_state_suffix_is_never_clipped() {
        let entry = Entry {
            identity: "echo-1".into(),
            fingerprint: 1,
            kind: EntryKind::Echo {
                text: "x".repeat(18),
                state: "已发送".into(),
            },
        };
        let rendered = render_one(entry, false, 10);
        assert!(rendered
            .lines
            .iter()
            .any(|line| line.to_string().contains("（已发送）")));
        assert!(rendered
            .lines
            .iter()
            .all(|line| crate::display::line_width(&line.to_string()) <= 10));
    }

    #[test]
    fn a_non_error_notice_stays_muted() {
        let entry = Entry {
            identity: "n-1".into(),
            fingerprint: 1,
            kind: EntryKind::Notice {
                text: "回合已取消".into(),
                is_error: false,
            },
        };
        let rendered = render_one(entry, false, 80);
        let styles = rendered
            .lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.style.fg)
            .collect::<Vec<_>>();
        assert!(styles.contains(&Some(Color::DarkGray)));
        assert!(!styles.contains(&Some(Color::Yellow)));
    }

    #[test]
    fn a_settled_question_row_opens_the_recorded_answers() {
        let questions = parse_questions(Some(&serde_json::json!([
            {"id": "q", "question": "Q", "options": [{"label": "A"}]}
        ])));
        let record = QuestionRecord {
            questions,
            timed: false,
            state: RecordedQuestionState::Settled,
            answers: vec![nano_decisions::contract::AnswerItem {
                id: "q".into(),
                selected: vec!["A".into()],
                custom: None,
            }],
        };
        let entry = Entry {
            identity: "call-c".into(),
            fingerprint: 1,
            kind: EntryKind::Tool(ToolRecord {
                call_id: "c".into(),
                name: "ask_user_question".into(),
                status: ToolStatus::Succeeded,
                display_status: DisplayStatus::Succeeded,
                turn: None,
                step: None,
                arguments: "{}".into(),
                result: None,
                result_fact: None,
                approval: None,
                display: ToolDisplay::AskUser { record },
            }),
        };
        let rendered = render_one(entry, false, 80);
        assert!(rendered.hits.iter().flatten().any(|hit| matches!(
            &hit.kind,
            HitKind::OpenQuestion { call_id } if call_id == "c"
        )));
    }
}
