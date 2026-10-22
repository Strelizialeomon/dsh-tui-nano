//! Rendering and display-cell wrapping.
//!
//! S03 owns the styled transcript. This view is deliberately a plain feed plus
//! the input, queue, and status surfaces S02 owns, so the conversation host can
//! be exercised in a real terminal before the reading layer exists.

use std::time::Instant;

use nano_attachments::{AttachmentState, CandidateKind};
use nano_controls::{PanelMode, PanelRow};
use nano_decisions::{
    ApprovalCardState, ApprovalFocus, DecisionClick, DecisionTop, PlanFocus, QuestionCard,
    QuestionCardState,
};
use nano_jobs::{BashRecord, BashState, JobStatus};
use nano_schedule::RowKind;
use nano_sessions::{format_clock, Occupancy};
use nano_subagents::{ComposerGate, Lamp};
use nano_transcript::DiffTheme;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::app::{ActivityKind, App, Connection};
use unicode_width::UnicodeWidthChar;

/// Draw one frame.
pub fn draw(frame: &mut Frame, app: &mut App) {
    let full = frame.area();
    app.send_area = Rect::default();
    app.jump_area = Rect::default();
    if app.terminal_active() {
        draw_terminal(frame, app, full);
        return;
    }
    // The conversation surfaces keep 2 terminal cells of breathing room on
    // both sides; the interactive terminal keeps its full grid.
    let area = inset_horizontal(full, 2);
    if app.schedule.open {
        draw_schedule(frame, app, full);
        return;
    }
    let child_active = app.subagent_active();
    let queue_height = if child_active || app.model.inbox().is_empty() {
        0
    } else {
        (app.model.inbox().len().min(4) as u16) + 1
    };
    // Root decision cards belong to the parent conversation and never take the
    // child composer seat.
    let decision = !child_active && app.decision_owns_seat();
    let completion_height =
        if !decision && !child_active && app.completion.visible() && app.picker.is_none() {
            (app.completion.candidates().len().min(6) as u16) + 1
        } else {
            0
        };
    let (dock_rows, dock_overflow) = if child_active {
        (Vec::new(), 0)
    } else {
        app.subagents.dock()
    };
    let dock_height = if dock_rows.is_empty() && dock_overflow == 0 {
        0
    } else {
        (dock_rows.len() + usize::from(dock_overflow > 0)) as u16 + 1
    };
    // The header reserves one blank row beneath the title, matching the
    // design's one-line gap before the transcript.
    let header_height = if child_active { 3 } else { 2 };
    let activity_height = u16::from(app.activity_kind().is_some());
    let bottom_height = if decision {
        decision_height(app, area.width, area.height)
    } else {
        input_height(app, area.width)
    };
    // Ordinary state: activity sits above the composer. Decision state: the
    // card takes the seat first, then the activity row, then the status bar.
    let (activity_slot, bottom_slot) = if decision {
        (bottom_height, activity_height)
    } else {
        (activity_height, bottom_height)
    };
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(header_height),
            Constraint::Min(3),
            Constraint::Length(queue_height),
            Constraint::Length(completion_height),
            Constraint::Length(activity_slot),
            Constraint::Length(bottom_slot),
            Constraint::Length(1),
            Constraint::Length(dock_height),
        ])
        .split(area);

    draw_header(frame, app, layout[0]);
    app.subagent_dock_area = Rect::default();
    app.subagent_dock_hits.clear();
    if app.sessions_panel.open {
        draw_sessions(frame, app, layout[1]);
    } else if app.subagents.nav().list_open {
        draw_subagent_list(frame, app, layout[1]);
    } else if app.jobs_open {
        draw_jobs(frame, app, layout[1]);
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
    if completion_height > 0 {
        draw_completion(frame, app, layout[3]);
    }
    if decision {
        draw_decision(frame, app, layout[4]);
        draw_activity(frame, app, layout[5]);
    } else {
        app.decision_area = Rect::default();
        app.decision_hits.clear();
        draw_activity(frame, app, layout[4]);
        draw_input(frame, app, layout[5]);
    }
    draw_status(frame, app, layout[6], area.width);
    if dock_height > 0 {
        draw_subagent_dock(frame, app, layout[7]);
    }
    if app.panel.is_open() {
        draw_panel(frame, app, full);
    }
}

fn inset_horizontal(area: Rect, margin: u16) -> Rect {
    let margin = margin.min(area.width / 2);
    Rect {
        x: area.x.saturating_add(margin),
        y: area.y,
        width: area.width.saturating_sub(margin.saturating_mul(2)),
        height: area.height,
    }
}

fn draw_terminal(frame: &mut Frame, app: &mut App, area: Rect) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);
    app.terminal_screen_area = layout[0];
    let status = match app.terminal.as_mut() {
        Some(terminal) => {
            terminal.draw(frame, layout[0]);
            terminal.status_line()
        }
        None => {
            frame.render_widget(
                Paragraph::new("终端状态文件不可用；未附着官方终端。")
                    .style(Style::default().fg(Color::Red)),
                layout[0],
            );
            "终端不可用".to_string()
        }
    };
    let color = if app
        .terminal
        .as_ref()
        .and_then(|terminal| terminal.error())
        .is_some()
    {
        Color::Red
    } else {
        Color::DarkGray
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            truncate(&status, area.width.max(1) as usize),
            Style::default().fg(color),
        ))),
        layout[1],
    );
}

fn draw_header(frame: &mut Frame, app: &mut App, area: Rect) {
    app.subagent_exit_area = Rect::default();
    let title = Rect {
        height: area.height.min(1),
        ..area
    };
    draw_app_title(frame, app, title);
    let Some(mounted) = app.subagents.mounted() else {
        return;
    };
    if area.height < 2 {
        return;
    }
    let gate = app
        .subagents
        .mounted_gate()
        .unwrap_or(ComposerGate::Resolving);
    let (state, color) = match gate {
        ComposerGate::Enabled => ("可输入", Color::Green),
        ComposerGate::ReadOnly => ("只读", Color::DarkGray),
        ComposerGate::StopOnly => ("仅停止", Color::Yellow),
        ComposerGate::Resolving => ("读取历史中", Color::Cyan),
        ComposerGate::NotNavigable => ("不可进入", Color::DarkGray),
    };
    // The second row keeps the main title above and adds the child identity:
    // `← 子代理 <task> <state>`.
    let state_width = state.chars().map(display_width).sum::<usize>();
    let label_room = (area.width as usize)
        .saturating_sub(2 + 7 + 1 + state_width)
        .max(1);
    let label = truncate(&safe_text(&mounted.label), label_room);
    let row = Rect {
        y: area.y.saturating_add(1),
        height: 1,
        ..area
    };
    let spans = vec![
        Span::styled("← ", Style::default().fg(Color::Cyan)),
        Span::styled("子代理 ", Style::default().fg(Color::DarkGray)),
        Span::styled(label, Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(format!(" {state}"), Style::default().fg(color)),
    ];
    frame.render_widget(Paragraph::new(Line::from(spans)), row);
    app.subagent_exit_area = Rect::new(area.x, row.y, 2.min(area.width), 1);
}

fn draw_app_title(frame: &mut Frame, app: &App, area: Rect) {
    let path = safe_text(&app.display_path());
    let room = (area.width as usize).saturating_sub(6).max(1);
    let spans = vec![
        Span::styled("nano", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::styled(
            elide_left(&path, room),
            Style::default().fg(Color::DarkGray),
        ),
    ];
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_schedule(frame: &mut Frame, app: &mut App, area: Rect) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(area);
    draw_header(frame, app, layout[0]);
    draw_schedule_body(frame, app, layout[1]);
    draw_status(frame, app, layout[2], area.width);
}

fn draw_schedule_body(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" 定时任务 · 官方 schedule ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let width = inner.width.max(1) as usize;
    let rows = app.schedule.rows();
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut selected_line = None;
    for row in rows {
        if row.kind == RowKind::Selected && selected_line.is_none() {
            selected_line = Some(lines.len());
        }
        let style = schedule_style(row.kind);
        for segment in wrap_segments(&row.text, width) {
            lines.push(Line::from(Span::styled(segment.text, style)));
        }
    }
    let viewport = inner.height as usize;
    let offset = selected_line
        .map(|start| start.saturating_sub(viewport.saturating_sub(1)))
        .unwrap_or(0);
    frame.render_widget(
        Paragraph::new(Text::from(lines)).scroll((offset.min(u16::MAX as usize) as u16, 0)),
        inner,
    );
}

fn schedule_style(kind: RowKind) -> Style {
    match kind {
        RowKind::Title => Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
        RowKind::Plain => Style::default(),
        RowKind::Dim => Style::default().fg(Color::DarkGray),
        RowKind::Error => Style::default().fg(Color::Red),
        RowKind::Selected => Style::default().fg(Color::Black).bg(Color::Cyan),
        RowKind::SelectedDim => Style::default().fg(Color::Black).bg(Color::DarkGray),
        RowKind::Notice => Style::default().fg(Color::Yellow),
    }
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

/// Bottom subagent dock: at most four direct children plus one overflow row.
fn draw_subagent_dock(frame: &mut Frame, app: &mut App, area: Rect) {
    let (rows, overflow) = app.subagents.dock();
    app.subagent_dock_area = area;
    app.subagent_dock_hits.clear();
    // The dock has no title: the design keeps the top edge as a plain rule and
    // moves the count to the all-agents list.
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let blink_on = app.reduced_motion || app.subagents.lifecycle().blink_on(Instant::now());
    let width = inner.width.max(1) as usize;
    let hover = hover_background(DiffTheme::from_env());
    let mut lines: Vec<Line<'static>> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let selected = app.subagents.nav().dock_focus && app.subagents.nav().dock_selected == index;
        let marker = if selected { "›" } else { " " };
        let lamp = app.subagents.lamp(row);
        let (dot, color) = lamp_glyph(lamp, blink_on);
        let meta = row_status(lamp);
        let name = safe_text(row.display_label());
        let meta_width = meta.chars().map(display_width).sum::<usize>();
        // marker(1) + space(1) + dot(1) + space(1) + gap(1) + status.
        let name_room = width.saturating_sub(4 + 1 + meta_width).max(1);
        let name = truncate(&name, name_room);
        let left_width = 4 + name.chars().map(display_width).sum::<usize>();
        let pad = width.saturating_sub(left_width + meta_width).max(1);
        let row_style = if selected {
            Style::default().bg(hover)
        } else {
            Style::default()
        };
        let pointer_style = row_style.fg(Color::Cyan);
        let dot_style = row_style.fg(color);
        let status_style = row_style.fg(Color::DarkGray);
        let spans = vec![
            Span::styled(format!("{marker} "), pointer_style),
            Span::styled(dot.to_string(), dot_style),
            Span::styled(format!(" {name}"), row_style),
            Span::styled(" ".repeat(pad), row_style),
            Span::styled(meta.to_string(), status_style),
        ];
        lines.push(Line::from(spans));
        app.subagent_dock_hits.push((index, inner.y + index as u16));
    }
    if overflow > 0 {
        let selected =
            app.subagents.nav().dock_focus && app.subagents.nav().dock_selected == rows.len();
        let marker = if selected { "›" } else { " " };
        let style = if selected {
            Style::default().fg(Color::Cyan).bg(hover)
        } else {
            Style::default().fg(Color::Cyan)
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{marker} "), style),
            Span::styled("● ".to_string(), style),
            Span::styled(format!("还有 {overflow} 个子代理"), style),
        ]));
        app.subagent_dock_hits
            .push((rows.len(), inner.y + rows.len() as u16));
    }
    frame.render_widget(Paragraph::new(Text::from(lines)), inner);
}

