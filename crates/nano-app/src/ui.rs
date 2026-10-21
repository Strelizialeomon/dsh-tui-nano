//! Rendering and display-cell wrapping.
//!
//! S03 owns the styled transcript. This view is deliberately a plain feed plus
//! the input, queue, and status surfaces S02 owns, so the conversation host can
//! be exercised in a real terminal before the reading layer exists.

use nano_controls::{PanelMode, PanelRow};
use nano_conversation::{ConversationItem, MessageKind, PreviewView, ToolPreview, ToolStatus};
use nano_sessions::{format_clock, Occupancy};
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
    if app.sessions_panel.open {
        draw_sessions(frame, app, layout[1]);
    } else if app.skills_panel_open() {
        draw_skills_panel(frame, app, layout[1]);
    } else if app.instructions_panel_open() {
        draw_instructions_panel(frame, app, layout[1]);
    } else {
        draw_feed(frame, app, layout[1]);
    }
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
    let status_color = match app.connection {
        Connection::Ready => match app.model.status() {
            nano_conversation::ConversationStatus::Running => Color::Yellow,
            nano_conversation::ConversationStatus::Idle => Color::Green,
        },
        Connection::Starting => Color::Cyan,
        Connection::Disconnected | Connection::Failed(_) => Color::Red,
    };
    let connection_label = if app.switch.busy() {
        "切换中"
    } else {
        app.connection.label()
    };
    let mut spans = vec![
        Span::styled(" nano ", Style::default().fg(Color::Black).bg(Color::Cyan)),
        Span::styled(connection_label, Style::default().fg(status_color)),
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
    if let Some(meter) = &app.context {
        let occupancy = meter.occupancy();
        let colour = match occupancy.percent() {
            Some(percent) if percent >= 80 => Color::Red,
            Some(percent) if percent >= 60 => Color::Yellow,
            Some(_) => Color::Green,
            None => Color::DarkGray,
        };
        spans.push(Span::raw(" · "));
        spans.push(Span::styled(
            format!("ctx {}", occupancy.label()),
            Style::default().fg(colour),
        ));
        spans.push(Span::styled(
            " · 参考",
            Style::default().fg(Color::DarkGray),
        ));
    } else if let Some(error) = &app.context_error {
        spans.push(Span::styled(
            format!(" · ctx 未知（{error}）"),
            Style::default().fg(Color::Yellow),
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

/// Session panel: the official list plus context provenance and the last
/// compaction outcome. Every number is labelled with its source; occupancy is
/// never shown as exact usage.
fn draw_sessions(frame: &mut Frame, app: &App, area: Rect) {
    let title = match &app.sessions_panel.rename {
        Some(_) => " 会话 · 输入新标题 · Enter 应用，Esc 取消 ".to_string(),
        None if app.compact_in_flight || app.compact_maybe_running => {
            " 会话 · ↑↓ 选择 · Enter 切换 · n 改名 · c 压缩 · r 刷新 · Esc 取消压缩等待 "
                .to_string()
        }
        None => " 会话 · ↑↓ 选择 · Enter 切换 · n 改名 · c 压缩 · r 刷新 · Esc 关闭 ".to_string(),
    };
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let mut rows: Vec<Line<'static>> = Vec::new();
    if app.sessions_panel.loading {
        rows.push(Line::from(Span::styled(
            "正在读取官方会话列表…",
            Style::default().fg(Color::DarkGray),
        )));
    }
    if let Some(error) = &app.sessions_panel.error {
        rows.push(Line::from(Span::styled(
            error.clone(),
            Style::default().fg(Color::Red),
        )));
    }
    let prefix_rows = rows.len();
    let host_cwd = app.hello.as_ref().map(|hello| hello.cwd.as_str());
    push_session_row(
        &mut rows,
        app.sessions_panel.selected == 0,
        "＋ 新建会话（Nano 生成新 ID）".to_string(),
    );
    for (index, row) in app.sessions_panel.rows.iter().enumerate() {
        let activity = row
            .updated_at
            .map(short_clock)
            .unwrap_or_else(|| "最近活动未知".to_string());
        let state = row.state_label();
        let id: String = row.session_id.chars().take(8).collect();
        let mut line = format!("{} · {} · {}", row.display_title(), activity, id);
        if !state.is_empty() {
            line.push_str(&format!(" · {state}"));
        }
        if row.blank == Some(true) {
            line.push_str(" · 空白");
        }
        if !row.switchable() {
            line.push_str(" · 子代理（不可作主会话）");
        } else if let (Some(host), Some(recorded)) = (host_cwd, row.cwd.as_deref()) {
            if host != recorded {
                let name = std::path::Path::new(recorded)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or(recorded);
                line.push_str(&format!(" · 目录 {name}（不可切换）"));
            }
        }
        push_session_row(&mut rows, app.sessions_panel.selected == index + 1, line);
    }
    if let Some(rename) = &app.sessions_panel.rename {
        rows.push(Line::from(Span::styled(
            format!("新标题：{}", rename.buffer.text()),
            Style::default().fg(Color::Cyan),
        )));
    }
    let mut footer: Vec<Line<'static>> = vec![Line::from("")];
    push_context_details(&mut footer, app);
    if let Some(compact) = &app.compact {
        footer.push(Line::from(Span::styled(
            format!("压缩：{}", compact.display_text()),
            Style::default().fg(if compact.kind == nano_sessions::CompactionKind::Success {
                Color::Green
            } else {
                Color::Yellow
            }),
        )));
    }
    if app.compact_in_flight {
        footer.push(Line::from(Span::styled(
            "压缩：等待官方结果…（Esc 取消等待，历史以后端为准）",
            Style::default().fg(Color::Yellow),
        )));
    } else if app.compact_maybe_running {
        footer.push(Line::from(Span::styled(
            "压缩：结果未知，可能仍在运行；Esc 取消等待并重读官方状态",
            Style::default().fg(Color::Yellow),
        )));
    }
    if app.switch.state().failed() {
        footer.push(Line::from(Span::styled(
            format!(
                "{}；Alt+R 重试 / Alt+B 返回上一会话",
                app.switch.state().label()
            ),
            Style::default().fg(Color::Red),
        )));
    } else if app.switch.busy() {
        footer.push(Line::from(Span::styled(
            app.switch.state().label(),
            Style::default().fg(Color::Cyan),
        )));
    }

    // Keep the selected row visible while reserving room for the context and
    // compaction footer. A long session list scrolls instead of hiding the
    // cursor.
    let viewport = inner.height as usize;
    let footer_height = footer.len().min(viewport.saturating_sub(1));
    let row_viewport = viewport.saturating_sub(footer_height).max(1);
    let selected_line = prefix_rows + app.sessions_panel.selected;
    let start = selected_line.saturating_add(1).saturating_sub(row_viewport);
    let visible: Vec<Line<'static>> = rows
        .into_iter()
        .skip(start)
        .take(row_viewport)
        .chain(footer.into_iter().take(footer_height))
        .collect();
    frame.render_widget(Paragraph::new(Text::from(visible)), inner);
}

fn push_session_row(lines: &mut Vec<Line<'static>>, selected: bool, text: String) {
    let marker = if selected { "▶ " } else { "  " };
    let style = if selected {
        Style::default().fg(Color::Black).bg(Color::Cyan)
    } else {
        Style::default()
    };
    lines.push(Line::from(Span::styled(
        format!("{marker}{}", truncate(&text, 120)),
        style,
    )));
}

fn push_context_details(lines: &mut Vec<Line<'static>>, app: &App) {
    match &app.context {
        Some(meter) => {
            let occupancy = meter.occupancy();
            lines.push(Line::from(Span::styled(
                format!("上下文占用（参考值）：{}", occupancy.label()),
                Style::default().fg(match occupancy {
                    Occupancy::Known { .. } => Color::Cyan,
                    Occupancy::Unknown => Color::Yellow,
                }),
            )));
            if occupancy == Occupancy::Unknown {
                lines.push(Line::from(Span::styled(
                    "  缺 projectedTokens 或 contextWindow；不补零、不用累计用量冒充",
                    Style::default().fg(Color::Yellow),
                )));
            }
            lines.push(Line::from(Span::styled(
                format!("  {}", meter.provenance_label(now_ms())),
                Style::default().fg(Color::DarkGray),
            )));
            match meter.token_usage {
                Some(usage) => lines.push(Line::from(Span::styled(
                    format!(
                        "计费用量（整个日志）：输入 {} + 输出 {} + 缓存读 {} / 写 {} = {}",
                        usage.uncached_input_tokens,
                        usage.output_tokens,
                        usage.cache_read_tokens,
                        usage.cache_write_tokens,
                        usage.total()
                    ),
                    Style::default().fg(Color::DarkGray),
                ))),
                None => lines.push(Line::from(Span::styled(
                    "计费用量：未知（官方投影未提供）",
                    Style::default().fg(Color::Yellow),
                ))),
            }
            if let Some(breakdown) = meter.breakdown {
                lines.push(Line::from(Span::styled(
                    format!(
                        "估算构成（启发式，CJK/JSON 可能低估）：系统 {} · 工具 {} · 消息 {}",
                        breakdown.system_tokens, breakdown.tools_tokens, breakdown.message_tokens
                    ),
                    Style::default().fg(Color::DarkGray),
                )));
            }
            if let Some(pressure) = meter.pressure_tokens {
                lines.push(Line::from(Span::styled(
                    format!("最近一次 provider 压力：{} tokens（非当前占用）", pressure),
                    Style::default().fg(Color::DarkGray),
                )));
            }
        }
        None => {
            lines.push(Line::from(Span::styled(
                match &app.context_error {
                    Some(error) => format!("上下文：未知（{error}）"),
                    None => "上下文：尚未读取".to_string(),
                },
                Style::default().fg(Color::DarkGray),
            )));
        }
    }
}

/// Current wall clock in Unix epoch milliseconds, for provenance ages.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// HH:MM from an official durable event time.
fn short_clock(epoch_ms: u64) -> String {
    let text = format_clock(epoch_ms);
    text.get(0..5).map(str::to_string).unwrap_or(text)
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
    if app.scene.focus().input_focused() && !app.panel.is_open() && viewport > 0 {
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
    let title = format!(
        " {} · 查询：{} ",
        app.panel.mode().title(),
        app.panel.query()
    );
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
        PanelMode::Models | PanelMode::Efforts { .. } => {
            let stale = if app.controls.catalog_stale() {
                " · 目录可能过期"
            } else if app.controls.state_error().is_some() {
                " · 状态可能过期"
            } else {
                ""
            };
            format!(
                "当前：{} · 强度 {}{stale}",
                app.controls.model_label(),
                app.controls.effort_label()
            )
        }
        PanelMode::Permissions => {
            let permission = app.controls.permission();
            let default = if permission.catalog.default_preset.is_empty() {
                String::new()
            } else {
                format!(" · 新会话默认 {}", permission.catalog.default_preset)
            };
            let stale = if app.controls.state_error().is_some() {
                " · 状态可能过期"
            } else {
                ""
            };
            format!("当前：{}{default}{stale}", app.controls.permission_label())
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
            if let Some(error) = app.controls.state_error() {
                format!("控制状态读取失败：{error}（保留上次值，可能过期）")
            } else if app.controls.permission().available == Some(false) {
                "官方权限服务未组合".to_string()
            } else if !app.controls.permission().loaded {
                "权限状态尚未读取".to_string()
            } else if app.panel.query().is_empty() {
                "官方权限预设为空".to_string()
            } else {
                format!("没有匹配「{}」的权限预设", app.panel.query())
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
    // `max` is a display-cell budget, not a character count: CJK labels must
    // not overflow or lose their tail because each glyph takes two cells.
    let total: usize = text.chars().map(display_width).sum();
    if total <= max {
        return text.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let budget = max.saturating_sub(1);
    let mut value = String::new();
    let mut width = 0;
    for character in text.chars() {
        let character_width = display_width(character);
        if width + character_width > budget {
            break;
        }
        value.push(character);
        width += character_width;
    }
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
    fn truncation_uses_display_cells_not_character_count() {
        let truncated = truncate("你好世界", 5);
        assert_eq!(truncated, "你好…");
        let width: usize = truncated.chars().map(display_width).sum();
        assert!(width <= 5);
        assert_eq!(truncate("abc", 3), "abc");
    }

    #[test]
    fn empty_line_has_one_segment() {
        let segments = wrap_segments("", 10);
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].width, 0);
    }
}
