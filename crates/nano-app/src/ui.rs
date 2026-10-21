//! Rendering: the S03 transcript plus the input, queue, status, and decision
//! surfaces.
//!
//! The feed is produced by `nano-transcript` (line budgets, diffs, expansion,
//! scroll). Question and approval cards take the input seat and record their
//! pointer targets for the app.

use std::time::Instant;

use nano_decisions::{
    ApprovalCardState, ApprovalFocus, DecisionClick, DecisionTop, PlanFocus, QuestionCard,
    QuestionCardState,
};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthChar;

use crate::app::{App, Connection};

/// Draw one frame.
pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let queue_height = if app.model.inbox().is_empty() {
        0
    } else {
        (app.model.inbox().len().min(4) as u16) + 1
    };
    let decision = app.decisions.top().is_some();
    let input_rows = if decision {
        0
    } else {
        wrapped_input_rows(app, area.width.saturating_sub(4).max(1))
    };
    let input_height = input_rows.min(8) as u16 + 2;
    let bottom_height = if decision {
        decision_height(app, area.width, area.height)
    } else {
        input_height
    };
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(queue_height),
            Constraint::Length(bottom_height),
            Constraint::Length(1),
        ])
        .split(area);

    draw_header(frame, app, layout[0]);
    draw_feed(frame, app, layout[1]);
    if queue_height > 0 {
        draw_queue(frame, app, layout[2]);
    }
    if decision {
        draw_decision(frame, app, layout[3]);
    } else {
        app.decision_area = Rect::default();
        app.decision_hits.clear();
        draw_input(frame, app, layout[3]);
    }
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
    if app.replay_truncated {
        spans.push(Span::styled(
            " · 更早记录未回放",
            Style::default().fg(Color::Yellow),
        ));
    }
    if !app.view.scroll().sticky() {
        spans.push(Span::styled(
            " · 已上滚（Ctrl+End 回底）",
            Style::default().fg(Color::Yellow),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_feed(frame: &mut Frame, app: &mut App, area: Rect) {
    app.feed_area = area;
    app.feed_height = area.height as usize;
    if app.transcript_dirty || !app.model.previews().is_empty() {
        app.view.sync(&app.model, Instant::now());
        app.transcript_dirty = false;
    }
    let render = app
        .view
        .render(area.width.max(1) as usize, area.height.max(1) as usize);
    app.feed_hits = render.hits;
    if render.lines.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "输入消息后按 Enter 发送。",
                Style::default().fg(Color::DarkGray),
            ))),
            area,
        );
        return;
    }
    frame.render_widget(Paragraph::new(Text::from(render.lines)), area);
}