/// Focused dock rows use the design's hover token through the same
/// `NANO_THEME` selection as the diff tokens (batch 2 landed the read first).
fn hover_background(theme: DiffTheme) -> Color {
    match theme {
        DiffTheme::Dark => Color::Rgb(36, 43, 53),
        DiffTheme::Light => Color::Rgb(232, 232, 229),
    }
}

fn muted_style() -> Style {
    Style::default().fg(Color::DarkGray)
}

/// All-agents list: recursive descendants with parent relationships and
/// diagnostics for unreadable branches.
fn draw_subagent_list(frame: &mut Frame, app: &mut App, area: Rect) {
    let title = if app.subagent_list_loading {
        " 全部子代理 · 读取中… · ↑↓ 选择 · r 刷新 · Esc 关闭 ".to_string()
    } else {
        format!(
            " 全部子代理 · {} · ↑↓ 选择 · Enter 进入 · r 刷新 · Esc 关闭 ",
            app.subagents.local_count()
        )
    };
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    app.subagent_list_area = area;
    let rows = app.subagents.list_rows();
    let selected = app.subagents.nav().list_selected;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);
    let width = inner.width.max(1) as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    if rows.is_empty() {
        lines.push(Line::from(Span::styled(
            if app.subagent_list_loading {
                "正在读取官方子代理目录…"
            } else {
                "没有已知子代理。"
            },
            Style::default().fg(Color::DarkGray),
        )));
    }
    for (index, row) in rows.iter().enumerate() {
        let indent = "  ".repeat(row.depth.saturating_sub(1) as usize);
        let branch = if row.depth > 1 { "└ " } else { "" };
        let mut text = format!(
            "{indent}{branch}{} · {} · {}",
            safe_text(row.display_label()),
            row.mode.label(),
            row.activity.label()
        );
        if row.mode == nano_subagents::AgentMode::External {
            text.push_str(" · 不可进入");
        }
        if let Some(duration) = row.duration_ms(now_ms) {
            text.push_str(&format!(" · {}", format_duration_ms(duration)));
        }
        if let Some(completed) = row.last_turn_completed {
            if completed {
                text.push_str(" · 上回合完成");
            }
        }
        let style = if index == selected {
            Style::default().fg(Color::Black).bg(Color::Cyan)
        } else {
            Style::default()
        };
        for segment in wrap_segments(&truncate(&text, width.saturating_mul(2)), width) {
            lines.push(Line::from(Span::styled(segment.text, style)));
        }
    }
    for diagnostic in app.subagents.diagnostics() {
        let mut text = format!(
            "! 读不到 {}: {}",
            safe_text(&diagnostic.id),
            safe_text(&diagnostic.reason)
        );
        if let Some(parent) = &diagnostic.parent_id {
            text.push_str(&format!("（父 {}）", safe_text(parent)));
        }
        for segment in wrap_segments(&truncate(&text, width.saturating_mul(2)), width) {
            lines.push(Line::from(Span::styled(
                segment.text,
                Style::default().fg(Color::Red),
            )));
        }
    }
    if let Some(error) = app.subagents.last_error() {
        for segment in wrap_segments(&truncate(&safe_text(error), width.saturating_mul(2)), width) {
            lines.push(Line::from(Span::styled(
                segment.text,
                Style::default().fg(Color::Red),
            )));
        }
    }
    let viewport = inner.height as usize;
    let start = selected
        .saturating_add(1)
        .saturating_sub(viewport.max(1))
        .min(lines.len().saturating_sub(viewport.max(1)));
    let visible: Vec<Line<'static>> = lines.into_iter().skip(start).take(viewport).collect();
    frame.render_widget(Paragraph::new(Text::from(visible)), inner);
}

/// Replace terminal control characters with a visible marker. Labels come
/// from model/tool input and must never be able to inject escape sequences.
fn safe_text(text: &str) -> String {
    text.chars()
        .map(|character| {
            if character.is_control() {
                '?'
            } else {
                character
            }
        })
        .collect()
}

fn format_duration_ms(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{}m{:02}s", ms / 60_000, (ms % 60_000) / 1000)
    }
}

fn lamp_glyph(lamp: Lamp, blink_on: bool) -> (&'static str, Color) {
    match lamp {
        Lamp::Running => (
            if blink_on { "●" } else { "○" },
            if blink_on {
                Color::Cyan
            } else {
                Color::DarkGray
            },
        ),
        Lamp::Completed => ("●", Color::Green),
        Lamp::Warned => ("●", Color::Yellow),
        Lamp::Error => ("●", Color::Red),
        Lamp::Inactive => ("●", Color::DarkGray),
    }
}

/// Right-hand status word for one dock row. The official terminal lamp is the
/// authority; `activity` alone cannot tell a finished child from a live one.
fn row_status(lamp: Lamp) -> &'static str {
    match lamp {
        Lamp::Running => "执行中",
        Lamp::Completed => "完成",
        Lamp::Warned => "终止",
        Lamp::Error => "错误",
        Lamp::Inactive => "未运行",
    }
}

fn draw_feed(frame: &mut Frame, app: &mut App, area: Rect) {
    app.feed_area = area;
    app.jump_area = Rect::default();
    app.feed_height = area.height as usize;
    if app.transcript_dirty || !app.active_model().previews().is_empty() {
        // The mounted child owns its own official model; the root model keeps
        // its history and previews untouched while the child view is active.
        if app.subagent_active() {
            if let Some(key) = app.active_child_key.clone() {
                if let Some(model) = app.child_models.get(&key) {
                    app.view.sync(model, Instant::now());
                }
            }
        } else {
            app.view.sync(&app.model, Instant::now());
        }
        app.transcript_dirty = false;
    }
    let render = app
        .view
        .render(area.width.max(1) as usize, area.height.max(1) as usize);
    app.feed_hits = render.hits;
    let mut lines = render.lines;
    // Local `!` records are S08 display data until the official synthetic
    // message is durable. Show them only while following the bottom.
    if !app.subagent_active() && app.view.scroll().sticky() && !app.bash_records.is_empty() {
        let before = lines.len();
        for record in &app.bash_records {
            push_bash_record(&mut lines, record, area.width.max(1) as usize);
        }
        let added = lines.len().saturating_sub(before);
        app.feed_hits.extend(std::iter::repeat(None).take(added));
        let viewport = area.height as usize;
        if lines.len() > viewport {
            let extra = lines.len() - viewport;
            lines.drain(0..extra);
            app.feed_hits.drain(0..extra.min(app.feed_hits.len()));
        }
    }
    if lines.is_empty() {
        let empty = if app.subagent_active() {
            "正在读取子代理历史…"
        } else {
            "输入消息后按 Enter 发送。"
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                empty,
                Style::default().fg(Color::DarkGray),
            ))),
            area,
        );
    } else {
        frame.render_widget(Paragraph::new(Text::from(lines)), area);
    }
    if let Some(rect) = jump_button_rect(area, !app.view.scroll().sticky()) {
        app.jump_area = rect;
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                " 回到最新 ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::REVERSED),
            ))),
            rect,
        );
    }
}

/// Bottom-right "回到最新" button, 2 cells from the right edge.
fn jump_button_rect(area: Rect, visible: bool) -> Option<Rect> {
    if !visible || area.height == 0 || area.width < 12 {
        return None;
    }
    Some(Rect {
        x: area.right().saturating_sub(12),
        y: area.y.saturating_add(area.height.saturating_sub(1)),
        width: 10,
        height: 1,
    })
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
        // Keep the tail: the official truncation/spill notice is appended at
        // the end and must not be cut away by the display bound.
        wrap_tail_block(lines, &stdout, width, 4_000, Color::White);
    }
    let stderr = record.stderr();
    if !stderr.is_empty() {
        lines.push(Line::from(Span::styled(
            "[stderr]",
            Style::default().fg(Color::Red),
        )));
        wrap_tail_block(lines, &stderr, width, 4_000, Color::Red);
    }
    if let Some(error) = &record.inject_error {
        lines.push(Line::from(Span::styled(
            format!("（输出未注入会话：{error}）"),
            Style::default().fg(Color::Yellow),
        )));
    }
}

