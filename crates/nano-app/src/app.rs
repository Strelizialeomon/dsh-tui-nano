//! Application state and key handling.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use nano_attachments::{
    begin_params, check_image, chunk_params, discard_params, encode_base64, finish_params,
    list_params, parse_candidates, parse_finished, probe_image, AttachmentCard,
    AttachmentDraftStore, AttachmentKind, AttachmentState, CompletionState, ComposerAttachments,
    ImageLimitError, ImageLimits, ImageMediaType, ImageProbeError,
};
use nano_conversation::{
    CommandRouter, ConversationModel, Delivery, DraftStore, FeatureContribution, FeatureRegistry,
    FocusId, InputBuffer, InputEvent, InputOutcome, InputRoute, SceneId, SceneState,
    SessionAddress, SubmitError,
};
use nano_runtime::{CapabilityStatus, Hello};
use serde_json::Value;

use crate::cli::Cli;
use crate::link::{HostLink, LinkEvent, RequestError, RequestErrorKind};

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
    /// Start one attachment staging upload.
    AttachmentBegin {
        /// Receipt identity.
        receipt_id: String,
    },
    /// One attachment chunk acknowledgement.
    AttachmentChunk {
        /// Receipt identity.
        receipt_id: String,
        /// Sequence that was acknowledged.
        sequence: u64,
    },
    /// Finish one attachment staging upload.
    AttachmentFinish {
        /// Receipt identity.
        receipt_id: String,
    },
    /// One `@file` completion response.
    FileReferences {
        /// Revision the response must match.
        revision: u64,
        /// Protocol id, for clearing the cancellation slot.
        protocol_id: String,
    },
}

/// A submission's draft snapshot, used to restore on an explicit failure.
#[derive(Debug, Clone)]
pub struct SubmittedDraft {
    /// Original text, byte-for-byte.
    pub text: String,
    /// Attachments and their text anchors.
    pub attachments: ComposerAttachments,
}

/// One attachment staging upload in flight.
struct UploadJob {
    kind: AttachmentKind,
    image_bytes: Option<Vec<u8>>,
    file: Option<File>,
    total: u64,
    offset: u64,
    sequence: u64,
    finishing: bool,
}

impl UploadJob {
    /// Read the next raw chunk, or `None` when every byte was sent.
    fn next_chunk(&mut self, max_bytes: usize) -> std::io::Result<Option<Vec<u8>>> {
        if self.offset >= self.total {
            return Ok(None);
        }
        if let Some(bytes) = &self.image_bytes {
            let start = self.offset as usize;
            let end = (start + max_bytes).min(bytes.len());
            self.offset = end as u64;
            return Ok(Some(bytes[start..end].to_vec()));
        }
        let Some(file) = &mut self.file else {
            return Ok(None);
        };
        let start = self.offset;
        file.seek(SeekFrom::Start(start))?;
        let mut chunk = vec![0u8; max_bytes];
        let read = file.read(&mut chunk)?;
        if read == 0 {
            return Ok(None);
        }
        chunk.truncate(read);
        self.offset += read as u64;
        Ok(Some(chunk))
    }
}

