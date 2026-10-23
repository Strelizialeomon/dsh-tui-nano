//! S09 terminal lifecycle: create, retain, attach, input, resize, scroll,
//! selection and close. The official terminal service is the only source of
//! process and screen truth; this state never replays unacknowledged input.

use std::collections::BTreeSet;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use nano_theme::Theme;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use serde_json::{json, Value};
use tui_term::widget::{Cursor, PseudoTerminal};

use crate::input::{encode_key, encode_mouse, encode_paste, InputBudget, InputRejection};
use crate::scroll::{ScrollCurve, ScrollProfile};
use crate::selection::Selection;
use crate::store::{PendingTerminal, TerminalStore};
use crate::wire::{
    parse_attachment, parse_create, parse_environment, parse_hold, parse_list, parse_notification,
    parse_shells, EndReason, Notification, ProcessState, Refusal, TerminalAttachment,
    TerminalEnvironment, TerminalInfo, TerminalShell,
};

/// Official client phases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// No terminal request started.
    Idle,
    /// Reading environment/shells or listing terminals.
    Loading,
    /// A create request is in flight.
    Creating,
    /// Retaining or attaching.
    Connecting,
    /// A live attachment owns the screen.
    Connected,
    /// The host transport ended; the process fact is unknown.
    Disconnected,
    /// A close request is in flight.
    Closing,
    /// The terminal is closed for this host lifetime.
    Closed,
    /// An operation failed; retry is explicit.
    Failed,
}

impl Phase {
    /// Stable label used by the status line.
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Loading => "loading",
            Self::Creating => "creating",
            Self::Connecting => "connecting",
            Self::Connected => "connected",
            Self::Disconnected => "disconnected",
            Self::Closing => "closing",
            Self::Closed => "closed",
            Self::Failed => "failed",
        }
    }
}

/// One request the app must send through the host link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestKind {
    /// Read limits and cwd.
    Environment,
    /// Discover verified shells.
    Shells,
    /// Discover shells for the picker.
    ShellsPicker,
    /// List retained terminals.
    List,
    /// Create a terminal.
    Create,
    /// Hold a terminal for this window.
    Retain,
    /// Release one hold.
    Release,
    /// Attach input/output.
    Attach,
    /// Detach without killing the process.
    Detach,
    /// Close the process range.
    Close,
}

/// One side effect for the application shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Send one request; the response returns through `on_response`.
    Request {
        /// Pending identity.
        kind: RequestKind,
        /// Protocol method.
        method: &'static str,
        /// Protocol params.
        params: Value,
        /// Operation epoch; stale responses are ignored.
        epoch: u64,
    },
    /// Send one client-to-host notification.
    Notify {
        /// Protocol method.
        method: &'static str,
        /// Protocol params.
        params: Value,
    },
    /// Put text on the OS clipboard through OSC 52.
    Clipboard(String),
    /// Update the application status line.
    Status {
        /// Whether this is an error.
        error: bool,
        /// Message.
        message: String,
    },
}

/// A failed request as classified by the app's host link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// Whether the host explicitly rejected the request. `false` means the
    /// request may have reached the host (unknown result).
    pub definite: bool,
    /// Sanitized message.
    pub message: String,
}

impl Failure {
    /// Definite host rejection.
    pub fn definite(message: impl Into<String>) -> Self {
        Self {
            definite: true,
            message: message.into(),
        }
    }

    /// Unknown delivery/result.
    pub fn unknown(message: impl Into<String>) -> Self {
        Self {
            definite: false,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone)]
struct ShellPicker {
    selected: usize,
}

/// S09 terminal feature state for one root session.
pub struct TerminalState {
    store: Option<TerminalStore>,
    store_error: Option<String>,
    root_session_id: String,
    epoch: u64,
    phase: Phase,
    environment: Option<TerminalEnvironment>,
    shells: Vec<TerminalShell>,
    selected_shell: Option<String>,
    shell_picker: Option<ShellPicker>,
    terminal_id: Option<String>,
    creating_id: Option<String>,
    hold_id: Option<String>,
    attachment_id: Option<String>,
    info: Option<TerminalInfo>,
    parser: Option<vt100::Parser>,
    writable: bool,
    output_drained: bool,
    budget: InputBudget,
    last_sequence: u64,
    sent_size: Option<(u16, u16)>,
    viewport: (u16, u16),
    scroll_profile: ScrollProfile,
    scroll: ScrollCurve,
    selection: Option<Selection>,
    reattach_attempts: u8,
    close_after_list: bool,
    notice: Option<String>,
    error: Option<String>,
    busy: bool,
}

impl TerminalState {
    /// Build the feature with an opened store.
    pub fn new(store: TerminalStore, viewport: (u16, u16)) -> Self {
        let mut state = Self::blank(viewport);
        state.store = Some(store);
        state
    }

    fn blank(viewport: (u16, u16)) -> Self {
        let scroll_profile = ScrollProfile::detect();
        Self {
            store: None,
            store_error: None,
            root_session_id: String::new(),
            epoch: 0,
            phase: Phase::Idle,
            environment: None,
            shells: Vec::new(),
            selected_shell: None,
            shell_picker: None,
            terminal_id: None,
            creating_id: None,
            hold_id: None,
            attachment_id: None,
            info: None,
            parser: None,
            writable: false,
            output_drained: false,
            budget: InputBudget::default(),
            last_sequence: 0,
            sent_size: None,
            viewport: normalize_viewport(viewport),
            scroll_profile,
            scroll: ScrollCurve::new(scroll_profile),
            selection: None,
            reattach_attempts: 0,
            close_after_list: false,
            notice: None,
            error: None,
            busy: false,
        }
    }

    /// Build a feature whose store could not be opened. Terminal operations
    /// fail explicitly instead of pretending the preference file is absent.
    pub fn unavailable(message: impl Into<String>, viewport: (u16, u16)) -> Self {
        let mut state = Self::blank(viewport);
        state.store_error = Some(message.into());
        state.phase = Phase::Failed;
        state.error = state.store_error.clone();
        state
    }

    /// Bind the official root session used by attach requests.
    pub fn set_root_session_id(&mut self, session_id: impl Into<String>) {
        self.root_session_id = session_id.into();
    }

    /// Root session this terminal feature is bound to.
    pub fn root_session_id(&self) -> &str {
        &self.root_session_id
    }

    /// Current operation epoch. Responses from an older epoch are ignored.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Current phase.
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Current terminal metadata.
    pub fn info(&self) -> Option<&TerminalInfo> {
        self.info.as_ref()
    }

    /// Whether a live attachment owns the screen.
    pub fn connected(&self) -> bool {
        self.phase == Phase::Connected
    }

    /// Last error, for the status line.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Current non-error notice.
    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// Update the viewport used for create/resize. Draw sets this every frame.
    pub fn set_viewport(&mut self, cols: u16, rows: u16) {
        self.viewport = normalize_viewport((cols, rows));
    }

    /// Open or reattach the terminal view.
    pub fn open(&mut self) -> Vec<Command> {
        if matches!(
            self.phase,
            Phase::Loading | Phase::Creating | Phase::Connecting | Phase::Closing
        ) {
            return Vec::new();
        }
        if self.phase == Phase::Connected && self.attachment_id.is_some() {
            return Vec::new();
        }
        if let Some(error) = self.store_failure() {
            return self.fail(error);
        }
        self.shell_picker = None;
        self.epoch = self.epoch.saturating_add(1);
        self.error = None;
        self.notice = None;
        self.phase = Phase::Loading;
        self.busy = true;
        vec![self.cmd(RequestKind::List, "nano.terminal.list", json!({}))]
    }

    /// Close the process range. The response decides closing/closed/failed.
    pub fn close_terminal(&mut self) -> Vec<Command> {
        if self.phase == Phase::Closing {
            return Vec::new();
        }
        if matches!(self.phase, Phase::Loading | Phase::Creating) && self.terminal_id.is_none() {
            self.notice = Some("终端正在创建或读取，请稍候再关闭".to_string());
            return vec![status_ok("终端正在创建或读取，请稍候再关闭")];
        }
        self.epoch = self.epoch.saturating_add(1);
        self.shell_picker = None;
        if let Some(id) = self.terminal_id.clone() {
            self.phase = Phase::Closing;
            self.writable = false;
            return vec![self.cmd(
                RequestKind::Close,
                "nano.terminal.close",
                json!({ "terminalId": id }),
            )];
        }
        // A lost create response may have left an identity in the official
        // owner. Query before declaring the view closed so the orphan cannot
        // consume the Session's terminal quota.
        let pending = self
            .store
            .as_ref()
            .map(|store| store.pending().to_vec())
            .unwrap_or_default();
        if !pending.is_empty() {
            self.phase = Phase::Closing;
            self.close_after_list = true;
            return vec![self.cmd(RequestKind::List, "nano.terminal.list", json!({}))];
        }
        self.phase = Phase::Closed;
        self.notice = Some("终端已关闭".to_string());
        vec![status_ok("终端已关闭")]
    }

