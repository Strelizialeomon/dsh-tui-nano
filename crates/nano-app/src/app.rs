//! Application state and key handling.

use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use nano_conversation::{
    CommandRouter, ConversationModel, Delivery, DraftStore, FeatureContribution, FeatureRegistry,
    FocusId, InputBuffer, InputEvent, InputOutcome, InputRoute, OrderedContent, SceneId,
    SceneState, SessionAddress, SubmitError,
};
use nano_runtime::{Hello, SessionMode};
use nano_sessions::{
    CompactionOutcome, ContextMeter, RenameOutcome, SessionSummary, SwitchMachine, SwitchTarget,
    TargetMode,
};
use serde_json::{json, Value};

use crate::cli::Cli;
use crate::link::{HostLink, LinkEvent, RequestError, RequestErrorKind};

/// Official `/compact` busy sentence from `dsh-command-compact 0.2.1-rc.2`.
/// Shown when this window already has a compaction wait in flight, so the
/// second request is not queued behind the first on the long-request worker.
const COMPACTION_BUSY_TEXT: &str = "Compaction is unavailable because this process has an active compaction, or the agent is not idle.";

/// Connection lifecycle as the UI may claim it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Connection {
    /// Spawning and handshaking.
    Starting,
    /// Handshake and subscription succeeded.
    Ready,
    /// The transport ended; no automatic reconnect.
    Disconnected,
    /// Startup or protocol failure.
    Failed(String),
}

impl Connection {
    /// Status-line label.
    pub fn label(&self) -> &str {
        match self {
            Self::Starting => "启动中",
            Self::Ready => "已连接",
            Self::Disconnected => "已断开",
            Self::Failed(_) => "失败",
        }
    }
}

/// A request awaiting its response.
#[derive(Debug, Clone)]
pub enum Pending {
    /// A conversation submission.
    Submit {
        /// Business identity.
        request_id: String,
        /// Text snapshot, used to restore the draft on explicit failure.
        text: String,
    },
    /// An edit of one queued item.
    Edit {
        /// Official message id.
        message_id: String,
    },
    /// A queue removal.
    Delete {
        /// Official message id.
        message_id: String,
    },
    /// A queued item promoted to steering.
    Steer {
        /// Official message id.
        message_id: String,
    },
    /// Stop the active turn.
    Cancel,
    /// Read the absolute baseline.
    Snapshot,
    /// Rebuild the observable subscription after `streamEnded failed`.
    Resubscribe,
    /// One item of a clear-and-stop batch.
    ClearStopDelete {
        /// Batch generation; stale responses are ignored.
        generation: u64,
        /// Official message id being deleted.
        message_id: String,
    },
    /// Read the official session list.
    SessionsList,
    /// Rename the active root session.
    Rename {
        /// Official session identity being renamed.
        session_id: String,
        /// Submitted title, kept to restore the editor on failure.
        title: String,
    },
    /// Read the context-occupancy projection.
    Context,
    /// The official `/compact` command is running.
    Compact,
    /// Cancel the running `/compact` wait.
    CompactCancel,
}

/// Inline rename editor inside the session panel.
#[derive(Debug, Clone)]
pub struct RenameInput {
    /// Official session identity; only the active root is accepted by the host.
    pub session_id: String,
    /// Editable title buffer.
    pub buffer: InputBuffer,
}

/// Session panel state. Rows are official summaries only.
#[derive(Debug, Clone, Default)]
pub struct SessionPanel {
    /// Whether the panel owns the keyboard.
    pub open: bool,
    /// A list request is in flight.
    pub loading: bool,
    /// Official rows, newest first.
    pub rows: Vec<SessionSummary>,
    /// Selected row; index 0 is the "new session" row, rows start at 1.
    pub selected: usize,
    /// Last panel-level error.
    pub error: Option<String>,
    /// Active rename editor.
    pub rename: Option<RenameInput>,
}

impl SessionPanel {
    /// Close the panel and drop transient state. The rows are cleared so a
    /// stale `current` marker cannot survive a switch or a failed refresh.
    pub fn close(&mut self) {
        self.open = false;
        self.loading = false;
        self.rows.clear();
        self.selected = 0;
        self.rename = None;
        self.error = None;
    }

    /// Selected official row, when the cursor is not on "new session".
    pub fn selected_row(&self) -> Option<&SessionSummary> {
        self.selected
            .checked_sub(1)
            .and_then(|index| self.rows.get(index))
    }

    /// Keep the selection inside the list.
    pub fn clamp(&mut self) {
        let max = self.rows.len();
        if self.selected > max {
            self.selected = max;
        }
    }
}

/// One frame of application state.
pub struct App {
    /// Launch options.
    pub cli: Cli,
    /// Conversation state.
    pub model: ConversationModel,
    /// Per-address drafts.
    pub drafts: DraftStore,
    /// Static feature registry.
    pub registry: FeatureRegistry,
    /// Main scene and focus.
    pub scene: SceneState,
    /// Background host link, when startup succeeded.
    pub link: Option<HostLink>,
    /// Connection fact.
    pub connection: Connection,
    /// Whether requests may be sent.
    pub ready: bool,
    /// Last error shown in the status bar.
    pub error: Option<String>,
    /// Last short notice.
    pub notice: Option<String>,
    /// Selected queue index.
    pub queue_selected: usize,
    /// Queue item being edited in the input box.
    pub edit_target: Option<String>,
    /// Draft that was displaced by entering queue edit mode.
    pub edit_backup: Option<InputBuffer>,
    /// Request id to text snapshot for failure restoration.
    pub submit_backups: HashMap<String, String>,
    /// In-flight requests.
    pub pending: HashMap<u64, Pending>,
    /// Feed lines scrolled back from the bottom.
    pub scroll_back: usize,
    /// Last measured feed viewport height.
    pub feed_height: usize,
    /// Quit after the current input pass.
    pub should_quit: bool,
    /// Handshake facts, once ready.
    pub hello: Option<Hello>,
    /// Whether the replay window omitted older durable events.
    pub replay_truncated: bool,
    /// A stop was admitted but the official idle fact has not arrived yet.
    pub stop_pending: bool,
    /// When to rebuild a closed subscription.
    pub resubscribe_at: Option<Instant>,
    /// A resubscribe request is in flight.
    pub resubscribing: bool,
    /// Consecutive rebuild attempts without a durable event.
    pub resubscribe_attempts: usize,
    /// Number of queued deletes still pending in a clear-and-stop batch.
    pub clear_stop_remaining: usize,
    /// Whether the clear-and-stop batch cancels the active turn after deleting.
    pub clear_stop_cancel: bool,
    /// Batch generation for clear-and-stop responses.
    pub clear_stop_generation: u64,
    /// A delete in the current batch failed.
    pub clear_stop_failed: bool,
    /// After a truncated snapshot, run clear-and-stop on the full inbox.
    pub clear_after_snapshot: bool,
    /// After a truncated queue projection, reopen this item's editor once a
    /// full snapshot arrives.
    pub edit_after_snapshot: Option<String>,
    /// Explicit main-session switch phase.
    pub switch: SwitchMachine,
    /// Latest official context-occupancy reference.
    pub context: Option<ContextMeter>,
    /// Why the last context read failed, when it did.
    pub context_error: Option<String>,
    /// Official session list panel.
    pub panel: SessionPanel,
    /// Restart worker events, while a controlled restart owns the old link.
    pub restart_rx: Option<Receiver<LinkEvent>>,
    /// Latest settled manual-compaction outcome.
    pub compact: Option<CompactionOutcome>,
    /// A `/compact` request is in flight.
    pub compact_in_flight: bool,
    /// Local id of the in-flight `/compact` request, used to stop waiting.
    pub compact_request_id: Option<u64>,
    /// A compaction may still be running after a timeout; result unknown.
    pub compact_maybe_running: bool,
}

impl App {
    /// Create the app before the host is ready.
    pub fn new(cli: Cli, root_session_id: String, replay_truncated: bool) -> Self {
        let address = SessionAddress::root(root_session_id);
        let mut registry = FeatureRegistry::new();
        registry.register(
            FeatureContribution::new("conversation", SceneId::conversation()).with_key("enter"),
        );
        let mut scene = SceneState::conversation();
        scene.set_address(address.clone());
        Self {
            cli,
            model: ConversationModel::new(address.clone()),
            drafts: DraftStore::new(address),
            registry,
            scene,
            link: None,
            connection: Connection::Starting,
            ready: false,
            error: None,
            notice: Some("正在启动 Nano 宿主…".to_string()),
            queue_selected: 0,
            edit_target: None,
            edit_backup: None,
            submit_backups: HashMap::new(),
            pending: HashMap::new(),
            scroll_back: 0,
            feed_height: 10,
            should_quit: false,
            hello: None,
            replay_truncated,
            stop_pending: false,
            resubscribe_at: None,
            resubscribing: false,
            resubscribe_attempts: 0,
            clear_stop_remaining: 0,
            clear_stop_cancel: false,
            clear_stop_generation: 0,
            clear_stop_failed: false,
            clear_after_snapshot: false,
            edit_after_snapshot: None,
            switch: SwitchMachine::new(),
            context: None,
            context_error: None,
            panel: SessionPanel::default(),
            restart_rx: None,
            compact: None,
            compact_in_flight: false,
            compact_request_id: None,
            compact_maybe_running: false,
        }
    }

    /// Attach the host link.
    pub fn attach_link(&mut self, link: HostLink) {
        self.link = Some(link);
    }

