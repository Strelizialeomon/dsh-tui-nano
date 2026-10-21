//! Application state and key handling.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use nano_controls::{ControlsModel, ModelSelection, Panel, PanelAction, PanelMode};
use nano_conversation::{
    CommandRouter, ConversationModel, Delivery, DraftStore, FeatureContribution, FeatureRegistry,
    FocusId, InputBuffer, InputEvent, InputOutcome, InputRoute, OrderedContent, SceneId,
    SceneState, SessionAddress, SubmitError,
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
    /// Read the official controls baseline.
    ControlsState,
    /// Refresh the official model catalog.
    ControlsCatalog,
    /// Switch one session model route.
    ControlsSelectModel {
        /// Route the user asked for.
        requested: ModelSelection,
    },
    /// Switch one permission preset through the official command path.
    ControlsSetPermission {
        /// Preset the user asked for.
        preset: String,
    },
    /// Execute one official command line.
    ControlsCommand {
        /// Complete slash line.
        line: String,
        /// Name used in status messages.
        name: String,
    },
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
    /// Model, effort, permission, and command facts from the official services.
    pub controls: ControlsModel,
    /// Command palette and selection panes.
    pub panel: Panel,
}

impl App {
    /// Create the app before the host is ready.
    pub fn new(cli: Cli, root_session_id: String, replay_truncated: bool) -> Self {
        let address = SessionAddress::root(root_session_id);
        let mut registry = FeatureRegistry::new();
        registry.register(
            FeatureContribution::new("conversation", SceneId::conversation()).with_key("enter"),
        );
        // S04 owns slash-command routing. The official command list replaces
        // this contribution once the handshake and the first state read land.
        registry.register(
            FeatureContribution::new("controls", SceneId::conversation())
                .with_command("/model")
                .with_command("/permission")
                .with_key("ctrl+p"),
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
            controls: ControlsModel::new(),
            panel: Panel::new(),
        }
    }

    /// Attach the host link.
    pub fn attach_link(&mut self, link: HostLink) {
        self.link = Some(link);
    }

    /// Ctrl+P: open the command palette, or close the current pane.
    pub fn toggle_command_palette(&mut self) {
        if self.panel.is_open() {
            self.panel.close();
            return;
        }
        self.clear_transient_status();
        self.panel.open(PanelMode::Commands);
    }

    /// Publish the current command prefixes to the S02 scene registry. Official
    /// commands claim their prefixes so a same-name skill can never shadow
    /// them; `/model` and `/permission` remain reachable before the first read.
    fn register_controls_features(&mut self) {
        let mut contribution = FeatureContribution::new("controls", SceneId::conversation())
            .with_command("/model")
            .with_command("/permission")
            .with_key("ctrl+p");
        for prefix in self.controls.registry_prefixes() {
            contribution = contribution.with_command(prefix);
        }
        self.registry.replace(contribution);
    }

    /// Re-read the official controls baseline.
    pub fn refresh_controls_state(&mut self) {
        if !self.ready {
            return;
        }
        let Some(link) = &self.link else {
            return;
        };
        match link.request("nano.controls.state", serde_json::json!({})) {
            Ok(id) => {
                self.pending.insert(id, Pending::ControlsState);
            }
            Err(message) => {
                self.controls.note_state_failure(message.clone());
                self.error = Some(format!("读取控制状态失败：{message}"));
            }
        }
    }

    /// Refresh the official model catalog. Opening the model pane always
    /// refreshes; a failure keeps the last catalog marked possibly stale.
    pub fn refresh_controls_catalog(&mut self) {
        if !self.ready {
            return;
        }
        let Some(link) = &self.link else {
            return;
        };
        match link.request("nano.controls.modelCatalog", serde_json::json!({})) {
            Ok(id) => {
                self.pending.insert(id, Pending::ControlsCatalog);
            }
            Err(message) => {
                self.controls.note_catalog_failure(message.clone());
                self.error = Some(format!("模型目录刷新失败：{message}（保留上次值，可能过期）"));
            }
        }
    }