    /// Show the shell picker for the next terminal.
    pub fn choose_shell(&mut self) -> Vec<Command> {
        let pending = self
            .store
            .as_ref()
            .map(|store| !store.pending().is_empty())
            .unwrap_or(false);
        if self.terminal_id.is_some()
            || self.creating_id.is_some()
            || self.hold_id.is_some()
            || self.attachment_id.is_some()
            || pending
            || matches!(
                self.phase,
                Phase::Loading | Phase::Creating | Phase::Connecting | Phase::Closing
            )
        {
            self.notice = Some("终端正在创建、连接或已打开；先按 F11 关闭再选 shell".to_string());
            return vec![status_ok(
                "终端正在创建、连接或已打开；先按 F11 关闭再选 shell",
            )];
        }
        if self.shells.is_empty() {
            self.busy = true;
            return vec![self.cmd(RequestKind::ShellsPicker, "nano.terminal.shells", json!({}))];
        }
        self.shell_picker = Some(ShellPicker {
            selected: self.current_shell_index(),
        });
        self.notice = Some("选择 shell：↑/↓ 选择，Enter 新建，Esc 取消".to_string());
        vec![status_ok("选择 shell：↑/↓ 选择，Enter 新建，Esc 取消")]
    }

    /// Reattach after an explicit failure or a sequence gap.
    pub fn reattach(&mut self) -> Vec<Command> {
        if self.terminal_id.is_none() {
            self.notice = Some("没有可重新附着的终端".to_string());
            return vec![status_ok("没有可重新附着的终端")];
        }
        self.epoch = self.epoch.saturating_add(1);
        self.error = None;
        self.attachment_id = None;
        self.writable = false;
        self.output_drained = false;
        self.budget.reset();
        self.last_sequence = 0;
        self.phase = Phase::Connecting;
        self.begin_attach()
    }

    /// Handle one key while the terminal scene is active.
    pub fn handle_key(&mut self, key: KeyEvent) -> Vec<Command> {
        if self.shell_picker.is_some() {
            return self.handle_picker_key(key);
        }
        if self.phase == Phase::Closing {
            return Vec::new();
        }
        if matches!(self.phase, Phase::Idle | Phase::Closed | Phase::Failed)
            && key.code == KeyCode::Enter
            && !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return self.open();
        }
        if is_copy_key(&key) {
            return self.copy_selection();
        }
        if is_reattach_key(&key) {
            return self.reattach();
        }
        if is_shell_key(&key) {
            return self.choose_shell();
        }
        let application_cursor = self
            .parser
            .as_ref()
            .map(|parser| parser.screen().application_cursor())
            .unwrap_or(false);
        match encode_key(&key, application_cursor) {
            Some(bytes) => self.send_input(bytes),
            None => Vec::new(),
        }
    }

    /// Paste exact bytes into the process, bracketed when it asked for it.
    pub fn handle_paste(&mut self, text: String) -> Vec<Command> {
        if self.shell_picker.is_some() {
            return Vec::new();
        }
        let bracketed = self
            .parser
            .as_ref()
            .map(|parser| parser.screen().bracketed_paste())
            .unwrap_or(false);
        self.send_input(encode_paste(&text, bracketed))
    }