    /// Apply one key. Returns whether the screen needs a redraw.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return false;
        }
        self.clear_transient_status();
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('q'))
        {
            self.should_quit = true;
            return true;
        }
        // After a failed switch this window has no host. The retry and back
        // entries stay reachable even though no session is connected.
        if self.switch.state().failed() {
            if key.modifiers.contains(KeyModifiers::ALT) {
                match key.code {
                    KeyCode::Char('r') | KeyCode::Char('R') => {
                        self.retry_switch();
                        return true;
                    }
                    KeyCode::Char('b') | KeyCode::Char('B') => {
                        self.back_after_failure();
                        return true;
                    }
                    _ => {}
                }
            }
            return self.handle_failed_switch_key(key);
        }
        if self.switch.busy() {
            // A switch owns the host lifecycle; new work waits for it.
            self.notice = Some(self.switch.state().label());
            return false;
        }
        if self.panel.open {
            return self.handle_panel_key(key);
        }
        if key.modifiers.contains(KeyModifiers::ALT) {
            match key.code {
                KeyCode::Char('o') | KeyCode::Char('O') => {
                    self.open_session_panel();
                    return true;
                }
                KeyCode::Char('c') | KeyCode::Char('C') => {
                    self.request_compact();
                    return true;
                }
                _ => {}
            }
        }
        if matches!(key.code, KeyCode::PageUp) {
            self.scroll_back = self
                .scroll_back
                .saturating_add(self.feed_height.saturating_sub(1).max(1));
            return true;
        }
        if matches!(key.code, KeyCode::PageDown) {
            self.scroll_back = self
                .scroll_back
                .saturating_sub(self.feed_height.saturating_sub(1).max(1));
            return true;
        }
        if matches!(key.code, KeyCode::Tab) {
            self.toggle_focus();
            return true;
        }
        // Only the top focus receives keys. Queue focus must not leak Alt or
        // control shortcuts into the hidden input draft.
        if !self.scene.focus().input_focused() {
            return self.handle_queue_key(key);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('u') => {
                    self.drafts.current_mut().clear();
                    return true;
                }
                KeyCode::Char('j') => {
                    self.drafts.current_mut().handle(&InputEvent::Newline);
                    return true;
                }
                KeyCode::Char('y') => {
                    self.copy_draft();
                    return true;
                }
                KeyCode::End => {
                    self.scroll_back = 0;
                    return true;
                }
                _ => {}
            }
            return false;
        }
        if key.modifiers.contains(KeyModifiers::ALT) {
            match key.code {
                KeyCode::Up => {
                    self.select_queue_previous();
                    return true;
                }
                KeyCode::Down => {
                    self.select_queue_next();
                    return true;
                }
                KeyCode::Char('e') | KeyCode::Char('E') => {
                    self.begin_queue_edit();
                    return true;
                }
                KeyCode::Char('d') | KeyCode::Char('D') => {
                    self.delete_selected_queue();
                    return true;
                }
                KeyCode::Char('s') | KeyCode::Char('S') => {
                    self.steer_selected_queue();
                    return true;
                }
                KeyCode::Char('x') | KeyCode::Char('X') => {
                    self.clear_and_stop();
                    return true;
                }
                KeyCode::Enter => {
                    self.submit_draft(Delivery::Steer);
                    return true;
                }
                _ => {}
            }
        }
        if matches!(key.code, KeyCode::Esc) {
            if self.compact_in_flight || self.compact_maybe_running {
                self.cancel_compact();
            } else if self.edit_target.is_some() {
                self.cancel_queue_edit();
            } else {
                self.stop_turn();
            }
            return true;
        }
        if matches!(key.code, KeyCode::Enter) {
            if key.modifiers.contains(KeyModifiers::SHIFT) {
                self.drafts.current_mut().handle(&InputEvent::Newline);
                return true;
            }
            // The input model owns the IME gate: while composing, Enter is
            // ignored here and belongs to the terminal input method.
            match self.drafts.current_mut().handle(&InputEvent::Enter) {
                InputOutcome::Send => {
                    if self.edit_target.is_some() {
                        self.apply_queue_edit();
                    } else {
                        self.submit_draft(Delivery::Queue);
                    }
                }
                InputOutcome::RejectedEmpty => {
                    self.error = Some("内容为空，草稿已保留".to_string());
                }
                _ => {}
            }
            return true;
        }
        let event = match key.code {
            KeyCode::Backspace => Some(InputEvent::Backspace),
            KeyCode::Delete => Some(InputEvent::Delete),
            KeyCode::Left => Some(InputEvent::Left),
            KeyCode::Right => Some(InputEvent::Right),
            KeyCode::Up => Some(InputEvent::Up),
            KeyCode::Down => Some(InputEvent::Down),
            KeyCode::Home => Some(InputEvent::Home),
            KeyCode::End => Some(InputEvent::End),
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(InputEvent::Insert(character.to_string()))
            }
            _ => None,
        };
        if let Some(event) = event {
            let outcome = self.drafts.current_mut().handle(&event);
            if outcome == InputOutcome::RejectedEmpty {
                self.error = Some("内容为空，草稿已保留".to_string());
            }
            return outcome != InputOutcome::Ignored;
        }
        false
    }

    /// Insert a bracketed-paste payload exactly as received.
    pub fn handle_paste(&mut self, text: String) -> bool {
        if !self.scene.focus().input_focused() {
            return false;
        }
        self.clear_transient_status();
        let outcome = self.drafts.current_mut().handle(&InputEvent::Paste(text));
        outcome != InputOutcome::Ignored
    }

    /// Apply one link event. Returns whether the screen needs a redraw.
    pub fn handle_link(&mut self, event: LinkEvent) -> bool {
        match event {
            LinkEvent::Ready { hello, truncated } => {
                let expected = self.model.address().root_session_id.clone();
                if hello.root_session_id != expected {
                    let message = format!(
                        "宿主返回了错误的会话：期望 {expected}，收到 {}",
                        hello.root_session_id
                    );
                    self.link = None;
                    self.fail_switch(None, None, message);
                    return true;
                }
                self.ready = true;
                self.connection = Connection::Ready;
                self.replay_truncated = self.replay_truncated || truncated;
                self.model
                    .apply_replay_baseline(hello.session.seq, truncated);
                self.hello = Some(hello);
                if self.switch.busy() {
                    if let Some(target) = self.switch.succeed() {
                        self.notice = Some(format!("已切换到会话 {}", target.label()));
                    }
                } else {
                    self.notice = Some("已连接官方会话".to_string());
                }
                self.refresh_snapshot();
                self.refresh_context();
                true
            }
            LinkEvent::Host(event) => self.handle_host_event(event),
            LinkEvent::RequestDone { id, result } => {
                self.handle_request_done(id, result);
                true
            }
            LinkEvent::Fatal(message) => {
                if self.switch.busy() {
                    self.link = None;
                    self.fail_switch(None, None, message);
                    return true;
                }
                self.ready = false;
                self.connection = Connection::Failed(message.clone());
                let tail = self
                    .link
                    .as_ref()
                    .map(|link| link.diagnostics(5))
                    .unwrap_or_default();
                self.error = Some(if tail.is_empty() {
                    message
                } else {
                    format!("{message}；宿主诊断：{}", tail.join(" | "))
                });
                self.model.mark_disconnected();
                true
            }
            LinkEvent::StartupFailed {
                code,
                message,
                holder_pid,
                repair_path,
            } => {
                let message = match repair_path {
                    Some(path) => format!("{message}；修复路径：{path}"),
                    None => message,
                };
                self.ready = false;
                self.connection = Connection::Failed(message.clone());
                if self.switch.busy() {
                    self.link = None;
                    self.fail_switch(Some(code), holder_pid, message);
                } else {
                    self.error = Some(message);
                    self.model.mark_disconnected();
                }
                true
            }
            LinkEvent::Switched { link } => {
                self.link = Some(link);
                self.restart_rx = None;
                self.switch.host_started();
                self.connection = Connection::Starting;
                self.ready = false;
                self.notice = Some("旧宿主已退出；正在启动新宿主…".to_string());
                true
            }
            LinkEvent::SwitchFailed {
                message,
                code,
                holder_pid,
            } => {
                self.link = None;
                self.restart_rx = None;
                self.ready = false;
                self.connection = Connection::Failed(message.clone());
                self.fail_switch(code, holder_pid, message);
                true
            }
        }
    }

    /// Enter the explicit "no usable host" state after a failed switch.
    fn fail_switch(&mut self, code: Option<String>, holder_pid: Option<u32>, message: String) {
        self.switch.fail(code, holder_pid, message.clone());
        let label = self.switch.state().label();
        self.error = Some(format!("{label}：{message}；当前没有可用宿主"));
        self.model.mark_disconnected();
    }

    fn handle_host_event(&mut self, event: nano_runtime::HostEvent) -> bool {
        use nano_runtime::HostEvent;
        match event {
            HostEvent::Event(params) => {
                if params.kind == "sessions.context" {
                    return self.apply_context_value(&params.payload);
                }
                if params.kind == "session.event" {
                    self.resubscribe_attempts = 0;
                }
                let changed = self.model.apply_event(&params.kind, &params.payload);
                if self.stop_pending
                    && params.kind == "conversation.status"
                    && params.payload.get("status").and_then(Value::as_str) == Some("idle")
                {
                    self.stop_pending = false;
                    self.notice = Some("官方已确认停止".to_string());
                }
                if self.model.inbox().is_empty() && !self.scene.focus().input_focused() {
                    self.scene.focus_mut().pop();
                }
                if changed {
                    self.clamp_queue_selection();
                }
                changed
            }
            HostEvent::StreamEnded(params) => {
                if params.outcome == nano_runtime::protocol::StreamOutcome::Failed {
                    if self.ready && self.resubscribe_attempts < 2 {
                        // The host is alive; only the observable subscription
                        // was closed. Rebuild it from the last applied seq.
                        self.notice = Some(format!(
                            "订阅被关闭（{}）；正在重建",
                            params.reason.unwrap_or_else(|| "慢消费者".to_string())
                        ));
                        self.resubscribe_at = Some(Instant::now() + Duration::from_millis(500));
                        self.model.drop_previews();
                    } else {
                        let root = &self.model.address().root_session_id;
                        self.ready = false;
                        self.connection = Connection::Disconnected;
                        self.error = Some(format!(
                            "订阅反复被关闭（可能含超过 S01 事件帧上限的持久事件）；会话 {root}，重启时用 --resume {root}"
                        ));
                        self.model.mark_disconnected();
                    }
                }
                true
            }
            HostEvent::TransportClosed { lost } => {
                self.ready = false;
                self.connection = Connection::Disconnected;
                self.stop_pending = false;
                self.resubscribe_at = None;
                self.resubscribing = false;
                let root = &self.model.address().root_session_id;
                self.error = Some(if lost {
                    format!("宿主进程已退出（会话 {root}）；重启时用 --resume {root}")
                } else {
                    format!("通信已断开（会话 {root}）")
                });
                self.model.mark_disconnected();
                true
            }
            HostEvent::ProtocolFailure(message) => {
                self.ready = false;
                self.connection = Connection::Failed(message.clone());
                self.error = Some(format!("协议错误：{message}"));
                true
            }
            HostEvent::StartupError(params) => {
                self.ready = false;
                self.connection = Connection::Failed(params.message.clone());
                self.error = Some(params.message);
                true
            }
            HostEvent::Hello(_) => false,
        }
    }

    fn handle_request_done(&mut self, id: u64, result: Result<Value, RequestError>) {
        let Some(pending) = self.pending.remove(&id) else {
            return;
        };
        if !self.ready
            && matches!(
                pending,
                Pending::Resubscribe
                    | Pending::ClearStopDelete { .. }
                    | Pending::Snapshot
                    | Pending::Cancel
            )
        {
            self.resubscribing = false;
            self.clear_stop_remaining = 0;
            return;
        }
        match pending {
            Pending::Submit { request_id, text } => match result {
                Ok(value) => {
                    let accepted = value
                        .get("accepted")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    let duplicate = value
                        .get("duplicate")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    let message_id = value
                        .get("messageId")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    if accepted {
                        self.model
                            .submit_accepted(&request_id, &message_id, duplicate);
                        self.submit_backups.remove(&request_id);
                    } else {
                        self.model.submit_failed(&request_id, "官方未接受");
                        self.restore_draft(&request_id, &text);
                    }
                }
                Err(error)
                    if matches!(error.kind, RequestErrorKind::Rpc | RequestErrorKind::Local) =>
                {
                    self.model.submit_failed(&request_id, error.message.clone());
                    self.restore_draft(&request_id, &text);
                    self.error = Some(format!("发送失败：{}", error.message));
                }
                Err(error) => {
                    // A transport failure may have delivered the prompt. Never
                    // claim failure or restore the draft for an unknown result.
                    self.submit_backups.remove(&request_id);
                    self.model.submit_unknown(&request_id, error.message);
                }
            },
            Pending::Edit { message_id } => match result {
                Ok(_) => {
                    self.edit_target = None;
                    if let Some(backup) = self.edit_backup.take() {
                        *self.drafts.current_mut() = backup;
                    } else {
                        self.drafts.current_mut().take();
                    }
                    self.scene.focus_mut().reset(FocusId::input());
                    self.notice = Some(format!("队列项 {message_id} 已更新"));
                }
                Err(error) => {
                    self.error = Some(format!("编辑失败：{}", error.message));
                    if error.message.contains("queue-item-not-found") {
                        self.refresh_snapshot();
                    }
                }
            },
            Pending::Delete { message_id } => match result {
                Ok(_) => {
                    self.model.cancel_submission(&message_id);
                    self.notice = Some(format!("队列项 {message_id} 已删除（持久取消）"));
                }
                Err(error) => {
                    self.error = Some(format!("删除失败：{}", error.message));
                    self.refresh_snapshot();
                }
            },
            Pending::Steer { message_id } => match result {
                Ok(_) => {
                    self.notice = Some(format!("队列项 {message_id} 已转为插话（best-effort）"));
                }
                Err(error) => {
                    self.error = Some(format!("插话失败：{}", error.message));
                    self.refresh_snapshot();
                }
            },
            Pending::Cancel => match result {
                Ok(_) => {
                    self.notice = Some("已请求停止；等待官方 idle 事实".to_string());
                }
                Err(error) => {
                    self.stop_pending = false;
                    self.error = Some(format!("停止失败：{}", error.message));
                }
            },
            Pending::Snapshot => match result {
                Ok(value) => match self.model.apply_snapshot(&value) {
                    Ok(()) => {
                        self.model.reconcile();
                        self.clamp_queue_selection();
                        if self.model.status() == nano_conversation::ConversationStatus::Idle {
                            self.stop_pending = false;
                        }
                        if self.edit_after_snapshot.take().is_some() {
                            self.begin_queue_edit();
                        }
                        if self.clear_after_snapshot {
                            self.clear_after_snapshot = false;
                            self.clear_and_stop();
                        }
                    }
                    Err(message) => {
                        self.edit_after_snapshot = None;
                        self.clear_after_snapshot = false;
                        self.error = Some(format!("快照无效：{message}"));
                    }
                },
                Err(error) => {
                    self.edit_after_snapshot = None;
                    self.clear_after_snapshot = false;
                    self.error = Some(format!("读取会话状态失败：{}", error.message));
                }
            },
            Pending::Resubscribe => {
                self.resubscribing = false;
                match result {
                    Ok(_) => {
                        self.notice = Some("订阅已重建；正在对齐官方 baseline".to_string());
                        self.refresh_snapshot();
                        self.refresh_context();
                    }
                    Err(error) => {
                        let root = self.model.address().root_session_id.clone();
                        self.ready = false;
                        self.connection = Connection::Disconnected;
                        self.error = Some(format!(
                            "订阅重建失败：{}；会话 {root}，重启时用 --resume {root}",
                            error.message
                        ));
                        self.model.mark_disconnected();
                    }
                }
            }
            Pending::ClearStopDelete {
                generation,
                message_id,
            } => {
                if generation != self.clear_stop_generation {
                    return;
                }
                match result {
                    Ok(_) => self.model.cancel_submission(&message_id),
                    Err(error) => {
                        self.clear_stop_failed = true;
                        if error.message.contains("queue-item-not-found") {
                            self.refresh_snapshot();
                        }
                    }
                }
                self.clear_stop_remaining = self.clear_stop_remaining.saturating_sub(1);
                if self.clear_stop_remaining == 0 {
                    self.finish_clear_stop();
                }
            }
            Pending::SessionsList => {
                self.panel.loading = false;
                match result {
                    Ok(value) => match SessionSummary::parse_result(&value) {
                        Ok(rows) => {
                            self.panel.rows = rows;
                            self.panel.error = None;
                            self.panel.clamp();
                        }
                        Err(error) => {
                            self.panel.error = Some(format!("会话列表无效：{error}"));
                        }
                    },
                    Err(error) => {
                        self.panel.error = Some(format!("读取会话列表失败：{}", error.message));
                    }
                }
            }
            Pending::Rename { session_id, title } => match result {
                Ok(value) => match RenameOutcome::parse(&value) {
                    Ok(outcome) => {
                        if let Some(row) = self
                            .panel
                            .rows
                            .iter_mut()
                            .find(|row| row.session_id == session_id)
                        {
                            row.title = Some(outcome.title.clone());
                        }
                        self.panel.rename = None;
                        self.panel.error = None;
                        self.notice = Some(format!("会话已重命名为「{}」", outcome.title));
                    }
                    Err(error) => {
                        self.panel.error = Some(format!("改名结果无效：{error}"));
                    }
                },
                Err(error)
                    if matches!(error.kind, RequestErrorKind::Rpc | RequestErrorKind::Local) =>
                {
                    let message = format!("改名失败：{}", error.message);
                    if self.panel.open {
                        self.panel.error = Some(message);
                        // Restore the editor only while its panel is still open;
                        // a late response must not revive a closed panel.
                        self.panel.rename = Some(RenameInput {
                            session_id,
                            buffer: InputBuffer::from_text(title),
                        });
                    } else {
                        self.notice = Some(message);
                    }
                }
                Err(error) => {
                    // No receipt is not failure: the title may already have been
                    // accepted and logged. Do not resend; reread the official list.
                    let message = format!(
                        "改名结果未知（{}）；正在重读官方标题，不自动重发",
                        error.message
                    );
                    self.panel.rename = None;
                    if self.panel.open {
                        self.panel.error = Some(message);
                        if self.link.is_some() {
                            self.refresh_sessions();
                        }
                    } else {
                        self.notice = Some(message);
                    }
                }
            },
            Pending::Context => {
                if !self.ready {
                    return;
                }
                match result {
                    Ok(value) => {
                        self.apply_context_value(&value);
                    }
                    Err(error) => {
                        self.context_error = Some(error.message);
                    }
                }
            }
            Pending::Compact => {
                self.compact_in_flight = false;
                self.compact_request_id = None;
                self.compact_maybe_running = false;
                match result {
                    Ok(value) => match CompactionOutcome::parse(&value) {
                        Ok(outcome) => {
                            let text = outcome.display_text();
                            self.compact = Some(outcome.clone());
                            self.notice = Some(text);
                            self.refresh_context();
                            if outcome.history_may_have_changed() {
                                // The official surface may already have changed;
                                // reread the absolute state instead of keeping a
                                // stale display.
                                self.refresh_snapshot();
                            }
                        }
                        Err(error) => {
                            self.error = Some(format!("压缩结果无法解析：{error}"));
                        }
                    },
                    Err(error)
                        if matches!(
                            error.kind,
                            RequestErrorKind::Rpc | RequestErrorKind::Local
                        ) =>
                    {
                        self.error = Some(format!("压缩失败：{}", error.message));
                    }
                    Err(error) => {
                        // The command may still be running; never claim failure
                        // or success, and never retry automatically. Keep the
                        // cancel entry reachable after the wait timed out.
                        self.compact_maybe_running = true;
                        self.notice = Some(format!(
                            "压缩结果未知（{}）；正在重读官方状态，不自动重试",
                            error.message
                        ));
                        self.refresh_context();
                        self.refresh_snapshot();
                    }
                }
            }
            Pending::CompactCancel => match result {
                Ok(value) => {
                    let cancelled = value
                        .get("cancelled")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    if !cancelled {
                        // The wait may already have settled after a timeout.
                        // Keep the local cancellation record and reread the
                        // official state; do not claim success or failure.
                        self.notice = Some("没有正在进行的压缩等待；已重读官方状态".to_string());
                        self.refresh_context();
                        self.refresh_snapshot();
                    }
                }
                Err(error) => {
                    self.notice = Some(format!("取消压缩失败：{}", error.message));
                }
            },
        }
    }

    /// Drop a transient status line once the user acts again. Connection
    /// failures stay visible because they are not transient.
    fn clear_transient_status(&mut self) {
        if matches!(self.connection, Connection::Ready | Connection::Starting) {
            self.error = None;
            self.notice = None;
            self.model.clear_notice();
        }
    }

    fn restore_draft(&mut self, request_id: &str, text: &str) {
        if self.submit_backups.remove(request_id).is_none() {
            return;
        }
        if self.edit_target.is_some() {
            // Do not merge a failed prompt into a queue edit; the queue text is
            // the draft the user is actively editing. Report the failed text
            // instead of silently writing it back.
            self.error = Some(format!(
                "发送失败，原稿未自动合并（正在编辑队列项）：{text}"
            ));
            return;
        }
        let current = self.drafts.current().text().to_string();
        if current.is_empty() {
            self.drafts.current_mut().set_text(text);
        } else if current != text && !current.starts_with(text) {
            // The user typed while the request was in flight. Keep both the
            // failed text and the new draft instead of silently dropping one.
            self.drafts
                .current_mut()
                .set_text(format!("{text}\n\n{current}"));
        }
    }

    /// Route the current input and submit it when it is an ordinary prompt.
    pub fn submit_draft(&mut self, delivery: Delivery) {
        let text = self.drafts.current().text().to_string();
        if text.trim().is_empty() {
            self.error = Some("内容为空，草稿已保留".to_string());
            return;
        }
        let route = CommandRouter::new(&self.registry, self.scene.active().clone()).route(&text);
        match route {
            InputRoute::Prompt { .. } => {}
            InputRoute::Bash { .. } => {
                self.error = Some("人工 Shell（S08）尚未接入；草稿已保留".to_string());
                return;
            }
            InputRoute::Command { owner, .. } => {
                self.error = Some(format!("命令入口 {owner} 尚未接入；草稿已保留"));
                return;
            }
            InputRoute::Skill { .. } => {
                self.error = Some("技能入口（S05）尚未接入；草稿已保留".to_string());
                return;
            }
            InputRoute::Unregistered { owner, .. } => {
                self.error = Some(format!("命令入口 {} 尚未注册；草稿已保留", owner.label()));
                return;
            }
        }
        if !self.ready {
            self.error = Some("宿主未就绪；草稿已保留".to_string());
            return;
        }
        if self.stop_pending {
            self.error = Some("正在停止，等待官方 idle；草稿已保留".to_string());
            return;
        }
        match self
            .model
            .begin_submit_content(&OrderedContent::from_text(&text), delivery)
        {
            Ok(plan) => {
                self.drafts.current_mut().take();
                self.submit_backups
                    .insert(plan.request_id.clone(), text.clone());
                let Some(link) = &self.link else {
                    self.model
                        .submit_unknown(&plan.request_id, "宿主连接已关闭");
                    return;
                };
                match link.request("nano.conversation.submit", plan.params) {
                    Ok(id) => {
                        self.pending.insert(
                            id,
                            Pending::Submit {
                                request_id: plan.request_id,
                                text,
                            },
                        );
                    }
                    Err(message) => {
                        self.model.submit_unknown(&plan.request_id, message);
                    }
                }
            }
            Err(SubmitError::Empty) => {
                self.error = Some("内容为空，草稿已保留".to_string());
            }
            Err(error) => {
                self.error = Some(error.to_string());
            }
        }
    }

    /// Send the draft to the terminal clipboard with OSC 52. The payload is the
    /// original buffer bytes, not the display-normalized text.
    pub fn copy_draft(&mut self) {
        let text = self.drafts.current().text();
        if text.is_empty() {
            self.notice = Some("草稿为空，没有可复制的内容".to_string());
            return;
        }
        let encoded = base64(text.as_bytes());
        use std::io::Write as _;
        let mut stdout = std::io::stdout();
        let _ = write!(stdout, "\x1b]52;c;{encoded}\x07");
        let _ = stdout.flush();
        self.notice = Some("已发送复制草稿（原文）请求".to_string());
    }

    /// Stop the current turn, keeping the queue.
    pub fn stop_turn(&mut self) {
        if !self.ready {
            self.error = Some("宿主未就绪".to_string());
            return;
        }
        if self.model.status() == nano_conversation::ConversationStatus::Idle {
            self.notice = Some("当前没有活动回合".to_string());
            return;
        }
        if self.stop_pending {
            self.notice = Some("已在停止中，等待官方 idle".to_string());
            return;
        }
        let Some(link) = &self.link else {
            return;
        };
        match link.request("nano.conversation.cancel", self.model.cancel_params()) {
            Ok(id) => {
                self.stop_pending = true;
                self.pending.insert(id, Pending::Cancel);
            }
            Err(message) => {
                self.error = Some(format!("停止失败：{message}"));
            }
        }
    }

    /// Ask for the absolute inbox/status baseline.
    pub fn refresh_snapshot(&mut self) {
        if !self.ready {
            return;
        }
        let Some(link) = &self.link else {
            return;
        };
        if let Ok(id) = link.request("nano.conversation.snapshot", self.model.snapshot_params()) {
            self.pending.insert(id, Pending::Snapshot);
        }
    }

    fn begin_queue_edit(&mut self) {
        if self.edit_target.is_some() {
            self.notice = Some("已经在编辑队列项；Enter 应用，Esc 取消".to_string());
            return;
        }
        if self.model.inbox().truncated {
            let selected = self.selected_queue_item().map(|item| item.id.clone());
            self.edit_after_snapshot = selected;
            self.notice = Some("队列投影被截断；正在读取完整快照".to_string());
            self.refresh_snapshot();
            return;
        }
        let Some(item) = self.selected_queue_item().cloned() else {
            self.error = Some("没有选中的队列项".to_string());
            return;
        };
        if !item.editable() {
            self.error = Some("带附件的队列项不能编辑；请删除后重发".to_string());
            return;
        }
        self.edit_backup = Some(self.drafts.current().clone());
        self.drafts.current_mut().set_text(item.text.clone());
        self.edit_target = Some(item.id.clone());
        self.scene.focus_mut().push(FocusId::input());
        self.notice = Some(format!("编辑队列项 {}；Enter 应用，Esc 取消", item.id));
    }

    fn cancel_queue_edit(&mut self) {
        self.edit_target = None;
        if let Some(backup) = self.edit_backup.take() {
            *self.drafts.current_mut() = backup;
        } else {
            self.drafts.current_mut().clear();
        }
        self.scene.focus_mut().reset(FocusId::input());
        self.notice = Some("已取消队列编辑".to_string());
    }

    fn apply_queue_edit(&mut self) {
        let Some(message_id) = self.edit_target.clone() else {
            return;
        };
        let text = self.drafts.current().text().to_string();
        if text.trim().is_empty() {
            self.error = Some("队列项不能改成空内容".to_string());
            return;
        }
        if !self.ready {
            self.error = Some("宿主未就绪；编辑草稿已保留".to_string());
            return;
        }
        let params = self.model.queue_edit_params(&message_id, &text);
        let Some(link) = &self.link else {
            return;
        };
        match link.request("nano.conversation.editQueued", params) {
            Ok(id) => {
                self.pending.insert(id, Pending::Edit { message_id });
            }
            Err(message) => {
                self.error = Some(format!("编辑失败：{message}"));
            }
        }
    }

    fn delete_selected_queue(&mut self) {
        let Some(item) = self.selected_queue_item().cloned() else {
            self.error = Some("没有选中的队列项".to_string());
            return;
        };
        if !self.ready {
            self.error = Some("宿主未就绪".to_string());
            return;
        }
        let params = self.model.queue_delete_params(&item.id);
        let Some(link) = &self.link else {
            return;
        };
        match link.request("nano.conversation.deleteQueued", params) {
            Ok(id) => {
                self.pending.insert(
                    id,
                    Pending::Delete {
                        message_id: item.id,
                    },
                );
            }
            Err(message) => {
                self.error = Some(format!("删除失败：{message}"));
            }
        }
    }

    fn steer_selected_queue(&mut self) {
        let Some(item) = self.selected_queue_item().cloned() else {
            self.error = Some("没有选中的队列项".to_string());
            return;
        };
        if !self.ready {
            self.error = Some("宿主未就绪".to_string());
            return;
        }
        let params = self.model.queue_steer_params(&item.id);
        let Some(link) = &self.link else {
            return;
        };
        match link.request("nano.conversation.steerQueued", params) {
            Ok(id) => {
                self.pending.insert(
                    id,
                    Pending::Steer {
                        message_id: item.id,
                    },
                );
            }
            Err(message) => {
                self.error = Some(format!("插话失败：{message}"));
            }
        }
    }

    /// Current queue item under the selection cursor.
    pub fn selected_queue_item(&self) -> Option<&nano_conversation::QueuedItem> {
        self.model.inbox().iter().nth(self.queue_selected)
    }

    fn select_queue_previous(&mut self) {
        if self.queue_selected > 0 {
            self.queue_selected -= 1;
        }
    }

    fn select_queue_next(&mut self) {
        let len = self.model.inbox().len();
        if self.queue_selected + 1 < len {
            self.queue_selected += 1;
        }
    }

    fn clamp_queue_selection(&mut self) {
        let len = self.model.inbox().len();
        if len == 0 {
            self.queue_selected = 0;
        } else if self.queue_selected >= len {
            self.queue_selected = len - 1;
        }
    }

    fn toggle_focus(&mut self) {
        if self.scene.focus().input_focused() {
            if !self.model.inbox().is_empty() {
                self.scene.focus_mut().push(FocusId::queue());
            }
        } else {
            self.scene.focus_mut().pop();
        }
    }

    fn handle_queue_key(&mut self, key: KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL)
            || key.modifiers.contains(KeyModifiers::ALT)
        {
            return false;
        }
        if self.model.inbox().is_empty() {
            self.scene.focus_mut().pop();
            return true;
        }
        match key.code {
            KeyCode::Up => {
                self.select_queue_previous();
                true
            }
            KeyCode::Down => {
                self.select_queue_next();
                true
            }
            KeyCode::Enter if key.modifiers.is_empty() => {
                self.begin_queue_edit();
                true
            }
            KeyCode::Char('d') | KeyCode::Char('D') => {
                self.delete_selected_queue();
                true
            }
            KeyCode::Char('s') | KeyCode::Char('S') => {
                self.steer_selected_queue();
                true
            }
            KeyCode::Char('x') | KeyCode::Char('X') => {
                self.clear_and_stop();
                true
            }
            KeyCode::Esc => {
                self.scene.focus_mut().pop();
                true
            }
            _ => false,
        }
    }

    /// Whether the current input is an edit of a queued item.
    pub fn editing_queue(&self) -> Option<&str> {
        self.edit_target.as_deref()
    }

    /// Status-bar text.
    pub fn status_hint(&self) -> String {
        if self.switch.state().failed() {
            return format!(
                "{}；当前没有可用宿主（Alt+R 重试 / Alt+B 返回上一会话）",
                self.switch.state().label()
            );
        }
        if self.switch.busy() {
            return self.switch.state().label();
        }
        if self.panel.open {
            return "会话列表：↑↓ 选择 · Enter 切换 · n 改名 · c 压缩 · r 刷新 · Esc 关闭"
                .to_string();
        }
        if self.stop_pending {
            return "停止中：等待官方 idle 事实（草稿保留）".to_string();
        }
        if let Some(error) = &self.error {
            return error.clone();
        }
        if let Some(notice) = &self.notice {
            return notice.clone();
        }
        if let Some(error) = self.model.last_error() {
            return error.to_string();
        }
        if let Some(notice) = self.model.last_notice() {
            return notice.to_string();
        }
        if !self.scene.focus().input_focused() {
            return "队列焦点：↑↓ 选择 · Enter 编辑 · d 删除 · s 插话 · x 清空并停止 · Tab 返回"
                .to_string();
        }
        if let Some(message_id) = &self.edit_target {
            return format!("编辑队列项 {message_id}：Enter 应用，Esc 取消");
        }
        "Enter 发送/排队 · Alt+Enter 插话 · Ctrl+J 换行 · Esc 停止 · Alt+O 会话 · Alt+C 压缩 · Ctrl+Y 复制原文 · Ctrl+C 退出"
            .to_string()
    }
}

