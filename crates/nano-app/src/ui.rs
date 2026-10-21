//! Rendering and display-cell wrapping.
//!
//! S03 owns the styled transcript. This view is deliberately a plain feed plus
//! the input, queue, and status surfaces S02 owns, so the conversation host can
//! be exercised in a real terminal before the reading layer exists.

use nano_attachments::{AttachmentState, CandidateKind};
use nano_conversation::{ConversationItem, MessageKind, PreviewView, ToolPreview, ToolStatus};
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
    let completion_height = if app.completion.visible() && app.picker.is_none() {
        (app.completion.candidates().len().min(6) as u16) + 1
    } else {
        0
    };
    let attachment_rows = if app.picker.is_some() {
        0
    } else {
        attachment_lines(app).len() as u16
    };
    let input_rows = if app.picker.is_some() {
        1
    } else {
        wrapped_input_rows(app, area.width.saturating_sub(4).max(1))
    };
    let input_height = (attachment_rows + input_rows.min(8) as u16) + 2;
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(queue_height),
            Constraint::Length(completion_height),
            Constraint::Length(input_height),
            Constraint::Length(1),
        ])
        .split(area);

    draw_header(frame, app, layout[0]);
    if app.skills_panel_open() {
        draw_skills_panel(frame, app, layout[1]);
    } else if app.instructions_panel_open() {
        draw_instructions_panel(frame, app, layout[1]);
    } else {
        draw_feed(frame, app, layout[1]);
    }
    if queue_height > 0 {
        draw_queue(frame, app, layout[2]);
    }
    if completion_height > 0 {
        draw_completion(frame, app, layout[3]);
    }
    draw_input(frame, app, layout[4]);
    draw_status(frame, app, layout[5], area.width);
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

/// Skills catalog with an optional read-only instruction preview.
fn draw_skills_panel(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(" 技能 ", Style::default().fg(Color::Cyan)));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    let width = inner.width as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let state = if !app.skills.loaded() {
        "未加载"
    } else if app.skills.may_be_incomplete() {
        "可能不完整"
    } else {
        "完整"
    };
    lines.push(Line::from(vec![
        Span::styled(
            format!("共 {} 个用户可调用技能", app.skills.entries().len()),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" · {state} · 来源：官方技能目录"),
            Style::default().fg(Color::DarkGray),
        ),
    ]));
    if let Some(reason) = app.skills.stale_reason() {
        wrap_block(
            &mut lines,
            &format!("刷新失败：{reason}；保留上次可用目录"),
            width,
            300,
            Color::Yellow,
        );
    }
    if app.skills.entries().is_empty() {
        lines.push(Line::from(Span::styled(
            "没有用户可调用的技能；/name 仍按普通文本提交。",
            Style::default().fg(Color::DarkGray),
        )));
    }

    let preview_open = app.skill_preview_target.is_some();
    let header_rows = lines.len();
    let preview_rows = if preview_open {
        (inner.height as usize).saturating_sub(header_rows) / 2
    } else {
        0
    };
    let list_rows = (inner.height as usize)
        .saturating_sub(header_rows)
        .saturating_sub(preview_rows);

    // Render each entry as its own block so the window follows real wrapped
    // heights instead of assuming one title line plus one detail line.
    let entries = app.skills.entries();
    let mut blocks: Vec<Vec<Line<'static>>> = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let selected = index == app.skill_selection;
        let marker = if selected { "▸ " } else { "  " };
        let mut spans = vec![
            Span::styled(
                format!("{marker}/{}", entry.name),
                Style::default()
                    .fg(if selected { Color::Cyan } else { Color::White })
                    .add_modifier(if selected {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            ),
            Span::raw("  "),
            Span::styled(
                truncate(&entry.description, width.saturating_sub(16)),
                Style::default(),
            ),
        ];
        if entry.user_only() {
            spans.push(Span::styled(
                "  [仅用户可调用]",
                Style::default().fg(Color::Yellow),
            ));
        }
        if entry.shadowed_by_command {
            spans.push(Span::styled(
                "  [命令优先]",
                Style::default().fg(Color::Magenta),
            ));
        }
        let mut block = vec![Line::from(spans)];
        let mut detail = Vec::new();
        if let Some(when) = &entry.when_to_use {
            detail.push(format!("适用：{when}"));
        }
        if let Some(path) = &entry.path {
            detail.push(format!("文件：{path}"));
        } else {
            detail.push("无文件路径（不可预览）".to_string());
        }
        detail.push(format!("来源：{} / {}", entry.source, entry.provider));
        wrap_block(
            &mut block,
            &format!("    {}", detail.join(" · ")),
            width,
            usize::MAX,
            Color::DarkGray,
        );
        blocks.push(block);
    }

    if list_rows > 0 && !blocks.is_empty() {
        let selected = app.skill_selection.min(blocks.len() - 1);
        let heights: Vec<usize> = blocks.iter().map(Vec::len).collect();
        let start = visible_block_start(&heights, selected, list_rows);
        let mut drawn = 0;
        for (index, block) in blocks.iter().enumerate().skip(start) {
            if drawn + block.len() > list_rows && index != selected {
                break;
            }
            drawn += block.len();
            lines.extend(block.iter().cloned());
            if drawn >= list_rows {
                break;
            }
        }
    }

    if preview_open {
        while lines.len() < inner.height as usize - preview_rows {
            lines.push(Line::from(""));
        }
        draw_skill_preview(&mut lines, app, width);
        lines.truncate(inner.height as usize);
    }
    frame.render_widget(Paragraph::new(Text::from(lines)), inner);
}