/// A file path being typed in the attachment picker.
#[derive(Debug, Clone, Default)]
pub struct AttachmentPicker {
    /// Raw path text.
    pub path: String,
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
    /// Request id to draft snapshot for failure restoration.
    pub submit_backups: HashMap<String, SubmittedDraft>,
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
    /// Per-address attachments and their text anchors.
    pub attachments: AttachmentDraftStore,
    /// Deployment image limits, when the host reports them.
    pub attachment_limits: Option<ImageLimits>,
    /// Active `@file` completion state.
    pub completion: CompletionState,
    /// Protocol id of the in-flight file-reference request, for cancellation.
    completion_cancel: Option<String>,
    /// Open attachment path picker.
    pub picker: Option<AttachmentPicker>,
    /// Staging uploads keyed by receipt id.
    uploads: HashMap<String, UploadJob>,
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
            drafts: DraftStore::new(address.clone()),
            attachments: AttachmentDraftStore::new(address),
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
            attachment_limits: None,
            completion: CompletionState::default(),
            completion_cancel: None,
            picker: None,
            uploads: HashMap::new(),
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
        if self.picker.is_some() {
            return self.handle_picker_key(key);
        }
        // The completion popup owns unmodified navigation and insertion keys
        // while visible; modified shortcuts still reach the input.
        if self.scene.focus().input_focused()
            && self.edit_target.is_none()
            && self.completion.visible()
            && key.modifiers.is_empty()
        {
            match key.code {
                KeyCode::Up => {
                    self.completion.move_selection(-1);
                    return true;
                }
                KeyCode::Down => {
                    self.completion.move_selection(1);
                    return true;
                }
                KeyCode::Enter | KeyCode::Tab => {
                    self.accept_completion();
                    return true;
                }
                KeyCode::Esc => {
                    self.cancel_completion();
                    return true;
                }
                KeyCode::Left | KeyCode::Right | KeyCode::Home | KeyCode::End => {
                    // Moving away from the token dismisses the popup, then the
                    // key keeps its ordinary editing meaning.
                    self.cancel_completion();
                }
                _ => {}
            }
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
                    self.attachments.current_mut().clear();
                    self.completion.clear();
                    return true;
                }
                KeyCode::Char('j') => {
                    let old = self.drafts.current().text().to_string();
                    self.drafts.current_mut().handle(&InputEvent::Newline);
                    let new = self.drafts.current().text().to_string();
                    self.shift_attachment_anchors(&old, &new);
                    self.refresh_completion();
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
                KeyCode::Left => {
                    self.move_attachment(-1);
                    return true;
                }
                KeyCode::Right => {
                    self.move_attachment(1);
                    return true;
                }
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
                KeyCode::Char('a') | KeyCode::Char('A') => {
                    self.open_picker();
                    return true;
                }
                KeyCode::Backspace => {
                    self.remove_attachment_before_cursor();
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
            if self.edit_target.is_some() {
                self.cancel_queue_edit();
            } else {
                self.stop_turn();
            }
            return true;
        }
        if matches!(key.code, KeyCode::Enter) {
            if key.modifiers.contains(KeyModifiers::SHIFT) {
                let old = self.drafts.current().text().to_string();
                self.drafts.current_mut().handle(&InputEvent::Newline);
                let new = self.drafts.current().text().to_string();
                self.shift_attachment_anchors(&old, &new);
                self.refresh_completion();
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
            let old = self.drafts.current().text().to_string();
            let outcome = self.drafts.current_mut().handle(&event);
            if outcome == InputOutcome::RejectedEmpty {
                self.error = Some("内容为空，草稿已保留".to_string());
            }
            if outcome != InputOutcome::Ignored {
                let new = self.drafts.current().text().to_string();
                self.shift_attachment_anchors(&old, &new);
                self.refresh_completion();
            }
            return outcome != InputOutcome::Ignored;
        }
        false
    }

    /// Insert a bracketed-paste payload exactly as received.
    pub fn handle_paste(&mut self, text: String) -> bool {
        if let Some(picker) = &mut self.picker {
            picker.path.push_str(&text);
            return true;
        }
        if !self.scene.focus().input_focused() {
            return false;
        }
        self.clear_transient_status();
        let old = self.drafts.current().text().to_string();
        let outcome = self.drafts.current_mut().handle(&InputEvent::Paste(text));
        if outcome != InputOutcome::Ignored {
            let new = self.drafts.current().text().to_string();
            self.shift_attachment_anchors(&old, &new);
            self.refresh_completion();
        }
        outcome != InputOutcome::Ignored
    }

    /// Apply one link event. Returns whether the screen needs a redraw.
    pub fn handle_link(&mut self, event: LinkEvent) -> bool {
        match event {
            LinkEvent::Ready { hello, truncated } => {
                self.ready = true;
                self.connection = Connection::Ready;
                self.replay_truncated = self.replay_truncated || truncated;
                self.model
                    .apply_replay_baseline(hello.session.seq, truncated);
                self.attachment_limits = nano_attachments::ImageLimits::from_hello(&hello);
                self.hello = Some(hello);
                self.notice = Some("已连接官方会话".to_string());
                self.refresh_snapshot();
                true
            }
            LinkEvent::Host(event) => self.handle_host_event(event),
            LinkEvent::RequestDone { id, result } => {
                self.handle_request_done(id, result);
                true
            }
            LinkEvent::Fatal(message) => {
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
                self.mark_uploads_unknown("宿主连接已关闭");
                true
            }
        }
    }

    fn handle_host_event(&mut self, event: nano_runtime::HostEvent) -> bool {
        use nano_runtime::HostEvent;
        match event {
            HostEvent::Event(params) => {
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
                self.mark_uploads_unknown("连接断开");
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
                    | Pending::AttachmentBegin { .. }
                    | Pending::AttachmentChunk { .. }
                    | Pending::AttachmentFinish { .. }
                    | Pending::FileReferences { .. }
            )
        {
            self.resubscribing = false;
            self.clear_stop_remaining = 0;
            return;
        }
        match pending {
            Pending::Submit { request_id } => match result {
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
                        if let Some(snapshot) = self.submit_backups.get(&request_id).cloned() {
                            self.restore_draft(&request_id, &snapshot);
                        }
                    }
                }
                Err(error)
                    if matches!(error.kind, RequestErrorKind::Rpc | RequestErrorKind::Local) =>
                {
                    self.model.submit_failed(&request_id, error.message.clone());
                    if let Some(snapshot) = self.submit_backups.get(&request_id).cloned() {
                        self.restore_draft(&request_id, &snapshot);
                    }
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
            Pending::AttachmentBegin { receipt_id } => match result {
                Ok(_) => self.send_next_chunk(&receipt_id),
                Err(error) => self.fail_upload(&receipt_id, error),
            },
            Pending::AttachmentChunk {
                receipt_id,
                sequence,
            } => match result {
                Ok(value) => {
                    let received = value
                        .get("received")
                        .and_then(Value::as_u64)
                        .unwrap_or_default();
                    self.update_upload_progress(&receipt_id, received);
                    if let Some(job) = self.uploads.get_mut(&receipt_id) {
                        job.sequence = sequence.saturating_add(1);
                    }
                    self.send_next_chunk(&receipt_id);
                }
                Err(error) => self.fail_upload(&receipt_id, error),
            },
            Pending::AttachmentFinish { receipt_id } => match result {
                Ok(value) => self.finish_upload(&receipt_id, &value),
                Err(error) => self.fail_upload(&receipt_id, error),
            },
            Pending::FileReferences {
                revision,
                protocol_id,
            } => {
                if self.completion_cancel.as_deref() == Some(protocol_id.as_str()) {
                    self.completion_cancel = None;
                }
                let current = revision == self.completion.revision();
                match result {
                    Ok(value) => match parse_candidates(&value) {
                        Ok(candidates) => {
                            self.completion.apply(revision, candidates);
                        }
                        Err(message) if current => {
                            self.completion.clear();
                            self.error = Some(format!("文件补全结果无效：{message}"));
                        }
                        Err(_) => {}
                    },
                    Err(error) if matches!(error.kind, RequestErrorKind::Unknown) => {
                        if current {
                            self.completion.clear();
                        }
                    }
                    Err(error) => {
                        if current {
                            self.completion.clear();
                            if !error.message.contains("cancelled") {
                                self.error = Some(format!("文件补全失败：{}", error.message));
                            }
                        }
                    }
                }
            }
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

    /// Address value for S06 request params.
    fn address_value(&self) -> Value {
        self.model.address().wire_value()
    }

    /// Whether one capability is composed and usable now.
    fn capability_supported(&self, name: &str) -> bool {
        self.hello
            .as_ref()
            .and_then(|hello| hello.capabilities.get(name))
            .is_some_and(|capability| capability.status == CapabilityStatus::Supported)
    }

    fn attachments_supported(&self) -> bool {
        self.capability_supported("attachments")
    }

    fn file_references_supported(&self) -> bool {
        self.capability_supported("fileReferences")
    }

    /// Shift attachment anchors after one text edit.
    fn shift_attachment_anchors(&mut self, old: &str, new: &str) {
        if old == new {
            return;
        }
        let prefix = common_prefix_bytes(old, new);
        let removed = old.len().saturating_sub(prefix);
        let inserted = new.len().saturating_sub(prefix);
        let suffix = common_suffix_bytes(&old[prefix..], &new[prefix..]);
        let removed = removed.saturating_sub(suffix);
        let inserted = inserted.saturating_sub(suffix);
        self.attachments
            .current_mut()
            .shift_for_edit(prefix, removed, inserted);
    }

    /// Ask the host for `@file` candidates for the current token.
    fn refresh_completion(&mut self) {
        if !self.ready || self.edit_target.is_some() || !self.file_references_supported() {
            self.cancel_completion();
            return;
        }
        let text = self.drafts.current().text().to_string();
        let cursor = self.drafts.current().cursor();
        let update = self.completion.update(&text, cursor);
        if update.cancel.is_some() {
            self.cancel_completion_request();
        }
        let Some(request) = update.request else {
            return;
        };
        let Some(link) = &self.link else {
            return;
        };
        let params = list_params(&self.address_value(), &request.query);
        match link.request_handle("nano.fileReferences.list", params) {
            Ok(handle) => {
                self.completion.set_pending(handle.id, request.revision);
                self.completion_cancel = Some(handle.protocol_id.clone());
                self.pending.insert(
                    handle.id,
                    Pending::FileReferences {
                        revision: request.revision,
                        protocol_id: handle.protocol_id,
                    },
                );
            }
            Err(message) => {
                self.completion.clear();
                self.error = Some(format!("文件补全请求失败：{message}"));
            }
        }
    }

    /// Cancel the in-flight file-reference request without changing the popup.
    fn cancel_completion_request(&mut self) {
        if let Some(protocol_id) = self.completion_cancel.take() {
            if let Some(link) = &self.link {
                let _ = link.cancel_request(&protocol_id);
            }
        }
    }

    /// Dismiss the popup and cancel its pending request.
    fn cancel_completion(&mut self) {
        let _ = self.completion.dismiss();
        self.cancel_completion_request();
    }

    /// Insert the selected `@file` candidate.
    fn accept_completion(&mut self) {
        let text = self.drafts.current().text().to_string();
        let cursor = self.drafts.current().cursor();
        let Some(accepted) = self.completion.accept(&text, cursor) else {
            self.cancel_completion();
            return;
        };
        self.drafts.current_mut().set_text(accepted.text.clone());
        self.drafts.current_mut().set_cursor(accepted.cursor);
        self.shift_attachment_anchors(&text, &accepted.text);
        if accepted.directory {
            // A directory mention keeps its quote open for another level.
            self.refresh_completion();
        } else {
            // A file mention is complete; dismiss without reopening the same
            // query until the user edits the token.
            self.cancel_completion();
        }
    }

    /// Open the attachment path picker.
    fn open_picker(&mut self) {
        if self.edit_target.is_some() {
            self.error = Some("编辑队列项时不能附加文件".to_string());
            return;
        }
        if !self.attachments_supported() {
            self.error = Some("宿主未提供附件服务；图片和文件不可用".to_string());
            return;
        }
        self.picker = Some(AttachmentPicker::default());
        self.notice = Some("输入文件路径，Enter 附加，Esc 取消".to_string());
    }

    /// Handle one key while the attachment picker is open.
    fn handle_picker_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc => {
                self.picker = None;
                self.notice = Some("已取消附加文件".to_string());
                true
            }
            KeyCode::Enter => {
                let path = self
                    .picker
                    .as_ref()
                    .map(|picker| picker.path.trim().to_string())
                    .unwrap_or_default();
                if path.is_empty() {
                    self.error = Some("附件路径为空".to_string());
                    return true;
                }
                match self.attach_path(&path) {
                    Ok(()) => {
                        self.picker = None;
                        true
                    }
                    Err(message) => {
                        self.error = Some(message);
                        true
                    }
                }
            }
            KeyCode::Backspace => {
                if let Some(picker) = &mut self.picker {
                    picker.path.pop();
                }
                true
            }
            KeyCode::Char(character)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                if let Some(picker) = &mut self.picker {
                    picker.path.push(character);
                }
                true
            }
            _ => false,
        }
    }

    /// Resolve a picker path against the host working directory.
    fn resolve_path(&self, raw: &str) -> PathBuf {
        let path = if raw == "~" || raw.starts_with("~/") {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default();
            if raw == "~" {
                home
            } else {
                home.join(&raw[2..])
            }
        } else {
            PathBuf::from(raw)
        };
        if path.is_absolute() {
            return path;
        }
        let base = self
            .hello
            .as_ref()
            .map(|hello| PathBuf::from(&hello.cwd))
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        base.join(path)
    }

    /// Inspect, validate, and start staging one local file.
    fn attach_path(&mut self, raw: &str) -> Result<(), String> {
        if !self.ready {
            return Err("宿主未就绪；草稿已保留".to_string());
        }
        if !self.attachments_supported() {
            return Err("宿主未提供附件服务".to_string());
        }
        let path = self.resolve_path(raw);
        let metadata = std::fs::metadata(&path)
            .map_err(|error| format!("无法读取「{}」：{error}", path.display()))?;
        if !metadata.is_file() {
            return Err("只能附加普通文件".to_string());
        }
        let total = metadata.len();
        let declared = ImageMediaType::from_extension(&path);
        let mut header = [0u8; 16];
        let mut probe_file = File::open(&path)
            .map_err(|error| format!("无法打开「{}」：{error}", path.display()))?;
        let read = probe_file
            .read(&mut header)
            .map_err(|error| format!("无法读取「{}」：{error}", path.display()))?;
        let looks_image = probe_image(&header[..read]).is_ok();
        let name = nano_attachments::wire::display_name(&path);
        if declared.is_some() || looks_image {
            return self.attach_image(path, name, total, declared);
        }
        if let Some(limits) = &self.attachment_limits {
            if limits.file_admission == "unsupported" {
                return Err("当前部署不支持通用文件附件".to_string());
            }
        }
        let file = File::open(&path)
            .map_err(|error| format!("无法打开「{}」：{error}", path.display()))?;
        let receipt_id = nano_conversation::new_request_id();
        let cursor = self.drafts.current().cursor();
        self.attachments.current_mut().insert(
            cursor,
            AttachmentCard {
                receipt_id: receipt_id.clone(),
                kind: AttachmentKind::File,
                name: name.clone(),
                bytes: total,
                media_type: None,
                width: None,
                height: None,
                state: AttachmentState::Uploading { sent: 0, total },
            },
        );
        self.uploads.insert(
            receipt_id.clone(),
            UploadJob {
                kind: AttachmentKind::File,
                image_bytes: None,
                file: Some(file),
                total,
                offset: 0,
                sequence: 0,
                finishing: false,
            },
        );
        self.start_upload(&receipt_id);
        self.notice = Some(format!("正在附加文件「{name}」"));
        Ok(())
    }

    fn attach_image(
        &mut self,
        path: PathBuf,
        name: String,
        total: u64,
        declared: Option<ImageMediaType>,
    ) -> Result<(), String> {
        let limits = self
            .attachment_limits
            .as_ref()
            .ok_or_else(|| "宿主未报告图片限额，无法安全检查图片".to_string())?;
        if total > limits.max_image_bytes {
            return Err(ImageLimitError::TooLarge {
                bytes: total as usize,
                limit: limits.max_image_bytes,
            }
            .to_string());
        }
        let bytes = std::fs::read(&path)
            .map_err(|error| format!("无法读取「{}」：{error}", path.display()))?;
        let probe = probe_image(&bytes).map_err(|error| match error {
            ImageProbeError::Unsupported => "不支持的图片格式（仅 PNG/JPEG/WebP/GIF）".to_string(),
            ImageProbeError::Corrupt(detail) => format!("图片已损坏：{detail}"),
        })?;
        check_image(declared, &probe, bytes.len(), limits).map_err(|error| error.to_string())?;
        let cards = self.attachments.current().cards();
        let image_count = cards
            .iter()
            .filter(|card| card.kind == AttachmentKind::Image)
            .count() as u64;
        if image_count + 1 > limits.max_images_per_message {
            return Err(format!(
                "每条消息最多 {} 张图片",
                limits.max_images_per_message
            ));
        }
        let image_bytes: u64 = cards
            .iter()
            .filter(|card| card.kind == AttachmentKind::Image)
            .map(|card| card.bytes)
            .sum();
        if image_bytes + bytes.len() as u64 > limits.max_message_image_bytes {
            return Err("图片总字节超过部署上限".to_string());
        }
        let receipt_id = nano_conversation::new_request_id();
        let cursor = self.drafts.current().cursor();
        let media_type = probe.media_type.as_str().to_string();
        self.attachments.current_mut().insert(
            cursor,
            AttachmentCard {
                receipt_id: receipt_id.clone(),
                kind: AttachmentKind::Image,
                name: name.clone(),
                bytes: bytes.len() as u64,
                media_type: Some(media_type),
                width: Some(probe.width),
                height: Some(probe.height),
                state: AttachmentState::Uploading { sent: 0, total },
            },
        );
        self.uploads.insert(
            receipt_id.clone(),
            UploadJob {
                kind: AttachmentKind::Image,
                image_bytes: Some(bytes),
                file: None,
                total,
                offset: 0,
                sequence: 0,
                finishing: false,
            },
        );
        self.start_upload(&receipt_id);
        self.notice = Some(format!("正在附加图片「{name}」"));
        Ok(())
    }

    /// Begin one staging upload.
    fn start_upload(&mut self, receipt_id: &str) {
        let Some(job) = self.uploads.get(receipt_id) else {
            return;
        };
        let kind = job.kind;
        let total = job.total;
        let Some(card) = self.attachments.current().card(receipt_id) else {
            return;
        };
        let name = card.name.clone();
        let media_type = card.media_type.clone();
        let params = begin_params(
            &self.address_value(),
            receipt_id,
            kind,
            Some(&name),
            media_type.as_deref(),
            total,
        );
        let Some(link) = &self.link else {
            self.fail_upload_message(receipt_id, "宿主连接已关闭".to_string());
            return;
        };
        match link.request_handle("nano.attachment.begin", params) {
            Ok(handle) => {
                self.pending.insert(
                    handle.id,
                    Pending::AttachmentBegin {
                        receipt_id: receipt_id.to_string(),
                    },
                );
            }
            Err(message) => self.fail_upload_message(receipt_id, message),
        }
    }

    /// Send the next staging chunk, or finish when every byte was sent.
    fn send_next_chunk(&mut self, receipt_id: &str) {
        let max_bytes = self.chunk_bytes();
        let next = match self.uploads.get_mut(receipt_id) {
            Some(job) if !job.finishing => match job.next_chunk(max_bytes) {
                Ok(next) => next,
                Err(error) => {
                    self.fail_upload_message(receipt_id, format!("读取文件失败：{error}"));
                    return;
                }
            },
            _ => return,
        };
        let Some(link) = &self.link else {
            self.fail_upload_message(receipt_id, "宿主连接已关闭".to_string());
            return;
        };
        match next {
            Some(bytes) => {
                let sequence = self
                    .uploads
                    .get(receipt_id)
                    .map(|job| job.sequence)
                    .unwrap_or_default();
                let encoded = encode_base64(&bytes);
                match link.request_handle(
                    "nano.attachment.chunk",
                    chunk_params(receipt_id, sequence, &encoded),
                ) {
                    Ok(handle) => {
                        self.pending.insert(
                            handle.id,
                            Pending::AttachmentChunk {
                                receipt_id: receipt_id.to_string(),
                                sequence,
                            },
                        );
                    }
                    Err(message) => self.fail_upload_message(receipt_id, message),
                }
            }
            None => {
                if let Some(job) = self.uploads.get_mut(receipt_id) {
                    job.finishing = true;
                }
                match link.request_handle("nano.attachment.finish", finish_params(receipt_id)) {
                    Ok(handle) => {
                        self.pending.insert(
                            handle.id,
                            Pending::AttachmentFinish {
                                receipt_id: receipt_id.to_string(),
                            },
                        );
                    }
                    Err(message) => self.fail_upload_message(receipt_id, message),
                }
            }
        }
    }

    fn chunk_bytes(&self) -> usize {
        self.attachment_limits
            .as_ref()
            .map(|limits| limits.max_chunk_bytes)
            .unwrap_or(49_152)
            .min(49_152) as usize
    }

    fn update_upload_progress(&mut self, receipt_id: &str, received: u64) {
        let total = self
            .uploads
            .get(receipt_id)
            .map(|job| job.total)
            .unwrap_or_default();
        if let Some(card) = self.attachments.current_mut().card_mut(receipt_id) {
            card.state = AttachmentState::Uploading {
                sent: received,
                total,
            };
        }
    }

    fn finish_upload(&mut self, receipt_id: &str, value: &Value) {
        self.uploads.remove(receipt_id);
        let finished = match parse_finished(value) {
            Ok(finished) => finished,
            Err(message) => {
                self.fail_upload_message(receipt_id, message);
                return;
            }
        };
        let Some(card) = self.attachments.current_mut().card_mut(receipt_id) else {
            return;
        };
        card.state = AttachmentState::Ready;
        if let Some(file) = finished.file {
            card.name = file.name;
            card.bytes = file.bytes;
        }
        let name = card.name.clone();
        self.notice = Some(format!("附件「{name}」已就绪"));
    }

    fn fail_upload(&mut self, receipt_id: &str, error: RequestError) {
        let message = match error.kind {
            RequestErrorKind::Unknown => {
                self.set_upload_state(
                    receipt_id,
                    AttachmentState::Unknown(format!("{}；不会自动重发", error.message)),
                );
                return;
            }
            _ => error.message,
        };
        self.fail_upload_message(receipt_id, message);
    }

    fn fail_upload_message(&mut self, receipt_id: &str, message: String) {
        self.uploads.remove(receipt_id);
        self.set_upload_state(receipt_id, AttachmentState::Failed(message.clone()));
        if let Some(link) = &self.link {
            // A definite failure can drop any staging the host still holds.
            let _ = link.request("nano.attachment.discard", discard_params(receipt_id));
        }
        self.error = Some(format!("附件上传失败：{message}"));
    }

    fn set_upload_state(&mut self, receipt_id: &str, state: AttachmentState) {
        if let Some(card) = self.attachments.current_mut().card_mut(receipt_id) {
            card.state = state;
        }
    }

    /// Mark every in-flight upload as unknown after the transport ends.
    fn mark_uploads_unknown(&mut self, reason: &str) {
        let ids: Vec<String> = self.uploads.keys().cloned().collect();
        for receipt_id in ids {
            self.set_upload_state(&receipt_id, AttachmentState::Unknown(reason.to_string()));
        }
        self.uploads.clear();
        self.completion.clear();
        self.completion_cancel = None;
    }

    /// Reorder the attachment nearest the cursor. Only anchors move; uploaded
    /// bytes are not sent again.
    fn move_attachment(&mut self, delta: isize) {
        let cursor = self.drafts.current().cursor();
        if self
            .attachments
            .current_mut()
            .move_at_cursor(cursor, delta)
            .is_some()
        {
            self.notice = Some("已调整附件顺序（只影响本次列表）".to_string());
        } else {
            self.notice = Some("没有可移动的附件".to_string());
        }
    }

    /// Remove the nearest attachment at or before the cursor.
    fn remove_attachment_before_cursor(&mut self) {
        let cursor = self.drafts.current().cursor();
        let removed = self.attachments.current_mut().remove_before(cursor);
        let Some(card) = removed else {
            self.notice = Some("没有可移除的附件".to_string());
            return;
        };
        self.uploads.remove(&card.receipt_id);
        if let Some(link) = &self.link {
            let _ = link.request("nano.attachment.discard", discard_params(&card.receipt_id));
        }
        self.notice = Some(format!("已移除本地卡片「{}」", card.name));
    }

    fn restore_draft(&mut self, request_id: &str, submitted: &SubmittedDraft) {
        if self.submit_backups.remove(request_id).is_none() {
            return;
        }
        if self.edit_target.is_some() {
            // Do not merge a failed prompt into a queue edit; the queue text is
            // the draft the user is actively editing.
            self.error = Some(format!(
                "发送失败，原稿未自动合并（正在编辑队列项）：{}",
                submitted.text
            ));
            return;
        }
        let current = self.drafts.current().text().to_string();
        if current.is_empty() {
            self.drafts.current_mut().set_text(submitted.text.clone());
            *self.attachments.current_mut() = submitted.attachments.clone();
        } else if current == submitted.text || current.starts_with(&submitted.text) {
            // The user retyped or extended the same text; restore the failed
            // attachments if the current draft has none.
            if self.attachments.current().is_empty() {
                *self.attachments.current_mut() = submitted.attachments.clone();
            }
        } else {
            // The user typed while the request was in flight. Keep both the
            // failed draft and the new input, with the failed attachments back
            // at their original offsets.
            let prefix = format!("{}\n\n", submitted.text);
            let prefix_len = prefix.len();
            self.attachments.current_mut().shift_all(prefix_len);
            self.attachments
                .current_mut()
                .extend_from(&submitted.attachments);
            self.drafts
                .current_mut()
                .set_text(format!("{prefix}{current}"));
        }
    }

    /// Route the current input and submit it when it is an ordinary prompt.
    pub fn submit_draft(&mut self, delivery: Delivery) {
        let text = self.drafts.current().text().to_string();
        let has_attachments = !self.attachments.current().is_empty();
        if text.trim().is_empty() && !has_attachments {
            self.error = Some("内容为空，草稿已保留".to_string());
            return;
        }
        // Commands, skills, and `!` are routed only for text-only input; an
        // attachment makes this an ordinary prompt.
        if !has_attachments {
            let route =
                CommandRouter::new(&self.registry, self.scene.active().clone()).route(&text);
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
        }
        if !self.ready {
            self.error = Some("宿主未就绪；草稿已保留".to_string());
            return;
        }
        if self.stop_pending {
            self.error = Some("正在停止，等待官方 idle；草稿已保留".to_string());
            return;
        }
        if let Err(blocker) = self.attachments.current().ready() {
            self.error = Some(blocker.message());
            return;
        }
        let content = self.attachments.current().content(&text);
        match self.model.begin_submit_content(&content, delivery) {
            Ok(plan) => {
                let snapshot = SubmittedDraft {
                    text: text.clone(),
                    attachments: self.attachments.current().clone(),
                };
                self.drafts.current_mut().take();
                self.attachments.current_mut().clear();
                self.completion.clear();
                self.submit_backups
                    .insert(plan.request_id.clone(), snapshot);
                let Some(link) = &self.link else {
                    self.model.submit_failed(&plan.request_id, "宿主连接已关闭");
                    if let Some(snapshot) = self.submit_backups.get(&plan.request_id).cloned() {
                        self.restore_draft(&plan.request_id, &snapshot);
                    }
                    self.error = Some("发送失败：宿主连接已关闭".to_string());
                    return;
                };
                match link.request("nano.conversation.submit", plan.params) {
                    Ok(id) => {
                        self.pending.insert(
                            id,
                            Pending::Submit {
                                request_id: plan.request_id,
                            },
                        );
                    }
                    Err(message) => {
                        self.model.submit_failed(&plan.request_id, message.clone());
                        if let Some(snapshot) = self.submit_backups.get(&plan.request_id).cloned() {
                            self.restore_draft(&plan.request_id, &snapshot);
                        }
                        self.error = Some(format!("发送失败：{message}"));
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
        self.completion.clear();
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
        "Enter 发送 · Alt+Enter 插话 · @ 引用文件 · Alt+A 附加 · Alt+←→ 排序 · Alt+Backspace 移除附件 · Ctrl+J 换行 · Esc 停止 · Ctrl+C 退出"
            .to_string()
    }
}

impl App {
    /// Time-based work: rebuild a closed observable subscription.
    pub fn tick(&mut self) -> bool {
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

/// Longest common byte prefix ending on a character boundary.
fn common_prefix_bytes(left: &str, right: &str) -> usize {
    let mut prefix = 0;
    for ((left_offset, left_char), (right_offset, right_char)) in
        left.char_indices().zip(right.char_indices())
    {
        if left_char != right_char {
            break;
        }
        prefix = left_offset + left_char.len_utf8();
        let _ = right_offset;
    }
    prefix
}

/// Longest common byte suffix of two strings that starts on a character boundary.
fn common_suffix_bytes(left: &str, right: &str) -> usize {
    let mut left_chars = left.char_indices().rev();
    let mut right_chars = right.char_indices().rev();
    let mut suffix = 0;
    loop {
        match (left_chars.next(), right_chars.next()) {
            (Some((left_offset, left_char)), Some((right_offset, right_char)))
                if left_char == right_char =>
            {
                let left_end = left_offset + left_char.len_utf8();
                let right_end = right_offset + right_char.len_utf8();
                if left_end > left.len() || right_end > right.len() {
                    break;
                }
                suffix = left.len() - left_offset;
            }
            _ => break,
        }
    }
    suffix
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

    fn hello_with_attachments(max_image_bytes: u64) -> Hello {
        use nano_runtime::protocol::{
            AttachmentLimits, Capability, Limits, ProtocolVersion, SessionFactMode, SessionFacts,
            Versions,
        };
        use std::collections::BTreeMap;
        let mut capabilities = BTreeMap::new();
        for name in ["attachments", "fileReferences"] {
            capabilities.insert(
                name.to_string(),
                Capability {
                    status: CapabilityStatus::Supported,
                    mode: None,
                    detail: None,
                    required: None,
                },
            );
        }
        Hello {
            protocol: ProtocolVersion { major: 1, minor: 1 },
            runtime_id: "rt-test".into(),
            generation: 1,
            versions: Versions {
                official: "0.2.1-rc.2".into(),
                bridge: "0.1.0".into(),
                node: "v24.18.0".into(),
            },
            cwd: "/work".into(),
            root_session_id: "root".into(),
            session: SessionFacts {
                mode: SessionFactMode::Created,
                seq: 1,
                header: None,
            },
            capabilities,
            limits: Limits {
                max_frame_bytes: 33_554_432,
                max_pending_requests: 64,
                max_attachment_chunk_bytes: 49_152,
            },
            attachment_limits: Some(AttachmentLimits {
                max_image_bytes,
                max_images_per_message: 20,
                max_message_image_bytes: 200 * 1024 * 1024,
                max_image_pixels: 64_000_000,
                max_image_dimension: 8192,
                media_types: vec![
                    "image/png".into(),
                    "image/jpeg".into(),
                    "image/webp".into(),
                    "image/gif".into(),
                ],
                max_chunk_bytes: 49_152,
                file_admission: "verbatim".into(),
            }),
        }
    }

    fn png_fixture() -> Vec<u8> {
        let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        bytes.extend_from_slice(&[0, 0, 0, 13]);
        bytes.extend_from_slice(b"IHDR");
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
        bytes
    }

    fn ready_card(receipt_id: &str, kind: AttachmentKind) -> AttachmentCard {
        AttachmentCard {
            receipt_id: receipt_id.into(),
            kind,
            name: "pixel.png".into(),
            bytes: 33,
            media_type: Some("image/png".into()),
            width: Some(1),
            height: Some(1),
            state: AttachmentState::Ready,
        }
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
    fn a_submission_without_a_host_is_restored_as_unsent() {
        let mut app = app();
        app.ready = true;
        app.drafts.current_mut().set_text("hello");
        app.submit_draft(Delivery::Queue);
        assert_eq!(app.drafts.current().text(), "hello");
        // A definitive local failure retires the echo and restores the draft.
        assert!(app.model.live().echoes.is_empty());
        assert!(app.error.as_deref().unwrap().contains("发送失败"));
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
        let failed = SubmittedDraft {
            text: "failed text".into(),
            attachments: ComposerAttachments::default(),
        };
        app.submit_backups.insert("r-1".into(), failed.clone());
        app.restore_draft("r-1", &failed);
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
        app.submit_backups.insert(
            plan.request_id.clone(),
            SubmittedDraft {
                text: plan.text.clone(),
                attachments: ComposerAttachments::default(),
            },
        );
        app.pending.insert(
            9,
            Pending::Submit {
                request_id: plan.request_id.clone(),
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
        app.submit_backups.insert(
            plan.request_id.clone(),
            SubmittedDraft {
                text: plan.text.clone(),
                attachments: ComposerAttachments::default(),
            },
        );
        app.pending.insert(
            9,
            Pending::Submit {
                request_id: plan.request_id.clone(),
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
    fn a_stale_file_reference_error_does_not_clear_newer_candidates() {
        use nano_attachments::{CandidateKind, FileReferenceCandidate};
        let mut app = app();
        app.ready = true;
        let first = app.completion.update("see @sr", 7);
        let stale = first.request.unwrap().revision;
        let second = app.completion.update("see @srx", 8);
        let current = second.request.unwrap().revision;
        app.completion.apply(
            current,
            vec![FileReferenceCandidate {
                path: "src/lib.rs".into(),
                kind: CandidateKind::File,
            }],
        );
        app.pending.insert(
            7,
            Pending::FileReferences {
                revision: stale,
                protocol_id: "old".into(),
            },
        );
        app.handle_request_done(
            7,
            Err(RequestError {
                kind: RequestErrorKind::Rpc,
                message: "cancelled".into(),
            }),
        );
        assert!(app.completion.visible());
        assert_eq!(app.completion.candidates().len(), 1);
    }

    #[test]
    fn accepting_a_file_candidate_replaces_only_the_token() {
        use nano_attachments::{CandidateKind, FileReferenceCandidate};
        let mut app = app();
        app.drafts.current_mut().set_text("see @sr");
        let update = app.completion.update("see @sr", 7);
        let revision = update.request.unwrap().revision;
        app.completion.apply(
            revision,
            vec![FileReferenceCandidate {
                path: "src/lib.rs".into(),
                kind: CandidateKind::File,
            }],
        );
        app.accept_completion();
        assert_eq!(app.drafts.current().text(), "see @src/lib.rs");
        assert_eq!(app.drafts.current().cursor(), "see @src/lib.rs".len());
    }

    #[test]
    fn a_local_png_is_probed_and_staged_before_the_host_is_contacted() {
        let mut app = app();
        app.hello = Some(hello_with_attachments(20 * 1024 * 1024));
        app.attachment_limits = ImageLimits::from_hello(app.hello.as_ref().unwrap());
        app.ready = true;
        let dir = std::env::temp_dir().join(nano_conversation::new_request_id());
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pixel.png");
        std::fs::write(&path, png_fixture()).unwrap();
        app.attach_path(path.to_str().unwrap()).unwrap();
        let cards = app.attachments.current().cards();
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].kind, AttachmentKind::Image);
        assert_eq!(cards[0].width, Some(1));
        // There is no host link in this test, so the staged card is failed,
        // not silently dropped.
        assert!(matches!(cards[0].state, AttachmentState::Failed(_)));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_image_over_the_deployment_limit_is_refused_before_staging() {
        let mut app = app();
        app.hello = Some(hello_with_attachments(4));
        app.attachment_limits = ImageLimits::from_hello(app.hello.as_ref().unwrap());
        app.ready = true;
        let dir = std::env::temp_dir().join(nano_conversation::new_request_id());
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pixel.png");
        std::fs::write(&path, png_fixture()).unwrap();
        let error = app.attach_path(path.to_str().unwrap()).unwrap_err();
        assert!(error.contains("超过"), "{error}");
        assert!(app.attachments.current().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_failed_submission_restores_attachment_cards() {
        let mut app = app();
        app.ready = true;
        app.drafts.current_mut().set_text("look ");
        let cursor = app.drafts.current().cursor();
        app.attachments
            .current_mut()
            .insert(cursor, ready_card("r-img", AttachmentKind::Image));
        app.submit_draft(Delivery::Queue);
        assert_eq!(app.drafts.current().text(), "look ");
        let cards = app.attachments.current().cards();
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].receipt_id, "r-img");
        assert_eq!(app.attachments.current().anchors()[0].offset, cursor);
    }

    #[test]
    fn finishing_an_upload_during_a_queue_edit_keeps_the_card_ready() {
        let mut app = app();
        app.drafts.current_mut().set_text("original");
        queue_event(&mut app, "queued text");
        app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::ALT));
        assert_eq!(app.editing_queue(), Some("m-1"));
        let mut card = ready_card("r-img", AttachmentKind::Image);
        card.state = AttachmentState::Uploading { sent: 1, total: 2 };
        app.attachments.current_mut().insert(0, card);
        app.uploads.insert(
            "r-img".into(),
            UploadJob {
                kind: AttachmentKind::Image,
                image_bytes: None,
                file: None,
                total: 2,
                offset: 1,
                sequence: 1,
                finishing: true,
            },
        );
        app.finish_upload(
            "r-img",
            &serde_json::json!({
                "receiptId": "r-img",
                "kind": "image",
                "ready": true,
                "image": {"mediaType": "image/png", "bytes": 1}
            }),
        );
        assert!(app.uploads.is_empty());
        assert_eq!(
            app.attachments.current().card("r-img").unwrap().state,
            AttachmentState::Ready
        );
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.editing_queue(), None);
        assert_eq!(app.attachments.current().cards().len(), 1);
    }

    #[test]
    fn transport_loss_marks_uploading_cards_unknown() {
        let mut app = app();
        let mut card = ready_card("r-img", AttachmentKind::Image);
        card.state = AttachmentState::Uploading { sent: 1, total: 2 };
        app.attachments.current_mut().insert(0, card);
        app.uploads.insert(
            "r-img".into(),
            UploadJob {
                kind: AttachmentKind::Image,
                image_bytes: None,
                file: None,
                total: 2,
                offset: 1,
                sequence: 1,
                finishing: false,
            },
        );
        app.handle_link(LinkEvent::Host(nano_runtime::HostEvent::TransportClosed {
            lost: true,
        }));
        let card = app.attachments.current().cards()[0].clone();
        assert!(matches!(card.state, AttachmentState::Unknown(_)));
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
