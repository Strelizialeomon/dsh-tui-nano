//! Rendering and display-cell wrapping.
//!
//! S03 owns the styled transcript. This view is deliberately a plain feed plus
//! the input, queue, and status surfaces S02 owns, so the conversation host can
//! be exercised in a real terminal before the reading layer exists.

use nano_controls::{PanelMode, PanelRow};
use nano_conversation::{ConversationItem, MessageKind, PreviewView, ToolPreview, ToolStatus};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
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
    draw_feed(frame, app, layout[1]);
    if queue_height > 0 {
        draw_queue(frame, app, layout[2]);
    }
    draw_input(frame, app, layout[3]);
    draw_status(frame, app, layout[4], area.width);
    if app.panel.is_open() {
        draw_panel(frame, app, area);
    }
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
    let title = match app.editing_queue() {
        Some(message_id) => format!(" 编辑队列项 {message_id} · Enter 应用，Esc 取消 "),
        None => " 输入 · Enter 发送/排队 · Alt+Enter 插话 · Ctrl+J 换行（Shift+Enter 视终端） "
            .to_string(),
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
    let hint = app.status_line();
    let style = if app.error.is_some() {
        Style::default().fg(Color::Red)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let text = truncate(&hint, width as usize);
    frame.render_widget(Paragraph::new(Line::from(Span::styled(text, style))), area);
}

/// Draw the command palette or one selection pane over the conversation.
fn draw_panel(frame: &mut Frame, app: &mut App, area: Rect) {
    let rows = app.panel.rows(&app.controls);
    app.panel.clamp(rows.len());
    let selected_index = app.panel.selected();
    let width = ((area.width as u32 * 3 / 4).clamp(40, 96)).min(area.width as u32) as u16;
    let inner_width = width.saturating_sub(4).max(1) as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    push_wrapped(
        &mut lines,
        &panel_header(app),
        inner_width,
        Style::default().fg(Color::DarkGray),
    );
    let mut selected_line = 0;
    if rows.is_empty() {
        push_wrapped(
            &mut lines,
            &empty_panel_reason(app),
            inner_width,
            Style::default().fg(Color::Yellow),
        );
    } else {
        let mut previous_group: Option<String> = None;
        for (index, row) in rows.iter().enumerate() {
            if let Some(group) = row.group.as_deref() {
                if previous_group.as_deref() != Some(group) {
                    lines.push(Line::from(Span::styled(
                        group.to_string(),
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::BOLD),
                    )));
                    previous_group = Some(group.to_string());
                }
            }
            let selected = index == selected_index;
            if selected {
                selected_line = lines.len();
            }
            lines.push(panel_row_line(row, selected, inner_width));
            if selected {
                if let Some(detail) = &row.detail {
                    for segment in wrap_segments(&detail.replace('\r', ""), inner_width) {
                        lines.push(Line::from(Span::styled(
                            segment.text,
                            Style::default().fg(Color::DarkGray),
                        )));
                    }
                }
            }
        }
    }
    lines.push(Line::from(Span::styled(
        "↑↓ 选择 · Enter 确认 · Esc 返回 · 输入筛选",
        Style::default().fg(Color::DarkGray),
    )));
    let height = (lines.len() as u16 + 2)
        .min(area.height.saturating_sub(2))
        .max(3);
    let rect = centered(area, width, height);
    frame.render_widget(Clear, rect);
    let title = format!(" {} · 查询：{} ", app.panel.mode().title(), app.panel.query());
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let viewport = inner.height as usize;
    let offset = selected_line.saturating_sub(viewport.saturating_sub(2));
    let visible: Vec<Line<'static>> = lines.into_iter().skip(offset).take(viewport).collect();
    frame.render_widget(Paragraph::new(Text::from(visible)), inner);
}

/// One-line header describing the current official value for this pane.
fn panel_header(app: &App) -> String {
    match app.panel.mode() {
        PanelMode::Models => {
            let stale = if app.controls.catalog_stale() {
                " · 目录可能过期"
            } else {
                ""
            };
            format!(
                "当前：{} · 强度 {}{stale}",
                app.controls.model_label(),
                app.controls.effort_label()
            )
        }
        PanelMode::Efforts { .. } => format!(
            "当前：{} · 强度 {}",
            app.controls.model_label(),
            app.controls.effort_label()
        ),
        PanelMode::Permissions => {
            let permission = app.controls.permission();
            let default = if permission.catalog.default_preset.is_empty() {
                String::new()
            } else {
                format!(" · 新会话默认 {}", permission.catalog.default_preset)
            };
            format!("当前：{}{default}", app.controls.permission_label())
        }
        PanelMode::Commands => "官方命令优先于同名技能；不可用命令显示原因".to_string(),
    }
}

/// Explicit reason for an empty pane.
fn empty_panel_reason(app: &App) -> String {
    match app.panel.mode() {
        PanelMode::Commands => {
            if !app.controls.commands_available() {
                "官方命令服务未组合：没有可执行命令".to_string()
            } else if let Some(error) = app.controls.state_error() {
                format!("控制状态读取失败：{error}（命令列表未知）")
            } else if app.panel.query().is_empty() {
                "没有可用命令".to_string()
            } else {
                format!("没有匹配「{}」的命令", app.panel.query())
            }
        }
        PanelMode::Models => {
            if let Some(error) = app.controls.catalog_error() {
                format!("模型目录读取失败：{error}（上次值保留，可能过期）")
            } else if !app.controls.catalog_attempted() {
                "模型目录尚未加载".to_string()
            } else {
                format!("官方模型目录为空（或没有匹配「{}」）", app.panel.query())
            }
        }
        PanelMode::Efforts { .. } => "该模型没有可用的强度目录（目录可能未加载）".to_string(),
        PanelMode::Permissions => {
            if !app.controls.permission().loaded {
                "权限状态尚未读取".to_string()
            } else {
                "官方权限预设为空或服务未组合".to_string()
            }
        }
    }
}

/// One row: marker, label, current flag, and either the inline detail or the
/// unavailability reason.
fn panel_row_line(row: &PanelRow, selected: bool, width: usize) -> Line<'static> {
    let marker = if selected { "▶ " } else { "  " };
    let current = if row.current { "（当前）" } else { "" };
    let mut text = format!("{marker}{}{current}", row.label);
    if let Some(reason) = &row.disabled {
        text.push_str(" · ");
        text.push_str(reason);
    } else if !selected {
        if let Some(detail) = &row.detail {
            text.push_str("  ");
            text.push_str(detail);
        }
    }
    let style = if row.disabled.is_some() {
        Style::default().fg(Color::DarkGray)
    } else if selected {
        Style::default().fg(Color::Black).bg(Color::Cyan)
    } else {
        Style::default()
    };
    Line::from(Span::styled(truncate(&text, width), style))
}

/// Center a rectangle inside the frame area.
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

/// Push display-wrapped lines with one style.
fn push_wrapped(lines: &mut Vec<Line<'static>>, text: &str, width: usize, style: Style) {
    for segment in wrap_segments(&text.replace('\r', ""), width.max(1)) {
        lines.push(Line::from(Span::styled(segment.text, style)));
    }
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
