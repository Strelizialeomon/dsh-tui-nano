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
use crate::display::{elide, wrap_line};
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

impl RenderCache {
    /// Empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    fn ensure(&mut self, entry: &Entry, width: usize, expanded: bool) -> &RenderedEntry {
        let key = (
            entry.identity.clone(),
            entry.fingerprint,
            width.max(1),
            expanded,
        );
        if !self.lines.contains_key(&key) {
            let rendered = render_entry(entry, width.max(1), expanded);
            self.heights.insert(key.clone(), rendered.height());
            self.order.push_back(key.clone());
            self.lines.insert(key.clone(), rendered);
            while self.order.len() > MAX_CACHED_ENTRIES {
                if let Some(oldest) = self.order.pop_front() {
                    self.lines.remove(&oldest);
                }
            }
            while self.heights.len() > MAX_CACHED_ENTRIES * 4 {
                self.heights.clear();
                for key in &self.order {
                    if let Some(rendered) = self.lines.get(key) {
                        self.heights.insert(key.clone(), rendered.height());
                    }
                }
                break;
            }
        }
        self.lines.get(&key).expect("entry was just cached")
    }

    fn height(&mut self, entry: &Entry, width: usize, expanded: bool) -> usize {
        let key = (
            entry.identity.clone(),
            entry.fingerprint,
            width.max(1),
            expanded,
        );
        if let Some(height) = self.heights.get(&key) {
            return *height;
        }
        self.ensure(entry, width, expanded).height()
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
    let mut entries = Vec::new();
    let mut starts = Vec::new();
    let mut start = 0usize;
    for entry in model.entries() {
        let expanded = model.is_expanded(&entry.identity);
        let height = cache.height(entry, width, expanded);
        starts.push(start);
        entries.push(EntryOffset {
            identity: entry.identity.clone(),
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
    for (index, entry) in model.entries().iter().enumerate() {
        let offset = &entries[index];
        let entry_end = offset.start + offset.height;
        if entry_end <= window_start || offset.start >= window_end {
            continue;
        }
        let expanded = model.is_expanded(&entry.identity);
        let rendered = cache.ensure(entry, width, expanded).clone();
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
        EntryKind::User { text } => {
            labeled_block(&mut output, width, "你", Color::Cyan, text, false);
        }
        EntryKind::Assistant {
            text,
            reasoning,
            reasoning_ms,
            interrupted,
        } => {
            if !reasoning.trim().is_empty() {
                let duration = reasoning_ms
                    .map(format_duration)
                    .unwrap_or_else(|| "用时未知".to_string());
                let toggle = if expanded { "收起" } else { "展开" };
                output.lines.push(Line::from(vec![
                    Span::styled("思考 ", Style::default().fg(Color::DarkGray)),
                    Span::styled(duration, Style::default().fg(Color::DarkGray)),
                    Span::styled(format!(" · {toggle}"), Style::default().fg(Color::DarkGray)),
                ]));
                output.hits.push(Some(Hit {
                    identity: entry.identity.clone(),
                    kind: HitKind::Toggle,
                }));
                if expanded {
                    let rows = wrap_block(reasoning, width.saturating_sub(2), MAX_BLOCK_ROWS);
                    for row in rows {
                        output.lines.push(Line::from(Span::styled(
                            format!("  {row}"),
                            Style::default().fg(Color::DarkGray),
                        )));
                        output.hits.push(Some(Hit {
                            identity: entry.identity.clone(),
                            kind: HitKind::Toggle,
                        }));
                    }
                }
            }
            let suffix = if *interrupted {
                "（中断前的片段）"
            } else {
                ""
            };
            let label = if suffix.is_empty() {
                "助手".to_string()
            } else {
                format!("助手{suffix}")
            };
            if !text.trim().is_empty() {
                labeled_block(&mut output, width, &label, Color::Green, text, true);
            }
        }
        EntryKind::Notice { text, is_error } => {
            let label = if *is_error {
                "回合（失败）"
            } else {
                "回合"
            };
            let color = if *is_error { Color::Red } else { Color::Yellow };
            labeled_block(&mut output, width, label, color, text, false);
        }
        EntryKind::Context { text, bash } => {
            let (label, color) = if *bash {
                ("bash", Color::Yellow)
            } else {
                ("上下文", Color::DarkGray)
            };
            render_context(
                &mut output,
                &entry.identity,
                width,
                label,
                color,
                text,
                expanded,
            );
        }
        EntryKind::Tool(record) => render_tool(&mut output, record, width, expanded),
        EntryKind::Preview(preview) => render_preview(&mut output, preview, width),
        EntryKind::Echo { text, state } => {
            labeled_block(
                &mut output,
                width,
                &format!("你（{state}）"),
                Color::Cyan,
                text,
                false,
            );
        }
    }
    // One display row between entries keeps blocks visually separate and is
    // included in the measured height for anchor math.
    output.lines.push(Line::default());
    output.hits.push(None);
    output
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
            output.lines.push(Line::from(vec![
                Span::styled("命令 ", Style::default().fg(Color::Magenta)),
                Span::styled(status.label(), status_style(status)),
                Span::styled(
                    description
                        .as_deref()
                        .map(|description| format!(" · {description}"))
                        .unwrap_or_default(),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
            output.hits.push(None);
            let command_rows = command
                .as_deref()
                .map(|command| wrap_block(command, width.saturating_sub(2), MAX_BLOCK_ROWS))
                .unwrap_or_default();
            let output_rows = wrap_block(body, width.saturating_sub(2), MAX_BLOCK_ROWS);
            if expanded {
                for row in &command_rows {
                    output.lines.push(Line::from(vec![
                        Span::styled("› ", Style::default().fg(Color::DarkGray)),
                        Span::raw(row.clone()),
                    ]));
                    output.hits.push(toggle_hit(record));
                }
                for row in &output_rows {
                    output.lines.push(Line::from(Span::styled(
                        format!("  {row}"),
                        Style::default().fg(Color::DarkGray),
                    )));
                    output.hits.push(toggle_hit(record));
                }
            } else {
                let shown_command = command_rows
                    .iter()
                    .take(COMMAND_BUDGET)
                    .cloned()
                    .collect::<Vec<_>>();
                for row in &shown_command {
                    output.lines.push(Line::from(vec![
                        Span::styled("› ", Style::default().fg(Color::DarkGray)),
                        Span::raw(row.clone()),
                    ]));
                    output.hits.push(toggle_hit(record));
                }
                let shown_output: Vec<String> = match status {
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
                for row in &shown_output {
                    output.lines.push(Line::from(Span::styled(
                        format!("  {row}"),
                        Style::default().fg(Color::DarkGray),
                    )));
                    output.hits.push(toggle_hit(record));
                }
                let hidden = command_rows
                    .len()
                    .saturating_sub(shown_command.len())
                    .saturating_add(output_rows.len().saturating_sub(shown_output.len()));
                if status == DisplayStatus::Pending {
                    output.lines.push(Line::from(Span::styled(
                        "  （等待结果；没有来源时不显示假输出）",
                        Style::default().fg(Color::DarkGray),
                    )));
                    output.hits.push(toggle_hit(record));
                } else if hidden > 0 {
                    output.lines.push(Line::from(Span::styled(
                        format!("  … 还有 {hidden} 行 · 点击展开"),
                        Style::default().fg(Color::DarkGray),
                    )));
                    output.hits.push(toggle_hit(record));
                }
            }
            let exit_style = match exit {
                ExitStatus::Code(0) => Style::default().fg(Color::Green),
                ExitStatus::Unknown => Style::default().fg(Color::Yellow),
                _ => Style::default().fg(Color::Red),
            };
            output.lines.push(Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(exit.label(), exit_style),
            ]));
            output.hits.push(toggle_hit(record));
        }
        ToolDisplay::File { path, diff } => {
            let stat = diff
                .as_ref()
                .map(DiffView::stat_label)
                .unwrap_or_else(|| "±?".to_string());
            output.lines.push(Line::from(vec![
                Span::styled("文件 ", Style::default().fg(Color::Magenta)),
                Span::styled(
                    path.clone().unwrap_or_else(|| "(未知文件)".to_string()),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("  {stat}"), Style::default().fg(Color::DarkGray)),
            ]));
            output.hits.push(toggle_hit(record));
            if let Some(diff) = diff {
                output.lines.push(Line::from(Span::styled(
                    format!("  {}", diff.state.label()),
                    Style::default().fg(Color::DarkGray),
                )));
                output.hits.push(toggle_hit(record));
                render_diff_body(output, diff, width, expanded, record);
            } else {
                output.lines.push(Line::from(Span::styled(
                    "  （没有可显示的差异数据）",
                    Style::default().fg(Color::DarkGray),
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
                Span::styled(
                    format!(" · {} 题", question.questions.len()),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    if question_hit.is_some() {
                        " · 点击查看"
                    } else {
                        ""
                    },
                    Style::default().fg(Color::DarkGray),
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
                        Span::styled(row, Style::default().fg(Color::DarkGray)),
                    ]));
                    output.hits.push(question_hit.clone());
                }
            }
        }
        ToolDisplay::Generic => {
            output.lines.push(Line::from(vec![
                Span::styled("工具 ", Style::default().fg(Color::Magenta)),
                Span::raw(record.name.clone()),
                Span::styled(
                    format!(" · {}", record.display_status.label()),
                    status_style(record.display_status),
                ),
            ]));
            output.hits.push(toggle_hit(record));
            let body = record.result.as_deref().unwrap_or("");
            let rows = wrap_block(body, width.saturating_sub(2), MAX_BLOCK_ROWS);
            let shown = if expanded {
                rows.len()
            } else {
                rows.len().min(FAILURE_BUDGET.max(OUTPUT_BUDGET))
            };
            for row in rows.iter().take(shown) {
                output.lines.push(Line::from(Span::styled(
                    format!("  {row}"),
                    Style::default().fg(Color::DarkGray),
                )));
                output.hits.push(toggle_hit(record));
            }
            if shown < rows.len() {
                output.lines.push(Line::from(Span::styled(
                    format!("  … 还有 {} 行 · 点击展开", rows.len() - shown),
                    Style::default().fg(Color::DarkGray),
                )));
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
        Span::styled("  审批 ", Style::default().fg(Color::Yellow)),
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
            Style::default().fg(Color::DarkGray),
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
                output.lines.push(Line::from(Span::styled(
                    format!("  {label}"),
                    Style::default().fg(Color::DarkGray),
                )));
                output.hits.push(toggle_hit(record));
            }
            DiffRow::Line(kind, old_no, new_no, text, emphasis) => {
                let number = (*old_no).or(*new_no);
                let number = number
                    .map(|number| format!("{number:>4}"))
                    .unwrap_or_else(|| "    ".to_string());
                let (marker, base, highlighted) = match kind {
                    DiffLineKind::Add => (
                        "+",
                        Style::default().fg(Color::Green).bg(Color::Rgb(0, 42, 0)),
                        Style::default()
                            .fg(Color::LightGreen)
                            .bg(Color::Rgb(0, 88, 0))
                            .add_modifier(Modifier::BOLD),
                    ),
                    DiffLineKind::Delete => (
                        "-",
                        Style::default().fg(Color::Red).bg(Color::Rgb(48, 0, 0)),
                        Style::default()
                            .fg(Color::LightRed)
                            .bg(Color::Rgb(96, 0, 0))
                            .add_modifier(Modifier::BOLD),
                    ),
                    DiffLineKind::Context => {
                        (" ", Style::default().fg(Color::DarkGray), Style::default())
                    }
                };
                let rendered = format!(
                    "{number} {} {text}",
                    if marker == " " { " " } else { marker }
                );
                let mut char_start = 0usize;
                for wrapped in wrap_line(&rendered, width) {
                    let row_chars = wrapped.chars().count();
                    let overlap = emphasis
                        .map(|(start, end)| {
                            // 4ch number + space + marker + space.
                            let start = 7 + start;
                            let end = 7 + end;
                            char_start < end && char_start + row_chars > start
                        })
                        .unwrap_or(false);
                    let style = if overlap { highlighted } else { base };
                    output.lines.push(Line::from(Span::styled(wrapped, style)));
                    output.hits.push(toggle_hit(record));
                    char_start += row_chars;
                }
            }
        }
    }
    if shown < rows.len() {
        output.lines.push(Line::from(Span::styled(
            format!("  … 还有 {} 行 · 点击展开", rows.len() - shown),
            Style::default().fg(Color::DarkGray),
        )));
        output.hits.push(toggle_hit(record));
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
    let mut suffix = String::new();
    if preview.gap {
        suffix.push_str("（预览不完整）");
    }
    if preview.truncated {
        suffix.push_str("（预览已截断）");
    }
    output.lines.push(Line::from(vec![
        Span::styled("助手（流式） ", Style::default().fg(Color::Green)),
        Span::styled(suffix, Style::default().fg(Color::Yellow)),
    ]));
    output.hits.push(None);
    if !preview.reasoning.trim().is_empty() {
        let duration = preview
            .reasoning_ms
            .map(format_duration)
            .unwrap_or_else(|| "思考中".to_string());
        output.lines.push(Line::from(Span::styled(
            format!("思考 {duration}"),
            Style::default().fg(Color::DarkGray),
        )));
        output.hits.push(None);
    }
    for row in wrap_block(&preview.text, width, MAX_BLOCK_ROWS) {
        output.lines.push(Line::from(Span::styled(
            row,
            Style::default().fg(Color::Green),
        )));
        output.hits.push(None);
    }
    for tool in &preview.tools {
        let name = tool.name.as_deref().unwrap_or("tool");
        let state = if tool.complete {
            "参数完成"
        } else {
            "参数流入"
        };
        output.lines.push(Line::from(vec![
            Span::styled("  工具 ", Style::default().fg(Color::Magenta)),
            Span::styled(
                format!("{name} · {state}"),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
        output.hits.push(None);
    }
}

fn render_context(
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
            Style::default().fg(Color::DarkGray),
        ),
    ]));
    output.hits.push(Some(Hit {
        identity: identity.to_string(),
        kind: HitKind::Toggle,
    }));
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
        output.hits.push(Some(Hit {
            identity: identity.to_string(),
            kind: HitKind::Toggle,
        }));
    }
    if shown < rows.len() {
        output.lines.push(Line::from(Span::styled(
            format!("  … 还有 {} 行 · 点击展开", rows.len() - shown),
            Style::default().fg(Color::DarkGray),
        )));
        output.hits.push(Some(Hit {
            identity: identity.to_string(),
            kind: HitKind::Toggle,
        }));
    }
}

fn labeled_block(
    output: &mut RenderedEntry,
    width: usize,
    label: &str,
    color: Color,
    text: &str,
    markdown: bool,
) {
    output.lines.push(Line::from(Span::styled(
        label.to_string(),
        Style::default().fg(color),
    )));
    output.hits.push(None);
    let rows = if markdown {
        markdown_rows(text, width)
    } else {
        wrap_block(text, width, MAX_BLOCK_ROWS)
            .into_iter()
            .map(Line::from)
            .collect()
    };
    for row in rows {
        output.lines.push(row);
        output.hits.push(None);
    }
}

fn markdown_rows(text: &str, width: usize) -> Vec<Line<'static>> {
    let mut rows = Vec::new();
    for line in text.split('\n') {
        let is_heading = line
            .trim_start()
            .chars()
            .take_while(|character| *character == '#')
            .count()
            > 0
            && line.trim_start().contains(' ');
        let is_bold = line.trim().starts_with("**") && line.trim().ends_with("**");
        let style = if is_heading || is_bold {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        for segment in wrap_line(&line.replace('\r', ""), width) {
            rows.push(Line::from(Span::styled(segment, style)));
        }
    }
    rows
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

fn status_style(status: DisplayStatus) -> Style {
    match status {
        DisplayStatus::Pending => Style::default().fg(Color::Cyan),
        DisplayStatus::Succeeded => Style::default().fg(Color::Green),
        DisplayStatus::Failed => Style::default().fg(Color::Red),
        DisplayStatus::Interrupted => Style::default().fg(Color::Yellow),
        DisplayStatus::Unknown => Style::default().fg(Color::Yellow),
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
                    kind: EntryKind::Context {
                        text: "line one\nline two\nline three".into(),
                        bash: false,
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
                    kind: EntryKind::Context {
                        text: "line one\nline two\nline three".into(),
                        bash: false,
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
    fn injected_context_is_one_line_until_expanded() {
        let entry = Entry {
            identity: "seq-1".into(),
            fingerprint: 1,
            kind: EntryKind::Context {
                text: "line one\nline two\nline three".into(),
                bash: false,
            },
        };
        let collapsed = render_one(entry.clone(), false, 80);
        assert!(collapsed
            .lines
            .iter()
            .any(|line| line.to_string().contains("line one")));
        assert!(!collapsed
            .lines
            .iter()
            .any(|line| line.to_string().contains("line three")));
        assert!(collapsed
            .lines
            .iter()
            .any(|line| line.to_string().contains("还有 2 行")));
        let expanded = render_one(entry, true, 80);
        assert!(expanded
            .lines
            .iter()
            .any(|line| line.to_string().contains("line three")));
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