    /// Handle one mouse event in the terminal screen area.
    pub fn handle_mouse(&mut self, event: MouseEvent, area: Rect) -> Vec<Command> {
        if area.width == 0 || area.height == 0 {
            return Vec::new();
        }
        if let Some(parser) = &self.parser {
            let mode = parser.screen().mouse_protocol_mode();
            let encoding = parser.screen().mouse_protocol_encoding();
            if mode != vt100::MouseProtocolMode::None
                && !event.modifiers.contains(KeyModifiers::SHIFT)
            {
                if let Some(bytes) = encode_mouse(&event, area, mode, encoding) {
                    return self.send_input(bytes);
                }
                return Vec::new();
            }
        }
        let cell = (
            event
                .row
                .saturating_sub(area.y)
                .min(area.height.saturating_sub(1)),
            event
                .column
                .saturating_sub(area.x)
                .min(area.width.saturating_sub(1)),
        );
        match event.kind {
            MouseEventKind::ScrollUp => {
                if self.selection.is_none() {
                    self.scroll.push(1);
                }
                Vec::new()
            }
            MouseEventKind::ScrollDown => {
                if self.selection.is_none() {
                    self.scroll.push(-1);
                }
                Vec::new()
            }
            MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                self.selection = Some(Selection::start(cell));
                Vec::new()
            }
            MouseEventKind::Drag(crossterm::event::MouseButton::Left) => {
                if let Some(selection) = self.selection.as_mut() {
                    selection.update(cell);
                }
                Vec::new()
            }
            MouseEventKind::Up(crossterm::event::MouseButton::Left) => {
                if let Some(selection) = self.selection.as_mut() {
                    selection.update(cell);
                }
                let commands = self.copy_selection();
                // The drag is over: release the selection so the wheel scrolls
                // again. A plain click never produces a clipboard payload.
                self.selection = None;
                commands
            }
            _ => Vec::new(),
        }
    }

    /// Apply merged scrolling and deferred resize once per frame.
    pub fn tick(&mut self) -> Vec<Command> {
        let mut commands = Vec::new();
        let lines = self.scroll.take_lines();
        if lines != 0 {
            if let Some(parser) = self.parser.as_mut() {
                let current = parser.screen().scrollback();
                let next = if lines >= 0 {
                    current.saturating_add(lines as usize)
                } else {
                    current.saturating_sub(lines.unsigned_abs() as usize)
                };
                parser.set_scrollback(next);
            }
        }
        if self.phase == Phase::Connected && self.attachment_id.is_some() && self.writable {
            commands.extend(self.maybe_resize());
        }
        self.busy = false;
        commands
    }

    /// Draw the terminal screen (or its current placeholder).
    pub fn draw(&mut self, frame: &mut Frame, area: Rect, theme: Theme) {
        self.set_viewport(area.width, area.height);
        if self.parser.is_none() {
            self.draw_placeholder(frame, area, theme);
            if self.shell_picker.is_some() {
                self.draw_picker(frame, area, theme);
            }
            return;
        }
        let parser = self.parser.as_ref().expect("parser checked");
        let mut cursor = Cursor::default();
        cursor.hide();
        let widget = PseudoTerminal::new(parser.screen()).cursor(cursor);
        frame.render_widget(widget, area);
        // tui-term reports a default foreground as `Reset`. Nano owns that
        // default, while explicit program colors stay untouched.
        let default_fg = theme.colors().fg;
        let buffer = frame.buffer_mut();
        for row in area.top()..area.bottom() {
            for column in area.left()..area.right() {
                if let Some(cell) = buffer.cell_mut((column, row)) {
                    if cell.fg == Color::Reset {
                        cell.set_fg(default_fg);
                    }
                }
            }
        }
        if let Some(selection) = &self.selection {
            let buffer = frame.buffer_mut();
            for row in 0..area.height {
                for column in 0..area.width {
                    if selection.contains(row, column) {
                        let x = area.x + column;
                        let y = area.y + row;
                        if let Some(cell) = buffer.cell_mut((x, y)) {
                            cell.modifier.insert(Modifier::REVERSED);
                        }
                    }
                }
            }
        }
        if self.shell_picker.is_some() {
            self.draw_picker(frame, area, theme);
            return;
        }
        if self.writable
            && self
                .info
                .as_ref()
                .map(|info| info.state.is_running())
                .unwrap_or(false)
            && parser.screen().scrollback() == 0
            && !parser.screen().hide_cursor()
        {
            let (row, column) = parser.screen().cursor_position();
            if row < area.height && column < area.width {
                frame.set_cursor_position((area.x + column, area.y + row));
            }
        }
    }

    /// Status line content for the app's bottom bar.
    pub fn status_line(&self) -> String {
        let mut parts = vec![format!("终端 · {}", self.phase.label())];
        if let Some(info) = &self.info {
            parts.push(format!(
                "{} {}x{}",
                if info.title.is_empty() {
                    info.shell.name.as_str()
                } else {
                    info.title.as_str()
                },
                info.cols,
                info.rows
            ));
            parts.push(match info.state {
                ProcessState::Running => "运行中".to_string(),
                ProcessState::Exited => format!(
                    "进程已退出{}",
                    info.exit_code
                        .map(|code| format!(" ({code})"))
                        .unwrap_or_default()
                ),
                ProcessState::Failed => "进程失败".to_string(),
            });
        }
        if self.attachment_id.is_some() {
            parts.push(if self.writable {
                "可输入".to_string()
            } else {
                "只读".to_string()
            });
            if self
                .info
                .as_ref()
                .map(|info| info.state == ProcessState::Exited)
                .unwrap_or(false)
            {
                // The official final exited state is published only after the
                // output stream drained, even though the attachment stays open.
                parts.push("输出已收完".to_string());
            }
        } else if self.phase == Phase::Connected {
            parts.push(if self.output_drained {
                "输出已收完".to_string()
            } else {
                "输出未附着".to_string()
            });
        }
        if let Some(parser) = &self.parser {
            let scrollback = parser.screen().scrollback();
            if scrollback > 0 {
                parts.push(format!("历史 {scrollback} 行（受 scrollback 限制）"));
            }
        }
        if let Some(message) = self.error.as_ref().or(self.notice.as_ref()) {
            parts.push(message.clone());
        }
        parts.push(
            if matches!(self.phase, Phase::Idle | Phase::Failed | Phase::Closed) {
                "Enter 打开 · F9 选 shell · F12 返回"
            } else {
                "F12 返回 · F11 关闭 · F10 重附 · F9 选 shell · F8 复制"
            }
            .to_string(),
        );
        parts.join(" · ")
    }

    /// Apply one request result from the current operation epoch.
    pub fn on_response(
        &mut self,
        kind: RequestKind,
        result: Result<Value, Failure>,
    ) -> Vec<Command> {
        self.on_response_epoch(self.epoch, kind, result)
    }

    /// Apply one request result. Stale epochs are discarded so a timeout from
    /// an earlier create/attach cannot mutate the current view.
    pub fn on_response_epoch(
        &mut self,
        epoch: u64,
        kind: RequestKind,
        result: Result<Value, Failure>,
    ) -> Vec<Command> {
        if epoch != self.epoch {
            return Vec::new();
        }
        self.busy = false;
        let value = match result {
            Ok(value) => value,
            Err(error) => return self.on_request_error(kind, error),
        };
        match kind {
            RequestKind::Environment => match parse_environment(&value) {
                Ok(environment) => {
                    self.environment = Some(environment);
                    vec![self.cmd(RequestKind::Shells, "nano.terminal.shells", json!({}))]
                }
                Err(error) => self.fail(error),
            },
            RequestKind::Shells => match parse_shells(&value) {
                Ok(shells) => {
                    self.shells = shells;
                    if let Err(message) = self.choose_default_shell() {
                        return self.fail(message);
                    }
                    self.begin_create()
                }
                Err(error) => self.fail(error),
            },
            RequestKind::ShellsPicker => match parse_shells(&value) {
                Ok(shells) => {
                    self.shells = shells;
                    self.shell_picker = Some(ShellPicker {
                        selected: self.current_shell_index(),
                    });
                    self.notice = Some("选择 shell：↑/↓ 选择，Enter 新建，Esc 取消".to_string());
                    vec![status_ok("选择 shell：↑/↓ 选择，Enter 新建，Esc 取消")]
                }
                Err(error) => self.fail(error),
            },
            RequestKind::List => match parse_list(&value) {
                Ok(terminals) => {
                    if self.close_after_list {
                        self.close_after_list = false;
                        return self.close_reconciled(terminals);
                    }
                    self.reconcile(terminals)
                }
                Err(error) => self.fail(error),
            },
            RequestKind::Create => match parse_create(&value) {
                Ok(info) => {
                    let id = info.id.clone();
                    if let Some(store) = self.store.as_mut() {
                        let _ = store.clear_pending(&id);
                    }
                    self.creating_id = None;
                    self.terminal_id = Some(id);
                    self.info = Some(info);
                    self.phase = Phase::Connecting;
                    self.begin_attach()
                }
                Err(error) => self.fail(error),
            },
            RequestKind::Retain => {
                if self.phase == Phase::Closing {
                    // Close owns the terminal now; the bridge releases holds.
                    return Vec::new();
                }
                match parse_hold(&value) {
                    Ok(hold_id) => {
                        self.hold_id = Some(hold_id);
                        vec![self.cmd(
                            RequestKind::Attach,
                            "nano.terminal.attach",
                            json!({ "sessionId": self.root_session_id, "terminalId": self.terminal_id }),
                        )]
                    }
                    Err(error) => self.fail(error),
                }
            }
            RequestKind::Release => Vec::new(),
            RequestKind::Detach => Vec::new(),
            RequestKind::Attach => {
                if self.phase == Phase::Closing {
                    return Vec::new();
                }
                match parse_attachment(&value) {
                    Ok(attachment) => self.apply_attachment(attachment),
                    Err(error) => self.fail(error),
                }
            }
            RequestKind::Close => {
                if let (Some(store), Some(id)) = (self.store.as_mut(), self.terminal_id.as_deref())
                {
                    let _ = store.clear_pending(id);
                }
                self.reset_terminal();
                self.phase = Phase::Closed;
                self.output_drained = true;
                self.notice = Some("终端已关闭（输出已排空）".to_string());
                vec![status_ok("终端已关闭（输出已排空）")]
            }
        }
    }

    /// Apply one live terminal notification.
    pub fn on_notification(&mut self, method: &str, params: &Value) -> Vec<Command> {
        let notification = match parse_notification(method, params) {
            Ok(notification) => notification,
            Err(error) => return self.fail(error),
        };
        match notification {
            Notification::Output {
                attachment_id,
                sequence,
                data,
                ..
            } => {
                if self.attachment_id.as_deref() != Some(attachment_id.as_str()) {
                    return Vec::new();
                }
                if sequence <= self.last_sequence {
                    return Vec::new();
                }
                if sequence != self.last_sequence + 1 {
                    return self.on_sequence_gap(sequence);
                }
                self.last_sequence = sequence;
                if let Some(parser) = self.parser.as_mut() {
                    parser.process(data.as_bytes());
                }
                Vec::new()
            }
            Notification::State {
                attachment_id,
                writable,
                info,
                ..
            } => {
                if self.attachment_id.as_deref() != Some(attachment_id.as_str()) {
                    return Vec::new();
                }
                self.writable = self.phase != Phase::Closing && writable && info.state.is_running();
                let size_changed = self
                    .info
                    .as_ref()
                    .map(|current| (current.cols, current.rows) != (info.cols, info.rows))
                    .unwrap_or(false);
                if size_changed {
                    if let Some(parser) = self.parser.as_mut() {
                        parser.set_size(info.rows, info.cols);
                    }
                }
                self.info = Some(info);
                Vec::new()
            }
            Notification::Ack {
                attachment_id,
                sequence,
                refusal,
            } => {
                if self.attachment_id.as_deref() != Some(attachment_id.as_str()) {
                    return Vec::new();
                }
                self.budget.acknowledge(sequence);
                if let Some(refusal) = refusal {
                    self.writable = false;
                    let message = match refusal {
                        Refusal::ReadOnly => "输入被拒绝：当前附件只读",
                        Refusal::NotRunning => "输入被拒绝：进程未运行",
                    };
                    self.notice = Some(message.to_string());
                    return vec![status_ok(message)];
                }
                Vec::new()
            }
            Notification::Ended {
                attachment_id,
                reason,
                detail,
                ..
            } => {
                if self.attachment_id.as_deref() != Some(attachment_id.as_str()) {
                    return Vec::new();
                }
                self.attachment_id = None;
                self.writable = false;
                self.budget.reset();
                self.output_drained = matches!(reason, EndReason::Drained | EndReason::Closed);
                if let Some(error) = self.ended_message(reason, detail) {
                    self.error = Some(error.clone());
                    return vec![status_err(error)];
                }
                Vec::new()
            }
        }
    }

    /// Host transport ended; the process fact is unknown until a new host.
    pub fn disconnect(&mut self, message: impl Into<String>) -> Vec<Command> {
        if self.phase == Phase::Closed {
            return Vec::new();
        }
        self.epoch = self.epoch.saturating_add(1);
        self.attachment_id = None;
        self.hold_id = None;
        self.writable = false;
        self.budget.reset();
        self.phase = Phase::Disconnected;
        self.error = Some(message.into());
        vec![status_err(self.error.clone().unwrap_or_default())]
    }

    fn reconcile(&mut self, terminals: Vec<TerminalInfo>) -> Vec<Command> {
        let pending: Vec<PendingTerminal> = self
            .store
            .as_ref()
            .map(|store| store.pending().to_vec())
            .unwrap_or_default();
        let mut recovered: Option<TerminalInfo> = None;
        if let Some(id) = self.terminal_id.clone() {
            recovered = terminals.iter().find(|info| info.id == id).cloned();
            if recovered.is_none() {
                self.terminal_id = None;
                self.info = None;
                self.parser = None;
                self.attachment_id = None;
                self.hold_id = None;
            }
        }
        if recovered.is_none() {
            for entry in &pending {
                if let Some(info) = terminals.iter().find(|info| info.id == entry.id).cloned() {
                    recovered = Some(info);
                    break;
                }
            }
        }
        if recovered.is_none() && pending.is_empty() {
            // A single retained terminal in this host is this window's view.
            recovered = terminals.first().cloned();
        }
        // A pending id absent from list may still be an in-flight allocation:
        // list() excludes owner.pending. Keep it and retry create with the same
        // id instead of allocating a second shell.
        if let Some(info) = recovered {
            self.terminal_id = Some(info.id.clone());
            self.info = Some(info);
            self.phase = Phase::Connecting;
            return self.begin_attach();
        }
        self.discover()
    }

    /// Close after reconciling a lost create response. A pending identity that
    /// the host still owns is closed explicitly; one it does not own is pruned.
    fn close_reconciled(&mut self, terminals: Vec<TerminalInfo>) -> Vec<Command> {
        let pending = self
            .store
            .as_ref()
            .map(|store| store.pending().to_vec())
            .unwrap_or_default();
        if let Some(info) = pending
            .iter()
            .find_map(|entry| terminals.iter().find(|info| info.id == entry.id).cloned())
        {
            self.terminal_id = Some(info.id.clone());
            self.info = Some(info);
        } else if let Some(entry) = pending.first() {
            // list() cannot see an in-flight allocation. The official close()
            // awaits owner.pending by id, so close that identity explicitly.
            self.terminal_id = Some(entry.id.clone());
        }
        if let Some(id) = self.terminal_id.clone() {
            self.phase = Phase::Closing;
            self.writable = false;
            return vec![self.cmd(
                RequestKind::Close,
                "nano.terminal.close",
                json!({ "terminalId": id }),
            )];
        }
        self.phase = Phase::Closed;
        self.notice = Some("终端已关闭".to_string());
        vec![status_ok("终端已关闭")]
    }

    fn discover(&mut self) -> Vec<Command> {
        if let Some(error) = self.store_failure() {
            return self.fail(error);
        }
        self.phase = Phase::Loading;
        vec![self.cmd(
            RequestKind::Environment,
            "nano.terminal.environment",
            json!({}),
        )]
    }

    fn begin_create(&mut self) -> Vec<Command> {
        let environment = match &self.environment {
            Some(environment) => environment.clone(),
            None => return self.fail("终端环境尚未就绪"),
        };
        let Some(store) = self.store.as_mut() else {
            return self.fail("终端状态文件不可用");
        };
        let id = match store.pending().last() {
            Some(pending) => pending.id.clone(),
            None => new_terminal_id(),
        };
        let pending = store.set_pending(&id);
        if let Err(error) = pending {
            return self.fail(error);
        }
        self.creating_id = Some(id.clone());
        self.phase = Phase::Creating;
        let (cols, rows) = self.clamped_size(&environment);
        let mut params = json!({
            "terminalId": id,
            "cols": cols,
            "rows": rows,
        });
        if let Some(shell) = self.selected_shell.clone() {
            params["shellPath"] = Value::String(shell);
        }
        vec![self.cmd(RequestKind::Create, "nano.terminal.create", params)]
    }

    fn begin_attach(&mut self) -> Vec<Command> {
        let Some(id) = self.terminal_id.clone() else {
            return self.fail("没有终端身份");
        };
        if self.hold_id.is_none() {
            return vec![self.cmd(
                RequestKind::Retain,
                "nano.terminal.retain",
                json!({ "terminalId": id }),
            )];
        }
        vec![self.cmd(
            RequestKind::Attach,
            "nano.terminal.attach",
            json!({ "sessionId": self.root_session_id, "terminalId": id }),
        )]
    }

    fn apply_attachment(&mut self, attachment: TerminalAttachment) -> Vec<Command> {
        if self.terminal_id.as_deref() != Some(attachment.terminal_id.as_str()) {
            return self.fail("附着返回了不同的终端身份");
        }
        let scrollback = self
            .environment
            .as_ref()
            .map(|environment| environment.scrollback)
            .unwrap_or(1000);
        let mut parser = vt100::Parser::new(attachment.info.rows, attachment.info.cols, scrollback);
        parser.process(attachment.snapshot.as_bytes());
        self.parser = Some(parser);
        self.info = Some(attachment.info.clone());
        self.attachment_id = Some(attachment.attachment_id);
        self.writable = attachment.writable && attachment.info.state.is_running();
        self.output_drained =
            attachment.info.state != ProcessState::Running && !attachment.writable;
        self.last_sequence = attachment.sequence;
        self.sent_size = Some((attachment.info.cols, attachment.info.rows));
        self.budget.reset();
        self.selection = None;
        self.reattach_attempts = 0;
        if let (Some(store), Some(id)) = (self.store.as_mut(), self.terminal_id.as_deref()) {
            let _ = store.clear_pending(id);
        }
        self.phase = Phase::Connected;
        self.error = None;
        self.notice = Some("终端已附着".to_string());
        let mut commands = vec![status_ok("终端已附着")];
        commands.extend(self.maybe_resize());
        commands
    }

    fn send_input(&mut self, bytes: Vec<u8>) -> Vec<Command> {
        if bytes.is_empty() {
            return Vec::new();
        }
        let (Some(attachment_id), Some(environment)) =
            (self.attachment_id.clone(), self.environment.as_ref())
        else {
            self.notice = Some("终端尚未附着".to_string());
            return vec![status_ok("终端尚未附着")];
        };
        if !self.writable
            || self
                .info
                .as_ref()
                .map(|info| !info.state.is_running())
                .unwrap_or(true)
        {
            self.notice = Some("输入已禁用：终端只读或进程未运行".to_string());
            return vec![status_ok("输入已禁用：终端只读或进程未运行")];
        }
        let text = match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(_) => return self.fail("终端输入必须是 UTF-8"),
        };
        let sequence = match self.budget.allocate(
            text.len(),
            environment.max_input_bytes,
            environment.max_buffered_bytes,
        ) {
            Ok(sequence) => sequence,
            Err(rejection) => return self.input_full(rejection),
        };
        vec![Command::Notify {
            method: "nano.terminal.input",
            params: json!({
                "attachmentId": attachment_id,
                "sequence": sequence,
                "bytes": text,
            }),
        }]
    }

    fn input_full(&mut self, rejection: InputRejection) -> Vec<Command> {
        let message = match rejection {
            InputRejection::TooLarge => "单条输入超过官方 maxInputBytes（inputFull）；附件已取消",
            InputRejection::Full => "未确认输入已满官方缓冲（inputFull）；附件已取消",
            InputRejection::Disabled => "输入已禁用",
        };
        self.error = Some(message.to_string());
        self.writable = false;
        let attachment = self.attachment_id.take();
        self.budget.reset();
        let mut commands = vec![status_err(message)];
        if let Some(attachment_id) = attachment {
            commands.push(self.cmd(
                RequestKind::Detach,
                "nano.terminal.detach",
                json!({ "attachmentId": attachment_id }),
            ));
        }
        commands
    }

    fn maybe_resize(&mut self) -> Vec<Command> {
        let (Some(attachment_id), Some(environment)) =
            (self.attachment_id.clone(), self.environment.as_ref())
        else {
            return Vec::new();
        };
        let (cols, rows) = self.clamped_size(environment);
        if self.sent_size == Some((cols, rows)) {
            return Vec::new();
        }
        let sequence = match self.budget.allocate(
            0,
            environment.max_input_bytes,
            environment.max_buffered_bytes,
        ) {
            Ok(sequence) => sequence,
            Err(rejection) => return self.input_full(rejection),
        };
        self.sent_size = Some((cols, rows));
        if let Some(parser) = self.parser.as_mut() {
            parser.set_size(rows, cols);
        }
        vec![Command::Notify {
            method: "nano.terminal.resize",
            params: json!({
                "attachmentId": attachment_id,
                "sequence": sequence,
                "cols": cols,
                "rows": rows,
            }),
        }]
    }

    fn on_sequence_gap(&mut self, sequence: u64) -> Vec<Command> {
        self.reattach_attempts = self.reattach_attempts.saturating_add(1);
        if self.reattach_attempts > 3 {
            return self.fail(format!(
                "终端输出序列缺口（期望 {}，收到 {sequence}），重附着反复失败",
                self.last_sequence + 1
            ));
        }
        self.attachment_id = None;
        self.writable = false;
        self.budget.reset();
        self.last_sequence = 0;
        self.notice = Some("终端输出序列缺口，正在重新附着".to_string());
        let mut commands = vec![status_ok("终端输出序列缺口，正在重新附着")];
        self.phase = Phase::Connecting;
        commands.extend(self.begin_attach());
        commands
    }

    fn on_request_error(&mut self, kind: RequestKind, error: Failure) -> Vec<Command> {
        if self.phase == Phase::Disconnected {
            return Vec::new();
        }
        if kind == RequestKind::List && self.close_after_list {
            self.close_after_list = false;
            self.phase = Phase::Failed;
            self.error = Some(format!("关闭对账失败：{}", error.message));
            return vec![status_err(self.error.clone().unwrap_or_default())];
        }
        if self.phase == Phase::Closing
            && matches!(
                kind,
                RequestKind::Retain
                    | RequestKind::Attach
                    | RequestKind::Environment
                    | RequestKind::Shells
                    | RequestKind::ShellsPicker
            )
        {
            // The close request owns the outcome; late attach/discovery errors
            // must not turn a verified close into a failed phase.
            return Vec::new();
        }
        match kind {
            RequestKind::Create => {
                let id = self.creating_id.clone();
                if error.definite {
                    if let (Some(store), Some(id)) = (self.store.as_mut(), id.as_deref()) {
                        let _ = store.clear_pending(id);
                    }
                    self.creating_id = None;
                    self.terminal_id = None;
                    self.fail(format!("创建终端失败：{}", error.message))
                } else {
                    self.creating_id = None;
                    self.phase = Phase::Failed;
                    self.error = Some(format!(
                        "创建结果未知（{}）；重新打开会对齐同一 id，不另起 shell",
                        error.message
                    ));
                    vec![status_err(self.error.clone().unwrap_or_default())]
                }
            }
            RequestKind::Attach | RequestKind::Retain => {
                self.phase = Phase::Failed;
                self.writable = false;
                self.attachment_id = None;
                // Keep any existing hold: an attach failure is retryable and
                // must not let unattended cleanup reclaim a visible terminal.
                self.error = Some(format!("附着终端失败：{}", error.message));
                vec![status_err(self.error.clone().unwrap_or_default())]
            }
            RequestKind::Close => {
                self.phase = Phase::Failed;
                let message = if error.definite {
                    format!("关闭终端失败：{}；资源保留可重试", error.message)
                } else {
                    format!(
                        "关闭结果未知（{}）；重新打开会先查询官方状态",
                        error.message
                    )
                };
                self.error = Some(message.clone());
                vec![status_err(message)]
            }
            _ => self.fail(error.message),
        }
    }

    fn ended_message(&self, reason: EndReason, detail: Option<String>) -> Option<String> {
        match reason {
            EndReason::Drained | EndReason::Detached | EndReason::Closed => None,
            EndReason::SlowConsumer => {
                Some("终端输出过慢，附件被关闭；屏幕可从快照重新附着".to_string())
            }
            EndReason::Failed => Some(match detail {
                Some(detail) => format!("终端附件失败：{detail}"),
                None => "终端附件失败；进程和恢复屏幕仍保留".to_string(),
            }),
            EndReason::InputFailed => Some(match detail {
                Some(detail) => format!("终端输入被拒绝：{detail}"),
                None => "终端输入被拒绝；输出仍可读".to_string(),
            }),
        }
    }

    fn copy_selection(&mut self) -> Vec<Command> {
        let Some(selection) = self.selection.as_ref() else {
            return Vec::new();
        };
        let Some(parser) = self.parser.as_ref() else {
            return Vec::new();
        };
        match selection.text(parser.screen()) {
            Some(text) => {
                self.notice = Some(format!("已复制 {} 字节", text.len()));
                vec![Command::Clipboard(text), status_ok("已复制所选终端文本")]
            }
            None => Vec::new(),
        }
    }

    fn handle_picker_key(&mut self, key: KeyEvent) -> Vec<Command> {
        let picker = self.shell_picker.as_mut().expect("picker checked");
        match key.code {
            KeyCode::Up => {
                if picker.selected > 0 {
                    picker.selected -= 1;
                }
                Vec::new()
            }
            KeyCode::Down => {
                if picker.selected + 1 < self.shells.len() {
                    picker.selected += 1;
                }
                Vec::new()
            }
            KeyCode::Enter => {
                if self.terminal_id.is_some()
                    || self.creating_id.is_some()
                    || self.hold_id.is_some()
                    || self.attachment_id.is_some()
                {
                    self.shell_picker = None;
                    self.notice = Some("终端已创建；未更换 shell".to_string());
                    return vec![status_ok("终端已创建；未更换 shell")];
                }
                let selected = picker.selected;
                let path = self.shells.get(selected).map(|shell| shell.path.clone());
                self.shell_picker = None;
                match path {
                    Some(path) => {
                        if let Some(store) = self.store.as_mut() {
                            let _ = store.set_shell_path(Some(path.clone()));
                        }
                        self.selected_shell = Some(path);
                        self.epoch = self.epoch.saturating_add(1);
                        self.reset_terminal();
                        self.discover()
                    }
                    None => self.fail("没有可选的 shell"),
                }
            }
            KeyCode::Esc => {
                self.shell_picker = None;
                vec![status_ok("已取消 shell 选择")]
            }
            _ => Vec::new(),
        }
    }

    fn choose_default_shell(&mut self) -> Result<(), String> {
        if self.shells.is_empty() {
            return Err("官方执行环境没有可用 shell".to_string());
        }
        let saved = self
            .store
            .as_ref()
            .and_then(|store| store.shell_path().map(str::to_string));
        let chosen = saved
            .as_deref()
            .and_then(|path| self.shells.iter().find(|shell| shell.path == path))
            .or_else(|| self.shells.first())
            .map(|shell| shell.path.clone())
            .ok_or_else(|| "官方执行环境没有可用 shell".to_string())?;
        self.selected_shell = Some(chosen);
        Ok(())
    }

    fn current_shell_index(&self) -> usize {
        self.selected_shell
            .as_ref()
            .and_then(|path| self.shells.iter().position(|shell| &shell.path == path))
            .unwrap_or(0)
    }

    fn clamped_size(&self, environment: &TerminalEnvironment) -> (u16, u16) {
        let cols = self.viewport.0.clamp(2, environment.max_cols.max(2));
        let rows = self.viewport.1.clamp(1, environment.max_rows.max(1));
        (cols, rows)
    }

    fn store_failure(&self) -> Option<String> {
        if self.store.is_none() {
            self.store_error
                .clone()
                .or_else(|| Some("终端状态文件不可用".to_string()))
        } else {
            None
        }
    }

    fn reset_terminal(&mut self) {
        self.attachment_id = None;
        self.hold_id = None;
        self.terminal_id = None;
        self.creating_id = None;
        self.info = None;
        self.parser = None;
        self.writable = false;
        self.output_drained = false;
        self.budget.reset();
        self.last_sequence = 0;
        self.sent_size = None;
        self.selection = None;
        self.scroll = ScrollCurve::new(self.scroll_profile);
        self.reattach_attempts = 0;
        self.close_after_list = false;
        self.error = None;
    }

    fn fail(&mut self, message: impl Into<String>) -> Vec<Command> {
        let message = message.into();
        self.phase = Phase::Failed;
        self.error = Some(message.clone());
        self.busy = false;
        vec![status_err(message)]
    }

    fn cmd(&self, kind: RequestKind, method: &'static str, params: Value) -> Command {
        Command::Request {
            kind,
            method,
            params,
            epoch: self.epoch,
        }
    }

    fn draw_placeholder(&self, frame: &mut Frame, area: Rect, theme: Theme) {
        let colors = theme.colors();
        let title = format!(" 终端 · {} ", self.phase.label());
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border_style())
            .title(title);
        let mut lines = vec![
            Line::from(Span::styled(
                "官方交互终端",
                Style::default()
                    .fg(colors.accent)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
        ];
        if let Some(error) = &self.error {
            lines.push(Line::from(Span::styled(
                error.clone(),
                Style::default().fg(colors.error),
            )));
        } else if let Some(notice) = &self.notice {
            lines.push(Line::from(Span::styled(
                notice.clone(),
                Style::default().fg(colors.warning),
            )));
        } else {
            lines.push(Line::from(Span::styled(
                "正在读取官方终端状态…",
                Style::default().fg(colors.muted),
            )));
        }
        if matches!(self.phase, Phase::Idle | Phase::Failed | Phase::Closed) {
            lines.push(Line::from(""));
            lines.push(Line::from("Enter 打开终端 · F9 选择 shell · F12 返回"));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "终端输出不进入会话历史，也不会发送给模型。",
            Style::default().fg(colors.muted),
        )));
        let paragraph = Paragraph::new(lines)
            .block(block)
            .alignment(Alignment::Left)
            .wrap(ratatui::widgets::Wrap { trim: false });
        frame.render_widget(paragraph, area);
    }

    fn draw_picker(&self, frame: &mut Frame, area: Rect, theme: Theme) {
        let Some(picker) = &self.shell_picker else {
            return;
        };
        if area.width < 2 || area.height < 2 {
            return;
        }
        let height = ((self.shells.len().min(8) + 2) as u16).min(area.height);
        let width = area.width.min(60);
        let popup = Rect::new(
            area.x + area.width.saturating_sub(width) / 2,
            area.y + area.height.saturating_sub(height) / 2,
            width,
            height,
        );
        frame.render_widget(ratatui::widgets::Clear, popup);
        frame.buffer_mut().set_style(popup, theme.base_style());
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border_style())
            .title(" 选择 shell ");
        let inner = block.inner(popup);
        frame.render_widget(block, popup);
        let rows: Vec<Line> = self
            .shells
            .iter()
            .enumerate()
            .skip(picker.selected.saturating_sub(6))
            .take(inner.height as usize)
            .map(|(index, shell)| {
                let marker = if index == picker.selected {
                    "▶ "
                } else {
                    "  "
                };
                let name = if shell.name.is_empty() {
                    shell.path.clone()
                } else {
                    format!("{}  {}", shell.name, shell.path)
                };
                let style = if index == picker.selected {
                    Style::default()
                        .fg(theme.colors().fg)
                        .bg(theme.colors().selected_bg)
                } else {
                    Style::default()
                };
                Line::from(Span::styled(format!("{marker}{name}"), style))
            })
            .collect();
        frame.render_widget(Paragraph::new(rows), inner);
    }
}