impl App {
    /// Open the official session list panel.
    pub fn open_session_panel(&mut self) {
        if self.switch.busy() {
            return;
        }
        if !self.ready {
            self.error = Some("宿主未就绪，无法读取会话列表".to_string());
            return;
        }
        self.panel.open = true;
        self.panel.error = None;
        self.panel.selected = 0;
        self.scene.focus_mut().push(FocusId::new("sessions"));
        self.refresh_sessions();
    }

    /// Close the panel and return focus to the input editor.
    pub fn close_session_panel(&mut self) {
        self.panel.close();
        if self.scene.focus().top() == Some(&FocusId::new("sessions")) {
            self.scene.focus_mut().pop();
        }
    }

    /// Panel keys never fall through to the conversation input.
    fn handle_panel_key(&mut self, key: KeyEvent) -> bool {
        if let Some(rename) = &mut self.panel.rename {
            match key.code {
                KeyCode::Esc => {
                    self.panel.rename = None;
                    return true;
                }
                KeyCode::Enter if key.modifiers.is_empty() => {
                    self.apply_rename();
                    return true;
                }
                _ => {
                    let Some(event) = text_input_event(&key) else {
                        return false;
                    };
                    return rename.buffer.handle(&event) != InputOutcome::Ignored;
                }
            }
        }
        if key.modifiers.contains(KeyModifiers::CONTROL)
            || key.modifiers.contains(KeyModifiers::ALT)
        {
            return false;
        }
        match key.code {
            KeyCode::Esc if self.compact_in_flight || self.compact_maybe_running => {
                self.cancel_compact();
                true
            }
            KeyCode::Esc => {
                self.close_session_panel();
                true
            }
            KeyCode::Up => {
                if self.panel.selected > 0 {
                    self.panel.selected -= 1;
                }
                true
            }
            KeyCode::Down => {
                if self.panel.selected < self.panel.rows.len() {
                    self.panel.selected += 1;
                }
                true
            }
            KeyCode::Enter => {
                self.activate_selected_session();
                true
            }
            KeyCode::Char('r') | KeyCode::Char('R') => {
                self.refresh_sessions();
                true
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                self.begin_rename();
                true
            }
            KeyCode::Char('c') | KeyCode::Char('C') => {
                self.request_compact();
                true
            }
            _ => false,
        }
    }