    /// Open the model pane and refresh its catalog.
    pub fn open_models_panel(&mut self) {
        self.clear_transient_status();
        if self.panel.is_open() {
            self.panel.set_mode(PanelMode::Models);
        } else {
            self.panel.open(PanelMode::Models);
        }
        self.refresh_controls_catalog();
    }

    /// Open the permission pane and re-read the official projection.
    pub fn open_permissions_panel(&mut self) {
        self.clear_transient_status();
        if self.panel.is_open() {
            self.panel.set_mode(PanelMode::Permissions);
        } else {
            self.panel.open(PanelMode::Permissions);
        }
        self.refresh_controls_state();
    }

    /// Keys for the command palette and selection panes.
    fn handle_panel_key(&mut self, key: KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL)
            || key.modifiers.contains(KeyModifiers::ALT)
        {
            return false;
        }
        match key.code {
            KeyCode::Esc => {
                self.panel.back();
                true
            }
            KeyCode::Up => {
                self.panel.move_up();
                true
            }
            KeyCode::Down => {
                let rows = self.panel.rows(&self.controls).len();
                self.panel.move_down(rows);
                true
            }
            KeyCode::Backspace => {
                self.panel.backspace();
                true
            }
            KeyCode::Enter => {
                self.activate_panel_row();
                true
            }
            KeyCode::Char(character) if !character.is_control() => {
                self.panel.push_char(character);
                true
            }
            _ => false,
        }
    }

    /// Resolve the selected pane row.
    fn activate_panel_row(&mut self) {
        let rows = self.panel.rows(&self.controls);
        self.panel.clamp(rows.len());
        let Some(row) = rows.get(self.panel.selected()) else {
            return;
        };
        let action = match &row.disabled {
            Some(reason) => {
                self.error = Some(reason.clone());
                return;
            }
            None => row.action.clone(),
        };
        match action {
            PanelAction::OpenModels => self.open_models_panel(),
            PanelAction::OpenEfforts { provider, model } => {
                self.panel.set_mode(PanelMode::Efforts { provider, model });
            }
            PanelAction::OpenPermissions => self.open_permissions_panel(),
            PanelAction::SelectModel {
                provider,
                model,
                effort,
            } => {
                self.panel.close();
                self.select_model_route(&provider, &model, effort.as_deref());
            }
            PanelAction::SetPermission(preset) => {
                self.panel.close();
                self.set_permission_preset(&preset);
            }
            PanelAction::ExecuteCommand(line) => {
                self.panel.close();
                self.execute_controls_command(&line);
            }
            PanelAction::PrefillCommand(line) => {
                self.panel.close();
                self.drafts.current_mut().set_text(line);
                self.scene.focus_mut().reset(FocusId::input());
                self.notice = Some("已填入命令；补参数后按 Enter".to_string());
            }
            PanelAction::Close => self.panel.close(),
        }
    }

    /// Route one typed slash command owned by S04. The command business is
    /// always official: `/model` opens a view pane, `/permission <preset>` and
    /// every other registered command run through the official command path.
    fn dispatch_controls_line(&mut self, name: &str, args: &str, raw: &str) {
        if !self.ready {
            self.error = Some("宿主未就绪；命令未发送（草稿已保留）".to_string());
            return;
        }
        match name {
            "model" => {
                self.drafts.current_mut().take();
                self.open_models_panel();
            }
            "permission" => {
                self.drafts.current_mut().take();
                let preset = args.trim();
                if preset.is_empty() {
                    self.open_permissions_panel();
                } else {
                    self.set_permission_preset(preset);
                }
            }
            _ => {
                if self.controls.find_command(name).is_none() {
                    self.error = Some(format!(
                        "命令 /{name} 不在官方命令服务中；草稿已保留"
                    ));
                    return;
                }
                self.drafts.current_mut().take();
                self.execute_controls_command(raw.trim_end());
            }
        }
    }

    /// Ask the official session controller for one exact model route.
    pub fn select_model_route(&mut self, provider: &str, model: &str, effort: Option<&str>) {
        if !self.ready {
            self.error = Some("宿主未就绪".to_string());
            return;
        }
        let Some(link) = &self.link else {
            return;
        };
        let requested = ModelSelection {
            provider: provider.to_string(),
            model: model.to_string(),
            reasoning_effort: effort.map(str::to_string),
        };
        let params = self.controls.select_model_params(provider, model, effort);
        match link.request("nano.controls.selectModel", params) {
            Ok(id) => {
                self.pending
                    .insert(id, Pending::ControlsSelectModel { requested });
            }
            Err(message) => {
                self.error = Some(format!("模型切换失败：{message}"));
            }
        }
    }

    /// Ask the official `/permission <preset>` command path to switch presets.
    pub fn set_permission_preset(&mut self, preset: &str) {
        if !self.ready {
            self.error = Some("宿主未就绪".to_string());
            return;
        }
        let Some(link) = &self.link else {
            return;
        };
        match link.request(
            "nano.controls.setPermission",
            self.controls.set_permission_params(preset),
        ) {
            Ok(id) => {
                self.pending.insert(
                    id,
                    Pending::ControlsSetPermission {
                        preset: preset.to_string(),
                    },
                );
            }
            Err(message) => {
                self.error = Some(format!("权限写入失败：{message}"));
            }
        }
    }

    /// Execute one official command line.
    pub fn execute_controls_command(&mut self, line: &str) {
        if line.len() > nano_controls::MAX_COMMAND_LINE_BYTES {
            self.error = Some("命令过长；未发送".to_string());
            return;
        }
        if !self.ready {
            self.error = Some("宿主未就绪".to_string());
            return;
        }
        let name = line
            .trim_start_matches('/')
            .split_whitespace()
            .next()
            .unwrap_or(line)
            .to_string();
        let Some(link) = &self.link else {
            return;
        };
        match link.request(
            "nano.controls.executeCommand",
            self.controls.execute_command_params(line),
        ) {
            Ok(id) => {
                self.pending.insert(
                    id,
                    Pending::ControlsCommand {
                        line: line.to_string(),
                        name,
                    },
                );
            }
            Err(message) => {
                self.error = Some(format!("命令失败：{message}"));
            }
        }
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
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('p') | KeyCode::Char('P'))
        {
            self.toggle_command_palette();
            return true;
        }
        if self.panel.is_open() {
            return self.handle_panel_key(key);
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
            if self.edit_target.is_some() {
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
        if self.panel.is_open() || !self.scene.focus().input_focused() {
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
                self.ready = true;
                self.connection = Connection::Ready;
                self.replay_truncated = self.replay_truncated || truncated;
                self.model
                    .apply_replay_baseline(hello.session.seq, truncated);
                self.controls.set_capabilities(
                    capability_supported(&hello, "modelSelection"),
                    capability_supported(&hello, "permissionPresets"),
                    capability_supported(&hello, "commands"),
                );
                self.register_controls_features();
                self.hello = Some(hello);
                self.notice = Some("已连接官方会话".to_string());
                self.refresh_snapshot();
                self.refresh_controls_state();
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
                self.controls.reset();
                self.register_controls_features();
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
                let controls_changed = self.controls.apply_event(&params.kind, &params.payload);
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
                changed || controls_changed
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
                self.controls.reset();
                self.register_controls_features();
                true
            }
            HostEvent::ProtocolFailure(message) => {
                self.ready = false;
                self.connection = Connection::Failed(message.clone());
                self.error = Some(format!("协议错误：{message}"));
                self.controls.reset();
                self.register_controls_features();
                true
            }
            HostEvent::StartupError(params) => {
                self.ready = false;
                self.connection = Connection::Failed(params.message.clone());
                self.error = Some(params.message);
                self.controls.reset();
                self.register_controls_features();
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
                    | Pending::ControlsState
                    | Pending::ControlsCatalog
                    | Pending::ControlsSelectModel { .. }
                    | Pending::ControlsSetPermission { .. }
                    | Pending::ControlsCommand { .. }
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
            Pending::ControlsState => match result {
                Ok(value) => match self.controls.apply_state(&value) {
                    Ok(()) => {
                        self.register_controls_features();
                    }
                    Err(message) => {
                        self.controls.note_state_failure(message.clone());
                        self.error = Some(format!("控制状态无效：{message}"));
                    }
                },
                Err(error) => {
                    self.controls.note_state_failure(error.message.clone());
                    self.error = Some(format!("读取控制状态失败：{}", error.message));
                }
            },
            Pending::ControlsCatalog => match result {
                Ok(value) => match self.controls.apply_catalog(&value) {
                    Ok(()) => {
                        if self.controls.catalog().is_some_and(|catalog| catalog.is_empty()) {
                            self.notice =
                                Some("官方模型目录为空；保留上次选择与原因".to_string());
                        }
                    }
                    Err(message) => {
                        self.controls.note_catalog_failure(message.clone());
                        self.error = Some(format!("模型目录无效：{message}"));
                    }
                },
                Err(error) => {
                    self.controls.note_catalog_failure(error.message.clone());
                    self.error = Some(format!(
                        "模型目录刷新失败：{}（保留上次值，可能过期）",
                        error.message
                    ));
                }
            },
            Pending::ControlsSelectModel { requested } => match result {
                Ok(value) => match self.controls.apply_selection(&value) {
                    Ok(()) => {
                        self.notice = Some(format!(
                            "会话内模型已切换为 {}；新会话默认值由官方后台保存（本次回执不代表默认值已保存）",
                            requested.identity()
                        ));
                    }
                    Err(message) => {
                        self.error = Some(format!("模型切换回执无效：{message}"));
                        self.refresh_controls_state();
                    }
                },
                Err(error) => match error.kind {
                    RequestErrorKind::Unknown => {
                        self.error = Some(
                            "模型切换结果未知；正在重读官方投影，不自动重发".to_string(),
                        );
                        self.refresh_controls_state();
                    }
                    _ => {
                        let writer = if error.message.contains("writer-held") {
                            "；另一个窗口正在写这个会话，本窗口不接管"
                        } else {
                            ""
                        };
                        self.error = Some(format!("模型切换失败：{}{writer}", error.message));
                        self.refresh_controls_state();
                    }
                },
            },
            Pending::ControlsSetPermission { preset } => match result {
                Ok(value) => {
                    let executed = value
                        .get("executed")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    if !executed {
                        self.error =
                            Some("权限切换未执行：官方命令服务没有 /permission".to_string());
                        self.refresh_controls_state();
                        return;
                    }
                    match self.controls.apply_permission(value.get("permission")) {
                        Ok(()) => {
                            let kind = value.get("kind").and_then(Value::as_str);
                            let text = value.get("text").and_then(Value::as_str).unwrap_or("");
                            if kind == Some("error") {
                                self.error = Some(format!(
                                    "权限切换失败：{preset}（官方：{text}）；重读当前值：{}",
                                    self.controls.permission_label()
                                ));
                            } else {
                                self.notice = Some(format!(
                                    "权限已切换为 {}（官方回执；模型已收到 changed-by-user 通知）",
                                    self.controls.permission_label()
                                ));
                            }
                        }
                        Err(message) => {
                            self.error = Some(format!(
                                "权限回执缺少官方当前值：{message}；正在重读"
                            ));
                            self.refresh_controls_state();
                        }
                    }
                }
                Err(error) => match error.kind {
                    RequestErrorKind::Unknown => {
                        self.error = Some(
                            "权限写入结果未知；正在重读官方投影（可能已变、可能未变）"
                                .to_string(),
                        );
                        self.refresh_controls_state();
                    }
                    _ => {
                        self.error =
                            Some(format!("权限写入失败：{}；正在重读官方投影", error.message));
                        self.refresh_controls_state();
                    }
                },
            },
            Pending::ControlsCommand { line, name } => match result {
                Ok(value) => {
                    let executed = value
                        .get("executed")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    if !executed {
                        self.error = Some(format!("官方命令服务没有 {line}"));
                        return;
                    }
                    let kind = value.get("kind").and_then(Value::as_str);
                    let text = value.get("text").and_then(Value::as_str).unwrap_or("");
                    if kind == Some("error") {
                        self.error = Some(format!("命令 {name} 失败：{text}"));
                    } else if text.is_empty() {
                        self.notice = Some(format!("命令 {name} 已完成"));
                    } else {
                        self.notice = Some(format!("命令 {name}：{text}"));
                    }
                }
                Err(error) => match error.kind {
                    RequestErrorKind::Unknown => {
                        self.error = Some(format!(
                            "命令 {name} 结果未知（可能已执行）；不自动重发"
                        ));
                    }
                    _ => {
                        self.error = Some(format!("命令 {name} 失败：{}", error.message));
                    }
                },
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
            InputRoute::Command {
                owner,
                name,
                args,
                raw,
            } => {
                if owner == "controls" {
                    self.dispatch_controls_line(&name, &args, &raw);
                    return;
                }
                self.error = Some(format!("命令入口 {owner} 尚未接入；草稿已保留"));
                return;
            }
            InputRoute::Skill { .. } => {
                self.error = Some("技能入口（S05）尚未接入；草稿已保留".to_string());
                return;
            }
            InputRoute::Unregistered {
                owner: _,
                name,
                args,
                raw,
            } => {
                // Slash-command business belongs to S04. A name the official
                // registry advertises wins over a not-yet-registered skill.
                self.dispatch_controls_line(&name, &args, &raw);
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

    /// Bottom status: the resident permission fact plus the transient hint.
    pub fn status_line(&self) -> String {
        format!("权限 {} · {}", self.controls.permission_label(), self.status_hint())
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
        "Enter 发送/排队 · Alt+Enter 插话 · Ctrl+J 换行 · Esc 停止 · Ctrl+Y 复制原文 · Ctrl+C 退出"
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
            "controls.model",
            "controls.permission",
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

/// Whether the handshake reports one official capability as usable now.
fn capability_supported(hello: &Hello, name: &str) -> bool {
    hello
        .capabilities
        .get(name)
        .is_some_and(|capability| capability.status == CapabilityStatus::Supported)
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

    fn controls_state(value: serde_json::Value) -> App {
        let mut app = app();
        app.ready = true;
        app.pending.insert(1, Pending::ControlsState);
        app.handle_request_done(1, Ok(value));
        app
    }

    fn controls_state_value() -> serde_json::Value {
        json!({
            "address": {"rootSessionId": "root"},
            "model": {
                "lastUsed": {"provider": "p", "model": "chat"},
                "next": {"provider": "p", "model": "chat"}
            },
            "permission": {
                "current": "workspace-write",
                "catalog": {
                    "options": [
                        {"value": "workspace-write", "name": "Workspace", "sandbox": "workspace-write", "approval": "ask"},
                        {"value": "danger-full-access", "name": "Danger", "sandbox": "danger-full-access", "approval": "never"}
                    ],
                    "defaultOptions": [],
                    "defaultPreset": "workspace-write"
                }
            },
            "commands": [
                {"name": "compact", "description": "Compact"},
                {"name": "permission", "description": "Switch", "inputHint": "<preset>"}
            ]
        })
    }

    #[test]
    fn ctrl_p_opens_the_palette_and_escape_closes_it() {
        let mut app = app();
        assert!(app.handle_key(KeyEvent::new(
            KeyCode::Char('p'),
            KeyModifiers::CONTROL
        )));
        assert!(app.panel.is_open());
        assert_eq!(app.panel.mode(), &PanelMode::Commands);
        assert!(app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(!app.panel.is_open());
    }

    #[test]
    fn typing_slash_model_opens_the_model_pane_instead_of_sending() {
        let mut app = app();
        app.ready = true;
        app.drafts.current_mut().set_text("/model");
        assert!(app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        assert!(app.panel.is_open());
        assert_eq!(app.panel.mode(), &PanelMode::Models);
        assert!(app.drafts.current().is_empty());
        assert!(app.model.submissions().echoes().is_empty());
    }

    #[test]
    fn typing_bare_slash_permission_opens_the_permission_pane() {
        let mut app = app();
        app.ready = true;
        app.drafts.current_mut().set_text("/permission");
        assert!(app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        assert!(app.panel.is_open());
        assert_eq!(app.panel.mode(), &PanelMode::Permissions);
        assert!(app.drafts.current().is_empty());
    }

    #[test]
    fn a_command_needs_a_ready_host_and_keeps_the_draft_until_then() {
        let mut app = app();
        app.drafts.current_mut().set_text("/permission danger-full-access");
        assert!(app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        assert_eq!(
            app.drafts.current().text(),
            "/permission danger-full-access"
        );
        assert!(app.error.as_deref().unwrap().contains("宿主未就绪"));
    }

    #[test]
    fn an_unknown_command_keeps_the_draft_and_never_becomes_a_prompt() {
        let mut app = app();
        app.ready = true;
        app.drafts.current_mut().set_text("/nope");
        assert!(app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        assert_eq!(app.drafts.current().text(), "/nope");
        assert!(app.error.as_deref().unwrap().contains("不在官方命令服务中"));
        assert!(app.model.submissions().echoes().is_empty());
    }

    #[test]
    fn an_official_command_takes_the_draft_and_routes_to_s04() {
        let mut app = controls_state(controls_state_value());
        app.drafts.current_mut().set_text("/compact");
        assert!(app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        assert!(app.drafts.current().is_empty());
        assert!(app.model.submissions().echoes().is_empty());
    }

    #[test]
    fn a_controls_state_read_registers_official_command_prefixes() {
        let app = controls_state(controls_state_value());
        assert_eq!(app.controls.permission_label(), "workspace-write");
        let contribution = app
            .registry
            .command_owner(&SceneId::conversation(), "/compact")
            .expect("official command prefix is registered");
        assert_eq!(contribution.id.0, "controls");
        assert!(app
            .registry
            .command_owner(&SceneId::conversation(), "/permission")
            .is_some());
    }

    #[test]
    fn a_controls_state_event_updates_the_permission_label() {
        let mut app = app();
        let changed = app.handle_host_event(nano_runtime::HostEvent::Event(
            nano_runtime::protocol::EventParams {
                runtime_id: "rt".into(),
                generation: 1,
                subscription_id: "sub".into(),
                sequence: 0,
                kind: "controls.permission".into(),
                payload: json!({
                    "address": {"rootSessionId": "root"},
                    "permission": {
                        "current": "custom",
                        "catalog": {"options": [], "defaultOptions": [], "defaultPreset": "workspace-write"}
                    }
                }),
            },
        ));
        assert!(changed);
        assert_eq!(app.controls.permission_label(), "custom（非命名预设）");
        assert!(app.status_line().starts_with("权限 custom"));
    }

    #[test]
    fn a_writer_held_model_rejection_keeps_the_old_selection() {
        let mut app = controls_state(controls_state_value());
        app.pending.insert(
            2,
            Pending::ControlsSelectModel {
                requested: ModelSelection {
                    provider: "p".into(),
                    model: "reasoner".into(),
                    reasoning_effort: None,
                },
            },
        );
        app.handle_request_done(
            2,
            Err(RequestError {
                kind: RequestErrorKind::Rpc,
                message: "session/writer-held: another writer owns this session".into(),
            }),
        );
        assert_eq!(
            app.controls.model.next.as_ref().unwrap().model.as_str(),
            "chat"
        );
        assert!(app.error.as_deref().unwrap().contains("本窗口不接管"));
    }

    #[test]
    fn an_unknown_model_result_is_not_shown_as_unchanged_or_changed() {
        let mut app = controls_state(controls_state_value());
        app.pending.insert(
            3,
            Pending::ControlsSelectModel {
                requested: ModelSelection::new("p", "reasoner"),
            },
        );
        app.handle_request_done(
            3,
            Err(RequestError {
                kind: RequestErrorKind::Unknown,
                message: "transport closed".into(),
            }),
        );
        assert!(app.error.as_deref().unwrap().contains("结果未知"));
        assert_eq!(
            app.controls.model.next.as_ref().unwrap().model.as_str(),
            "chat"
        );
    }

    #[test]
    fn a_failed_catalog_refresh_keeps_the_last_catalog_marked_stale() {
        let mut app = app();
        app.ready = true;
        app.pending.insert(4, Pending::ControlsCatalog);
        app.handle_request_done(
            4,
            Ok(json!({
                "default": {"provider": "p", "model": "chat"},
                "routableProviders": ["p"],
                "groups": [{"id": "p", "name": "P", "models": [{"id": "chat", "name": "Chat"}]}],
                "failures": []
            })),
        );
        assert!(app.controls.catalog().is_some());
        app.pending.insert(5, Pending::ControlsCatalog);
        app.handle_request_done(
            5,
            Err(RequestError {
                kind: RequestErrorKind::Rpc,
                message: "catalog failed".into(),
            }),
        );
        assert!(app.controls.catalog().is_some());
        assert!(app.controls.catalog_stale());
        assert!(app.error.as_deref().unwrap().contains("可能过期"));
    }

    #[test]
    fn an_unknown_permission_result_is_reported_as_unknown() {
        let mut app = controls_state(controls_state_value());
        app.pending.insert(
            6,
            Pending::ControlsSetPermission {
                preset: "danger-full-access".into(),
            },
        );
        app.handle_request_done(
            6,
            Err(RequestError {
                kind: RequestErrorKind::Unknown,
                message: "transport closed".into(),
            }),
        );
        assert!(app.error.as_deref().unwrap().contains("结果未知"));
        assert_eq!(app.controls.permission_label(), "workspace-write");
    }

    #[test]
    fn a_rejected_permission_command_shows_the_reread_current_value() {
        let mut app = controls_state(controls_state_value());
        app.pending.insert(
            7,
            Pending::ControlsSetPermission {
                preset: "danger-full-access".into(),
            },
        );
        app.handle_request_done(
            7,
            Ok(json!({
                "executed": true,
                "commandId": "cmd-1",
                "kind": "error",
                "text": "denied",
                "permission": {
                    "current": "custom",
                    "catalog": {"options": [], "defaultOptions": [], "defaultPreset": "workspace-write"}
                }
            })),
        );
        assert!(app.error.as_deref().unwrap().contains("权限切换失败"));
        assert_eq!(app.controls.permission_label(), "custom（非命名预设）");
    }

    #[test]
    fn a_command_error_is_not_reported_as_completed() {
        let mut app = app();
        app.ready = true;
        app.pending.insert(
            8,
            Pending::ControlsCommand {
                line: "/compact".into(),
                name: "compact".into(),
            },
        );
        app.handle_request_done(
            8,
            Ok(json!({"executed": true, "commandId": "cmd-2", "kind": "error", "text": "nope"})),
        );
        assert!(app.error.as_deref().unwrap().contains("命令 compact 失败"));
        assert!(!app
            .notice
            .as_deref()
            .unwrap_or_default()
            .contains("已完成"));
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