/// Toggle key handled by the app scene router. Function keys avoid the
/// Ctrl+Shift ambiguity on terminals that do not report the shift bit.
pub fn is_toggle_key(key: &KeyEvent) -> bool {
    key.modifiers.is_empty() && key.code == KeyCode::F(12)
}

/// Close key handled by the app scene router.
pub fn is_close_key(key: &KeyEvent) -> bool {
    key.modifiers.is_empty() && key.code == KeyCode::F(11)
}

/// Copy key for the current selection. Mouse release also copies.
pub fn is_copy_key(key: &KeyEvent) -> bool {
    key.modifiers.is_empty() && key.code == KeyCode::F(8)
}

/// Reattach key.
pub fn is_reattach_key(key: &KeyEvent) -> bool {
    key.modifiers.is_empty() && key.code == KeyCode::F(10)
}

/// Shell picker key.
pub fn is_shell_key(key: &KeyEvent) -> bool {
    key.modifiers.is_empty() && key.code == KeyCode::F(9)
}

fn status_ok(message: impl Into<String>) -> Command {
    Command::Status {
        error: false,
        message: message.into(),
    }
}

fn status_err(message: impl Into<String>) -> Command {
    Command::Status {
        error: true,
        message: message.into(),
    }
}

fn normalize_viewport((cols, rows): (u16, u16)) -> (u16, u16) {
    (cols.max(2), rows.max(1))
}