    /// After a failed switch only retry/back are meaningful.
    fn handle_failed_switch_key(&mut self, _key: KeyEvent) -> bool {
        self.error = Some(format!(
            "{}；当前没有可用宿主（Alt+R 重试 / Alt+B 返回上一会话）",
            self.switch.state().label()
        ));
        false
    }

    /// Ask the host for the official session list.
    pub fn refresh_sessions(&mut self) {
        if !self.ready {
            self.panel.error = Some("宿主未就绪".to_string());
            return;
        }
        if self.panel.loading {
            return;
        }
        let Some(link) = &self.link else {
            self.panel.error = Some("宿主连接已关闭".to_string());
            return;
        };
        match link.request("nano.sessions.list", json!({})) {
            Ok(id) => {
                self.panel.loading = true;
                self.panel.error = None;
                self.pending.insert(id, Pending::SessionsList);
            }
            Err(message) => {
                self.panel.error = Some(format!("读取会话列表失败：{message}"));
            }
        }
    }

    /// Switch to the selected row, or create a fresh session at row zero.
    fn activate_selected_session(&mut self) {
        if self.panel.selected == 0 {
            self.begin_switch(SwitchTarget::new(
                nano_runtime::paths::new_root_session_id(),
                TargetMode::Create,
            ));
            return;
        }
        let Some(row) = self.panel.selected_row().cloned() else {
            return;
        };
        if !row.switchable() {
            self.panel.error = Some("子代理会话由 S12 接管，不能作为主会话".to_string());
            return;
        }
        if let Some(cwd) = self.hello.as_ref().map(|hello| hello.cwd.as_str()) {
            if !row.switchable_in(cwd) {
                self.panel.error = Some(format!(
                    "该会话记录在 {}，与当前目录 {cwd} 不同；不能作为主会话",
                    row.cwd.as_deref().unwrap_or("未知目录")
                ));
                return;
            }
        }
        if row.current {
            self.notice = Some("已经在该会话".to_string());
            return;
        }
        let target = SwitchTarget::new(row.session_id.clone(), TargetMode::Resume)
            .with_title(Some(row.display_title().to_string()));
        self.begin_switch(target);
    }