fn decision_height(app: &App, width: u16, height: u16) -> u16 {
    let max = height.saturating_sub(8).max(6);
    let desired = match app.decisions.top() {
        Some(DecisionTop::Approval(card)) => {
            let command_rows = card
                .command()
                .map(|command| display_rows(command, width.saturating_sub(4) as usize).len())
                .unwrap_or(0)
                .min(6);
            7 + command_rows as u16
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
        return 8 + plan_rows;
    }
    if card.page() == card.questions().len() {
        return 6 + card.questions().len() as u16 * 2;
    }
    let Some(question) = card.questions().get(card.page()) else {
        return 6;
    };
    let mut rows = 4u16;
    rows += display_rows(&question.question, width.saturating_sub(4) as usize)
        .len()
        .min(3) as u16;
    for (index, option) in question.options.iter().enumerate() {
        let _ = index;
        rows += 1;
        if option.description.is_some() {
            rows += 1;
        }
    }
    rows += 1; // Other row
    rows += 1; // Skip / submit
    if card.error().is_some() {
        rows += 1;
    }
    if card.timed().remaining_ms.is_some() || card.timed().indefinite {
        rows += 1;
    }
    rows.min(24)
}

fn draw_decision(frame: &mut Frame, app: &mut App, area: Rect) {
    app.decision_area = area;
    let (title, lines, hits) = match app.decisions.top() {
        Some(DecisionTop::Approval(card)) => render_approval(card, area),
        Some(DecisionTop::Question(card)) => render_question(card, area),
        None => (String::new(), Vec::new(), Vec::new()),
    };
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    let mut mapped = Vec::with_capacity(inner.height as usize + 2);
    mapped.push(None);
    mapped.extend(hits);
    while mapped.len() < area.height as usize {
        mapped.push(None);
    }
    mapped.truncate(area.height as usize);
    app.decision_hits = mapped;
    frame.render_widget(block, area);
    let visible: Vec<Line<'static>> = lines.into_iter().take(inner.height as usize).collect();
    frame.render_widget(Paragraph::new(Text::from(visible)), inner);
}

fn render_approval(
    card: &nano_decisions::ApprovalCard,
    area: Rect,
) -> (String, Vec<Line<'static>>, Vec<Option<DecisionClick>>) {
    let width = area.width.saturating_sub(4) as usize;
    let title = format!(" 审批 · {} ", card.tool_name());
    let mut lines = Vec::new();
    let mut hits = Vec::new();
    lines.push(Line::from(Span::styled(
        format!("是否允许本次 {} 操作？", card.tool_name()),
        Style::default().add_modifier(Modifier::BOLD),
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
    if let Some(command) = card.command() {
        lines.push(Line::from(Span::styled(
            "命令",
            Style::default().fg(Color::DarkGray),
        )));
        hits.push(None);
        let rows = display_rows(command, width);
        let start = card.command_offset().min(rows.len());
        for row in rows.iter().skip(start).take(5) {
            lines.push(Line::from(Span::styled(
                format!("  {row}"),
                Style::default().fg(Color::Yellow),
            )));
            hits.push(None);
        }
        if rows.len() > start + 5 {
            lines.push(Line::from(Span::styled(
                format!(
                    "  … 还有 {} 行（PageUp/PageDown 滚动）",
                    rows.len() - start - 5
                ),
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
    lines.push(option_line(
        allow_focused,
        "1.",
        "允许一次",
        "仅本次；不更改后续权限",
        false,
    ));
    hits.push(Some(DecisionClick::ApprovalAllow));
    lines.push(option_line(
        reject_focused,
        "2.",
        "拒绝",
        "不执行本次操作",
        false,
    ));
    hits.push(Some(DecisionClick::ApprovalReject));
    lines.push(Line::from(Span::styled(
        "↑↓ 切换 · Enter 确认 · 1 选允许 · 2 选拒绝 · Esc 取消（不会允许）",
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

fn render_question(
    card: &QuestionCard,
    area: Rect,
) -> (String, Vec<Line<'static>>, Vec<Option<DecisionClick>>) {
    let width = area.width.saturating_sub(4) as usize;
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
                    Style::default().fg(Color::Black).bg(Color::Cyan)
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
                Style::default().fg(Color::Black).bg(Color::Cyan)
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
        lines.push(Line::from(Span::styled(
            "Enter 提交 · Tab/←→ 回看 · Esc 收起",
            Style::default().fg(Color::DarkGray),
        )));
        hits.push(Some(DecisionClick::QuestionSubmit));
    } else if let Some(question) = card.questions().get(card.page()) {
        lines.push(Line::from(Span::styled(
            question.question.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        )));
        hits.push(None);
        let draft = card.draft(card.page()).cloned().unwrap_or_default();
        for (index, option) in question.options.iter().enumerate() {
            let focused = card.focus() == index;
            let selected = draft.selected.iter().any(|label| label == &option.label);
            lines.push(option_line(
                focused,
                &format!("{}.", index + 1),
                &option.label,
                option.description.as_deref().unwrap_or(""),
                selected,
            ));
            hits.push(Some(DecisionClick::QuestionOption(index)));
            if let Some(description) = &option.description {
                for row in display_rows(description, width.saturating_sub(5)) {
                    lines.push(Line::from(Span::styled(
                        format!("     {row}"),
                        Style::default().fg(Color::DarkGray),
                    )));
                    hits.push(Some(DecisionClick::QuestionOption(index)));
                }
            }
        }
        let other_focused = card.focus() == question.options.len();
        let caret = if other_focused { "▏" } else { "" };
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
        lines.push(Line::from(vec![
            Span::styled("跳过本题", Style::default().fg(Color::DarkGray)),
            Span::styled(
                "  （Alt+N；提交空结果）",
                Style::default().fg(Color::DarkGray),
            ),
        ]));
        hits.push(Some(DecisionClick::QuestionSkip));
        let footer = if question.multi_select {
            "↑↓ 移动 · 空格勾选 · Enter 确认已选项"
        } else {
            "↑↓ 移动 · Enter 选择 · 数字直选"
        };
        lines.push(Line::from(Span::styled(
            format!(
                "{footer}{} · Alt+N 跳过 · Alt+T 停止倒计时 · Esc 收起",
                if card.questions().len() > 1 {
                    " · Tab 切题"
                } else {
                    ""
                }
            ),
            Style::default().fg(Color::DarkGray),
        )));
        hits.push(None);
    }
    if let Some(error) = card.error() {
        lines.push(Line::from(Span::styled(
            error.to_string(),
            Style::default().fg(Color::Red),
        )));
        hits.push(None);
    }
    if let Some(remaining) = card.timed().remaining_at(Instant::now()) {
        lines.push(Line::from(Span::styled(
            format!("剩余 {} · Alt+T 停止倒计时", format_duration(remaining)),
            Style::default().fg(Color::Yellow),
        )));
        hits.push(None);
    } else if card.timed().indefinite {
        lines.push(Line::from(Span::styled(
            "倒计时已停止；问题保持可回答",
            Style::default().fg(Color::DarkGray),
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
        "",
        false,
    ));
    hits.push(Some(DecisionClick::PlanView));
    lines.push(option_line(
        changes_focused,
        "2.",
        "请求修改",
        "取消本次等待，回到输入框说明修改（不提交批准）",
        false,
    ));
    hits.push(Some(DecisionClick::PlanRequestChanges));
    lines.push(option_line(
        approve_focused,
        "3.",
        "批准",
        "返回官方批准标签",
        false,
    ));
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
    detail: &str,
    selected: bool,
) -> Line<'static> {
    let mut spans = vec![
        Span::styled(
            format!("{} ", if focused { ">" } else { " " }),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled(format!("{number} "), Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{} ", if selected { "[x]" } else { "[ ]" }),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            label.to_string(),
            if focused {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            },
        ),
    ];
    if !detail.is_empty() {
        spans.push(Span::styled(
            format!("  {detail}"),
            Style::default().fg(Color::DarkGray),
        ));
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
    let focused = !app.scene.focus().input_focused() && app.decisions.top().is_none();
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

    #[test]
    fn display_rows_count_wide_cells() {
        assert_eq!(display_rows("你好世界", 4), vec!["你好", "世界"]);
    }

    #[test]
    fn a_timed_question_shows_the_countdown_and_the_hold_key() {
        let questions = nano_decisions::parse_questions(Some(&serde_json::json!([
            {"id": "q", "question": "Pick", "options": [{"label": "A"}]}
        ])));
        let card = QuestionCard::new(
            Some("r".into()),
            Some("c".into()),
            questions,
            Some(nano_decisions::TimedState::start(90_000, Instant::now())),
        );
        let (_title, lines, _hits) = render_question(&card, Rect::new(0, 0, 120, 20));
        let text = lines
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("剩余"));
        assert!(text.contains("Alt+T"));
    }

    #[test]
    fn the_approval_card_defaults_to_reject_and_never_preselects_allow() {
        let card = nano_decisions::ApprovalCard::new(
            "r".into(),
            "bash".into(),
            Some("sandbox".into()),
            Default::default(),
            None,
            None,
            Instant::now(),
        );
        let (_title, lines, _hits) = render_approval(&card, Rect::new(0, 0, 120, 20));
        let text = lines
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>();
        assert!(text.iter().any(|line| line.contains("> 2. [ ] 拒绝")));
        assert!(!text.iter().any(|line| line.contains("> 1. [ ] 允许")));
        assert_eq!(card.focus(), ApprovalFocus::Reject);
    }

    #[test]
    fn the_plan_review_card_defaults_to_request_changes() {
        let questions = nano_decisions::parse_questions(Some(&serde_json::json!([{
            "id": "plan",
            "question": "Review plan",
            "detail": "# Plan\nStep one",
            "intent": {"kind": "plan-review", "approve": "Approve"},
            "options": [{"label": "Approve"}, {"label": "Request changes"}]
        }])));
        let card = QuestionCard::new(Some("r".into()), Some("c".into()), questions, None);
        let (_title, lines, _hits) = render_plan_review(&card, Rect::new(0, 0, 120, 20));
        let text = lines
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>();
        assert!(text.iter().any(|line| line.contains("> 2. [ ] 请求修改")));
        assert!(!text.iter().any(|line| line.contains("> 3. [ ] 批准")));
        assert_eq!(card.plan_focus(), PlanFocus::RequestChanges);
    }
}