fn draw_jobs(frame: &mut Frame, app: &mut App, area: Rect) {
    let title = if app.monitor_form.is_some() {
        let form = app.monitor_form.as_ref().expect("form is present");
        format!(
            " 监听 · 附着到 {} · n 新输出 / a 从开头 / t 模式 / f 过滤 / r 正则 / Enter 创建 / Esc 取消 ",
            truncate(&form.job_label, 48)
        )
    } else if app.monitor_open {
        match app.monitor.selected_monitor() {
            Some(monitor) => format!(
                " 监听 · {} · {} · ↑↓ 切换，s 停止监听，b 重置预算，r 刷新，Esc 返回 ",
                monitor.job_id,
                monitor.mode.label()
            ),
            None => " 监听 · 暂无记录 · Esc 返回 ".to_string(),
        }
    } else if app.jobs.detail_open() {
        let job = app.jobs.detail_job();
        match job {
            Some(job) => format!(
                " 任务详情 · {} · {} · ↑↓ 滚动，m 监听，k 停止，r 重读，Esc 返回 ",
                job.id,
                job.status_line()
            ),
            None => " 任务详情 · 已移除 · Esc 返回 ".to_string(),
        }
    } else {
        format!(
            " 任务 · 官方 jobs · {} 个（{} 运行中） · ↑↓ 选择，Enter 详情，m 监听，k 停止，r 刷新，Esc 关闭 ",
            app.jobs.len(),
            app.jobs.active_count()
        )
    };
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let width = inner.width.max(1) as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    if app.monitor_form.is_some() {
        push_monitor_form(&mut lines, app, width);
    } else if app.monitor_open {
        push_monitor_view(&mut lines, app, width);
    } else if app.jobs.detail_open() {
        push_job_detail(&mut lines, app, width);
    } else if app.jobs.is_empty() {
        let message = if app.ready {
            "（没有可见任务。Agent 的前台命令在运行期间也会出现在这里。）"
        } else {
            "（宿主已断开；任务随宿主结束，不显示跨重启的运行状态）"
        };
        lines.push(Line::from(Span::styled(
            message,
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        push_job_list(&mut lines, app);
    }
    let viewport = inner.height as usize;
    let total = lines.len();
    let offset = if app.monitor_form.is_some() {
        app.jobs_scroll_back = 0;
        0
    } else if app.monitor_open {
        let max_scroll = total.saturating_sub(viewport);
        app.jobs_scroll_back.min(max_scroll)
    } else if app.jobs.detail_open() {
        app.jobs_detail_lines = total;
        let max_scroll = total.saturating_sub(viewport);
        total
            .saturating_sub(viewport)
            .saturating_sub(app.jobs_scroll_back.min(max_scroll))
    } else {
        // Keep the selected row inside the viewport; the list itself is not a
        // tail view. Up/Down therefore scroll a long list with the selection.
        list_offset(app.jobs.selected(), total, viewport)
    };
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
        let mut spans = vec![
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
        ];
        if let Some(monitor) = app.monitor.monitor_for_job(&job.id) {
            let budget = if monitor.exhausted {
                "预算用尽".to_string()
            } else {
                format!(
                    "{}/{}",
                    monitor.used.notifications, monitor.budgets.max_notifications
                )
            };
            let delivery = monitor
                .last_delivery()
                .map(|delivery| delivery.state.short_label())
                .unwrap_or("尚未投递");
            spans.push(Span::styled(
                format!(
                    " · 监听 {} · {} · {}",
                    monitor.mode.short_label(),
                    budget,
                    delivery
                ),
                Style::default().fg(if monitor.exhausted {
                    Color::Yellow
                } else {
                    Color::Cyan
                }),
            ));
        }
        lines.push(Line::from(spans));
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
    if let Some(monitor) = app.monitor.monitor_for_job(&job.id) {
        lines.push(Line::from(Span::styled(
            format!(
                "监听 · {} · {}",
                monitor.mode.label(),
                monitor.stream.label()
            ),
            Style::default().fg(Color::Cyan),
        )));
        lines.push(Line::from(Span::styled(
            monitor.budget_label(),
            Style::default().fg(if monitor.exhausted {
                Color::Yellow
            } else {
                Color::DarkGray
            }),
        )));
        lines.push(Line::from(Span::styled(
            format!("{} · {}", monitor.identity_label(), monitor.offset_label()),
            Style::default().fg(Color::DarkGray),
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
                // The buffer keeps its tail; render the tail as well, never the
                // head of a tail buffer.
                wrap_tail_block(lines, &buffer.text, width, 200_000, Color::Gray);
            }
        }
        None => lines.push(Line::from(Span::styled(
            "（尚未读取输出；按 r 读取）",
            Style::default().fg(Color::DarkGray),
        ))),
    }
}

fn push_monitor_form(lines: &mut Vec<Line<'static>>, app: &App, width: usize) {
    let Some(form) = &app.monitor_form else {
        return;
    };
    lines.push(Line::from(Span::styled(
        format!("附着监听 · {}", form.job_label),
        Style::default().fg(Color::Cyan),
    )));
    lines.push(Line::from(Span::styled(
        format!("任务 {}", form.job_id),
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::from(Span::styled(
        format!(
            "已有 {} 字节未读；先选范围：n 只看新输出（跳过这些字节） / a 从开头（重放 retained head）",
            form.unread_bytes
        ),
        Style::default().fg(Color::Yellow),
    )));
    lines.push(Line::from(Span::styled(
        if form.from_earliest {
            "当前范围：从开头；被 ring 裁掉的旧字节会标「已裁剪」，不会拼接假连续内容。"
        } else {
            "当前范围：只看新输出；附着后不会重放已有字节。"
        },
        Style::default().fg(Color::Gray),
    )));
    lines.push(Line::from(Span::styled(
        if form.quiet {
            "模式：quiet（官方 inject；不唤醒 Agent，空闲时可能一直等待；仍需 durable user/message 才算收件）"
        } else {
            "模式：notify（官方 followup；唤醒 Agent，会产生模型调用和费用）"
        },
        Style::default().fg(if form.quiet { Color::DarkGray } else { Color::Yellow }),
    )));
    let filter = if form.filter.is_empty() {
        "过滤：无（每行都是候选）".to_string()
    } else if form.filter_regex {
        format!(
            "过滤：正则 /{}/{}",
            form.filter,
            if form.editing_filter {
                "（编辑中）"
            } else {
                ""
            }
        )
    } else {
        format!(
            "过滤：字面「{}」{}",
            form.filter,
            if form.editing_filter {
                "（编辑中）"
            } else {
                ""
            }
        )
    };
    push_wrapped(lines, &filter, width, Style::default().fg(Color::Gray));
    lines.push(Line::from(Span::styled(
        "t 模式 · f 编辑过滤 · r 字面/正则 · Enter 创建 · Esc 取消",
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::from(Span::styled(
        "提示：同一任务已有活动监听时返回现有监听；停止监听不会停止任务。",
        Style::default().fg(Color::DarkGray),
    )));
}

fn push_monitor_view(lines: &mut Vec<Line<'static>>, app: &App, width: usize) {
    let Some(monitor) = app.monitor.selected_monitor() else {
        lines.push(Line::from(Span::styled(
            "（暂无监听。在任务列表选中任务后按 m 创建。）",
            Style::default().fg(Color::DarkGray),
        )));
        return;
    };
    lines.push(Line::from(vec![
        Span::styled(
            format!("监听 {}", monitor.job_id),
            Style::default().fg(Color::Cyan),
        ),
        Span::raw(" · "),
        Span::styled(
            monitor.stream.label(),
            Style::default().fg(if monitor.stream.is_terminal() {
                Color::Yellow
            } else {
                Color::Green
            }),
        ),
    ]));
    push_wrapped(
        lines,
        &monitor.identity_label(),
        width,
        Style::default().fg(Color::Yellow),
    );
    push_wrapped(
        lines,
        &monitor.mode.label().to_string(),
        width,
        Style::default().fg(Color::Gray),
    );
    push_wrapped(
        lines,
        &format!(
            "范围：{} · {}",
            monitor.from.label(),
            monitor.offset_label()
        ),
        width,
        Style::default().fg(Color::Gray),
    );
    if matches!(monitor.from, nano_monitor::MonitorFrom::Earliest) && monitor.initial_offset > 0 {
        lines.push(Line::from(Span::styled(
            format!(
                "retained head 从字节 {} 开始；更早字节已被 ring 裁剪（已裁剪）。",
                monitor.initial_offset
            ),
            Style::default().fg(Color::Yellow),
        )));
    }
    let filter = monitor
        .filter
        .as_ref()
        .map(|filter| filter.label())
        .unwrap_or_else(|| "无（每行都是候选）".to_string());
    push_wrapped(
        lines,
        &format!("合并窗口 {} ms · 过滤：{}", monitor.merge_window_ms, filter),
        width,
        Style::default().fg(Color::Gray),
    );
    let budget_color = if monitor.exhausted {
        Color::Yellow
    } else {
        Color::Gray
    };
    push_wrapped(
        lines,
        &monitor.budget_label(),
        width,
        Style::default().fg(budget_color),
    );
    if monitor.exhausted {
        lines.push(Line::from(Span::styled(
            if monitor.exhaust_reason.as_deref() == Some("expired") {
                "监听有效期已过：普通行投递已停止，计数保留；按 b 显式重置有效期。"
            } else {
                "预算已用尽：普通行投递已停止，计数保留；按 b 显式重置后恢复。"
            },
            Style::default().fg(Color::Yellow),
        )));
    }
    if monitor.stopped_by_user {
        lines.push(Line::from(Span::styled(
            "用户已停止监听；被观察任务未受影响。",
            Style::default().fg(Color::Yellow),
        )));
    }
    if monitor.lossy {
        lines.push(Line::from(Span::styled(
            "已裁剪：监听的请求位置早于官方 ring 保留范围，更早字节不可读。",
            Style::default().fg(Color::Yellow),
        )));
    }
    if monitor.gaps > 0 {
        lines.push(Line::from(Span::styled(
            format!(
                "输出缺口 {} 次：更早字节不可读，监听不会把缺口前后拼成假连续文本。",
                monitor.gaps
            ),
            Style::default().fg(Color::Yellow),
        )));
    }
    if let Some(detail) = &monitor.stream_detail {
        push_wrapped(
            lines,
            &format!("流结束原因：{detail}"),
            width,
            Style::default().fg(Color::DarkGray),
        );
    }
    if let Some(error) = &monitor.last_error {
        push_wrapped(
            lines,
            &format!("最近错误：{error}"),
            width,
            Style::default().fg(Color::Red),
        );
    }
    lines.push(Line::from(Span::styled(
        "投递回执：submitted/inserted/claimed 都不证明模型收到；只有 durable user/message 是 accepted。",
        Style::default().fg(Color::DarkGray),
    )));
    if monitor.deliveries.is_empty() {
        lines.push(Line::from(Span::styled(
            "（还没有通知投递）",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        for delivery in monitor.deliveries.iter().rev().take(10) {
            let color = if delivery.state.is_proven() {
                Color::Green
            } else if delivery.state.is_failure() {
                Color::Red
            } else {
                Color::Yellow
            };
            lines.push(Line::from(Span::styled(
                format!(
                    "· {} · {} 行 · {}..{} · {}",
                    delivery.state.label(),
                    delivery.lines,
                    delivery.first_offset,
                    delivery.last_offset,
                    delivery.message_id
                ),
                Style::default().fg(color),
            )));
            if let Some(detail) = &delivery.detail {
                push_wrapped(
                    lines,
                    &format!("  {detail}"),
                    width,
                    Style::default().fg(Color::DarkGray),
                );
            }
        }
    }
    lines.push(Line::from(Span::styled(
        "s 停止监听（不停止任务） · b 重置预算 · r 刷新 · Esc 返回",
        Style::default().fg(Color::DarkGray),
    )));
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

fn decision_height(app: &App, width: u16, height: u16) -> u16 {
    let max = height.saturating_sub(8).max(6);
    let desired = match app.decisions.top() {
        Some(DecisionTop::Approval(card)) => {
            let reason = card
                .display_reason()
                .or(card.reason())
                .unwrap_or("官方请求未附原因");
            let reason_rows = display_rows(reason, width as usize).len().max(1);
            let command_rows = card
                .command()
                .map(|command| display_rows(command, width.saturating_sub(2) as usize).len())
                .unwrap_or(1);
            let state_rows = usize::from(
                matches!(card.state(), ApprovalCardState::Settled(_)) || card.notice().is_some(),
            );
            (1 + 1 + reason_rows + command_rows + 4 + 1 + state_rows) as u16
        }
        Some(DecisionTop::Question(card)) => question_height(card, width),
        None => 6,
    };
    desired.clamp(6, max)
}

fn question_height(card: &QuestionCard, width: u16) -> u16 {
    if card.state() == QuestionCardState::ReadOnly {
        return 4 + card.questions().len() as u16 * 3;
    }
    if card.plan_review().is_some() {
        let plan_rows = if card.plan_expanded() {
            card.questions()[0]
                .detail
                .as_deref()
                .map(|detail| display_rows(detail, width.saturating_sub(4) as usize).len())
                .unwrap_or(0)
                .min(10) as u16
        } else {
            0
        };
        return 10 + plan_rows;
    }
    let mut rows = 1u16; // top accent line
    if card.questions().len() > 1 {
        rows += 1; // tabs
    }
    if card.page() == card.questions().len() {
        rows += 1; // submit title
        rows += card.questions().len() as u16;
        rows += display_rows(&submit_footer(card), width as usize).len() as u16;
    } else if let Some(question) = card.questions().get(card.page()) {
        rows += capped_rows(&question.question, width as usize, 3).len() as u16;
        for option in &question.options {
            rows += 1;
            if option.description.is_some() {
                rows += 1;
            }
        }
        rows += 1; // Other row
        rows += display_rows(&question_footer(card, question), width as usize).len() as u16;
    } else {
        rows += 1;
    }
    if card.error().is_some() {
        rows += 1;
    }
    if card.notice().is_some() {
        rows += 1;
    }
    rows
}

fn draw_decision(frame: &mut Frame, app: &mut App, area: Rect) {
    app.decision_area = area;
    let flat = match app.decisions.top() {
        Some(DecisionTop::Approval(_)) => true,
        Some(DecisionTop::Question(card)) => {
            card.state() != QuestionCardState::ReadOnly && card.plan_review().is_none()
        }
        None => false,
    };
    let (title, lines, hits) = match app.decisions.top() {
        Some(DecisionTop::Approval(card)) => render_approval(card, area),
        Some(DecisionTop::Question(card)) => render_question(card, area),
        None => (String::new(), Vec::new(), Vec::new()),
    };
    let block = if flat {
        Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(Color::Cyan))
    } else {
        Block::default().borders(Borders::ALL).title(title)
    };
    let inner = block.inner(area);
    let visible: Vec<Line<'static>> = lines.iter().take(inner.height as usize).cloned().collect();
    app.decision_hits = map_decision_hits(&hits, inner.height, area.height);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(Text::from(visible)), inner);
}

/// Map content-row hits onto the full card area. Row 0 is the top border and
/// the last row is the bottom border; hits past the inner height are clipped
/// with the text and must never answer a hidden option.
fn map_decision_hits(
    hits: &[Option<DecisionClick>],
    inner_height: u16,
    area_height: u16,
) -> Vec<Option<DecisionClick>> {
    let mut mapped = Vec::with_capacity(area_height as usize);
    mapped.push(None);
    mapped.extend(hits.iter().take(inner_height as usize).cloned());
    while mapped.len() < area_height as usize {
        mapped.push(None);
    }
    mapped.truncate(area_height as usize);
    mapped
}
fn render_approval(
    card: &nano_decisions::ApprovalCard,
    area: Rect,
) -> (String, Vec<Line<'static>>, Vec<Option<DecisionClick>>) {
    let width = area.width.max(1) as usize;
    let title = String::new();
    let mut lines = Vec::new();
    let mut hits = Vec::new();
    let question = if card.tool_name().eq_ignore_ascii_case("bash") {
        "是否运行这条检查命令？".to_string()
    } else {
        format!("是否运行 {}？", card.tool_name())
    };
    lines.push(Line::from(Span::styled(
        question,
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    )));
    hits.push(None);
    let reason = card
        .display_reason()
        .or(card.reason())
        .unwrap_or("官方请求未附原因");
    for row in display_rows(reason, width) {
        lines.push(Line::from(Span::styled(
            row,
            Style::default().fg(Color::DarkGray),
        )));
        hits.push(None);
    }
    let content_height = area.height.saturating_sub(1) as usize;
    let option_rows = 4usize;
    let footer_rows = 1usize;
    let state_rows = usize::from(
        matches!(card.state(), ApprovalCardState::Settled(_)) || card.notice().is_some(),
    );
    let available = content_height
        .saturating_sub(lines.len() + option_rows + footer_rows + state_rows)
        .max(1);
    let command_rows = card
        .command()
        .map(|command| display_rows(command, width.saturating_sub(2).max(1)))
        .unwrap_or_default();
    if card.command().is_some() {
        let start = card.command_offset().min(command_rows.len());
        for row in command_rows.iter().skip(start).take(available) {
            lines.push(Line::from(Span::styled(
                format!("  {row}"),
                Style::default().fg(Color::DarkGray),
            )));
            hits.push(None);
        }
    } else {
        lines.push(Line::from(Span::styled(
            "官方请求没有可关联的命令；不猜测命令",
            Style::default().fg(Color::DarkGray),
        )));
        hits.push(None);
    }
    let allow_focused = card.focus() == ApprovalFocus::Allow;
    let reject_focused = card.focus() == ApprovalFocus::Reject;
    lines.push(option_line(allow_focused, "1.", "允许一次", None, false));
    hits.push(Some(DecisionClick::ApprovalAllow));
    lines.push(Line::from(Span::styled(
        "     仅本次；不更改后续权限",
        Style::default().fg(Color::DarkGray),
    )));
    hits.push(Some(DecisionClick::ApprovalAllow));
    lines.push(option_line(reject_focused, "2.", "拒绝", None, false));
    hits.push(Some(DecisionClick::ApprovalReject));
    lines.push(Line::from(Span::styled(
        "     不执行本次操作",
        Style::default().fg(Color::DarkGray),
    )));
    hits.push(Some(DecisionClick::ApprovalReject));
    let mut footer = String::from("↑↓ 切换 · Enter 确认 · Esc 取消");
    if command_rows.len() > available {
        footer.push_str(" · PgUp/PgDn");
    }
    lines.push(Line::from(Span::styled(
        footer,
        Style::default().fg(Color::DarkGray),
    )));
    hits.push(None);
    if let ApprovalCardState::Settled(outcome) = card.state() {
        lines.push(Line::from(Span::styled(
            outcome.label().to_string(),
            Style::default().fg(Color::DarkGray),
        )));
        hits.push(None);
    } else if let Some(notice) = card.notice() {
        lines.push(Line::from(Span::styled(
            notice.to_string(),
            Style::default().fg(Color::Yellow),
        )));
        hits.push(None);
    }
    (title, lines, hits)
}

fn capped_rows(text: &str, width: usize, max_rows: usize) -> Vec<String> {
    let mut rows = display_rows(text, width);
    if rows.len() > max_rows {
        rows.truncate(max_rows);
        if let Some(last) = rows.last_mut() {
            *last = format!("{}…", truncate(last, width.saturating_sub(1)));
        }
    }
    rows
}

fn question_footer(card: &QuestionCard, question: &nano_decisions::Question) -> String {
    let action = if question.multi_select {
        "空格勾选 · Enter 确认"
    } else {
        "Enter 选择"
    };
    let mut footer = format!("↑↓ 移动 · {action} · Alt+N 跳过 · Alt+T 停止计时 · Esc 收起");
    if let Some(remaining) = card.timed().remaining_at(Instant::now()) {
        footer.push_str(&format!(" · 剩 {}", format_duration(remaining)));
    } else if card.timed().indefinite {
        footer.push_str(" · 计时已停");
    }
    footer
}

fn submit_footer(card: &QuestionCard) -> String {
    let mut footer = String::from("Enter 提交 · Tab/←→ 回看 · Esc 收起");
    if let Some(remaining) = card.timed().remaining_at(Instant::now()) {
        footer.push_str(&format!(" · 剩 {}", format_duration(remaining)));
    } else if card.timed().indefinite {
        footer.push_str(" · 计时已停");
    }
    footer
}

fn render_question(
    card: &QuestionCard,
    area: Rect,
) -> (String, Vec<Line<'static>>, Vec<Option<DecisionClick>>) {
    let width = area.width.max(1) as usize;
    if card.state() == QuestionCardState::ReadOnly {
        return render_read_only(card, area);
    }
    if card.plan_review().is_some() {
        return render_plan_review(card, area);
    }
    let title = if card.timed().continued {
        " 问卷 · 可继续回答 ".to_string()
    } else if card.timed().remaining_ms.is_some() {
        " 问卷 · 限时 ".to_string()
    } else {
        " 问卷 ".to_string()
    };
    let mut lines = Vec::new();
    let mut hits = Vec::new();
    if card.questions().len() > 1 {
        let mut tabs = Vec::new();
        for (index, question) in card.questions().iter().enumerate() {
            let current = index == card.page();
            let answered = card.skipped(index)
                || card
                    .draft(index)
                    .map(|draft| !draft.selected.is_empty() || !draft.custom.trim().is_empty())
                    .unwrap_or(false);
            let label = question
                .header
                .clone()
                .unwrap_or_else(|| format!("{}", index + 1));
            tabs.push(Span::styled(
                format!("{}{} ", if answered { "[x]" } else { "[ ]" }, label),
                if current {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::DarkGray)
                },
            ));
        }
        tabs.push(Span::styled(
            format!(
                "{}{} ",
                if card.page() == card.questions().len() {
                    "[x]"
                } else {
                    "[ ]"
                },
                "提交"
            ),
            if card.page() == card.questions().len() {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            },
        ));
        lines.push(Line::from(tabs));
        hits.push(Some(DecisionClick::QuestionPrev));
    }
    if card.page() == card.questions().len() {
        lines.push(Line::from(Span::styled(
            "提交这组答案",
            Style::default().add_modifier(Modifier::BOLD),
        )));
        hits.push(None);
        for (index, question) in card.questions().iter().enumerate() {
            let answer = card.review_answers().get(index).cloned();
            let text = answer
                .as_ref()
                .map(answer_text)
                .unwrap_or_else(|| "尚未回答".to_string());
            lines.push(Line::from(vec![
                Span::styled(
                    if text == "尚未回答" || text == "（跳过）" {
                        "[ ] "
                    } else {
                        "[x] "
                    },
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    question
                        .header
                        .clone()
                        .unwrap_or_else(|| question.id.clone()),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("  {text}"), Style::default().fg(Color::DarkGray)),
            ]));
            hits.push(Some(DecisionClick::QuestionOption(index)));
        }
        for row in display_rows(&submit_footer(card), width) {
            lines.push(Line::from(Span::styled(
                row,
                Style::default().fg(Color::DarkGray),
            )));
            hits.push(Some(DecisionClick::QuestionSubmit));
        }
    } else if let Some(question) = card.questions().get(card.page()) {
        for row in capped_rows(&question.question, width, 3) {
            lines.push(Line::from(Span::styled(
                row,
                Style::default().add_modifier(Modifier::BOLD),
            )));
            hits.push(None);
        }
        let draft = card.draft(card.page()).cloned().unwrap_or_default();
        for (index, option) in question.options.iter().enumerate() {
            let focused = card.focus() == index;
            let selected = draft.selected.iter().any(|label| label == &option.label);
            lines.push(option_line(
                focused,
                &format!("{}.", index + 1),
                &option.label,
                if question.multi_select {
                    Some(selected)
                } else {
                    None
                },
                selected,
            ));
            hits.push(Some(DecisionClick::QuestionOption(index)));
            if let Some(description) = &option.description {
                let rows = display_rows(description, width.saturating_sub(5));
                let mut row = rows.first().cloned().unwrap_or_default();
                if rows.len() > 1 {
                    row = format!("{}…", truncate(&row, width.saturating_sub(6)));
                }
                lines.push(Line::from(Span::styled(
                    format!("     {row}"),
                    Style::default().fg(Color::DarkGray),
                )));
                hits.push(Some(DecisionClick::QuestionOption(index)));
            }
        }
        let other_focused = card.focus() == question.options.len();
        let caret = if other_focused { "|" } else { "" };
        lines.push(Line::from(vec![
            Span::styled(
                format!("{} ", if other_focused { ">" } else { " " }),
                Style::default().fg(Color::Cyan),
            ),
            Span::styled(
                format!("{}. ", question.options.len() + 1),
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw("其他："),
            Span::styled(
                format!("{}{caret}", draft.custom),
                Style::default().fg(Color::Yellow),
            ),
        ]));
        hits.push(Some(DecisionClick::QuestionOther));
        for row in display_rows(&question_footer(card, question), width) {
            lines.push(Line::from(Span::styled(
                row,
                Style::default().fg(Color::DarkGray),
            )));
            hits.push(Some(DecisionClick::QuestionSkip));
        }
    }
    if let Some(error) = card.error() {
        lines.push(Line::from(Span::styled(
            error.to_string(),
            Style::default().fg(Color::Red),
        )));
        hits.push(None);
    }
    if let Some(notice) = card.notice() {
        lines.push(Line::from(Span::styled(
            notice.to_string(),
            Style::default().fg(Color::DarkGray),
        )));
        hits.push(None);
    }
    (title, lines, hits)
}

fn render_plan_review(
    card: &QuestionCard,
    area: Rect,
) -> (String, Vec<Line<'static>>, Vec<Option<DecisionClick>>) {
    let width = area.width.saturating_sub(4) as usize;
    let question = &card.questions()[0];
    let intent = question.plan_review().unwrap();
    let title = " 计划审阅 ".to_string();
    let mut lines = Vec::new();
    let mut hits = Vec::new();
    let summary = question
        .detail
        .as_deref()
        .unwrap_or("")
        .split('\n')
        .filter(|line| !line.trim().is_empty())
        .take(2)
        .collect::<Vec<_>>()
        .join(" ");
    lines.push(Line::from(Span::styled(
        format!("计划：{}", elide_plain(&summary, width)),
        Style::default().add_modifier(Modifier::BOLD),
    )));
    hits.push(Some(DecisionClick::PlanView));
    lines.push(Line::from(Span::styled(
        format!("批准选项：{}", intent.approve),
        Style::default().fg(Color::DarkGray),
    )));
    hits.push(None);
    if card.plan_expanded() {
        if let Some(detail) = question.detail.as_deref() {
            let rows = display_rows(detail, width);
            for row in rows.iter().skip(card.detail_offset()).take(10) {
                lines.push(Line::from(Span::styled(
                    row.clone(),
                    Style::default().fg(Color::DarkGray),
                )));
                hits.push(None);
            }
        }
    }
    let view_focused = card.plan_focus() == PlanFocus::View;
    let changes_focused = card.plan_focus() == PlanFocus::RequestChanges;
    let approve_focused = card.plan_focus() == PlanFocus::Approve;
    lines.push(option_line(
        view_focused,
        "1.",
        if card.plan_expanded() {
            "收起完整计划"
        } else {
            "查看完整计划"
        },
        None,
        false,
    ));
    hits.push(Some(DecisionClick::PlanView));
    lines.push(option_line(changes_focused, "2.", "请求修改", None, false));
    hits.push(Some(DecisionClick::PlanRequestChanges));
    lines.push(Line::from(Span::styled(
        "     取消本次等待，回到输入框说明修改（不提交批准）",
        Style::default().fg(Color::DarkGray),
    )));
    hits.push(Some(DecisionClick::PlanRequestChanges));
    lines.push(option_line(approve_focused, "3.", "批准", None, false));
    hits.push(Some(DecisionClick::PlanApprove));
    lines.push(Line::from(Span::styled(
        "     返回官方批准标签",
        Style::default().fg(Color::DarkGray),
    )));
    hits.push(Some(DecisionClick::PlanApprove));
    lines.push(Line::from(Span::styled(
        "↑↓ 选择 · Enter 确认 · Esc 取消（不会批准）",
        Style::default().fg(Color::DarkGray),
    )));
    hits.push(None);
    (title, lines, hits)
}

fn render_read_only(
    card: &QuestionCard,
    area: Rect,
) -> (String, Vec<Line<'static>>, Vec<Option<DecisionClick>>) {
    let width = area.width.saturating_sub(4) as usize;
    let title = " 问卷回看 · 只读 ".to_string();
    let mut lines = Vec::new();
    let mut hits = Vec::new();
    for (index, question) in card.questions().iter().enumerate() {
        let skipped = card.skipped(index);
        lines.push(Line::from(Span::styled(
            question
                .header
                .clone()
                .unwrap_or_else(|| question.question.clone()),
            Style::default().add_modifier(Modifier::BOLD),
        )));
        hits.push(None);
        if question.header.is_some() {
            for row in display_rows(&question.question, width) {
                lines.push(Line::from(Span::styled(
                    row,
                    Style::default().fg(Color::DarkGray),
                )));
                hits.push(None);
            }
        }
        let answer = card
            .draft(index)
            .map(|draft| {
                answer_text(&nano_decisions::AnswerItem {
                    id: question.id.clone(),
                    selected: draft.selected.clone(),
                    custom: Some(draft.custom.clone()).filter(|text| !text.trim().is_empty()),
                })
            })
            .unwrap_or_else(|| "（未记录）".to_string());
        let text = if skipped {
            "（跳过）".to_string()
        } else {
            answer
        };
        for row in display_rows(&format!("  {text}"), width) {
            lines.push(Line::from(Span::styled(
                row,
                Style::default().fg(Color::DarkGray),
            )));
            hits.push(None);
        }
    }
    lines.push(Line::from(Span::styled(
        "Esc 关闭",
        Style::default().fg(Color::DarkGray),
    )));
    hits.push(Some(DecisionClick::QuestionSubmit));
    (title, lines, hits)
}

fn option_line(
    focused: bool,
    number: &str,
    label: &str,
    checkbox: Option<bool>,
    selected: bool,
) -> Line<'static> {
    let mut spans = vec![
        Span::styled(
            format!("{} ", if focused { ">" } else { " " }),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled(format!("{number} "), Style::default().fg(Color::DarkGray)),
    ];
    if let Some(checked) = checkbox {
        spans.push(Span::styled(
            format!("{} ", if checked { "[x]" } else { "[ ]" }),
            Style::default().fg(Color::DarkGray),
        ));
    }
    let label_style = if focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else if selected {
        Style::default().fg(Color::Green)
    } else {
        Style::default()
    };
    spans.push(Span::styled(label.to_string(), label_style));
    if checkbox.is_none() && selected {
        spans.push(Span::styled(" ✓", Style::default().fg(Color::Green)));
    }
    Line::from(spans)
}

fn answer_text(answer: &nano_decisions::AnswerItem) -> String {
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

fn elide_plain(text: &str, width: usize) -> String {
    let text = text.replace('\n', " ");
    if text.chars().count() <= width {
        return text;
    }
    let mut value: String = text.chars().take(width.saturating_sub(1)).collect();
    value.push('…');
    value
}

fn display_rows(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows = Vec::new();
    for line in text.split('\n') {
        let line = line.replace('\r', "");
        if line.is_empty() {
            rows.push(String::new());
            continue;
        }
        let mut current = String::new();
        let mut used = 0usize;
        for character in line.chars() {
            let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
            if used + character_width > width && !current.is_empty() {
                rows.push(std::mem::take(&mut current));
                used = 0;
            }
            current.push(character);
            used += character_width;
        }
        if !current.is_empty() {
            rows.push(current);
        }
    }
    if rows.is_empty() {
        rows.push(String::new());
    }
    rows
}

fn format_duration(duration: std::time::Duration) -> String {
    let seconds = duration.as_secs();
    format!("{}:{:02}", seconds / 60, seconds % 60)
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

fn input_notice(app: &App) -> Option<(String, Color)> {
    if app.shell_confirm.is_some() {
        Some((
            "人工命令（用户权限，不是 Agent 授权）· Enter 执行 · Esc 取消".to_string(),
            Color::Yellow,
        ))
    } else if app.picker.is_some() {
        Some((
            "附加文件 · 输入路径 · Enter 确认 · Esc 取消".to_string(),
            Color::DarkGray,
        ))
    } else if let Some(message_id) = app.editing_queue() {
        Some((
            format!("编辑队列项 {message_id} · Enter 应用 · Esc 取消"),
            Color::DarkGray,
        ))
    } else if app.subagent_active() {
        let gate = app
            .subagents
            .mounted_gate()
            .unwrap_or(ComposerGate::Resolving);
        if gate.can_send() {
            None
        } else {
            Some((format!("子代理 · {}", gate.hint()), Color::Yellow))
        }
    } else {
        None
    }
}

fn input_text_width(width: u16) -> usize {
    width.saturating_sub(3).max(1) as usize
}

fn input_height(app: &App, width: u16) -> u16 {
    let notice = u16::from(input_notice(app).is_some());
    if app.picker.is_some() {
        return 2 + notice + 1;
    }
    let attachments = attachment_lines(app).len() as u16;
    let text_rows = wrapped_input_rows(app, input_text_width(width) as u16).clamp(1, 8) as u16;
    (2 + notice + attachments + text_rows).min(14)
}

/// The top and bottom composer lines. The left segment shows context
/// occupancy; the rest stays the ordinary border.
fn draw_input_border(frame: &mut Frame, row: Rect, occupancy: Occupancy) {
    let width = row.width as usize;
    let (segment, style) = match occupancy {
        Occupancy::Known { projected, window } if window > 0 => {
            let percent = projected.saturating_mul(100) / window;
            let color = if percent >= 95 {
                Color::Red
            } else if percent >= 80 {
                Color::Yellow
            } else {
                Color::Cyan
            };
            let cells = ((width as u64).saturating_mul(projected.min(window)) / window) as usize;
            (cells.min(width), Style::default().fg(color))
        }
        _ => (0, Style::default().fg(Color::DarkGray)),
    };
    let mut spans = Vec::new();
    if segment > 0 {
        spans.push(Span::styled("─".repeat(segment), style));
    }
    if segment < width {
        spans.push(Span::styled(
            "─".repeat(width - segment),
            Style::default().fg(Color::DarkGray),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), row);
}

fn draw_input(frame: &mut Frame, app: &mut App, area: Rect) {
    let gate = if app.subagent_active() {
        app.subagents
            .mounted_gate()
            .unwrap_or(ComposerGate::Resolving)
    } else {
        ComposerGate::Enabled
    };
    let sendable = gate.can_send();
    let occupancy = if app.subagent_active() {
        // The root meter does not describe the mounted child conversation.
        Occupancy::Unknown
    } else {
        app.context
            .as_ref()
            .map(|meter| meter.occupancy())
            .unwrap_or(Occupancy::Unknown)
    };
    app.send_area = Rect::default();
    if area.height >= 2 {
        draw_input_border(frame, Rect { height: 1, ..area }, occupancy);
        draw_input_border(
            frame,
            Rect {
                y: area.y.saturating_add(area.height - 1),
                height: 1,
                ..area
            },
            occupancy,
        );
    }
    let inner = Rect::new(
        area.x,
        area.y.saturating_add(1),
        area.width,
        area.height.saturating_sub(2),
    );
    let width = inner.width.max(1) as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut cursor: Option<(usize, usize)> = None;
    let mut send_start: Option<usize> = None;
    if let Some((notice, color)) = input_notice(app) {
        lines.push(Line::from(Span::styled(
            truncate(&notice, width),
            Style::default().fg(color),
        )));
    }
    if let Some(picker) = &app.picker {
        let prefix = "路径：";
        let text = format!("{prefix}{}", picker.path);
        lines.push(Line::from(vec![
            Span::styled(prefix, muted_style()),
            Span::raw(picker.path.clone()),
        ]));
        cursor = Some((lines.len() - 1, text.chars().map(display_width).sum()));
    } else {
        for (text, color) in attachment_lines(app) {
            lines.push(Line::from(Span::styled(text, Style::default().fg(color))));
        }
        let draft = app.drafts.current().text();
        if sendable {
            let text_width = input_text_width(area.width);
            let start = lines.len();
            send_start = Some(start);
            if draft.is_empty() {
                let placeholder = if app.subagent_active() {
                    app.subagents
                        .mounted()
                        .map(|mounted| format!("给 {} 发送消息…", safe_text(&mounted.label)))
                        .unwrap_or_else(|| "继续提问…".to_string())
                } else {
                    "继续提问…".to_string()
                };
                let placeholder = truncate(&placeholder, width.saturating_sub(3));
                lines.push(Line::from(vec![
                    Span::styled("> ", Style::default().fg(Color::Cyan)),
                    Span::styled(placeholder, muted_style()),
                ]));
                cursor = Some((start, 2));
            } else {
                let (row, column) = input_cursor(app, text_width);
                for (index, text) in input_lines(app, text_width).into_iter().enumerate() {
                    let prefix = if index == 0 { "> " } else { "  " };
                    lines.push(Line::from(vec![
                        Span::styled(prefix, Style::default().fg(Color::Cyan)),
                        Span::raw(text),
                    ]));
                }
                cursor = Some((start + row, 2 + column));
            }
        } else if draft.trim().is_empty() {
            lines.push(Line::from(Span::styled(
                "只读记录 · 没有可用的输入框",
                muted_style(),
            )));
        } else {
            for text in input_lines(app, width) {
                lines.push(Line::from(Span::styled(text, muted_style())));
            }
        }
    }
    let viewport = inner.height as usize;
    let offset = match cursor {
        Some((row, _)) if viewport > 0 => row.saturating_sub(viewport.saturating_sub(1)),
        _ => 0,
    };
    // The send button rides the first visible draft row, so a long draft that
    // scrolls its first line out of view still shows a reachable button.
    if let (true, Some(start)) = (sendable, send_start) {
        if viewport > 0 {
            let visible_row = start.saturating_sub(offset).min(viewport - 1);
            let absolute_row = offset + visible_row;
            if let Some(line) = lines.get_mut(absolute_row) {
                let used = line.spans.iter().map(|span| span.width()).sum::<usize>();
                if used < width {
                    line.spans.push(Span::raw(" ".repeat(width - used - 1)));
                    line.spans
                        .push(Span::styled("↑", Style::default().fg(Color::Cyan)));
                    app.send_area = Rect::new(
                        inner.x.saturating_add(width as u16 - 1),
                        inner.y.saturating_add(visible_row as u16),
                        1,
                        1,
                    );
                }
            }
        }
    }
    let visible: Vec<Line<'static>> = lines.into_iter().skip(offset).take(viewport).collect();
    frame.render_widget(Paragraph::new(Text::from(visible)), inner);
    if sendable
        && app.scene.focus().input_focused()
        && !app.panel.is_open()
        && (!app.drafts.current().text().is_empty() || app.picker.is_some())
        && viewport > 0
    {
        if let Some((row, column)) = cursor {
            let row = row.saturating_sub(offset).min(viewport.saturating_sub(1));
            let column = column.min(width.saturating_sub(2));
            frame.set_cursor_position((
                inner.x + column.min(u16::MAX as usize) as u16,
                inner.y + row.min(u16::MAX as usize) as u16,
            ));
        }
    }
}

/// Attachment card lines shown above the input text.
fn attachment_lines(app: &App) -> Vec<(String, Color)> {
    if app.subagent_active() && app.subagents.mounted_gate() == Some(ComposerGate::Resolving) {
        // The parent attachment draft must not appear in a child view before
        // the child's own address and draft are installed.
        return Vec::new();
    }
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
            .map(|character| {
                if character.is_control() {
                    '?'
                } else {
                    character
                }
            })
            .collect();
        lines.push(Line::from(Span::styled(
            format!("{marker} {display}{suffix}"),
            style,
        )));
    }
    frame.render_widget(Paragraph::new(Text::from(lines)), inner);
}

fn draw_activity(frame: &mut Frame, app: &App, area: Rect) {
    let Some(kind) = app.activity_kind() else {
        return;
    };
    if area.height == 0 {
        return;
    }
    let glyph = if app.reduced_motion || !app.activity_phase {
        "●"
    } else {
        "○"
    };
    let color = match kind {
        ActivityKind::Running => Color::Cyan,
        ActivityKind::AwaitingApproval | ActivityKind::AwaitingQuestion => Color::Yellow,
    };
    let spans = vec![
        Span::styled(format!("{glyph} "), Style::default().fg(color)),
        Span::styled(kind.label(), muted_style()),
    ];
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_status(frame: &mut Frame, app: &App, area: Rect, width: u16) {
    let width = width.max(1) as usize;
    let transient = safe_text(&app.status_transient());
    // A disconnected link or an error must stay visible even at 60 columns;
    // reserve a minimum middle slot before trimming either end.
    let critical = app.error.is_some() || !matches!(app.connection, Connection::Ready);
    let middle_min = if critical && !transient.is_empty() {
        12.min(width / 3)
    } else {
        0
    };
    let mut left = safe_text(&app.status_permission());
    if text_width(&left) > width {
        left = truncate(&left, width);
    }
    if middle_min > 0 {
        left = truncate(&left, (width / 3).max(8).min(width));
    }
    let mut right = safe_text(&app.display_path());
    let reserve = if middle_min > 0 { middle_min + 3 } else { 1 };
    let right_room = width.saturating_sub(text_width(&left) + reserve).max(1);
    if text_width(&right) > right_room {
        right = elide_left(&right, right_room);
    }
    let left_width = text_width(&left);
    let right_width = text_width(&right);
    let middle_room = width.saturating_sub(left_width + right_width + 3);
    let middle = if middle_room > 0 {
        format!("  {}", truncate(&transient, middle_room))
    } else {
        String::new()
    };
    let middle_width = text_width(&middle);
    let pad = width.saturating_sub(left_width + middle_width + right_width);
    let transient_color = if app.error.is_some() {
        Color::Red
    } else {
        Color::DarkGray
    };
    let spans = vec![
        Span::styled(left, muted_style()),
        Span::styled(middle, Style::default().fg(transient_color)),
        Span::raw(" ".repeat(pad)),
        Span::styled(right, muted_style()),
    ];
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Keep the tail of a path when it does not fit, marking the cut with `…`.
fn elide_left(text: &str, width: usize) -> String {
    if text_width(text) <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut tail = Vec::new();
    let mut used = 1usize;
    for character in text.chars().rev() {
        let character_width = display_width(character);
        if used + character_width > width {
            break;
        }
        used += character_width;
        tail.push(character);
    }
    let tail: String = tail.into_iter().rev().collect();
    format!("…{tail}")
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

fn text_width(text: &str) -> usize {
    text.chars().map(display_width).sum()
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

/// First visible row that keeps the selected list item on screen.
fn list_offset(selected: usize, total: usize, viewport: usize) -> usize {
    let visible = viewport.max(1);
    selected
        .saturating_sub(visible.saturating_sub(1))
        .min(total.saturating_sub(visible))
}

/// Wrap the tail of a block and mark display-side truncation explicitly.
/// Output tail semantics matter: the newest bytes and the official
/// truncation/spill notice live at the end.
fn wrap_tail_block(
    lines: &mut Vec<Line<'static>>,
    text: &str,
    width: usize,
    max_chars: usize,
    color: Color,
) {
    let characters = text.chars().count();
    if characters > max_chars {
        let tail: String = text.chars().skip(characters - max_chars).collect();
        lines.push(Line::from(Span::styled(
            format!("[显示已截断：仅显示最后 {max_chars} 字符；完整输出以官方输出为准]"),
            Style::default().fg(Color::Yellow),
        )));
        wrap_block(lines, &tail, width, max_chars, color);
    } else {
        wrap_block(lines, text, width, max_chars, color);
    }
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
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use nano_decisions::contract::parse_questions;
    use nano_decisions::{ApprovalCard, QuestionCard, TimedState};
    use serde_json::json;
    use std::time::Duration;

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

    #[test]
    fn the_tail_renderer_keeps_the_newest_text_and_marks_truncation() {
        let mut lines = Vec::new();
        wrap_tail_block(&mut lines, "abcdef", 10, 3, Color::White);
        let joined: String = lines
            .iter()
            .flat_map(|line| line.spans.iter().map(|span| span.content.as_ref()))
            .collect();
        assert!(joined.contains("显示已截断"));
        assert!(joined.contains("def"));
        assert!(!joined.contains("abc"));
    }

    #[test]
    fn the_list_offset_keeps_the_selection_visible() {
        assert_eq!(list_offset(0, 40, 10), 0);
        assert_eq!(list_offset(20, 40, 10), 11);
        assert_eq!(list_offset(39, 40, 10), 30);
        assert_eq!(list_offset(0, 3, 10), 0);
    }

    fn screen(app: &mut App, width: u16, height: u16) -> Vec<String> {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| draw(frame, app))
            .expect("draw the frame");
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect()
    }

    fn app() -> App {
        App::new(crate::cli::Cli::default(), "root".to_string(), false)
    }

    /// TestBackend repeats a wide character's cell as a symbol plus a blank
    /// continuation cell; compacting whitespace makes CJK text easy to match.
    fn compact(rows: &[String]) -> Vec<String> {
        rows.iter()
            .map(|row| {
                row.chars()
                    .filter(|character| !character.is_whitespace())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn the_main_screen_keeps_two_cell_margins_and_a_two_line_composer() {
        let mut app = app();
        let rows = screen(&mut app, 100, 30);
        assert!(rows[0].starts_with("  nano"), "{:?}", rows[0]);
        let quiet: Vec<&String> = rows.iter().filter(|row| row.contains('─')).collect();
        assert_eq!(quiet.len(), 2, "composer has a top and bottom line");
        let compact = compact(&rows);
        assert!(compact.iter().any(|row| row.contains(">继续提问…")));
        let status = compact.last().expect("status row");
        assert!(status.contains("权限："));
        assert!(status.ends_with(&app.display_path()), "{status:?}");
        assert!(!status.contains("·权限"));
    }

    #[test]
    fn the_jump_button_only_appears_above_the_minimum_width() {
        let area = Rect::new(2, 0, 100, 20);
        assert!(jump_button_rect(area, false).is_none());
        let rect = jump_button_rect(area, true).expect("visible");
        assert_eq!(rect.y, 19);
        assert_eq!(rect.right(), area.right() - 2);
        assert!(jump_button_rect(Rect::new(0, 0, 11, 20), true).is_none());
    }

    #[test]
    fn the_jump_button_follows_the_actual_scroll_state() {
        let mut app = app();
        let text = (1..=40)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let payload = json!({
            "address": {"rootSessionId": "root"},
            "seq": 1,
            "type": "assistant/message",
            "data": {
                "turn": 1,
                "step": 1,
                "message": {
                    "id": "a-1",
                    "role": "assistant",
                    "content": [{"type": "text", "text": text}]
                }
            }
        });
        app.model.apply_event("session.event", &payload);
        app.view.sync(&app.model, Instant::now());
        app.transcript_dirty = false;
        app.view.scroll_mut().to_top(200, 10);
        let rows = compact(&screen(&mut app, 100, 30));
        assert!(rows.iter().any(|row| row.contains("回到最新")));
    }

    #[test]
    fn a_single_select_question_hides_checkboxes_and_renders_the_description_once() {
        let questions = parse_questions(Some(&json!([{
            "id": "q",
            "question": "Pick one",
            "options": [
                {"label": "A", "description": "first description"},
                {"label": "B"}
            ]
        }])));
        let card = QuestionCard::new(None, None, questions, None);
        let (_, lines, _) = render_question(&card, Rect::new(0, 0, 60, 20));
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Pick one"));
        assert!(!text.contains("[ ]"));
        assert!(!text.contains("[x]"));
        assert_eq!(text.matches("first description").count(), 1);
        assert!(text.contains("Alt+N 跳过"));
        assert!(
            lines.iter().all(|line| text_width(&line.to_string()) <= 60),
            "card must fit 60 columns"
        );
    }

    #[test]
    fn a_selected_single_option_shows_a_check_mark() {
        let single = option_line(false, "1.", "A", None, true).to_string();
        assert!(single.contains("✓"));
        assert!(!single.contains("[ ]"));
        let multi = option_line(false, "1.", "A", Some(true), true).to_string();
        assert!(multi.contains("[x]"));
    }

    #[test]
    fn an_approval_card_uses_the_bash_question_and_shows_the_full_command() {
        let command = "line1\nline2\nline3\nline4\nline5\nline6\nline7";
        let card = ApprovalCard::new(
            "r".into(),
            "bash".into(),
            None,
            Default::default(),
            None,
            Some(command.into()),
            Instant::now(),
        );
        let (_, lines, _) = render_approval(&card, Rect::new(0, 0, 60, 30));
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("是否运行这条检查命令？"));
        assert!(text.contains("line7"), "full command, not a 5-row window");
        assert!(!text.contains("始终允许"));
        assert!(
            lines.iter().all(|line| text_width(&line.to_string()) <= 60),
            "card must fit 60 columns"
        );
    }

    #[test]
    fn a_timed_multi_select_footer_wraps_within_sixty_columns() {
        let questions = parse_questions(Some(&json!([{
            "id": "q",
            "question": "Pick many",
            "multi_select": true,
            "options": [{"label": "A"}, {"label": "B"}]
        }])));
        let card = QuestionCard::new(
            None,
            None,
            questions,
            Some(TimedState::start(30_000, Instant::now())),
        );
        let (_, lines, _) = render_question(&card, Rect::new(0, 0, 60, 24));
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Alt+T"));
        assert!(text.contains("剩"));
        assert!(lines.iter().all(|line| text_width(&line.to_string()) <= 60));
    }

    #[test]
    fn a_long_question_keeps_every_option_and_the_footer_visible() {
        let long = "很长的题目。".repeat(30);
        let detail = "很长的描述。".repeat(20);
        let questions = parse_questions(Some(&json!([{
            "id": "q",
            "question": long,
            "options": [
                {"label": "A", "description": detail},
                {"label": "B", "description": detail},
                {"label": "C", "description": detail},
                {"label": "D", "description": detail}
            ]
        }])));
        let card = QuestionCard::new(None, None, questions, None);
        let (_, lines, _) = render_question(&card, Rect::new(0, 0, 60, 24));
        let visible: Vec<String> = lines.iter().take(23).map(ToString::to_string).collect();
        let text = visible.join("\n");
        assert!(text.contains("4. D"), "fourth option stays visible");
        assert!(text.contains("其他"));
        assert!(text.contains("Alt+N 跳过"));
        assert!(visible.iter().all(|line| text_width(line) <= 60));
    }

    #[test]
    fn the_status_bar_right_aligns_the_path_and_truncates_the_middle_first() {
        let mut app = app();
        app.notice = Some("这是一个需要先截断的中段提示".to_string());
        let rows = screen(&mut app, 60, 20);
        let status = compact(&rows).last().expect("status row").clone();
        assert!(status.contains("权限："));
        // Both ends survive; the right path keeps its tail and the middle
        // notice is the first thing to go.
        assert!(status.ends_with("nano-app"), "{status:?}");
        assert!(status.contains('…'));
        assert!(!status.contains("中段提示"));
    }

    fn meter(projected: u64, window: u64) -> nano_sessions::ContextMeter {
        nano_sessions::ContextMeter {
            session_id: "root".into(),
            as_of_seq: 1,
            approximate: true,
            projected_tokens: Some(projected),
            context_window: Some(window),
            pressure_tokens: None,
            pressure_at: None,
            context_window_at: None,
            token_usage: None,
            breakdown: None,
        }
    }

    fn border_colors(app: &mut App) -> Vec<(usize, Color)> {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let width = 100;
        let height = 30;
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal.draw(|frame| draw(frame, app)).expect("draw");
        let buffer = terminal.backend().buffer();
        let row = (0..height)
            .find(|y| {
                (0..width)
                    .filter(|x| buffer[(*x, *y)].symbol() == "─")
                    .count()
                    > width as usize / 2
            })
            .expect("composer border row");
        (2..(width as usize - 2))
            .map(|x| (x, buffer[(x as u16, row)].fg))
            .collect()
    }

    #[test]
    fn the_context_line_uses_cyan_yellow_red_and_a_plain_unknown_border() {
        let mut app = app();
        let colors = border_colors(&mut app);
        assert!(colors.iter().all(|(_, color)| *color == Color::DarkGray));

        app.context = Some(meter(50, 100));
        let colors = border_colors(&mut app);
        let colored = colors
            .iter()
            .filter(|(_, color)| *color == Color::Cyan)
            .count();
        // 100 columns minus the 2ch margins on each side.
        assert_eq!(colored, 48);
        assert_eq!(colors[48].1, Color::DarkGray);

        app.context = Some(meter(80, 100));
        assert!(border_colors(&mut app)
            .iter()
            .any(|(_, color)| *color == Color::Yellow));
        app.context = Some(meter(95, 100));
        assert!(border_colors(&mut app)
            .iter()
            .any(|(_, color)| *color == Color::Red));
    }

    fn running_event() -> serde_json::Value {
        serde_json::json!({
            "address": {"rootSessionId": "root"},
            "status": "running"
        })
    }

    #[test]
    fn the_activity_row_has_idle_running_and_waiting_states_and_a_phase() {
        let mut base = app();
        assert_eq!(base.activity_kind(), None);

        base.model
            .apply_event("conversation.status", &running_event());
        assert_eq!(base.activity_kind(), Some(ActivityKind::Running));
        assert!(base.tick());
        assert!(!base.activity_phase);
        base.activity_at = Some(Instant::now() - Duration::from_millis(1));
        assert!(base.tick());
        assert!(base.activity_phase);
        let rows = compact(&screen(&mut base, 100, 30));
        assert!(rows.iter().any(|row| row.contains("○正在运行…·Esc可停止")));

        let mut approval = app();
        approval.decisions.apply_event(
            "decisions.approval",
            &serde_json::json!({
                "address": {"rootSessionId": "root"},
                "state": "pending",
                "requestId": "a-1",
                "toolName": "bash",
                "reason": "sandbox"
            }),
        );
        assert_eq!(
            approval.activity_kind(),
            Some(ActivityKind::AwaitingApproval)
        );
        let rows = compact(&screen(&mut approval, 100, 30));
        assert!(rows.iter().any(|row| row.contains("等待你的授权")));

        let mut question = app();
        question.decisions.apply_event(
            "decisions.question",
            &serde_json::json!({
                "address": {"rootSessionId": "root"},
                "state": "pending",
                "requestId": "q-1",
                "callId": "c-1",
                "timed": false,
                "questions": [{"id": "q1", "question": "Pick", "options": [{"label": "A"}]}]
            }),
        );
        assert_eq!(
            question.activity_kind(),
            Some(ActivityKind::AwaitingQuestion)
        );
        let rows = compact(&screen(&mut question, 100, 30));
        assert!(rows.iter().any(|row| row.contains("等待你的选择")));

        base.model.apply_event(
            "conversation.status",
            &serde_json::json!({
                "address": {"rootSessionId": "root"},
                "status": "idle"
            }),
        );
        base.tick();
        assert_eq!(base.activity_kind(), None);
        assert!(base.activity_at.is_none());
    }

    fn subagent_row(id: &str, mode: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "parentId": "root",
            "depth": 1,
            "mode": mode,
            "label": format!("task {id}"),
            "activity": "inactive",
            "hasChildren": false
        })
    }

    fn install_subagents(app: &mut App, agents: serde_json::Value) {
        app.subagents
            .apply_snapshot(&serde_json::json!({
                "agents": agents,
                "diagnostics": [],
                "parentLive": true
            }))
            .expect("valid snapshot");
    }

    #[test]
    fn the_dock_sits_below_the_status_bar_without_a_title_and_counts_in_the_list() {
        let mut app = app();
        let mut one_shot = subagent_row("c", "one-shot");
        one_shot["settledMs"] = serde_json::json!(2400);
        one_shot["activeThrough"] = serde_json::json!(5000);
        install_subagents(
            &mut app,
            serde_json::json!([
                subagent_row("a", "continuable"),
                subagent_row("b", "continuable"),
                one_shot,
                subagent_row("d", "continuable"),
                subagent_row("e", "continuable"),
                subagent_row("f", "external"),
            ]),
        );
        let rows = screen(&mut app, 100, 30);
        let rows_compact = compact(&rows);
        let dock = app.subagent_dock_area.y as usize;
        let status = rows_compact
            .iter()
            .position(|row| row.contains("权限："))
            .expect("status row");
        assert!(status < dock, "status row stays above the dock");
        assert!(rows_compact.last().unwrap().contains("还有1个子代理"));
        let dock_text = rows_compact[dock..].join("");
        assert!(!dock_text.contains("localagents"), "dock has no title");
        assert!(!dock_text.contains("用时"), "dock has no duration label");
        assert!(!dock_text.contains("2.4s"), "dock has no duration value");
        assert!(!dock_text.contains("一次性"), "dock has no mode suffix");
        assert!(
            !dock_text.contains("taskf"),
            "external rows stay out of the dock"
        );
        assert!(dock_text.contains("taska"));
        assert!(dock_text.contains("未运行"));

        app.enter_subagent_overflow();
        let list = compact(&screen(&mut app, 100, 30));
        assert!(
            list.iter().any(|row| row.contains("全部子代理·5·↑↓选择")),
            "list title keeps its hints and adds the local-only count"
        );
    }

    #[test]
    fn the_child_view_header_keeps_the_main_title_and_adds_a_child_row() {
        let mut app = app();
        install_subagents(
            &mut app,
            serde_json::json!([subagent_row("child", "continuable")]),
        );
        assert!(app.enter_subagent(0));
        app.apply_subagent_description(&serde_json::json!({
            "id": "child",
            "parentId": "root",
            "mode": "continuable",
            "label": "task child",
            "activity": "inactive",
            "parentLive": true,
            "seq": 1
        }));
        let rows = compact(&screen(&mut app, 100, 30));
        assert!(rows[0].starts_with("nano"));
        assert!(rows[1].contains("←子代理taskchild"));
        let status = rows.last().expect("status row");
        assert!(status.contains("权限：子代理审批由官方拒绝"));
        assert!(!status.contains("Enter发送"));
    }

    #[test]
    fn the_activity_row_sits_below_an_open_decision_card() {
        let mut app = app();
        app.decisions.apply_event(
            "decisions.approval",
            &serde_json::json!({
                "address": {"rootSessionId": "root"},
                "state": "pending",
                "requestId": "a-1",
                "toolName": "bash"
            }),
        );
        let rows = compact(&screen(&mut app, 100, 30));
        let activity = rows
            .iter()
            .position(|row| row.contains("等待你的授权"))
            .expect("activity row");
        let card_bottom = (app.decision_area.y + app.decision_area.height) as usize;
        assert_eq!(activity, card_bottom, "decision card, then activity");
        let status = rows
            .iter()
            .position(|row| row.contains("权限："))
            .expect("status row");
        assert!(status > activity);
    }

    #[test]
    fn a_long_draft_keeps_a_visible_send_button() {
        let mut app = app();
        app.drafts.current_mut().set_text(
            "line 1\nline 2\nline 3\nline 4\nline 5\nline 6\nline 7\nline 8\nline 9\nline 10\nline 11\nline 12",
        );
        let rows = screen(&mut app, 40, 24);
        assert_eq!(app.send_area.height, 1, "send hit area is set");
        let row = rows
            .get(app.send_area.y as usize)
            .expect("send row is on screen");
        assert!(row.contains('↑'), "{row:?}");
        assert_eq!(rows.iter().filter(|row| row.contains('↑')).count(), 1);
    }

    #[test]
    fn a_critical_error_stays_visible_in_the_narrow_status_bar() {
        let mut app = app();
        app.error = Some("通信已断开（会话 root）".to_string());
        let rows = compact(&screen(&mut app, 60, 20));
        let status = rows.last().expect("status row");
        assert!(status.contains("通信已断开"), "{status:?}");
        assert!(status.contains("权限："));
    }

    #[test]
    fn safety_and_mode_notices_stay_visible_in_the_composer() {
        let mut shell = app();
        shell.shell_confirm = Some("echo hi".to_string());
        let rows = compact(&screen(&mut shell, 100, 30));
        assert!(rows
            .iter()
            .any(|row| row.contains("用户权限，不是Agent授权")));

        let mut editing = app();
        editing.edit_target = Some("m-1".to_string());
        editing.drafts.current_mut().set_text("queued");
        let rows = compact(&screen(&mut editing, 100, 30));
        assert!(rows.iter().any(|row| row.contains("编辑队列项m-1")));

        let mut child = app();
        install_subagents(
            &mut child,
            serde_json::json!([subagent_row("child", "one-shot")]),
        );
        assert!(child.enter_subagent(0));
        let rows = compact(&screen(&mut child, 100, 30));
        assert!(rows.iter().any(|row| row.contains("正在读取子代理历史")));
    }

    #[test]
    fn the_header_is_followed_by_one_blank_row() {
        let mut app = app();
        let rows = screen(&mut app, 100, 20);
        assert!(rows[0].starts_with("  nano"));
        assert!(rows[1].trim().is_empty(), "{:?}", rows[1]);
    }

    #[test]
    fn dock_status_words_follow_the_official_lamp() {
        let mut app = app();
        let mut running = subagent_row("run", "continuable");
        running["activity"] = serde_json::json!("running");
        install_subagents(
            &mut app,
            serde_json::json!([
                running,
                subagent_row("ok", "continuable"),
                subagent_row("bad", "one-shot"),
                subagent_row("stop", "continuable"),
            ]),
        );
        for (id, reason) in [("ok", "completed"), ("bad", "error"), ("stop", "aborted")] {
            app.subagents
                .apply_event(&serde_json::json!({
                    "type": "end",
                    "childId": id,
                    "stopReason": reason
                }))
                .expect("valid lifecycle event");
        }
        let rows = compact(&screen(&mut app, 100, 30));
        let dock = rows[app.subagent_dock_area.y as usize..].join("");
        assert!(dock.contains("执行中"), "{dock:?}");
        assert!(dock.contains("完成"), "{dock:?}");
        assert!(dock.contains("错误"), "{dock:?}");
        assert!(dock.contains("终止"), "{dock:?}");

        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        let rows = compact(&screen(&mut app, 100, 30));
        let dock = rows[app.subagent_dock_area.y as usize..].join("");
        assert!(dock.contains('›'), "{dock:?}");
    }

    #[test]
    fn unknown_terminal_reason_uses_the_inactive_status_word() {
        let mut app = app();
        install_subagents(
            &mut app,
            serde_json::json!([subagent_row("mystery", "continuable")]),
        );
        app.subagents
            .apply_event(&serde_json::json!({
                "type": "end",
                "childId": "mystery",
                "stopReason": "future-reason"
            }))
            .expect("valid lifecycle event");
        let rows = compact(&screen(&mut app, 100, 30));
        let dock = rows[app.subagent_dock_area.y as usize..].join("");
        assert!(dock.contains("未运行"), "{dock:?}");
    }

    #[test]
    fn dock_hover_uses_theme_tokens() {
        assert_eq!(hover_background(DiffTheme::Dark), Color::Rgb(36, 43, 53));
        assert_eq!(
            hover_background(DiffTheme::Light),
            Color::Rgb(232, 232, 229)
        );
    }

    fn screen_bg(app: &mut App, width: u16, height: u16, x: u16, y: u16) -> Color {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| draw(frame, app))
            .expect("draw the frame");
        terminal.backend().buffer()[(x, y)].bg
    }

    #[test]
    fn a_focused_dock_row_uses_a_hover_background() {
        let mut app = app();
        install_subagents(
            &mut app,
            serde_json::json!([subagent_row("child", "continuable")]),
        );
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        let _ = screen(&mut app, 100, 30);
        let y = app.subagent_dock_area.y + 1;
        let bg = screen_bg(&mut app, 100, 30, 5, y);
        assert!(
            bg == Color::Rgb(36, 43, 53) || bg == Color::Rgb(232, 232, 229),
            "focused dock row uses the hover token, got {bg:?}"
        );
    }
}