    /// Start an inline rename for the active root session only.
    fn begin_rename(&mut self) {
        let Some(row) = self.panel.selected_row().cloned() else {
            self.panel.error = Some("「新建会话」没有标题".to_string());
            return;
        };
        if !row.current {
            self.panel.error =
                Some("冷会话改名会先 resume 并拿写锁；先打开该会话再改名".to_string());
            return;
        }
        if !self.ready {
            self.panel.error = Some("宿主未就绪".to_string());
            return;
        }
        self.panel.rename = Some(RenameInput {
            session_id: row.session_id.clone(),
            buffer: InputBuffer::from_text(row.title.clone().unwrap_or_default()),
        });
        self.panel.error = None;
    }

    /// Submit the rename; the official service decides normalization and errors.
    fn apply_rename(&mut self) {
        let Some(rename) = self.panel.rename.clone() else {
            return;
        };
        let title = rename.buffer.text().to_string();
        let Some(link) = &self.link else {
            self.panel.error = Some("宿主连接已关闭".to_string());
            return;
        };
        match link.request(
            "nano.sessions.rename",
            json!({"sessionId": rename.session_id, "title": title}),
        ) {
            Ok(id) => {
                self.panel.rename = None;
                self.pending.insert(
                    id,
                    Pending::Rename {
                        session_id: rename.session_id,
                        title,
                    },
                );
            }
            Err(message) => {
                self.panel.error = Some(format!("改名请求未能发出：{message}"));
            }
        }
    }

    /// Stop the current host and start the requested main session generation.
    fn begin_switch(&mut self, target: SwitchTarget) {
        let previous = self.current_target();
        self.begin_switch_inner(target, previous);
    }

    /// Retry the failed target after a switch failure. A create target that
    /// already materialized its session before the handshake failed is retried
    /// as a resume of the same identity, not as a second create.
    fn retry_switch(&mut self) {
        let previous = self.switch.state().previous().cloned();
        let Some(target) = self.retry_target() else {
            return;
        };
        self.begin_switch_inner(target, previous);
    }

    /// Retry target with the one safe mode correction: a create target whose
    /// session was already materialized before the handshake failed is resumed
    /// under the same id instead of being created a second time.
    fn retry_target(&self) -> Option<SwitchTarget> {
        let mut target = self.switch.retry_target()?;
        if target.mode == TargetMode::Create
            && self
                .switch
                .state()
                .failure_message()
                .is_some_and(|message| message.contains("already exists"))
        {
            target.mode = TargetMode::Resume;
        }
        Some(target)
    }

    /// Return to the session that was active before the failed switch.
    fn back_after_failure(&mut self) {
        let Some(target) = self.switch.back_target() else {
            self.notice = Some("没有可返回的上一会话".to_string());
            return;
        };
        self.begin_switch_inner(target, None);
    }

    fn begin_switch_inner(&mut self, target: SwitchTarget, previous: Option<SwitchTarget>) {
        if self.switch.busy() {
            return;
        }
        if self.ready
            && target.mode == TargetMode::Resume
            && target.session_id == self.model.address().root_session_id
        {
            self.notice = Some("已经在该会话".to_string());
            return;
        }
        if !self.switch.begin(target.clone(), previous) {
            return;
        }
        let address = SessionAddress::root(target.session_id.clone());
        self.reset_for_target(&address);
        let mode = match target.mode {
            TargetMode::Create => SessionMode::Create,
            TargetMode::Resume => SessionMode::Resume,
        };
        let (tx, rx) = channel();
        self.restart_rx = Some(rx);
        if let Some(link) = self.link.take() {
            crate::restart::spawn_restart(
                Some(link),
                target.session_id,
                mode,
                self.cli.host_options(),
                tx,
            );
        } else {
            self.switch.host_started();
            crate::restart::spawn_restart(
                None,
                target.session_id,
                mode,
                self.cli.host_options(),
                tx,
            );
        }
        self.notice = Some(self.switch.state().label());
    }

    /// The current connected session as a resume target.
    fn current_target(&self) -> Option<SwitchTarget> {
        if !matches!(self.connection, Connection::Ready) {
            return None;
        }
        Some(SwitchTarget::new(
            self.model.address().root_session_id.clone(),
            TargetMode::Resume,
        ))
    }