/// Generate a pre-create identity that satisfies the official id pattern.
pub fn new_terminal_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let counter = TERMINAL_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let seed = (nanos as u64) ^ (u64::from(std::process::id())) ^ counter;
    format!("term-{seed:016x}-{counter:04x}")
}

static TERMINAL_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Whether a key event belongs to the terminal's reserved Nano shortcuts.
pub fn is_reserved_key(key: &KeyEvent) -> bool {
    is_toggle_key(key)
        || is_close_key(key)
        || is_copy_key(key)
        || is_reattach_key(key)
        || is_shell_key(key)
}

/// Set of methods this feature may request; used by the app to classify pending
/// terminal responses in diagnostics.
pub fn request_methods() -> BTreeSet<&'static str> {
    [
        "nano.terminal.environment",
        "nano.terminal.shells",
        "nano.terminal.list",
        "nano.terminal.create",
        "nano.terminal.retain",
        "nano.terminal.release",
        "nano.terminal.attach",
        "nano.terminal.detach",
        "nano.terminal.close",
    ]
    .into_iter()
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn store() -> TerminalStore {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "nano-terminal-state-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        TerminalStore::open(&root, "session-test").expect("store")
    }

    fn state() -> TerminalState {
        TerminalState::new(store(), (80, 24))
    }

    fn environment() -> Value {
        json!({
            "cwd": "/work",
            "maxInputBytes": 1024,
            "maxBufferedBytes": 4096,
            "maxCols": 120,
            "maxRows": 40,
            "scrollback": 100
        })
    }

    fn info() -> Value {
        json!({
            "id": "term-1",
            "title": "zsh",
            "shell": { "path": "/bin/zsh", "args": ["-i"], "name": "zsh" },
            "cwd": "/work",
            "cols": 80,
            "rows": 24,
            "state": "running",
            "exitCode": null
        })
    }

    fn shells() -> Value {
        json!({ "shells": [{ "path": "/bin/zsh", "args": ["-i"], "name": "zsh" }] })
    }

    fn attachment(sequence: u64) -> Value {
        json!({
            "attachmentId": "att-1",
            "terminalId": "term-1",
            "sequence": sequence,
            "writable": true,
            "info": info(),
            "snapshot": "ready\r\n"
        })
    }

    fn request_kind(command: &Command) -> Option<RequestKind> {
        match command {
            Command::Request { kind, .. } => Some(*kind),
            _ => None,
        }
    }

    fn reach_connected(state: &mut TerminalState) {
        let open = state.open();
        assert_eq!(request_kind(&open[0]), Some(RequestKind::List));
        let list = state.on_response(RequestKind::List, Ok(json!({ "terminals": [] })));
        assert_eq!(request_kind(&list[0]), Some(RequestKind::Environment));
        let environment = state.on_response(RequestKind::Environment, Ok(environment()));
        assert_eq!(request_kind(&environment[0]), Some(RequestKind::Shells));
        let shells = state.on_response(RequestKind::Shells, Ok(shells()));
        assert_eq!(request_kind(&shells[0]), Some(RequestKind::Create));
        let created = state.on_response(RequestKind::Create, Ok(json!({ "terminal": info() })));
        assert_eq!(request_kind(&created[0]), Some(RequestKind::Retain));
        let retained = state.on_response(RequestKind::Retain, Ok(json!({ "holdId": "hold-1" })));
        assert_eq!(request_kind(&retained[0]), Some(RequestKind::Attach));
        let attached = state.on_response(RequestKind::Attach, Ok(attachment(0)));
        assert_eq!(state.phase(), Phase::Connected);
        assert!(state.writable);
        assert!(attached
            .iter()
            .all(|command| !matches!(command, Command::Status { error: true, .. })));
    }

    #[test]
    fn create_persists_the_id_before_the_request() {
        let mut state = state();
        let _ = state.open();
        let _ = state.on_response(RequestKind::List, Ok(json!({ "terminals": [] })));
        let _ = state.on_response(RequestKind::Environment, Ok(environment()));
        let create = state.on_response(RequestKind::Shells, Ok(shells()));
        let task = match &create[0] {
            Command::Request {
                kind: RequestKind::Create,
                params,
                ..
            } => params["terminalId"].as_str().unwrap().to_string(),
            other => panic!("expected create, got {other:?}"),
        };
        let pending = state.store.as_ref().unwrap().pending();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, task);
    }

    #[test]
    fn an_unknown_create_keeps_the_pending_id() {
        let mut state = state();
        let _ = state.open();
        let _ = state.on_response(RequestKind::List, Ok(json!({ "terminals": [] })));
        let _ = state.on_response(RequestKind::Environment, Ok(environment()));
        let _ = state.on_response(RequestKind::Shells, Ok(shells()));
        let commands = state.on_response(
            RequestKind::Create,
            Err(Failure::unknown("request timed out")),
        );
        assert_eq!(state.phase(), Phase::Failed);
        assert!(commands
            .iter()
            .any(|command| matches!(command, Command::Status { error: true, .. })));
        assert_eq!(state.store.as_ref().unwrap().pending().len(), 1);
    }

    #[test]
    fn an_existing_pending_id_is_recovered_by_list() {
        let mut state = state();
        state
            .store
            .as_mut()
            .unwrap()
            .set_pending("term-recovered")
            .unwrap();
        let _ = state.open();
        let commands = state.on_response(
            RequestKind::List,
            Ok(json!({
                "terminals": [{
                    "id": "term-recovered",
                    "title": "zsh",
                    "shell": { "path": "/bin/zsh", "args": ["-i"], "name": "zsh" },
                    "cwd": "/work",
                    "cols": 80,
                    "rows": 24,
                    "state": "running",
                    "exitCode": null
                }]
            })),
        );
        assert_eq!(request_kind(&commands[0]), Some(RequestKind::Retain));
        assert_eq!(state.terminal_id.as_deref(), Some("term-recovered"));
    }

    #[test]
    fn output_gap_reattaches_instead_of_splicing() {
        let mut state = state();
        reach_connected(&mut state);
        let commands = state.on_notification(
            "nano.terminal.output",
            &json!({
                "attachmentId": "att-1",
                "terminalId": "term-1",
                "sequence": 2,
                "data": "wrong"
            }),
        );
        assert_eq!(state.phase(), Phase::Connecting);
        assert!(commands
            .iter()
            .any(|command| request_kind(command) == Some(RequestKind::Attach)));
        assert!(state.notice().unwrap().contains("序列缺口"));
    }

    #[test]
    fn ack_refusal_disables_input_but_keeps_the_attachment() {
        let mut state = state();
        reach_connected(&mut state);
        let commands = state.on_notification(
            "nano.terminal.ack",
            &json!({
                "attachmentId": "att-1",
                "sequence": 1,
                "refusal": "not-running"
            }),
        );
        assert!(commands
            .iter()
            .any(|command| matches!(command, Command::Status { error: false, .. })));
        assert!(!state.writable);
        assert!(state.attachment_id.is_some());
        let paste = state.handle_paste("x".to_string());
        assert!(paste
            .iter()
            .all(|command| !matches!(command, Command::Notify { .. })));
    }

    #[test]
    fn a_too_large_input_cancels_the_attachment() {
        let mut state = state();
        let mut small = environment();
        small["maxInputBytes"] = json!(2);
        let _ = state.open();
        let _ = state.on_response(RequestKind::List, Ok(json!({ "terminals": [] })));
        let _ = state.on_response(RequestKind::Environment, Ok(small));
        let _ = state.on_response(RequestKind::Shells, Ok(shells()));
        let _ = state.on_response(RequestKind::Create, Ok(json!({ "terminal": info() })));
        let _ = state.on_response(RequestKind::Retain, Ok(json!({ "holdId": "hold-1" })));
        let _ = state.on_response(RequestKind::Attach, Ok(attachment(0)));
        let commands = state.handle_paste("abcd".to_string());
        assert!(commands
            .iter()
            .any(|command| request_kind(command) == Some(RequestKind::Detach)));
        assert!(commands
            .iter()
            .any(|command| matches!(command, Command::Status { error: true, .. })));
        assert!(state.attachment_id.is_none());
    }

    #[test]
    fn tick_clamps_resize_to_the_official_limits() {
        let mut state = state();
        reach_connected(&mut state);
        state.set_viewport(500, 300);
        let commands = state.tick();
        let resize = commands
            .iter()
            .find_map(|command| match command {
                Command::Notify {
                    method: "nano.terminal.resize",
                    params,
                } => Some(params.clone()),
                _ => None,
            })
            .expect("resize");
        assert_eq!(resize["cols"], 120);
        assert_eq!(resize["rows"], 40);
    }

    #[test]
    fn close_response_resets_the_view() {
        let mut state = state();
        reach_connected(&mut state);
        let commands = state.close_terminal();
        assert_eq!(state.phase(), Phase::Closing);
        assert_eq!(request_kind(&commands[0]), Some(RequestKind::Close));
        let commands = state.on_response(RequestKind::Close, Ok(json!({ "closed": true })));
        assert_eq!(state.phase(), Phase::Closed);
        assert!(state.info.is_none());
        assert!(state.parser.is_none());
        assert!(matches!(commands[0], Command::Status { error: false, .. }));
    }

    #[test]
    fn detached_end_keeps_the_screen_for_reattach() {
        let mut state = state();
        reach_connected(&mut state);
        state.on_notification(
            "nano.terminal.ended",
            &json!({
                "attachmentId": "att-1",
                "terminalId": "term-1",
                "reason": "detached"
            }),
        );
        assert!(state.attachment_id.is_none());
        assert!(state.parser.is_some());
        assert!(!state.output_drained);
        let commands = state.reattach();
        assert_eq!(state.phase(), Phase::Connecting);
        assert!(commands
            .iter()
            .any(|command| request_kind(command) == Some(RequestKind::Attach)));
    }
}