/// Right-hand preview content appended below the skill list.
fn draw_skill_preview(lines: &mut Vec<Line<'static>>, app: &App, width: usize) {
    let name = app.skill_preview_target.clone().unwrap_or_default();
    let mut title = vec![Span::styled(
        format!("── 预览 /{name} "),
        Style::default().fg(Color::Cyan),
    )];
    if let Some(preview) = &app.skill_preview {
        title.push(Span::styled(
            preview.path.clone(),
            Style::default().fg(Color::DarkGray),
        ));
    }
    lines.push(Line::from(title));
    if let Some(error) = &app.skill_preview_error {
        wrap_block(lines, error, width, 600, Color::Red);
        return;
    }
    let Some(preview) = &app.skill_preview else {
        lines.push(Line::from(Span::styled(
            "正在读取预览…",
            Style::default().fg(Color::DarkGray),
        )));
        return;
    };
    if let Some(label) = preview.truncation_label() {
        wrap_block(lines, &label, width, 200, Color::Yellow);
    }
    if preview.empty() {
        lines.push(Line::from(Span::styled(
            "（文件为空）",
            Style::default().fg(Color::DarkGray),
        )));
        return;
    }
    if preview.text.chars().count() > 4_000 {
        lines.push(Line::from(Span::styled(
            "（面板只显示前 4000 字符；完整内容以官方文件为准）",
            Style::default().fg(Color::Yellow),
        )));
    }
    wrap_block(lines, &preview.text, width, 4_000, Color::Gray);
}

/// Workspace-instruction sources, load time, and the official budget notice.
fn draw_instructions_panel(frame: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(" 项目规矩 ", Style::default().fg(Color::Cyan)));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    let width = inner.width as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    if app.instructions.available {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or(0);
        lines.push(Line::from(vec![
            Span::styled(
                format!("来源 {} 份", app.instructions.sources.len()),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(
                    " · 最近一次加载 {} · {}",
                    app.instructions.age_label(now),
                    if app.instructions.baseline {
                        "基线"
                    } else {
                        "增量"
                    }
                ),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    } else {
        wrap_block(
            &mut lines,
            "本次会话尚未加载项目规矩（官方 agent-instructions 未产生消息）。",
            width,
            300,
            Color::Yellow,
        );
    }
    if let Some(notice) = &app.instructions.budget_notice {
        // The bridge already bounds this at 8192 code points and sets
        // `truncated`; rendering the whole bound value avoids a second silent
        // cut without a marker.
        wrap_block(&mut lines, notice, width, 8_192, Color::Yellow);
    }
    if app.instructions.truncated {
        lines.push(Line::from(Span::styled(
            "来源投影已截断：只显示部分来源与提示。",
            Style::default().fg(Color::Yellow),
        )));
    }
    let order_hint = if app.instructions.baseline {
        "基线按官方加载顺序显示；同名内容已合并。"
    } else {
        "基线按官方加载顺序；增量来源按官方消息到达顺序追加；同名内容已合并。"
    };
    lines.push(Line::from(Span::styled(
        order_hint,
        Style::default().fg(Color::DarkGray),
    )));
    for source in &app.instructions.sources {
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {:<4}", source.action.label()),
                Style::default().fg(Color::Green),
            ),
            Span::raw(" "),
            Span::raw(source.path.clone()),
        ]));
    }
    let max_scroll = lines.len().saturating_sub(inner.height as usize);
    app.instructions_max_scroll = max_scroll;
    let offset = app.instructions_scroll.min(max_scroll);
    let visible: Vec<Line<'static>> = lines.into_iter().skip(offset).collect();
    frame.render_widget(Paragraph::new(Text::from(visible)), inner);
}