    /// Clear every address-bound display fact before the new generation starts.
    fn reset_for_target(&mut self, address: &SessionAddress) {
        self.model = ConversationModel::new(address.clone());
        self.drafts.switch_to(address.clone());
        self.scene
            .switch_to(SceneId::conversation(), address.clone());
        self.hello = None;
        self.ready = false;
        self.connection = Connection::Starting;
        self.error = None;
        self.queue_selected = 0;
        self.edit_target = None;
        self.edit_backup = None;
        self.submit_backups.clear();
        self.pending.clear();
        self.stop_pending = false;
        self.resubscribe_at = None;
        self.resubscribing = false;
        self.resubscribe_attempts = 0;
        self.clear_stop_remaining = 0;
        self.clear_stop_cancel = false;
        self.clear_stop_failed = false;
        self.clear_after_snapshot = false;
        self.edit_after_snapshot = None;
        self.context = None;
        self.context_error = None;
        self.compact = None;
        self.compact_in_flight = false;
        self.compact_request_id = None;
        self.compact_maybe_running = false;
        self.scroll_back = 0;
        self.replay_truncated = false;
        self.panel.close();
    }

    /// Read the official context-occupancy projection.
    pub fn refresh_context(&mut self) {
        if !self.ready {
            return;
        }
        let Some(link) = &self.link else {
            return;
        };
        if let Ok(id) = link.request("nano.sessions.context", json!({})) {
            self.pending.insert(id, Pending::Context);
        }
    }

    /// Run the official `/compact` command on the long-request worker.
    pub fn request_compact(&mut self) {
        if !self.ready {
            self.error = Some("宿主未就绪".to_string());
            return;
        }
        if self.switch.busy() {
            self.notice = Some(self.switch.state().label());
            return;
        }
        if self.compact_in_flight || self.compact_maybe_running {
            self.notice = Some(COMPACTION_BUSY_TEXT.to_string());
            return;
        }
        let Some(link) = &self.link else {
            self.error = Some("宿主连接已关闭".to_string());
            return;
        };
        match link.request_long("nano.sessions.compact", json!({}), Duration::from_secs(120)) {
            Ok(id) => {
                self.compact_in_flight = true;
                self.compact_maybe_running = true;
                self.compact_request_id = Some(id);
                self.pending.insert(id, Pending::Compact);
                self.notice = Some("已请求官方 /compact；等待官方结果".to_string());
            }
            Err(message) => {
                self.error = Some(format!("压缩请求未能发出：{message}"));
            }
        }
    }

    /// Stop waiting for the running compaction. The backend still finishes its
    /// cleanup and may already have changed history, so reread the official
    /// state instead of keeping a stale transcript or a "nothing happened" view.
    fn cancel_compact(&mut self) {
        if let Some(id) = self.compact_request_id.take() {
            self.pending.remove(&id);
        }
        self.compact_in_flight = false;
        self.compact_maybe_running = false;
        let outcome = CompactionOutcome::cancelled_wait();
        self.notice = Some(outcome.display_text());
        self.compact = Some(outcome);
        self.refresh_context();
        self.refresh_snapshot();
        let Some(link) = &self.link else {
            return;
        };
        if let Ok(id) = link.request("nano.sessions.compactCancel", json!({})) {
            self.pending.insert(id, Pending::CompactCancel);
        }
    }

    fn apply_context_value(&mut self, value: &Value) -> bool {
        match ContextMeter::parse(value) {
            Ok(meter) => {
                if !meter.matches(&self.model.address().root_session_id) {
                    return false;
                }
                self.context = Some(meter);
                self.context_error = None;
                true
            }
            Err(error) => {
                self.context_error = Some(error);
                true
            }
        }
    }

    /// Drain events from the restart worker without blocking the UI loop.
    fn poll_restart_events(&mut self) -> bool {
        let mut events = Vec::new();
        if let Some(rx) = &self.restart_rx {
            while let Ok(event) = rx.try_recv() {
                events.push(event);
            }
        }
        let mut changed = false;
        for event in events {
            changed |= self.handle_link(event);
        }
        changed
    }

    /// Time-based work: restart-worker events and closed-subscription rebuilds.
    pub fn tick(&mut self) -> bool {
        let mut changed = self.poll_restart_events();
        changed |= self.tick_resubscribe();
        changed
    }

    fn tick_resubscribe(&mut self) -> bool {
        if self.resubscribing {
            return false;
        }
        let Some(at) = self.resubscribe_at else {
            return false;
        };
        if Instant::now() < at {
            return false;
        }
        self.resubscribe_at = None;
        if self.resubscribe_attempts >= 2 {
            let root = self.model.address().root_session_id.clone();
            self.ready = false;
            self.connection = Connection::Disconnected;
            self.error = Some(format!(
                "订阅反复被关闭（可能含超过 S01 事件帧上限的持久事件）；会话 {root}，重启时用 --resume {root}"
            ));
            self.model.mark_disconnected();
            return true;
        }
        self.resubscribe_attempts += 1;
        self.send_resubscribe();
        true
    }

    fn send_resubscribe(&mut self) {
        if !self.ready || self.resubscribing {
            return;
        }
        let Some(link) = &self.link else {
            return;
        };
        let (params, replay_truncated) = self.model.resubscribe_params(&[
            "session.event",
            "conversation.status",
            "conversation.inbox",
            "conversation.stream",
            "sessions.context",
        ]);
        if replay_truncated {
            self.notice = Some("重连回放窗口超过上限；从较新的 seq 开始重建".to_string());
        }
        match link.request("nano.subscribe", params) {
            Ok(id) => {
                self.resubscribing = true;
                self.pending.insert(id, Pending::Resubscribe);
            }
            Err(message) => {
                self.ready = false;
                self.connection = Connection::Disconnected;
                self.error = Some(format!("订阅重建失败：{message}"));
                self.model.mark_disconnected();
            }
        }
    }

    /// Per-item delete then cancel: the official cancel hardcodes
    /// `keepInbox: true`, so clearing is necessarily a composite best-effort
    /// action with a claim race.
    pub fn clear_and_stop(&mut self) {
        if !self.ready {
            self.error = Some("宿主未就绪".to_string());
            return;
        }
        if self.clear_stop_remaining > 0 {
            self.notice = Some("清空批次进行中；等待官方删除回执".to_string());
            return;
        }
        if self.model.inbox().truncated {
            self.clear_after_snapshot = true;
            self.notice = Some("队列投影被截断；正在读取完整快照后再清空".to_string());
            self.refresh_snapshot();
            return;
        }
        let ids: Vec<String> = self
            .model
            .inbox()
            .iter()
            .map(|item| item.id.clone())
            .collect();
        let cancel_after = self.model.status() == nano_conversation::ConversationStatus::Running;
        if ids.is_empty() {
            if cancel_after {
                self.stop_turn();
            } else {
                self.notice = Some("队列已空".to_string());
            }
            return;
        }
        let Some(link) = &self.link else {
            return;
        };
        self.clear_stop_generation = self.clear_stop_generation.wrapping_add(1);
        let generation = self.clear_stop_generation;
        self.clear_stop_failed = false;
        self.clear_stop_remaining = 0;
        self.clear_stop_cancel = cancel_after;
        self.notice = Some(format!(
            "正在逐项清空队列（{} 项）{}",
            ids.len(),
            if cancel_after { "，随后停止" } else { "" }
        ));
        for message_id in &ids {
            let params = self.model.queue_delete_params(message_id);
            match link.request("nano.conversation.deleteQueued", params) {
                Ok(id) => {
                    self.clear_stop_remaining += 1;
                    self.pending.insert(
                        id,
                        Pending::ClearStopDelete {
                            generation,
                            message_id: message_id.clone(),
                        },
                    );
                }
                Err(message) => {
                    self.clear_stop_failed = true;
                    self.error = Some(format!("删除请求未能发出：{message}"));
                }
            }
        }
        if self.clear_stop_remaining == 0 {
            self.finish_clear_stop();
        }
    }

    fn finish_clear_stop(&mut self) {
        let cancel = self.clear_stop_cancel;
        let failed = self.clear_stop_failed;
        self.clear_stop_cancel = false;
        self.clear_stop_failed = false;
        self.refresh_snapshot();
        if cancel && self.model.status() == nano_conversation::ConversationStatus::Running {
            self.stop_turn();
        } else if failed {
            self.notice = Some("部分队列项未删除；已刷新官方 inbox 并显示剩余项".to_string());
        } else {
            self.notice = Some("清空请求已逐项发出；以官方 inbox 为准".to_string());
        }
    }
}

/// Map one key to a text-editing event for the rename editor.
fn text_input_event(key: &KeyEvent) -> Option<InputEvent> {
    match key.code {
        KeyCode::Backspace => Some(InputEvent::Backspace),
        KeyCode::Delete => Some(InputEvent::Delete),
        KeyCode::Left => Some(InputEvent::Left),
        KeyCode::Right => Some(InputEvent::Right),
        KeyCode::Up => Some(InputEvent::Up),
        KeyCode::Down => Some(InputEvent::Down),
        KeyCode::Home => Some(InputEvent::Home),
        KeyCode::End => Some(InputEvent::End),
        KeyCode::Char(character)
            if !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT) =>
        {
            Some(InputEvent::Insert(character.to_string()))
        }
        _ => None,
    }
}