#[cfg(test)]
mod scroll_tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn connected_with_history() -> TerminalState {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "nano-terminal-scroll-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        let store = TerminalStore::open(&root, "session-test").unwrap();
        let mut state = TerminalState::new(store, (80, 24));
        let _ = state.open();
        let _ = state.on_response(RequestKind::List, Ok(json!({ "terminals": [] })));
        let _ = state.on_response(
            RequestKind::Environment,
            Ok(json!({
                "cwd": "/work",
                "maxInputBytes": 1024,
                "maxBufferedBytes": 4096,
                "maxCols": 120,
                "maxRows": 40,
                "scrollback": 100
            })),
        );
        let _ = state.on_response(
            RequestKind::Shells,
            Ok(json!({ "shells": [{ "path": "/bin/zsh", "args": ["-i"], "name": "zsh" }] })),
        );
        let _ = state.on_response(
            RequestKind::Create,
            Ok(json!({
                "terminal": {
                    "id": "term-1",
                    "title": "zsh",
                    "shell": { "path": "/bin/zsh", "args": ["-i"], "name": "zsh" },
                    "cwd": "/work",
                    "cols": 80,
                    "rows": 24,
                    "state": "running",
                    "exitCode": null
                }
            })),
        );
        let _ = state.on_response(RequestKind::Retain, Ok(json!({ "holdId": "hold-1" })));
        let _ = state.on_response(
            RequestKind::Attach,
            Ok(json!({
                "attachmentId": "att-1",
                "terminalId": "term-1",
                "sequence": 0,
                "writable": true,
                "info": {
                    "id": "term-1",
                    "title": "zsh",
                    "shell": { "path": "/bin/zsh", "args": ["-i"], "name": "zsh" },
                    "cwd": "/work",
                    "cols": 80,
                    "rows": 24,
                    "state": "running",
                    "exitCode": null
                },
                "snapshot": ""
            })),
        );
        for sequence in 1..=40 {
            state.on_notification(
                "nano.terminal.output",
                &json!({
                    "attachmentId": "att-1",
                    "terminalId": "term-1",
                    "sequence": sequence,
                    "data": format!("line {sequence}\r\n")
                }),
            );
        }
        state
    }

    #[test]
    fn wheel_scrolls_the_parser_scrollback_and_clamps_at_the_bottom() {
        let mut state = connected_with_history();
        assert_eq!(state.parser.as_ref().unwrap().screen().scrollback(), 0);
        let area = Rect::new(0, 0, 80, 24);
        let wheel = MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        state.handle_mouse(wheel, area);
        state.tick();
        assert_eq!(state.parser.as_ref().unwrap().screen().scrollback(), 1);
        let down = MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        state.handle_mouse(down, area);
        state.tick();
        assert_eq!(state.parser.as_ref().unwrap().screen().scrollback(), 0);
    }
}