/// First block of a window that keeps the selected block visible, using the
/// real wrapped height of each block.
fn visible_block_start(heights: &[usize], selected: usize, rows: usize) -> usize {
    if heights.is_empty() {
        return 0;
    }
    let selected = selected.min(heights.len() - 1);
    let mut start = selected;
    let mut used = heights[selected];
    while start > 0 {
        let height = heights[start - 1];
        if used + height > rows {
            break;
        }
        start -= 1;
        used += height;
    }
    start
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
    let title = if app.picker.is_some() {
        " 附加文件 · 输入路径，Enter 确认，Esc 取消 ".to_string()
    } else if let Some(message_id) = app.editing_queue() {
        format!(" 编辑队列项 {message_id} · Enter 应用，Esc 取消 ")
    } else {
        " 输入 · Enter 发送 · Alt+Enter 插话 · @ 引用 · Alt+A 附加 · Alt+Backspace 移除附件 "
            .to_string()
    };
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    let width = inner.width.max(1) as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut cursor_row = 0usize;
    let cursor_column;
    if let Some(picker) = &app.picker {
        let prefix = "路径：";
        lines.push(Line::from(vec![
            Span::styled(prefix, Style::default().fg(Color::DarkGray)),
            Span::raw(picker.path.clone()),
        ]));
        cursor_column = prefix.chars().map(display_width).sum::<usize>()
            + picker.path.chars().map(display_width).sum::<usize>();
    } else {
        for (text, color) in attachment_lines(app) {
            lines.push(Line::from(Span::styled(text, Style::default().fg(color))));
        }
        cursor_row = lines.len();
        let (row, column) = input_cursor(app, width);
        cursor_row += row;
        cursor_column = column;
        for text in input_lines(app, width) {
            lines.push(Line::from(text));
        }
    }
    let viewport = inner.height as usize;
    let offset = if viewport == 0 {
        0
    } else {
        cursor_row.saturating_sub(viewport.saturating_sub(1))
    };
    let visible: Vec<Line<'static>> = lines.into_iter().skip(offset).take(viewport).collect();
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
}

/// Attachment card lines shown above the input text.
fn attachment_lines(app: &App) -> Vec<(String, Color)> {
    if app.editing_queue().is_some() {
        // Queue edits carry only text; the draft's attachments stay in memory
        // but are not part of this edit.
        return Vec::new();
    }
    let cards = app.attachments.current().ordered_cards();
    let mut lines = Vec::new();
    for card in cards.iter().take(3) {
        let color = match &card.state {
            AttachmentState::Ready => Color::Green,
            AttachmentState::Uploading { .. } => Color::Yellow,
            AttachmentState::Failed(_) | AttachmentState::Unknown(_) => Color::Red,
        };
        lines.push((format!("  {}", card.label()), color));
    }
    if cards.len() > 3 {
        lines.push((
            format!("  … 还有 {} 个附件", cards.len() - 3),
            Color::DarkGray,
        ));
    }
    lines
}

/// `@file` completion candidates above the input.
fn draw_completion(frame: &mut Frame, app: &App, area: Rect) {
    let title = format!(
        " 文件补全（{} 项）· ↑↓ 选择 · Enter/Tab 插入 · Esc 关闭 ",
        app.completion.candidates().len()
    );
    let block = Block::default().borders(Borders::TOP).title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let mut lines = Vec::new();
    for (index, candidate) in app.completion.candidates().iter().take(6).enumerate() {
        let selected = index == app.completion.selected();
        let marker = if selected { "▶" } else { " " };
        let suffix = match candidate.kind {
            CandidateKind::Directory => "/",
            CandidateKind::File => "",
        };
        let style = if selected {
            Style::default().fg(Color::Black).bg(Color::Cyan)
        } else {
            Style::default()
        };
        // A filesystem path may contain control characters; never let them
        // reach the terminal as escape sequences.
        let display: String = candidate
            .path
            .chars()
            .map(|character| if character.is_control() { '?' } else { character })
            .collect();
        lines.push(Line::from(Span::styled(
            format!("{marker} {display}{suffix}"),
            style,
        )));
    }
    frame.render_widget(Paragraph::new(Text::from(lines)), inner);
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
    fn the_window_keeps_the_selected_block_visible() {
        let heights = [3usize; 12];
        assert_eq!(visible_block_start(&heights, 0, 9), 0);
        assert_eq!(visible_block_start(&heights, 11, 9), 9);
        assert_eq!(visible_block_start(&heights, 5, 100), 0);
        assert_eq!(visible_block_start(&[], 0, 9), 0);
    }

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