/// Minimal standard base64 for the OSC 52 clipboard payload.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        output.push(ALPHABET[(first >> 2) as usize] as char);
        output.push(ALPHABET[(((first & 0b11) << 4) | (second >> 4)) as usize] as char);
        if chunk.len() > 1 {
            output.push(ALPHABET[(((second & 0b1111) << 2) | (third >> 6)) as usize] as char);
        } else {
            output.push('=');
        }
        if chunk.len() > 2 {
            output.push(ALPHABET[(third & 0b111111) as usize] as char);
        } else {
            output.push('=');
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;
    use serde_json::json;

    fn app() -> App {
        App::new(Cli::default(), "root".to_string(), false)
    }

    fn queue_event(app: &mut App, text: &str) {
        let payload = json!({
            "address": {"rootSessionId": "root"},
            "inbox": {
                "nextTurn": [{
                    "id": "m-1",
                    "role": "user",
                    "content": [{"type": "text", "text": text}],
                    "source": {"kind": "user", "rpcId": "r-1"}
                }],
                "nextStep": []
            }
        });
        app.model.apply_event("conversation.inbox", &payload);
    }

    #[test]
    fn enter_on_blank_keeps_the_draft_and_reports_rejection() {
        let mut app = app();
        app.drafts.current_mut().set_text("  \n");
        assert!(app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        assert_eq!(app.drafts.current().text(), "  \n");
        assert!(app.error.as_deref().unwrap().contains("内容为空"));
    }

    #[test]
    fn shift_enter_inserts_a_newline_instead_of_sending() {
        let mut app = app();
        app.drafts.current_mut().set_text("one");
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
        assert_eq!(app.drafts.current().text(), "one\n");
    }

    #[test]
    fn a_prompt_without_a_ready_host_is_rejected_and_kept() {
        let mut app = app();
        app.drafts.current_mut().set_text("hello");
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.drafts.current().text(), "hello");
        assert!(app.error.as_deref().unwrap().contains("宿主未就绪"));
    }

    #[test]
    fn shell_input_is_routed_to_s08_and_not_submitted() {
        let mut app = app();
        app.drafts.current_mut().set_text("!ls");
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.drafts.current().text(), "!ls");
        assert!(app.error.as_deref().unwrap().contains("S08"));
    }

    #[test]
    fn bracketed_paste_preserves_the_payload() {
        let mut app = app();
        app.handle_paste("a\r\nb".to_string());
        assert_eq!(app.drafts.current().text(), "a\r\nb");
    }

    #[test]
    fn alt_e_loads_a_queued_item_and_escape_restores_the_original_draft() {
        let mut app = app();
        app.drafts.current_mut().set_text("original");
        queue_event(&mut app, "queued text");
        app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::ALT));
        assert_eq!(app.editing_queue(), Some("m-1"));
        assert_eq!(app.drafts.current().text(), "queued text");
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.editing_queue(), None);
        assert_eq!(app.drafts.current().text(), "original");
    }

    #[test]
    fn queue_selection_stays_in_range() {
        let mut app = app();
        assert!(app.selected_queue_item().is_none());
        queue_event(&mut app, "one");
        assert_eq!(app.selected_queue_item().unwrap().id, "m-1");
        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::ALT));
        assert_eq!(app.queue_selected, 0);
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::ALT));
        assert_eq!(app.queue_selected, 0);
    }

    #[test]
    fn control_u_clears_the_draft() {
        let mut app = app();
        app.drafts.current_mut().set_text("draft");
        app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert!(app.drafts.current().is_empty());
    }

    #[test]
    fn a_prepared_submission_clears_the_input_and_keeps_an_echo() {
        let mut app = app();
        app.ready = true;
        app.drafts.current_mut().set_text("hello");
        app.submit_draft(Delivery::Queue);
        assert!(app.drafts.current().is_empty());
        let echoes = app.model.live().echoes;
        assert_eq!(echoes.len(), 1);
        assert_eq!(echoes[0].text, "hello");
        assert_eq!(echoes[0].state.label(), "未确认");
    }

    #[test]
    fn sending_is_blocked_until_the_official_idle_fact_arrives() {
        use nano_runtime::protocol::EventParams;
        let mut app = app();
        app.ready = true;
        app.stop_pending = true;
        app.drafts.current_mut().set_text("keep me");
        app.submit_draft(Delivery::Queue);
        assert_eq!(app.drafts.current().text(), "keep me");
        assert!(app.error.as_deref().unwrap().contains("正在停止"));
        assert!(app.status_hint().contains("停止中"));
        app.handle_link(LinkEvent::Host(nano_runtime::HostEvent::Event(
            EventParams {
                runtime_id: "rt".into(),
                generation: 1,
                subscription_id: "sub-1".into(),
                sequence: 1,
                kind: "conversation.status".into(),
                payload: json!({"address": {"rootSessionId": "root"}, "status": "idle"}),
            },
        )));
        assert!(!app.stop_pending);
    }

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64("第一行".as_bytes()), "56ys5LiA6KGM");
    }

    #[test]
    fn a_too_large_submission_is_rejected_before_it_reaches_the_host() {
        let mut app = app();
        app.ready = true;
        let oversized = "x".repeat(nano_conversation::MAX_SUBMIT_TEXT_BYTES + 1);
        app.drafts.current_mut().set_text(oversized.clone());
        app.submit_draft(Delivery::Queue);
        assert_eq!(app.drafts.current().text(), oversized);
        assert!(app.error.as_deref().unwrap().contains("内容过长"));
    }

    #[test]
    fn queue_focus_never_submits_the_hidden_input_draft() {
        let mut app = app();
        app.drafts.current_mut().set_text("hidden draft");
        queue_event(&mut app, "queued");
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert!(!app.scene.focus().input_focused());
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        assert_eq!(app.drafts.current().text(), "hidden draft");
        assert!(app.model.live().echoes.is_empty());
    }

    #[test]
    fn an_empty_inbox_drops_queue_focus() {
        let mut app = app();
        queue_event(&mut app, "queued");
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert!(!app.scene.focus().input_focused());
        app.handle_link(LinkEvent::Host(nano_runtime::HostEvent::Event(
            nano_runtime::protocol::EventParams {
                runtime_id: "rt".into(),
                generation: 1,
                subscription_id: "sub-1".into(),
                sequence: 2,
                kind: "conversation.inbox".into(),
                payload: json!({
                    "address": {"rootSessionId": "root"},
                    "inbox": {"nextTurn": [], "nextStep": []}
                }),
            },
        )));
        assert!(app.scene.focus().input_focused());
    }

    #[test]
    fn repeated_alt_e_does_not_overwrite_the_edit_backup() {
        let mut app = app();
        app.drafts.current_mut().set_text("original");
        queue_event(&mut app, "queued");
        app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::ALT));
        app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::ALT));
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.drafts.current().text(), "original");
    }

    #[test]
    fn a_failed_submission_keeps_both_the_failed_text_and_new_input() {
        let mut app = app();
        app.drafts.current_mut().set_text("new draft");
        app.submit_backups
            .insert("r-1".into(), "failed text".into());
        app.restore_draft("r-1", "failed text");
        assert_eq!(app.drafts.current().text(), "failed text\n\nnew draft");
    }

    #[test]
    fn a_snapshot_idle_clears_a_stuck_stop() {
        let mut app = app();
        app.ready = true;
        app.stop_pending = true;
        app.pending.insert(7, Pending::Snapshot);
        app.handle_request_done(
            7,
            Ok(json!({
                "address": {"rootSessionId": "root"},
                "seq": 1,
                "status": "idle",
                "inbox": {"nextTurn": [], "nextStep": []}
            })),
        );
        assert!(!app.stop_pending);
    }

    #[test]
    fn a_failed_stream_schedules_a_rebuild_without_dropping_the_connection() {
        let mut app = app();
        app.ready = true;
        app.connection = Connection::Ready;
        app.handle_link(LinkEvent::Host(nano_runtime::HostEvent::StreamEnded(
            nano_runtime::protocol::StreamEndedParams {
                runtime_id: "rt".into(),
                generation: 1,
                subscription_id: "sub-1".into(),
                outcome: nano_runtime::protocol::StreamOutcome::Failed,
                reason: Some("slow".into()),
            },
        )));
        assert_eq!(app.connection, Connection::Ready);
        assert!(app.resubscribe_at.is_some());
    }

    #[test]
    fn repeated_stream_failures_stop_retrying() {
        let mut app = app();
        app.ready = true;
        app.connection = Connection::Ready;
        app.resubscribe_attempts = 2;
        app.handle_link(LinkEvent::Host(nano_runtime::HostEvent::StreamEnded(
            nano_runtime::protocol::StreamEndedParams {
                runtime_id: "rt".into(),
                generation: 1,
                subscription_id: "sub-1".into(),
                outcome: nano_runtime::protocol::StreamOutcome::Failed,
                reason: Some("too large".into()),
            },
        )));
        assert_eq!(app.connection, Connection::Disconnected);
        assert!(app.error.as_deref().unwrap().contains("S01"));
    }

    #[test]
    fn an_unknown_transport_result_is_not_reported_as_a_failed_send() {
        let mut app = app();
        app.ready = true;
        let plan = app.model.begin_submit("text", Delivery::Queue).unwrap();
        app.submit_backups
            .insert(plan.request_id.clone(), "text".into());
        app.pending.insert(
            9,
            Pending::Submit {
                request_id: plan.request_id.clone(),
                text: "text".into(),
            },
        );
        app.handle_request_done(
            9,
            Err(RequestError {
                kind: RequestErrorKind::Unknown,
                message: "transport closed".into(),
            }),
        );
        assert!(app.drafts.current().is_empty());
        assert!(app.model.submissions().get(&plan.request_id).is_some());
    }

    #[test]
    fn a_local_rejection_restores_the_draft() {
        let mut app = app();
        app.ready = true;
        let plan = app.model.begin_submit("text", Delivery::Queue).unwrap();
        app.submit_backups
            .insert(plan.request_id.clone(), "text".into());
        app.pending.insert(
            9,
            Pending::Submit {
                request_id: plan.request_id.clone(),
                text: "text".into(),
            },
        );
        app.handle_request_done(
            9,
            Err(RequestError {
                kind: RequestErrorKind::Local,
                message: "not sent".into(),
            }),
        );
        assert_eq!(app.drafts.current().text(), "text");
    }

    #[test]
    fn the_session_panel_requires_a_ready_host() {
        let mut app = app();
        app.open_session_panel();
        assert!(!app.panel.open);
        assert!(app.error.as_deref().unwrap().contains("宿主未就绪"));
    }

    #[test]
    fn a_session_list_result_replaces_the_rows_and_clamps_the_selection() {
        let mut app = app();
        app.panel.open = true;
        app.panel.selected = 9;
        app.pending.insert(41, Pending::SessionsList);
        app.handle_request_done(
            41,
            Ok(json!({
                "sessions": [
                    {
                        "sessionId": "root",
                        "title": "当前",
                        "createdAt": 100,
                        "updatedAt": 200,
                        "agentAvailable": true,
                        "running": false,
                        "blank": false,
                        "current": true
                    },
                    {
                        "sessionId": "old",
                        "createdAt": 50,
                        "agentAvailable": false,
                        "running": false,
                        "current": false
                    }
                ]
            })),
        );
        assert_eq!(app.panel.rows.len(), 2);
        assert_eq!(app.panel.selected, 2);
        assert_eq!(app.panel.selected_row().unwrap().session_id, "old");
        assert!(!app.panel.loading);
    }

    #[test]
    fn a_failed_rename_restores_the_editor_and_reports_the_official_error() {
        let mut app = app();
        app.panel.open = true;
        app.pending.insert(
            7,
            Pending::Rename {
                session_id: "root".into(),
                title: "被拒绝的标题".into(),
            },
        );
        app.handle_request_done(
            7,
            Err(RequestError {
                kind: RequestErrorKind::Rpc,
                message: "nano/sessions/title-invalid".into(),
            }),
        );
        assert!(app
            .panel
            .error
            .as_deref()
            .unwrap()
            .contains("title-invalid"));
        assert_eq!(
            app.panel.rename.as_ref().unwrap().buffer.text(),
            "被拒绝的标题"
        );
    }

    #[test]
    fn context_events_only_apply_to_the_current_session() {
        use nano_runtime::protocol::EventParams;
        let mut app = app();
        let payload = json!({
            "sessionId": "other",
            "asOfSeq": 1,
            "approximate": true,
            "projectedTokens": 10,
            "contextWindow": 100,
            "tokenUsage": {
                "uncachedInputTokens": 0,
                "outputTokens": 0,
                "cacheReadTokens": 0,
                "cacheWriteTokens": 0
            }
        });
        app.handle_link(LinkEvent::Host(nano_runtime::HostEvent::Event(
            EventParams {
                runtime_id: "rt".into(),
                generation: 1,
                subscription_id: "sub-1".into(),
                sequence: 1,
                kind: "sessions.context".into(),
                payload,
            },
        )));
        assert!(app.context.is_none());
    }

    #[test]
    fn a_context_event_keeps_occupancy_and_billing_separate() {
        use nano_runtime::protocol::EventParams;
        let mut app = app();
        app.handle_link(LinkEvent::Host(nano_runtime::HostEvent::Event(
            EventParams {
                runtime_id: "rt".into(),
                generation: 1,
                subscription_id: "sub-1".into(),
                sequence: 1,
                kind: "sessions.context".into(),
                payload: json!({
                    "sessionId": "root",
                    "asOfSeq": 3,
                    "approximate": true,
                    "projectedTokens": 64000,
                    "contextWindow": 128000,
                    "tokenUsage": {
                        "uncachedInputTokens": 100,
                        "outputTokens": 20,
                        "cacheReadTokens": 3,
                        "cacheWriteTokens": 4
                    }
                }),
            },
        )));
        let meter = app.context.as_ref().unwrap();
        assert_eq!(meter.occupancy().percent(), Some(50));
        assert_eq!(meter.token_usage.unwrap().total(), 127);
    }

    #[test]
    fn a_compact_success_shows_the_official_text_and_rereads_state() {
        let mut app = app();
        app.ready = true;
        app.compact_in_flight = true;
        app.pending.insert(9, Pending::Compact);
        app.handle_request_done(
            9,
            Ok(json!({
                "commandId": "cmd-1",
                "kind": "success",
                "text": "Compacted 3 history items (~120 tokens).",
                "sourceEventSeq": 12
            })),
        );
        assert!(!app.compact_in_flight);
        assert_eq!(
            app.compact.as_ref().unwrap().display_text(),
            "Compacted 3 history items (~120 tokens)."
        );
        assert!(app.notice.as_deref().unwrap().contains("Compacted 3"));
    }

    #[test]
    fn a_compact_cancellation_is_not_shown_as_nothing_happened() {
        let mut app = app();
        app.compact_in_flight = true;
        app.pending.insert(10, Pending::Compact);
        app.handle_request_done(
            10,
            Ok(json!({
                "kind": "error",
                "text": "Compaction cancelled."
            })),
        );
        let outcome = app.compact.as_ref().unwrap();
        assert!(outcome.cancelled());
        assert!(outcome.history_may_have_changed());
    }

    #[test]
    fn switching_clears_every_old_address_display_fact_but_keeps_drafts() {
        let mut app = app();
        app.drafts.current_mut().set_text("old draft");
        app.model.apply_event(
            "conversation.inbox",
            &json!({
                "address": {"rootSessionId": "root"},
                "inbox": {
                    "nextTurn": [{
                        "id": "m-1",
                        "role": "user",
                        "content": [{"type": "text", "text": "queued"}],
                        "source": {"kind": "user", "rpcId": "r-1"}
                    }],
                    "nextStep": []
                }
            }),
        );
        app.context = Some(
            nano_sessions::ContextMeter::parse(&json!({
                "sessionId": "root",
                "asOfSeq": 1,
                "approximate": true,
                "tokenUsage": {
                    "uncachedInputTokens": 0,
                    "outputTokens": 0,
                    "cacheReadTokens": 0,
                    "cacheWriteTokens": 0
                }
            }))
            .unwrap(),
        );
        let old = SessionAddress::root("root");
        let new = SessionAddress::root("next");
        app.reset_for_target(&new);
        assert_eq!(app.model.address(), &new);
        assert!(app.model.inbox().is_empty());
        assert!(app.model.items().is_empty());
        assert!(app.context.is_none());
        assert_eq!(app.drafts.current().text(), "");
        assert_eq!(
            app.drafts.get(&old).map(|draft| draft.text().to_string()),
            Some("old draft".to_string()),
            "the old draft stays with its own address"
        );
    }

    #[test]
    fn an_occupied_new_host_enters_the_no_host_state_with_the_holder_pid() {
        let mut app = app();
        let target = SwitchTarget::new("next", TargetMode::Resume);
        let previous = SwitchTarget::new("root", TargetMode::Resume);
        assert!(app.switch.begin(target, Some(previous)));
        assert!(app.switch.host_started());
        app.handle_link(LinkEvent::StartupFailed {
            code: "occupied".into(),
            message: "root session is owned by pid 4242".into(),
            holder_pid: Some(4242),
            repair_path: None,
        });
        assert!(app.switch.state().failed());
        assert!(app.error.as_deref().unwrap().contains("被 pid 4242 占用"));
        assert_eq!(app.switch.retry_target().unwrap().session_id, "next");
        assert_eq!(app.switch.back_target().unwrap().session_id, "root");
        assert!(app.link.is_none());
    }

    #[test]
    fn rename_input_maps_keys_without_leaking_alt_shortcuts() {
        let enter = text_input_event(&KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        assert!(matches!(enter, Some(InputEvent::Insert(value)) if value == "a"));
        let alt = text_input_event(&KeyEvent::new(KeyCode::Char('a'), KeyModifiers::ALT));
        assert!(alt.is_none());
    }

    #[test]
    fn an_unknown_rename_result_is_not_reported_as_failure() {
        let mut app = app();
        app.ready = true;
        app.panel.open = true;
        app.pending.insert(
            71,
            Pending::Rename {
                session_id: "root".into(),
                title: "可能已生效".into(),
            },
        );
        app.handle_request_done(
            71,
            Err(RequestError {
                kind: RequestErrorKind::Unknown,
                message: "transport closed".into(),
            }),
        );
        assert!(app.panel.error.as_deref().unwrap().contains("未知"));
        assert!(
            app.panel.rename.is_none(),
            "a lost receipt does not reopen the editor"
        );
    }

    #[test]
    fn esc_while_a_compact_wait_is_in_flight_cancels_the_wait() {
        let mut app = app();
        app.panel.open = true;
        app.compact_in_flight = true;
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.panel.open, "the panel stays open");
        assert!(!app.compact_in_flight);
        assert!(app.compact.as_ref().unwrap().cancelled());
        assert!(app
            .notice
            .as_deref()
            .unwrap()
            .contains("Compaction cancelled."));
    }

    #[test]
    fn a_timed_out_compaction_can_still_be_cancelled() {
        let mut app = app();
        app.compact_in_flight = true;
        app.compact_request_id = Some(5);
        app.pending.insert(5, Pending::Compact);
        app.handle_request_done(
            5,
            Err(RequestError {
                kind: RequestErrorKind::Unknown,
                message: "timed out".into(),
            }),
        );
        assert!(!app.compact_in_flight);
        assert!(app.compact_maybe_running);
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.compact_maybe_running);
        assert!(app.compact.as_ref().unwrap().cancelled());
        assert!(app
            .notice
            .as_deref()
            .unwrap()
            .contains("Compaction cancelled."));
    }

    #[test]
    fn a_second_compact_shows_the_official_busy_sentence() {
        let mut app = app();
        app.ready = true;
        app.compact_in_flight = true;
        app.request_compact();
        assert_eq!(app.notice.as_deref(), Some(COMPACTION_BUSY_TEXT));
    }

    #[test]
    fn retry_of_a_materialized_create_target_resumes_the_same_id() {
        let mut app = app();
        let target = SwitchTarget::new("new-session", TargetMode::Create);
        assert!(app.switch.begin(target, None));
        assert!(app.switch.host_started());
        app.switch.fail(
            Some("startup-failed".into()),
            None,
            "session identity new-session already exists; refusing to create over it",
        );
        let retry = app.retry_target().unwrap();
        assert_eq!(retry.session_id, "new-session");
        assert_eq!(retry.mode, TargetMode::Resume);
    }

    #[test]
    fn closing_the_panel_clears_stale_rows() {
        let mut app = app();
        app.panel.open = true;
        app.panel.rows.push(
            SessionSummary::parse(&json!({
                "sessionId": "old",
                "createdAt": 1,
                "agentAvailable": false,
                "running": false,
                "current": true
            }))
            .unwrap(),
        );
        app.panel.selected = 1;
        app.panel.close();
        assert!(app.panel.rows.is_empty());
        assert_eq!(app.panel.selected, 0);
    }

    #[test]
    fn stale_agent_events_for_another_address_are_ignored() {
        let mut app = app();
        let payload = json!({
            "address": {"rootSessionId": "other"},
            "inbox": {"nextTurn": [{"id": "m-9", "content": [], "source": {}}], "nextStep": []}
        });
        assert!(!app.model.apply_event("conversation.inbox", &payload));
        assert!(app.model.inbox().is_empty());
    }
}
