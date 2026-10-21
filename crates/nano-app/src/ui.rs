//! Rendering and display-cell wrapping.
//!
//! S03 owns the styled transcript. This view is deliberately a plain feed plus
//! the input, queue, and status surfaces S02 owns, so the conversation host can
//! be exercised in a real terminal before the reading layer exists.

use nano_conversation::{ConversationItem, MessageKind, PreviewView, ToolPreview, ToolStatus};
use nano_jobs::{BashRecord, BashState, JobStatus};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::app::{App, Connection};
use unicode_width::UnicodeWidthChar;

/// Draw one frame.
pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let queue_height = if app.model.inbox().is_empty() {
        0
    } else {
        (app.model.inbox().len().min(4) as u16) + 1
    };
    let input_rows = wrapped_input_rows(app, area.width.saturating_sub(4).max(1));
    let input_height = input_rows.min(8) as u16 + 2;
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(queue_height),
            Constraint::Length(input_height),
            Constraint::Length(1),
        ])
        .split(area);

    draw_header(frame, app, layout[0]);
    if app.jobs_open {
        draw_jobs(frame, app, layout[1]);
    } else {
        draw_feed(frame, app, layout[1]);
    }
    if queue_height > 0 {
        draw_queue(frame, app, layout[2]);
    }
    draw_input(frame, app, layout[3]);
    draw_status(frame, app, layout[4], area.width);
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let cwd = app
        .cli
        .cwd
        .as_ref()
        .and_then(|path| path.file_name())
        .map(|name| name.to_string_lossy().to_string())
        .or_else(|| {
            std::env::current_dir().ok().and_then(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().to_string())
            })
        })
        .unwrap_or_else(|| ".".to_string());
    let status_color = match app.connection {
        Connection::Ready => match app.model.status() {
            nano_conversation::ConversationStatus::Running => Color::Yellow,
            nano_conversation::ConversationStatus::Idle => Color::Green,
        },
        Connection::Starting => Color::Cyan,
        Connection::Disconnected | Connection::Failed(_) => Color::Red,
    };
    let mut spans = vec![
        Span::styled(" nano ", Style::default().fg(Color::Black).bg(Color::Cyan)),
        Span::raw(format!(" {cwd} ")),
        Span::raw("· "),
        Span::styled(app.connection.label(), Style::default().fg(status_color)),
        Span::raw(" · "),
        Span::styled(
            app.model.status().label(),
            Style::default()
                .fg(status_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(
            " · seq {} · 队列 {}",
            app.model.seq(),
            app.model.inbox().total_count()
        )),
    ];
    if !app.jobs.is_empty() {
        spans.push(Span::raw(format!(
            " · 任务 {}/{}",
            app.jobs.active_count(),
            app.jobs.len()
        )));
    }
    if app.jobs_open {
        spans.push(Span::styled(
            " · 任务面板",
            Style::default().fg(Color::Black).bg(Color::Yellow),
        ));
    }
    if app.replay_truncated {
        spans.push(Span::styled(
            " · 更早记录未回放",
            Style::default().fg(Color::Yellow),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_feed(frame: &mut Frame, app: &mut App, area: Rect) {
    let width = area.width.max(1) as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    if app.model.history().truncated() {
        lines.push(Line::from(Span::styled(
            "（显示上限，更早的消息未加载）",
            Style::default().fg(Color::DarkGray),
        )));
    }
    for item in app.model.items() {
        push_item(&mut lines, item, width);
    }
    for preview in app.model.live().previews {
        push_preview(&mut lines, &preview, width);
    }
    for echo in app.model.live().echoes {
        let text = echo.text.clone();
        lines.push(Line::from(Span::styled(
            format!("你（{}）", echo.state.label()),
            Style::default().fg(Color::Cyan),
        )));
        wrap_block(&mut lines, &text, width, 200, Color::Cyan);
    }
    for record in &app.bash_records {
        push_bash_record(&mut lines, record, width);
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "输入消息后按 Enter 发送。",
            Style::default().fg(Color::DarkGray),
        )));
    }
    let viewport = area.height as usize;
    app.feed_height = viewport;
    let total = lines.len();
    let offset = total
        .saturating_sub(viewport)
        .saturating_sub(app.scroll_back);
    let text = Text::from(lines);
    frame.render_widget(
        Paragraph::new(text).scroll((offset.min(u16::MAX as usize) as u16, 0)),
        area,
    );
}

fn push_item(lines: &mut Vec<Line<'static>>, item: &ConversationItem, width: usize) {
    match item {
        ConversationItem::Message(message) => {
            let (label, color) = match message.kind {
                MessageKind::User => ("你", Color::Cyan),
                MessageKind::Assistant => ("助手", Color::Green),
                MessageKind::ToolResult => ("工具结果", Color::Magenta),
                MessageKind::SyntheticBash => ("bash", Color::Yellow),
                MessageKind::Injected => ("上下文", Color::DarkGray),
                MessageKind::Notice => ("回合", Color::Yellow),
            };
            let suffix = if message.interrupted {
                "（中断前的片段）"
            } else if message.is_error {
                "（失败）"
            } else {
                ""
            };
            let prefix = format!("{label} ");
            lines.push(Line::from(vec![
                Span::styled(prefix.clone(), Style::default().fg(color)),
                Span::styled(suffix, Style::default().fg(Color::Yellow)),
            ]));
            wrap_block(lines, &message.text, width, 600, color);
        }
        ConversationItem::ToolCall(call) => {
            let status = match call.status {
                ToolStatus::Pending => "运行中/待结果",
                ToolStatus::Succeeded => "完成",
                ToolStatus::Failed => "失败",
                ToolStatus::Unknown => "结果未知",
            };
            lines.push(Line::from(vec![
                Span::styled("工具 ", Style::default().fg(Color::Magenta)),
                Span::raw(format!("{} · {}", call.name, status)),
            ]));
            wrap_block(lines, &call.arguments, width, 300, Color::DarkGray);
            if let Some(result) = &call.result {
                wrap_block(lines, result, width, 300, Color::DarkGray);
            }
        }
    }
}

fn push_preview(lines: &mut Vec<Line<'static>>, preview: &PreviewView, width: usize) {
    let mut suffix = String::new();
    if preview.gap {
        suffix.push_str("（预览不完整）");
    }
    if preview.truncated {
        suffix.push_str("（预览已截断）");
    }
    lines.push(Line::from(vec![
        Span::styled("助手（流式） ", Style::default().fg(Color::Green)),
        Span::styled(suffix, Style::default().fg(Color::Yellow)),
    ]));
    wrap_block(lines, &preview.text, width, 600, Color::Green);
    for tool in &preview.tools {
        push_preview_tool(lines, tool);
    }
}

fn push_bash_record(lines: &mut Vec<Line<'static>>, record: &BashRecord, width: usize) {
    let color = match &record.state {
        BashState::Running => Color::Yellow,
        BashState::Done => match record.result.as_ref().and_then(|result| result.exit_code) {
            Some(0) => Color::Green,
            _ => Color::Red,
        },
        BashState::Failed(_) | BashState::Unknown(_) => Color::Red,
    };
    lines.push(Line::from(vec![
        Span::styled("bash ", Style::default().fg(Color::Yellow)),
        Span::styled(
            format!("!{}", record.command),
            Style::default().fg(Color::Yellow),
        ),
        Span::raw(" · "),
        Span::styled(record.outcome_label(), Style::default().fg(color)),
    ]));
    let stdout = record.stdout();
    if !stdout.is_empty() {
        wrap_block(lines, &stdout, width, 2_000, Color::White);
    }
    let stderr = record.stderr();
    if !stderr.is_empty() {
        lines.push(Line::from(Span::styled(
            "[stderr]",
            Style::default().fg(Color::Red),
        )));
        wrap_block(lines, &stderr, width, 2_000, Color::Red);
    }
    if let Some(error) = &record.inject_error {
        lines.push(Line::from(Span::styled(
            format!("（输出未注入会话：{error}）"),
            Style::default().fg(Color::Yellow),
        )));
    }
}

fn draw_jobs(frame: &mut Frame, app: &App, area: Rect) {
    let title = if app.jobs.detail_open() {
        let job = app.jobs.detail_job();
        match job {
            Some(job) => format!(
                " 任务详情 · {} · {} · ↑↓ 滚动，k 停止，r 重读，Esc 返回 ",
                job.id,
                job.status_line()
            ),
            None => " 任务详情 · 已移除 · Esc 返回 ".to_string(),
        }
    } else {
        format!(
            " 任务 · 官方 jobs · {} 个（{} 运行中） · ↑↓ 选择，Enter 详情，k 停止，r 刷新，Esc 关闭 ",
            app.jobs.len(),
            app.jobs.active_count()
        )
    };
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let width = inner.width.max(1) as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    if app.jobs.detail_open() {
        push_job_detail(&mut lines, app, width);
    } else if app.jobs.is_empty() {
        lines.push(Line::from(Span::styled(
            "（没有可见任务。Agent 的前台命令在运行期间也会出现在这里。）",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        push_job_list(&mut lines, app);
    }
    let viewport = inner.height as usize;
    let total = lines.len();
    let offset = total
        .saturating_sub(viewport)
        .saturating_sub(app.jobs_scroll_back);
    let visible: Vec<Line<'static>> = lines.into_iter().skip(offset).collect();
    frame.render_widget(Paragraph::new(Text::from(visible)), inner);
}

fn push_job_list(lines: &mut Vec<Line<'static>>, app: &App) {
    for (index, job) in app.jobs.jobs().iter().enumerate() {
        let marker = if index == app.jobs.selected() {
            "▶"
        } else {
            " "
        };
        let call = job
            .call_label()
            .map(|call| format!(" · 调用 {call}"))
            .unwrap_or_default();
        lines.push(Line::from(vec![
            Span::styled(
                format!("{marker} [{}] ", job.kind),
                Style::default().fg(Color::Magenta),
            ),
            Span::raw(format!("{} · ", truncate(&job.label, 96))),
            Span::styled(
                job.status_line(),
                Style::default().fg(job_color(job.status())),
            ),
            Span::styled(
                format!(" · {}{}", job.id, call),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    }
}

fn push_job_detail(lines: &mut Vec<Line<'static>>, app: &App, width: usize) {
    let Some(job) = app.jobs.detail_job() else {
        lines.push(Line::from(Span::styled(
            "（任务已从官方列表移除；按 Esc 返回）",
            Style::default().fg(Color::Red),
        )));
        return;
    };
    lines.push(Line::from(vec![
        Span::styled(
            format!("{} {}", job.kind, job.label),
            Style::default().fg(Color::Magenta),
        ),
        Span::raw(" · "),
        Span::styled(
            job.status_line(),
            Style::default().fg(job_color(job.status())),
        ),
    ]));
    lines.push(Line::from(Span::styled(
        format!(
            "输出 ring：已写 {} 字节 · 最早保留 {} 字节（只读跟随，不影响模型的 job_output 游标）",
            job.output.total, job.output.earliest
        ),
        Style::default().fg(Color::DarkGray),
    )));
    if let Some(call) = &job.tool_call {
        lines.push(Line::from(Span::styled(
            format!("所属调用：{}（根调用 {}）", call.call_id, call.root_call_id),
            Style::default().fg(Color::DarkGray),
        )));
    }
    for path in &job.output.spill_paths {
        lines.push(Line::from(Span::styled(
            format!("完整输出：{path}"),
            Style::default().fg(Color::Yellow),
        )));
    }
    if app.following.contains(&job.id) {
        lines.push(Line::from(Span::styled(
            "正在读取输出…",
            Style::default().fg(Color::Yellow),
        )));
    }
    match app.jobs.output(&job.id) {
        Some(buffer) => {
            if buffer.head_truncated {
                lines.push(Line::from(Span::styled(
                    "（显示端已裁剪较早的本地缓存；完整输出以官方 ring/spill 为准）",
                    Style::default().fg(Color::Yellow),
                )));
            }
            if buffer.lossy {
                lines.push(Line::from(Span::styled(
                    "（ring 已裁剪：更早字节不可读，不能用后续内容冒充连续输出）",
                    Style::default().fg(Color::Yellow),
                )));
            }
            if buffer.text.is_empty() {
                lines.push(Line::from(Span::styled(
                    "（暂无输出）",
                    Style::default().fg(Color::DarkGray),
                )));
            } else {
                wrap_block(lines, &buffer.text, width, 200_000, Color::Gray);
            }
        }
        None => lines.push(Line::from(Span::styled(
            "（尚未读取输出；按 r 读取）",
            Style::default().fg(Color::DarkGray),
        ))),
    }
}

fn job_color(status: JobStatus) -> Color {
    match status {
        JobStatus::Running => Color::Yellow,
        JobStatus::Stopping => Color::Yellow,
        JobStatus::Completed => Color::Green,
        JobStatus::Killed => Color::Red,
        JobStatus::Failed => Color::Red,
        JobStatus::Unknown => Color::Magenta,
    }
}

fn push_preview_tool(lines: &mut Vec<Line<'static>>, tool: &ToolPreview) {
    let name = tool.name.clone().unwrap_or_else(|| "tool".to_string());
    let state = if tool.complete {
        "参数完成"
    } else {
        "参数流入"
    };
    lines.push(Line::from(vec![
        Span::styled("  工具 ", Style::default().fg(Color::Magenta)),
        Span::raw(format!("{name} · {state}")),
    ]));
}

fn draw_queue(frame: &mut Frame, app: &App, area: Rect) {
    let focused = !app.scene.focus().input_focused();
    let total = app.model.inbox().total_count();
    let preview = if app.model.inbox().truncated {
        format!(" · 预览 {}/{}", app.model.inbox().len(), total)
    } else {
        String::new()
    };
    let title = format!(
        " 队列{}{} · 官方 inbox · {} 项（Alt+↑↓ 选择，Alt+E 编辑，Alt+D 删除，Alt+S 插话，Alt+X 清空并停止） ",
        if focused { "（焦点）" } else { "" },
        preview,
        total
    );
    let block = Block::default().borders(Borders::TOP).title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let items: Vec<_> = app.model.inbox().iter().collect();
    let visible = 4.min(items.len());
    let start = if items.len() <= visible {
        0
    } else {
        app.queue_selected
            .saturating_sub(visible.saturating_sub(1))
            .min(items.len() - visible)
    };
    let mut lines = Vec::new();
    for (offset, item) in items.iter().skip(start).take(visible).enumerate() {
        let index = start + offset;
        let selected = index == app.queue_selected;
        let marker = if selected { "▶" } else { " " };
        let text = truncate(&item.text.replace('\n', " "), 120);
        let style = if selected {
            Style::default().fg(Color::Black).bg(Color::Cyan)
        } else {
            Style::default()
        };
        lines.push(Line::from(Span::styled(
            format!("{marker} [{}] {text}", item.target.label()),
            style,
        )));
    }
    frame.render_widget(Paragraph::new(Text::from(lines)), inner);
}

fn draw_input(frame: &mut Frame, app: &mut App, area: Rect) {
    let title = if app.shell_confirm.is_some() {
        " 人工命令（用户权限，不是 Agent 授权）· Enter 执行，Esc 取消 ".to_string()
    } else {
        match app.editing_queue() {
            Some(message_id) => format!(" 编辑队列项 {message_id} · Enter 应用，Esc 取消 "),
            None => {
                " 输入 · Enter 发送/排队 · Alt+Enter 插话 · Ctrl+J 换行 · Ctrl+T 任务 ".to_string()
            }
        }
    };
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    let width = inner.width.max(1);
    let lines = input_lines(app, width as usize);
    let total_rows = lines.len();
    let (cursor_row, cursor_column) = input_cursor(app, width as usize);
    let viewport = inner.height as usize;
    let offset = if viewport == 0 {
        0
    } else {
        cursor_row.saturating_sub(viewport.saturating_sub(1))
    };
    let visible: Vec<Line<'static>> = lines
        .into_iter()
        .skip(offset)
        .take(viewport)
        .map(Line::from)
        .collect();
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(Text::from(visible)), inner);
    if app.scene.focus().input_focused() && viewport > 0 {
        let row = cursor_row
            .saturating_sub(offset)
            .min(viewport.saturating_sub(1));
        let column = cursor_column.min(inner.width.saturating_sub(1) as usize);
        frame.set_cursor_position((
            inner.x + column.min(u16::MAX as usize) as u16,
            inner.y + row.min(u16::MAX as usize) as u16,
        ));
    }
    let _ = total_rows;
}

fn draw_status(frame: &mut Frame, app: &App, area: Rect, width: u16) {
    let hint = app.status_hint();
    let style = if app.error.is_some() {
        Style::default().fg(Color::Red)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let text = truncate(&hint, width as usize);
    frame.render_widget(Paragraph::new(Line::from(Span::styled(text, style))), area);
}

/// Input lines after display-cell wrapping, with `\r` hidden.
fn input_lines(app: &App, width: usize) -> Vec<String> {
    let buffer = app.drafts.current();
    let mut rows = Vec::new();
    for line in buffer.text().split('\n') {
        let line = line.replace('\r', "");
        let segments = wrap_segments(&line, width);
        for segment in segments {
            rows.push(line[segment.start..segment.end].to_string());
        }
    }
    if rows.is_empty() {
        rows.push(String::new());
    }
    rows
}

fn wrapped_input_rows(app: &App, width: u16) -> usize {
    input_lines(app, width.max(1) as usize).len().max(1)
}

/// Cursor row and display column after wrapping.
fn input_cursor(app: &App, width: usize) -> (usize, usize) {
    let buffer = app.drafts.current();
    let text = buffer.text();
    let cursor = buffer.cursor().min(text.len());
    let mut line_start = 0;
    let mut rows = 0;
    for line in text.split('\n') {
        let line_end = line_start + line.len();
        if cursor <= line_end {
            let display = line.replace('\r', "");
            let local = cursor.saturating_sub(line_start).min(line.len());
            let char_index = text[line_start..line_start + local]
                .replace('\r', "")
                .chars()
                .count();
            let segments = wrap_segments(&display, width);
            for (index, segment) in segments.iter().enumerate() {
                let is_last = index + 1 == segments.len();
                if char_index < segment.display_start + segment.char_count || is_last {
                    let consumed = char_index.saturating_sub(segment.display_start);
                    let column = segment.text.chars().take(consumed).map(display_width).sum();
                    return (rows + index, column);
                }
            }
            return (rows, 0);
        }
        let display = line.replace('\r', "");
        rows += wrap_segments(&display, width).len();
        line_start = line_end + 1;
    }
    (rows, 0)
}

fn display_width(character: char) -> usize {
    UnicodeWidthChar::width(character).unwrap_or(0)
}

/// One wrapped display segment.
#[derive(Debug, Clone)]
struct Segment {
    /// Byte start in the (CR-stripped) line.
    start: usize,
    /// Byte end.
    end: usize,
    /// Character index of the segment start.
    display_start: usize,
    /// Number of characters in the segment.
    char_count: usize,
    /// Display width of the segment (kept for wrapping assertions).
    #[allow(dead_code)]
    width: usize,
    /// Segment text.
    text: String,
}

/// Greedy display-cell wrapping. Wide characters are never split.
fn wrap_segments(line: &str, width: usize) -> Vec<Segment> {
    let width = width.max(1);
    if line.is_empty() {
        return vec![Segment {
            start: 0,
            end: 0,
            display_start: 0,
            char_count: 0,
            width: 0,
            text: String::new(),
        }];
    }
    let mut segments = Vec::new();
    let mut start = 0;
    let mut start_display = 0;
    let mut characters = 0;
    let mut current_width = 0;
    let mut end = 0;
    for (offset, character) in line.char_indices() {
        let character_width = display_width(character);
        if current_width + character_width > width && offset > start {
            segments.push(Segment {
                start,
                end: offset,
                display_start: start_display,
                char_count: characters - start_display,
                width: current_width,
                text: line[start..offset].to_string(),
            });
            start = offset;
            start_display = characters;
            current_width = 0;
        }
        current_width += character_width;
        characters += 1;
        end = offset + character.len_utf8();
    }
    segments.push(Segment {
        start,
        end,
        display_start: start_display,
        char_count: characters - start_display,
        width: current_width,
        text: line[start..end].to_string(),
    });
    segments
}

fn wrap_block(
    lines: &mut Vec<Line<'static>>,
    text: &str,
    width: usize,
    max_chars: usize,
    color: Color,
) {
    let text: String = text.chars().take(max_chars).collect();
    for logical in text.split('\n') {
        let logical = logical.replace('\r', "");
        for segment in wrap_segments(&logical, width.max(1)) {
            lines.push(Line::from(Span::styled(
                segment.text,
                Style::default().fg(color),
            )));
        }
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut value: String = text.chars().take(max.saturating_sub(1)).collect();
    value.push('…');
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapping_never_splits_wide_characters() {
        let segments = wrap_segments("你好世界", 3);
        assert_eq!(
            segments
                .iter()
                .map(|segment| segment.text.as_str())
                .collect::<Vec<_>>(),
            vec!["你", "好", "世", "界"]
        );
        assert!(segments.iter().all(|segment| segment.width <= 3));
    }

    #[test]
    fn wrapping_keeps_byte_offsets_on_char_boundaries() {
        let line = "a你b";
        for segment in wrap_segments(line, 2) {
            assert!(line.is_char_boundary(segment.start));
            assert!(line.is_char_boundary(segment.end));
            assert_eq!(&line[segment.start..segment.end], segment.text);
        }
    }

    #[test]
    fn empty_line_has_one_segment() {
        let segments = wrap_segments("", 10);
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].width, 0);
    }
}