#[cfg(test)]
mod close_reconcile_tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn pending_state() -> TerminalState {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "nano-terminal-close-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        let mut store = TerminalStore::open(&root, "session-test").unwrap();
        store.set_pending("term-orphan").unwrap();
        TerminalState::new(store, (80, 24))
    }

    fn info(id: &str) -> Value {
        json!({
            "id": id,
            "title": "zsh",
            "shell": { "path": "/bin/zsh", "args": ["-i"], "name": "zsh" },
            "cwd": "/work",
            "cols": 80,
            "rows": 24,
            "state": "running",
            "exitCode": null
        })
    }

    #[test]
    fn closing_with_a_lost_create_response_queries_and_closes_the_orphan() {
        let mut state = pending_state();
        let commands = state.close_terminal();
        assert!(matches!(
            &commands[0],
            Command::Request {
                kind: RequestKind::List,
                ..
            }
        ));
        let commands = state.on_response(
            RequestKind::List,
            Ok(json!({ "terminals": [info("term-orphan")] })),
        );
        assert!(matches!(
            &commands[0],
            Command::Request {
                kind: RequestKind::Close,
                params,
                ..
            } if params["terminalId"] == "term-orphan"
        ));
        let _ = state.on_response(RequestKind::Close, Ok(json!({ "closed": true })));
        assert_eq!(state.phase(), Phase::Closed);
        assert!(state.store.as_ref().unwrap().pending().is_empty());
    }

    #[test]
    fn closing_an_in_flight_create_sends_close_for_the_pending_id() {
        let mut state = pending_state();
        let _ = state.close_terminal();
        let commands = state.on_response(RequestKind::List, Ok(json!({ "terminals": [] })));
        assert!(matches!(
            &commands[0],
            Command::Request {
                kind: RequestKind::Close,
                params,
                ..
            } if params["terminalId"] == "term-orphan"
        ));
        assert_eq!(state.phase(), Phase::Closing);
        assert_eq!(state.store.as_ref().unwrap().pending().len(), 1);
    }

    #[test]
    fn an_in_flight_create_retries_with_the_same_id() {
        let mut state = pending_state();
        let _ = state.open();
        let commands = state.on_response(RequestKind::List, Ok(json!({ "terminals": [] })));
        assert!(matches!(
            &commands[0],
            Command::Request {
                kind: RequestKind::Environment,
                ..
            }
        ));
        let _ = state.on_response(
            RequestKind::Environment,
            Ok(json!({
                "cwd": "/work",
                "maxInputBytes": 1024,
                "maxBufferedBytes": 4096,
                "maxCols": 120,
                "maxRows": 40,
                "scrollback": 100
            })),
        );
        let commands = state.on_response(
            RequestKind::Shells,
            Ok(json!({ "shells": [{ "path": "/bin/zsh", "args": ["-i"], "name": "zsh" }] })),
        );
        assert!(matches!(
            &commands[0],
            Command::Request {
                kind: RequestKind::Create,
                params,
                ..
            } if params["terminalId"] == "term-orphan"
        ));
    }
}

#[cfg(test)]
mod fix_tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn store() -> TerminalStore {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "nano-terminal-fix-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        TerminalStore::open(&root, "session-test").unwrap()
    }

    fn info() -> Value {
        json!({
            "id": "term-1",
            "title": "zsh",
            "shell": { "path": "/bin/zsh", "args": ["-i"], "name": "zsh" },
            "cwd": "/work",
            "cols": 80,
            "rows": 24,
            "state": "running",
            "exitCode": null
        })
    }

    fn connected() -> TerminalState {
        let mut state = TerminalState::new(store(), (80, 24));
        state.set_root_session_id("session-test");
        let _ = state.open();
        let _ = state.on_response(RequestKind::List, Ok(json!({ "terminals": [] })));
        let _ = state.on_response(
            RequestKind::Environment,
            Ok(json!({
                "cwd": "/work",
                "maxInputBytes": 1024,
                "maxBufferedBytes": 4096,
                "maxCols": 120,
                "maxRows": 40,
                "scrollback": 100
            })),
        );
        let _ = state.on_response(
            RequestKind::Shells,
            Ok(json!({ "shells": [{ "path": "/bin/zsh", "args": ["-i"], "name": "zsh" }] })),
        );
        let _ = state.on_response(RequestKind::Create, Ok(json!({ "terminal": info() })));
        let _ = state.on_response(RequestKind::Retain, Ok(json!({ "holdId": "hold-1" })));
        let _ = state.on_response(
            RequestKind::Attach,
            Ok(json!({
                "attachmentId": "att-1",
                "terminalId": "term-1",
                "sequence": 0,
                "writable": true,
                "info": info(),
                "snapshot": ""
            })),
        );
        state
    }

    fn feed_history(state: &mut TerminalState) {
        for sequence in 1..=40 {
            let _ = state.on_notification(
                "nano.terminal.output",
                &json!({
                    "attachmentId": "att-1",
                    "terminalId": "term-1",
                    "sequence": sequence,
                    "data": format!("line {sequence}\r\n")
                }),
            );
        }
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16, modifiers: KeyModifiers) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers,
        }
    }

    #[test]
    fn both_initial_and_reattach_attach_requests_include_the_session_id() {
        let mut connected = connected();
        let commands = connected.reattach();
        assert!(matches!(
            &commands[0],
            Command::Request {
                kind: RequestKind::Attach,
                params,
                ..
            } if params["sessionId"] == "session-test"
        ));

        let mut fresh = TerminalState::new(store(), (80, 24));
        fresh.set_root_session_id("session-test");
        let _ = fresh.open();
        let _ = fresh.on_response(RequestKind::List, Ok(json!({ "terminals": [] })));
        let _ = fresh.on_response(
            RequestKind::Environment,
            Ok(json!({
                "cwd": "/work",
                "maxInputBytes": 1024,
                "maxBufferedBytes": 4096,
                "maxCols": 120,
                "maxRows": 40,
                "scrollback": 100
            })),
        );
        let _ = fresh.on_response(
            RequestKind::Shells,
            Ok(json!({ "shells": [{ "path": "/bin/zsh", "args": ["-i"], "name": "zsh" }] })),
        );
        let _ = fresh.on_response(RequestKind::Create, Ok(json!({ "terminal": info() })));
        let commands = fresh.on_response(RequestKind::Retain, Ok(json!({ "holdId": "hold-1" })));
        assert!(matches!(
            &commands[0],
            Command::Request {
                kind: RequestKind::Attach,
                params,
                ..
            } if params["sessionId"] == "session-test"
        ));
    }

    #[test]
    fn a_click_clears_the_selection_so_the_wheel_scrolls_again() {
        let mut state = connected();
        feed_history(&mut state);
        let area = Rect::new(0, 0, 80, 24);
        state.handle_mouse(
            mouse(
                MouseEventKind::Down(crossterm::event::MouseButton::Left),
                2,
                2,
                KeyModifiers::NONE,
            ),
            area,
        );
        state.handle_mouse(
            mouse(
                MouseEventKind::Up(crossterm::event::MouseButton::Left),
                2,
                2,
                KeyModifiers::NONE,
            ),
            area,
        );
        state.handle_mouse(
            mouse(MouseEventKind::ScrollUp, 2, 2, KeyModifiers::NONE),
            area,
        );
        state.tick();
        assert_eq!(state.parser.as_ref().unwrap().screen().scrollback(), 1);
    }

    #[test]
    fn shift_bypasses_program_mouse_reporting_for_selection() {
        let mut state = connected();
        state.on_notification(
            "nano.terminal.output",
            &json!({
                "attachmentId": "att-1",
                "terminalId": "term-1",
                "sequence": 1,
                "data": "\u{1b}[?1000h\u{1b}[?1006h"
            }),
        );
        let area = Rect::new(0, 0, 80, 24);
        let plain = state.handle_mouse(
            mouse(
                MouseEventKind::Down(crossterm::event::MouseButton::Left),
                2,
                2,
                KeyModifiers::NONE,
            ),
            area,
        );
        assert!(
            plain
                .iter()
                .any(|command| matches!(command, Command::Notify { .. })),
            "plain mouse events go to the program"
        );
        let shifted = state.handle_mouse(
            mouse(
                MouseEventKind::Down(crossterm::event::MouseButton::Left),
                3,
                3,
                KeyModifiers::SHIFT,
            ),
            area,
        );
        assert!(
            shifted
                .iter()
                .all(|command| !matches!(command, Command::Notify { .. })),
            "Shift starts a local selection instead"
        );
    }

    #[test]
    fn choose_shell_is_refused_while_a_create_is_in_flight() {
        let mut state = TerminalState::new(store(), (80, 24));
        let _ = state.open();
        let _ = state.on_response(RequestKind::List, Ok(json!({ "terminals": [] })));
        let _ = state.on_response(
            RequestKind::Environment,
            Ok(json!({
                "cwd": "/work",
                "maxInputBytes": 1024,
                "maxBufferedBytes": 4096,
                "maxCols": 120,
                "maxRows": 40,
                "scrollback": 100
            })),
        );
        let _ = state.on_response(
            RequestKind::Shells,
            Ok(json!({ "shells": [{ "path": "/bin/zsh", "args": ["-i"], "name": "zsh" }] })),
        );
        let commands = state.choose_shell();
        assert!(matches!(commands[0], Command::Status { error: false, .. }));
        assert!(state.shell_picker.is_none());
    }

    #[test]
    fn the_picker_renders_without_a_parser_and_clamps_to_narrow_areas() {
        let mut state = TerminalState::new(store(), (80, 24));
        state.shells = vec![TerminalShell {
            path: "/bin/zsh".into(),
            args: vec!["-i".into()],
            name: "zsh".into(),
        }];
        state.shell_picker = Some(ShellPicker { selected: 0 });
        let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
        terminal
            .draw(|frame| state.draw(frame, frame.area(), Theme::Dark))
            .expect("picker draw");
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(
            text.contains("zsh"),
            "picker list must be visible: {text:?}"
        );

        let mut narrow = Terminal::new(TestBackend::new(15, 6)).unwrap();
        narrow
            .draw(|frame| state.draw(frame, frame.area(), Theme::Dark))
            .expect("narrow picker draw must not panic");
    }

    #[test]
    fn default_foreground_follows_theme_and_explicit_colors_survive() {
        let mut state = connected();
        let _ = state.on_notification(
            "nano.terminal.output",
            &json!({
                "attachmentId": "att-1",
                "terminalId": "term-1",
                "sequence": 1,
                "data": "plain \u{1b}[31mred\u{1b}[0m"
            }),
        );
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let backend = TestBackend::new(20, 4);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| state.draw(frame, frame.area(), Theme::Light))
            .expect("draw terminal");
        let buffer = terminal.backend().buffer();
        let cells = buffer.content();
        let plain = cells
            .iter()
            .find(|cell| cell.symbol() == "p")
            .expect("plain cell");
        assert_eq!(plain.fg, Theme::Light.colors().fg);
        assert_eq!(plain.bg, Color::Reset);
        let red = cells
            .iter()
            .find(|cell| cell.symbol() == "r")
            .expect("explicit red cell");
        assert_eq!(red.fg, Color::Indexed(1));
    }

    #[test]
    fn a_disconnect_ignores_late_terminal_results() {
        let mut state = connected();
        let stale_epoch = state.epoch();
        let _ = state.disconnect("host gone");
        assert_eq!(state.phase(), Phase::Disconnected);
        let _ = state.on_response_epoch(
            stale_epoch,
            RequestKind::Attach,
            Ok(json!({
                "attachmentId": "att-late",
                "terminalId": "term-1",
                "sequence": 9,
                "writable": true,
                "info": info(),
                "snapshot": ""
            })),
        );
        assert_eq!(state.phase(), Phase::Disconnected);
        let _ = state.on_response_epoch(
            state.epoch(),
            RequestKind::Close,
            Err(Failure::definite("closed after the transport ended")),
        );
        assert_eq!(state.phase(), Phase::Disconnected);
    }

    #[test]
    fn an_attach_failure_keeps_the_hold_for_a_retry() {
        let mut state = connected();
        let _ = state.on_response(
            RequestKind::Attach,
            Err(Failure::definite("transient attach failure")),
        );
        assert_eq!(state.phase(), Phase::Failed);
        assert_eq!(state.hold_id.as_deref(), Some("hold-1"));
    }

    #[test]
    fn a_successful_close_clears_a_stale_error() {
        let mut state = connected();
        state.error = Some("inputFull: stale".to_string());
        let commands = state.on_response(RequestKind::Close, Ok(json!({ "closed": true })));
        assert_eq!(state.phase(), Phase::Closed);
        assert!(state.error.is_none());
        assert!(commands
            .iter()
            .any(|command| matches!(command, Command::Status { error: false, .. })));
        assert!(!state.status_line().contains("inputFull: stale"));
    }

    #[test]
    fn an_exited_process_with_an_open_attachment_reports_output_drained() {
        let mut state = connected();
        let mut exited = info();
        exited["state"] = json!("exited");
        exited["exitCode"] = json!(0);
        state.info = Some(serde_json::from_value(exited).unwrap());
        state.writable = false;
        let status = state.status_line();
        assert!(status.contains("进程已退出"), "{status}");
        assert!(status.contains("输出已收完"), "{status}");
        assert!(!status.contains("输出未确认排空"), "{status}");
    }

    #[test]
    fn scrolled_history_is_labelled_as_bounded() {
        let mut state = connected();
        feed_history(&mut state);
        let area = Rect::new(0, 0, 80, 24);
        state.handle_mouse(
            mouse(MouseEventKind::ScrollUp, 2, 2, KeyModifiers::NONE),
            area,
        );
        state.tick();
        assert!(state.status_line().contains("受 scrollback 限制"));
    }
}

#[cfg(test)]
mod picker_tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn store() -> TerminalStore {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "nano-terminal-picker-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        TerminalStore::open(&root, "session-test").unwrap()
    }

    #[test]
    fn f9_after_close_opens_the_picker() {
        let mut state = TerminalState::new(store(), (80, 24));
        state.set_root_session_id("session-test");
        state.shells = vec![TerminalShell {
            path: "/bin/zsh".into(),
            args: vec!["-i".into()],
            name: "zsh".into(),
        }];
        state.phase = Phase::Closed;
        let key = KeyEvent::new(KeyCode::F(9), KeyModifiers::NONE);
        let commands = state.handle_key(key);
        assert!(state.shell_picker.is_some(), "commands: {commands:?}");
    }
}
