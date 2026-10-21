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
use nano_controls::{ControlsModel, ModelSelection, Panel, PanelAction, PanelMode};
use nano_conversation::{
    CommandRouter, ConversationItem, ConversationModel, Delivery, DraftStore, FeatureContribution,
    FeatureRegistry, FocusId, InputBuffer, InputEvent, InputOutcome, InputRoute,
    SceneId, SceneState, SessionAddress, SubmitError,
};
use nano_jobs::{BashRecord, JobsModel, SHELL_NOTICE};
use nano_runtime::{CapabilityStatus, Hello};
use nano_skills::{
    InstructionState, InvocationFact, InvocationTracker, NameResolution, SkillCatalog, SkillPreview,
};
use serde_json::{json, Value};

use crate::cli::Cli;
use crate::link::{HostLink, LinkEvent, RequestError, RequestErrorKind, SHELL_REQUEST_TIMEOUT};

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

/// Focus identity for the S05 skills panel.
fn skills_focus() -> FocusId {
    FocusId::new("skills")
}

/// Focus identity for the S05 workspace-instruction panel.
fn instructions_focus() -> FocusId {
    FocusId::new("instructions")
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
    /// Refresh the user-invocable skill catalog.
    SkillsList,
    /// Preview one skill's instruction file.
    SkillPreview {
        /// Skill name whose preview was requested.
        name: String,
    },
    /// Read the absolute workspace-instruction projection.
    InstructionsSnapshot,
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
    /// One manual `!` command.
    Shell {
        /// Local display record.
        record_id: u64,
    },
    /// A jobs baseline read.
    JobsSnapshot,
    /// A non-consuming output follow.
    JobsFollow {
        /// Official job id.
        job_id: String,
    },
    /// A human stop request.
    JobsKill {
        /// Official job id.
        job_id: String,
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
    /// Official task projections, never a second task table.
    pub jobs: JobsModel,
    /// Whether the task panel is open.
    pub jobs_open: bool,
    /// Lines scrolled back from the end of the open job detail.
    pub jobs_scroll_back: usize,
    /// Jobs whose follow request is in flight.
    pub following: std::collections::HashSet<String>,
    /// Jobs that produced output while a follow was in flight.
    pub follow_dirty: std::collections::HashSet<String>,
    /// A jobs snapshot request is in flight.
    pub jobs_snapshot_pending: bool,
    /// Line count of the last rendered job detail, for scroll bounds.
    pub jobs_detail_lines: usize,
    /// Local `!` records shown until the official synthetic message is durable.
    pub bash_records: Vec<BashRecord>,
    /// Local bash record id sequence.
    pub bash_counter: u64,
    /// Whether the first-use user-permission notice was acknowledged.
    pub shell_notice_acked: bool,
    /// Raw `!` line awaiting the first-use acknowledgement.
    pub shell_confirm: Option<String>,
    /// Model, effort, permission, and command facts from the official services.
    pub controls: ControlsModel,
    /// Command palette and selection panes.
    pub panel: Panel,
    /// Last-good skill catalog and command names.
    pub skills: SkillCatalog,
    /// Selected row in the skills panel.
    pub skill_selection: usize,
    /// One opened skill preview.
    pub skill_preview: Option<SkillPreview>,
    /// Why the last preview failed.
    pub skill_preview_error: Option<String>,
    /// Skill name whose preview is currently requested or shown.
    pub skill_preview_target: Option<String>,
    /// A catalog refresh is in flight.
    pub skills_refresh_in_flight: bool,
    /// A refresh arrived while one was in flight.
    pub skills_refresh_queued: bool,
    /// Official workspace-instruction projection.
    pub instructions: InstructionState,
    /// Scroll offset in the instructions panel, in lines.
    pub instructions_scroll: usize,
    /// Largest useful instructions scroll offset, measured at draw time.
    pub instructions_max_scroll: usize,
    /// Submitted `/name` facts until the official injection decision.
    pub invocation: InvocationTracker,
}

impl App {
    /// Create the app before the host is ready.
    pub fn new(cli: Cli, root_session_id: String, replay_truncated: bool) -> Self {
        let address = SessionAddress::root(root_session_id);
        let jobs_root = address.root_session_id.clone();
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
        registry.register(nano_skills::contribution());
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
            jobs: JobsModel::new(jobs_root),
            jobs_open: false,
            jobs_scroll_back: 0,
            following: std::collections::HashSet::new(),
            follow_dirty: std::collections::HashSet::new(),
            jobs_snapshot_pending: false,
            jobs_detail_lines: 0,
            bash_records: Vec::new(),
            bash_counter: 0,
            shell_notice_acked: false,
            shell_confirm: None,
            controls: ControlsModel::new(),
            panel: Panel::new(),
            skills: SkillCatalog::new(),
            skill_selection: 0,
            skill_preview: None,
            skill_preview_error: None,
            skill_preview_target: None,
            skills_refresh_in_flight: false,
            skills_refresh_queued: false,
            instructions: InstructionState::new(),
            instructions_scroll: 0,
            instructions_max_scroll: 0,
            invocation: InvocationTracker::new(),
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
        // Refresh so a command registered or removed since the last read is
        // reflected when the palette opens.
        self.refresh_controls_state();
    }

    /// Claim command prefixes in the S02 scene registry. Official commands
    /// must win over a same-name skill, so each prefix is claimed once when it
    /// first appears; stale prefixes stay claimed and are then reported as
    /// missing by the official command service, never as another owner's.
    fn claim_controls_commands(&mut self) {
        let scene = SceneId::conversation();
        let mut contribution = FeatureContribution::new("controls", scene.clone());
        let mut any = false;
        for prefix in ["/model".to_string(), "/permission".to_string()]
            .into_iter()
            .chain(self.controls.registry_prefixes())
        {
            if self.registry.command_owner(&scene, &prefix).is_none() {
                contribution = contribution.with_command(prefix);
                any = true;
            }
        }
        if any {
            self.registry.register(contribution);
        }
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
                if self.edit_target.is_some() {
                    self.error =
                        Some("正在编辑队列项；先应用或取消再选择命令".to_string());
                    return;
                }
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
        if raw.len() > nano_controls::MAX_COMMAND_LINE_BYTES {
            self.error = Some("命令过长；未发送（草稿已保留）".to_string());
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
                    let reason = if let Some(error) = self.controls.state_error() {
                        format!("命令列表未知（{error}）；/{name} 未发送，草稿已保留")
                    } else if !self.controls.commands_available() {
                        format!("官方命令服务未组合；/{name} 未发送，草稿已保留")
                    } else {
                        format!("命令 /{name} 不在官方命令服务中；草稿已保留")
                    };
                    self.error = Some(
                        if self.skills.resolve(name) == NameResolution::Command {
                            format!("/{name} 是宿主命令，命令优先；{reason}")
                        } else {
                            reason
                        },
                    );
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
            && matches!(key.code, KeyCode::Char('t') | KeyCode::Char('T'))
        {
            self.toggle_jobs();
            return true;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('p') | KeyCode::Char('P'))
        {
            self.toggle_command_palette();
            return true;
        }
        if self.jobs_open {
            return self.handle_jobs_key(key);
        }
        if self.panel.is_open() {
            return self.handle_panel_key(key);
        }
        // S05 panels own every key while focused, including PageUp/PageDown
        // and Tab; the feed/queue shortcuts must not leak underneath them.
        if self.skills_panel_open() {
            return self.handle_skills_key(key);
        }
        if self.instructions_panel_open() {
            return self.handle_instructions_key(key);
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
        // Only the top focus receives keys. The queue dock must not leak
        // shortcuts into the hidden input draft; S05 panels were handled above.
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
                KeyCode::Char('k') | KeyCode::Char('K') => {
                    self.open_skills_panel();
                    return true;
                }
                KeyCode::Char('r') | KeyCode::Char('R') => {
                    self.open_instructions_panel();
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
            if self.shell_confirm.take().is_some() {
                self.notice = Some("已取消人工命令；草稿保留".to_string());
            } else if self.edit_target.is_some() {
                self.cancel_queue_edit();
            } else {
                self.stop_turn();
            }
            return true;
        }
        if matches!(key.code, KeyCode::Enter) {
            // A held Enter must not send or satisfy the first-use gate by
            // auto-repeat; one physical press is one action.
            if key.kind == KeyEventKind::Repeat {
                return true;
            }
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
        // The task and control panels own the keyboard; they must own paste
        // too, or a paste would silently edit the hidden draft.
        if self.jobs_open
            || self.panel.is_open()
            || self.skills_panel_open()
            || self.instructions_panel_open()
            || !self.scene.focus().input_focused()
        {
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
                self.controls.set_capabilities(
                    capability_supported(&hello, "modelSelection"),
                    capability_supported(&hello, "permissionPresets"),
                    capability_supported(&hello, "commands"),
                );
                self.claim_controls_commands();
                self.hello = Some(hello);
                self.notice = Some("已连接官方会话".to_string());
                self.refresh_snapshot();
                self.refresh_jobs_snapshot();
                self.refresh_controls_state();
                self.refresh_skills();
                self.refresh_instructions();
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
                self.handle_host_loss();
                self.controls.reset();
                self.claim_controls_commands();
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
                    self.observe_invocation(&params.payload);
                }
                if params.kind == "skills.changed" {
                    // The official registry invalidated its catalog; only the
                    // signal is consumed. `refresh_skills` coalesces a signal
                    // that arrives while one request is already in flight.
                    self.refresh_skills();
                }
                if params.kind == "instructions.changed" {
                    let root = self.model.address().root_session_id.clone();
                    if let Err(error) = self.instructions.apply(&params.payload, &root) {
                        self.error = Some(format!("项目规矩投影无效：{error}"));
                    }
                }
                let changed = match params.kind.as_str() {
                    "jobs.event" => {
                        let previous_notice = self.jobs.last_notice().map(str::to_string);
                        let previous_error = self.jobs.last_error().map(str::to_string);
                        let changed = self.jobs.apply_event(&params.kind, &params.payload);
                        if self.jobs.last_notice().map(str::to_string) != previous_notice {
                            self.notice = self.jobs.last_notice().map(str::to_string);
                            self.jobs.clear_diagnostics();
                        }
                        if self.jobs.last_error().map(str::to_string) != previous_error {
                            self.error = self.jobs.last_error().map(str::to_string);
                            self.jobs.clear_diagnostics();
                        }
                        if !self.jobs.detail_open() {
                            self.jobs_scroll_back = 0;
                        }
                        if params.payload.get("type").and_then(Value::as_str) == Some("settled") {
                            if let Some(job_id) = self.jobs.detail_id().map(str::to_string) {
                                let settled_id = params
                                    .payload
                                    .get("job")
                                    .and_then(|job| job.get("id"))
                                    .and_then(Value::as_str);
                                if settled_id == Some(job_id.as_str()) {
                                    self.follow_job(&job_id);
                                }
                            }
                        }
                        changed
                    }
                    "jobs.output" => {
                        let changed = self.jobs.apply_event(&params.kind, &params.payload);
                        self.handle_jobs_output(&params.payload);
                        changed
                    }
                    _ => self.model.apply_event(&params.kind, &params.payload),
                };
                if params.kind == "session.event" {
                    self.prune_bash_records();
                }
                let controls_changed = self.controls.apply_event(&params.kind, &params.payload);
                if self.stop_pending
                    && params.kind == "conversation.status"
                    && params.payload.get("status").and_then(Value::as_str) == Some("idle")
                {
                    self.stop_pending = false;
                    self.notice = Some("官方已确认停止".to_string());
                }
                // Only the queue dock is dismissed by an empty inbox. S05
                // panels share the focus stack but are explicit overlays; a
                // stream chunk or a catalog invalidation must not close them.
                if self.model.inbox().is_empty()
                    && self.scene.focus().top() == Some(&FocusId::queue())
                {
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
                self.handle_host_loss();
                let root = &self.model.address().root_session_id;
                self.error = Some(if lost {
                    format!("宿主进程已退出（会话 {root}）；重启时用 --resume {root}")
                } else {
                    format!("通信已断开（会话 {root}）")
                });
                self.model.mark_disconnected();
                self.mark_uploads_unknown("连接断开");
                self.controls.reset();
                self.claim_controls_commands();
                true
            }
            HostEvent::ProtocolFailure(message) => {
                self.ready = false;
                self.connection = Connection::Failed(message.clone());
                self.error = Some(format!("协议错误：{message}"));
                self.handle_host_loss();
                self.controls.reset();
                self.claim_controls_commands();
                true
            }
            HostEvent::StartupError(params) => {
                self.ready = false;
                self.connection = Connection::Failed(params.message.clone());
                self.error = Some(params.message);
                self.handle_host_loss();
                self.controls.reset();
                self.claim_controls_commands();
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
                    | Pending::ControlsState
                    | Pending::ControlsCatalog
                    | Pending::ControlsSelectModel { .. }
                    | Pending::ControlsSetPermission { .. }
                    | Pending::ControlsCommand { .. }
                    | Pending::SkillsList
                    | Pending::SkillPreview { .. }
                    | Pending::InstructionsSnapshot
            )
        {
            self.resubscribing = false;
            self.clear_stop_remaining = 0;
            self.skills_refresh_in_flight = false;
            self.skills_refresh_queued = false;
            self.clear_skill_preview();
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
                        self.invocation.clear();
                        self.model.submit_failed(&request_id, "官方未接受");
                        if let Some(snapshot) = self.submit_backups.get(&request_id).cloned() {
                            self.restore_draft(&request_id, &snapshot);
                        }
                    }
                }
                Err(error)
                    if matches!(error.kind, RequestErrorKind::Rpc | RequestErrorKind::Local) =>
                {
                    self.invocation.clear();
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
                        // Jobs events are live-only, so re-read the official
                        // baseline after a gap instead of trusting the old list.
                        self.refresh_jobs_snapshot();
                        self.refresh_skills();
                        self.refresh_instructions();
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
            },
            Pending::Shell { record_id } => {
                let command = self
                    .bash_records
                    .iter()
                    .find(|record| record.id == record_id)
                    .map(|record| record.command.clone());
                match result {
                    Ok(value) => {
                        let mut inject_error = None;
                        if let Some(record) = self
                            .bash_records
                            .iter_mut()
                            .find(|record| record.id == record_id)
                        {
                            if let Err(message) = record.finish(&value) {
                                record.fail(message.clone());
                                self.error = Some(format!("人工命令结果无法解析：{message}"));
                            } else {
                                self.notice = Some(format!("人工命令：{}", record.outcome_label()));
                                inject_error = record.inject_error.clone();
                            }
                        }
                        if let Some(message) = inject_error {
                            self.error = Some(format!("命令已执行，但输出未能注入会话：{message}"));
                        }
                    }
                    Err(error)
                        if matches!(
                            error.kind,
                            RequestErrorKind::Rpc | RequestErrorKind::Local
                        ) =>
                    {
                        if let Some(record) = self
                            .bash_records
                            .iter_mut()
                            .find(|record| record.id == record_id)
                        {
                            record.fail(error.message.clone());
                        }
                        if let Some(command) = command {
                            self.restore_text(&format!("!{command}"));
                        }
                        self.error = Some(format!("人工命令未能执行：{}", error.message));
                    }
                    Err(error) => {
                        if let Some(record) = self
                            .bash_records
                            .iter_mut()
                            .find(|record| record.id == record_id)
                        {
                            record.unknown(error.message.clone());
                        }
                        self.error = Some(format!(
                            "人工命令结果未知（可能已执行，不会自动重发）：{}",
                            error.message
                        ));
                    }
                }
                self.trim_bash_records();
            }
            Pending::JobsSnapshot => {
                self.jobs_snapshot_pending = false;
                if !self.ready {
                    return;
                }
                match result {
                    Ok(value) => match self.jobs.apply_snapshot(&value) {
                        Ok(applied) => {
                            if applied {
                                if let Some(job_id) = self.jobs.detail_id().map(str::to_string) {
                                    self.follow_job(&job_id);
                                }
                            }
                        }
                        Err(message) => {
                            self.error = Some(format!("任务列表无效：{message}"));
                        }
                    },
                    Err(error) => {
                        if matches!(error.kind, RequestErrorKind::Rpc | RequestErrorKind::Local) {
                            self.error = Some(format!("读取任务列表失败：{}", error.message));
                        }
                    }
                }
            }
            Pending::JobsFollow { job_id } => {
                self.following.remove(&job_id);
                let was_dirty = self.follow_dirty.remove(&job_id);
                if !self.ready {
                    return;
                }
                match result {
                    Ok(value) => {
                        if let Err(message) = self.jobs.apply_follow(&job_id, &value) {
                            self.jobs.record_error(message.clone());
                            self.error = Some(format!("任务输出无效：{message}"));
                        } else {
                            // Output may have arrived while this read was in
                            // flight; drain once more until the cursor reaches
                            // the projection's total. Running jobs are included,
                            // not just settlements.
                            let behind = self.jobs.job(&job_id).is_some_and(|job| {
                                self.jobs.next_offset(&job_id) < job.output.total
                            });
                            if behind || was_dirty {
                                self.follow_job(&job_id);
                            }
                        }
                    }
                    Err(error) => {
                        self.jobs.record_error(error.message.clone());
                        if error.code == Some(1028) {
                            self.error =
                                Some(format!("任务不存在或已被移除（job/not-found）：{job_id}"));
                        } else if error.kind == RequestErrorKind::Unknown {
                            self.error = Some(format!("任务输出读取结果未知：{}", error.message));
                        } else if matches!(
                            error.kind,
                            RequestErrorKind::Rpc | RequestErrorKind::Local
                        ) {
                            self.error = Some(format!("读取任务输出失败：{}", error.message));
                        }
                    }
                }
            }
            Pending::JobsKill { job_id } => {
                if !self.ready {
                    return;
                }
                match result {
                    Ok(value) => {
                        let outcome = value
                            .get("outcome")
                            .and_then(Value::as_str)
                            .unwrap_or("requested");
                        let notice = if outcome == "already-finished" {
                            "任务已经结束".to_string()
                        } else {
                            "已请求停止；等待官方终态".to_string()
                        };
                        self.notice = Some(notice);
                        self.refresh_jobs_snapshot();
                    }
                    Err(error) => {
                        if error.code == Some(1028) {
                            self.error =
                                Some(format!("任务不存在或已被移除（job/not-found）：{job_id}"));
                        } else if error.kind == RequestErrorKind::Unknown {
                            self.error = Some(format!(
                                "停止请求结果未知（可能已被接受）；请刷新任务列表：{}",
                                error.message
                            ));
                        } else {
                            self.error = Some(format!("停止失败：{}", error.message));
                        }
                        self.refresh_jobs_snapshot();
                    }
                }
            },
            Pending::ControlsState => match result {
                Ok(value) => match self.controls.apply_state(&value) {
                    Ok(()) => {
                        self.claim_controls_commands();
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
                        let selected = self
                            .controls
                            .model
                            .next
                            .as_ref()
                            .map(ModelSelection::identity)
                            .unwrap_or_else(|| requested.identity());
                        self.notice = Some(format!(
                            "会话内模型已切换为 {selected}；新会话默认值由官方后台保存（本次回执不代表默认值已保存）"
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
                        let writer = if error.code == Some(1019) {
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
                    let kind = value.get("kind").and_then(Value::as_str);
                    let text = value.get("text").and_then(Value::as_str).unwrap_or("");
                    let applied = self.controls.apply_permission(value.get("permission"));
                    if kind == Some("error") {
                        self.error = Some(format!(
                            "权限切换失败：{preset}（官方：{text}）；重读当前值：{}",
                            self.controls.permission_label()
                        ));
                    } else if applied.is_ok() {
                        // Only the official receipt is claimed. Whether the
                        // model received a changed-by-user notice depends on
                        // the approval policy actually changing, which this
                        // command result does not describe.
                        self.notice = Some(format!(
                            "权限已切换为 {}（官方回执）",
                            self.controls.permission_label()
                        ));
                    } else {
                        self.notice = Some(format!("权限命令已完成：{text}"));
                        self.refresh_controls_state();
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
                        let writer = if error.code == Some(1019) {
                            "；另一个窗口正在写这个会话，本窗口不接管"
                        } else {
                            ""
                        };
                        self.error = Some(format!(
                            "权限写入失败：{}{writer}；正在重读官方投影",
                            error.message
                        ));
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
            Pending::SkillsList => {
                self.skills_refresh_in_flight = false;
                let root = self.model.address().root_session_id.clone();
                match result {
                    Ok(value) => {
                        if let Err(error) = self.skills.apply_list(&value, &root) {
                            self.skills.apply_failure(format!("技能目录无效：{error}"));
                        }
                    }
                    Err(error) => {
                        self.skills
                            .apply_failure(format!("技能目录刷新失败：{}", error.message));
                    }
                }
                self.clamp_skill_selection();
                self.reconcile_skill_preview();
                if self.skills_refresh_queued {
                    self.skills_refresh_queued = false;
                    self.refresh_skills();
                }
            }
            Pending::SkillPreview { name } => {
                if self.skill_preview_target.as_deref() != Some(name.as_str()) {
                    // The panel was closed or the selection moved on.
                    return;
                }
                match result {
                    Ok(value) => {
                        let root = self.model.address().root_session_id.clone();
                        match SkillPreview::parse(&value, &root) {
                            Ok(preview) => {
                                self.skill_preview = Some(preview);
                                self.skill_preview_error = None;
                            }
                            Err(error) => {
                                self.skill_preview_error = Some(format!("预览无效：{error}"));
                            }
                        }
                    }
                    Err(error) => {
                        self.skill_preview_error = Some(error.message);
                    }
                }
            }
            Pending::InstructionsSnapshot => {
                let root = self.model.address().root_session_id.clone();
                match result {
                    Ok(value) => {
                        if let Err(error) = self.instructions.apply(&value, &root) {
                            self.error = Some(format!("项目规矩投影无效：{error}"));
                        }
                    }
                    Err(error) => {
                        self.error = Some(format!("读取项目规矩来源失败：{}", error.message));

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
            self.jobs.clear_diagnostics();
        }
    }

    /// The host is gone: no job it owned can still be running. Drop the last
    /// projection instead of showing it as live across a restart.
    fn handle_host_loss(&mut self) {
        self.jobs.clear();
        self.jobs_snapshot_pending = false;
        self.following.clear();
        self.follow_dirty.clear();
        self.jobs_scroll_back = 0;
        self.jobs_detail_lines = 0;
        for record in &mut self.bash_records {
            if matches!(record.state, nano_jobs::BashState::Running) {
                record.unknown("宿主已断开，命令可能已执行");
            }
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

    /// Put a failed command line back without dropping newer typing.
    fn restore_text(&mut self, text: &str) {
        let current = self.drafts.current().text().to_string();
        if current.is_empty() {
            self.drafts.current_mut().set_text(text.to_string());
        } else if current != text && !current.starts_with(text) {
            self.drafts
                .current_mut()
                .set_text(format!("{text}\n\n{current}"));
        }
    }

    /// Run one `!command` through the official shell seam. The command is not
    /// a job and never enters `job_list`; the first use shows the permission
    /// notice and needs a second Enter.
    pub fn submit_bash(&mut self, command: String, raw: String) {
        if self.edit_target.is_some() {
            self.error = Some("正在编辑队列项；先按 Enter 应用或 Esc 取消".to_string());
            return;
        }
        if command.trim().is_empty() {
            self.error = Some("`!` 后面需要命令；草稿已保留".to_string());
            return;
        }
        if !self.ready {
            self.error = Some("宿主未就绪；草稿已保留".to_string());
            return;
        }
        if !self.shell_notice_acked {
            if self.shell_confirm.as_deref() != Some(raw.as_str()) {
                self.shell_confirm = Some(raw);
                self.notice = Some(format!("{SHELL_NOTICE} 再按 Enter 执行，Esc 取消"));
                return;
            }
            self.shell_notice_acked = true;
        }
        self.shell_confirm = None;
        // The local feed is bounded. Refuse rather than push a record we would
        // immediately have no room to keep; finished records free a slot.
        if self.bash_records.len() >= 20
            && !self
                .bash_records
                .iter()
                .any(|record| !matches!(record.state, nano_jobs::BashState::Running))
        {
            self.error = Some("同时运行的人工命令已达 20 条；等一条结束再试".to_string());
            return;
        }
        self.drafts.current_mut().take();
        self.bash_counter = self.bash_counter.wrapping_add(1);
        let record_id = self.bash_counter;
        self.bash_records
            .push(BashRecord::new(record_id, command.clone()));
        self.trim_bash_records();
        let params = json!({
            "address": self.model.address().wire_value(),
            "command": command,
            // `!!` is ordinary shell text, not a context-injection path.
            "inject": !raw.starts_with("!!"),
        });
        let Some(link) = &self.link else {
            if let Some(record) = self.bash_records.iter_mut().find(|r| r.id == record_id) {
                record.fail("宿主连接已关闭");
            }
            self.restore_text(&raw);
            return;
        };
        match link.request_with_timeout("nano.shell.exec", params, SHELL_REQUEST_TIMEOUT) {
            Ok(id) => {
                self.pending.insert(id, Pending::Shell { record_id });
                self.notice = Some(format!("已以用户权限运行：{raw}"));
            }
            Err(message) => {
                if let Some(record) = self.bash_records.iter_mut().find(|r| r.id == record_id) {
                    record.fail(message.clone());
                }
                self.restore_text(&raw);
                self.error = Some(format!("人工命令未能发出：{message}"));
            }
        }
    }

    /// Open or close the task panel. Opening reads an official baseline; the
    /// panel never owns job state.
    fn toggle_jobs(&mut self) {
        if self.jobs_open {
            self.jobs_open = false;
            self.jobs.close_detail();
            self.jobs_scroll_back = 0;
            self.notice = Some("任务面板已关闭".to_string());
        } else {
            self.jobs_open = true;
            self.refresh_jobs_snapshot();
        }
    }

    /// Whether the connected host reports a usable official job registry.
    fn jobs_supported(&self) -> bool {
        self.hello.as_ref().is_some_and(crate::link::jobs_supported)
    }

    /// Read the official job list without changing any output cursor.
    fn refresh_jobs_snapshot(&mut self) {
        if !self.ready || self.jobs_snapshot_pending || !self.jobs_supported() {
            return;
        }
        let Some(link) = &self.link else {
            return;
        };
        let params = json!({ "address": self.model.address().wire_value() });
        match link.request("nano.jobs.snapshot", params) {
            Ok(id) => {
                self.jobs_snapshot_pending = true;
                self.pending.insert(id, Pending::JobsSnapshot);
            }
            Err(message) => {
                self.error = Some(format!("读取任务列表失败：{message}"));
            }
        }
    }

    /// Keys while the task panel is open.
    fn handle_jobs_key(&mut self, key: KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL)
            || key.modifiers.contains(KeyModifiers::ALT)
        {
            return false;
        }
        if self.jobs.detail_open() {
            return match key.code {
                KeyCode::Esc => {
                    self.jobs.close_detail();
                    self.jobs_scroll_back = 0;
                    true
                }
                KeyCode::Up => {
                    let limit = self.jobs_detail_lines.saturating_sub(1);
                    self.jobs_scroll_back = self.jobs_scroll_back.saturating_add(1).min(limit);
                    true
                }
                KeyCode::Down => {
                    self.jobs_scroll_back = self.jobs_scroll_back.saturating_sub(1);
                    true
                }
                KeyCode::PageUp => {
                    let limit = self.jobs_detail_lines.saturating_sub(1);
                    self.jobs_scroll_back = self.jobs_scroll_back.saturating_add(10).min(limit);
                    true
                }
                KeyCode::PageDown => {
                    self.jobs_scroll_back = self.jobs_scroll_back.saturating_sub(10);
                    true
                }
                KeyCode::Char('k') | KeyCode::Char('K') => {
                    self.kill_detail_job();
                    true
                }
                KeyCode::Char('r') | KeyCode::Char('R') => {
                    if let Some(job_id) = self.jobs.detail_id().map(str::to_string) {
                        self.follow_job(&job_id);
                    }
                    true
                }
                _ => false,
            };
        }
        match key.code {
            KeyCode::Esc => {
                self.jobs_open = false;
                self.jobs_scroll_back = 0;
                true
            }
            KeyCode::Up => {
                self.jobs.select_previous();
                true
            }
            KeyCode::Down => {
                self.jobs.select_next();
                true
            }
            KeyCode::Enter => {
                if let Some(job_id) = self.jobs.selected_job().map(|job| job.id.clone()) {
                    self.jobs.open_detail(&job_id);
                    self.jobs_scroll_back = 0;
                    self.follow_job(&job_id);
                }
                true
            }
            KeyCode::Char('k') | KeyCode::Char('K') => {
                self.kill_selected_job();
                true
            }
            KeyCode::Char('r') | KeyCode::Char('R') => {
                self.refresh_jobs_snapshot();
                true
            }
            _ => false,
        }
    }

    /// Read retained output from the stored absolute offset. This is the only
    /// read the UI issues, and it never advances the model's cursor.
    fn follow_job(&mut self, job_id: &str) {
        if !self.ready || self.following.contains(job_id) {
            return;
        }
        let Some(link) = &self.link else {
            return;
        };
        let params = json!({
            "address": self.model.address().wire_value(),
            "jobId": job_id,
            "from": self.jobs.next_offset(job_id),
        });
        match link.request("nano.jobs.follow", params) {
            Ok(id) => {
                self.following.insert(job_id.to_string());
                self.pending.insert(
                    id,
                    Pending::JobsFollow {
                        job_id: job_id.to_string(),
                    },
                );
            }
            Err(message) => {
                self.jobs.record_error(message.clone());
                self.error = Some(format!("读取任务输出失败：{message}"));
            }
        }
    }

    /// An output commit only announces an advancement; follow the open detail.
    fn handle_jobs_output(&mut self, payload: &Value) {
        let Some(job_id) = self.jobs.output_job_id(payload) else {
            return;
        };
        if self.jobs.detail_id() != Some(job_id.as_str()) {
            return;
        }
        if self.following.contains(&job_id) {
            // Remember the advancement; the in-flight read may have started
            // before these bytes. The response handler drains it.
            self.follow_dirty.insert(job_id);
        } else {
            self.follow_job(&job_id);
        }
    }

    fn kill_selected_job(&mut self) {
        if let Some(job_id) = self.jobs.selected_job().map(|job| job.id.clone()) {
            self.kill_job(&job_id);
        }
    }

    fn kill_detail_job(&mut self) {
        if let Some(job_id) = self.jobs.detail_id().map(str::to_string) {
            self.kill_job(&job_id);
        }
    }

    /// Request cancellation. The answer is a request fact; the terminal state
    /// arrives as an official projection.
    fn kill_job(&mut self, job_id: &str) {
        if !self.ready {
            self.error = Some("宿主未就绪".to_string());
            return;
        }
        let Some(link) = &self.link else {
            return;
        };
        let params = json!({
            "address": self.model.address().wire_value(),
            "jobId": job_id,
        });
        match link.request("nano.jobs.kill", params) {
            Ok(id) => {
                self.pending.insert(
                    id,
                    Pending::JobsKill {
                        job_id: job_id.to_string(),
                    },
                );
            }
            Err(message) => {
                self.error = Some(format!("停止请求未能发出：{message}"));
            }
        }
    }

    /// Keep the local bash feed bounded without dropping a running command.
    fn trim_bash_records(&mut self) {
        while self.bash_records.len() > 20 {
            let Some(index) = self
                .bash_records
                .iter()
                .position(|record| !matches!(record.state, nano_jobs::BashState::Running))
            else {
                break;
            };
            self.bash_records.remove(index);
        }
    }

    /// Drop a local bash record once its official synthetic message is durable.
    fn prune_bash_records(&mut self) {
        if self.bash_records.is_empty() {
            return;
        }
        let injected: Vec<String> = self
            .bash_records
            .iter()
            .filter_map(|record| record.injected_message_id.clone())
            .collect();
        for message_id in injected {
            let durable = self.model.items().iter().any(|item| {
                matches!(
                    item,
                    ConversationItem::Message(message)
                        if message.message_id.as_deref() == Some(message_id.as_str())
                )
            });
            if durable {
                self.bash_records
                    .retain(|record| !record.matches_injected(&message_id));
            }
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
        let mut skill_name = None;
        if !has_attachments {
            let route =
                CommandRouter::new(&self.registry, self.scene.active().clone()).route(&text);
            match route {
                InputRoute::Prompt { .. } => {
                    self.shell_confirm = None;
                }
                InputRoute::Bash { command, raw } => {
                    self.submit_bash(command, raw);
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
                InputRoute::Skill { name, args, raw } => {
                    // Commands win before the line ever becomes a prompt. The
                    // line is submitted literally in every other case,
                    // including an unknown or deleted name; only the turn's own
                    // events may say whether the official host injected a body.
                    if self.skills.resolve(&name) == NameResolution::Command {
                        self.dispatch_controls_line(&name, &args, &raw);
                        return;
                    }
                    skill_name = Some(name);
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
                    // No link means this process cannot observe the result;
                    // keep the accepted echo as unknown instead of claiming a
                    // definite failure. The S02 submission state machine owns
                    // the later official reconciliation.
                    self.invocation.clear();
                    self.model
                        .submit_unknown(&plan.request_id, "宿主连接已关闭");
                    return;
                };
                if let Some(name) = skill_name {
                    self.invocation
                        .note_submission(name, plan.request_id.clone());
                }
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
                        self.invocation.clear();
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

    /// Ask for the official workspace-instruction projection.
    pub fn refresh_instructions(&mut self) {
        if !self.ready {
            return;
        }
        let Some(link) = &self.link else {
            return;
        };
        let params = json!({ "address": self.model.address().wire_value() });
        if let Ok(id) = link.request("nano.instructions.snapshot", params) {
            self.pending.insert(id, Pending::InstructionsSnapshot);
        }
    }

    /// Ask for the user-invocable skill catalog. Concurrent signals coalesce
    /// into one follow-up refresh instead of a request storm.
    pub fn refresh_skills(&mut self) {
        if !self.ready {
            return;
        }
        if self.skills_refresh_in_flight {
            self.skills_refresh_queued = true;
            return;
        }
        let Some(link) = &self.link else {
            return;
        };
        let params = json!({ "address": self.model.address().wire_value() });
        match link.request("nano.skills.list", params) {
            Ok(id) => {
                self.pending.insert(id, Pending::SkillsList);
                self.skills_refresh_in_flight = true;
            }
            Err(message) => {
                self.skills
                    .apply_failure(format!("技能目录请求失败：{message}"));
            }
        }
    }

    /// Open the skills panel over the conversation scene.
    pub fn open_skills_panel(&mut self) {
        self.scene.focus_mut().push(skills_focus());
        self.clear_skill_preview();
        self.clamp_skill_selection();
        self.refresh_skills();
    }

    /// Open the workspace-instruction sources panel.
    pub fn open_instructions_panel(&mut self) {
        self.scene.focus_mut().push(instructions_focus());
        self.instructions_scroll = 0;
        self.instructions_max_scroll = 0;
        self.refresh_instructions();
    }

    /// Whether the skills panel currently owns focus.
    pub fn skills_panel_open(&self) -> bool {
        self.scene.focus().top() == Some(&skills_focus())
    }

    /// Whether the instructions panel currently owns focus.
    pub fn instructions_panel_open(&self) -> bool {
        self.scene.focus().top() == Some(&instructions_focus())
    }

    fn clear_skill_preview(&mut self) {
        self.skill_preview = None;
        self.skill_preview_error = None;
        self.skill_preview_target = None;
    }

    fn clamp_skill_selection(&mut self) {
        let last = self.skills.entries().len().saturating_sub(1);
        if self.skill_selection > last {
            self.skill_selection = last;
        }
    }

    /// Drop an open preview whose skill disappeared or whose official path
    /// changed. A catalog refresh must not leave an old body under a new
    /// catalog (S05 §6.6).
    fn reconcile_skill_preview(&mut self) {
        let Some(target) = self.skill_preview_target.clone() else {
            return;
        };
        let current = self
            .skills
            .find(&target)
            .and_then(|entry| entry.path.clone());
        match &self.skill_preview {
            Some(preview) => {
                if current.as_deref() != Some(preview.path.as_str()) {
                    self.clear_skill_preview();
                }
            }
            None => {
                if current.is_none() && self.skill_preview_error.is_none() {
                    self.clear_skill_preview();
                }
            }
        }
    }

    fn close_panel(&mut self) {
        self.scene.focus_mut().pop();
        self.clear_skill_preview();
    }

    fn handle_skills_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc => {
                self.close_panel();
                true
            }
            KeyCode::Up => {
                if self.skill_selection > 0 {
                    self.skill_selection -= 1;
                    self.clear_skill_preview();
                }
                true
            }
            KeyCode::Down => {
                if self.skill_selection + 1 < self.skills.entries().len() {
                    self.skill_selection += 1;
                    self.clear_skill_preview();
                }
                true
            }
            KeyCode::Enter => {
                self.insert_selected_skill();
                true
            }
            KeyCode::Char('p') | KeyCode::Char('P') | KeyCode::Right => {
                self.toggle_skill_preview();
                true
            }
            KeyCode::Left => {
                self.clear_skill_preview();
                true
            }
            _ => false,
        }
    }

    fn handle_instructions_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc => {
                self.close_panel();
                true
            }
            KeyCode::Up => {
                self.instructions_scroll = self.instructions_scroll.saturating_sub(1);
                true
            }
            KeyCode::Down => {
                self.instructions_scroll = self
                    .instructions_scroll
                    .saturating_add(1)
                    .min(self.instructions_max_scroll);
                true
            }
            KeyCode::PageUp => {
                self.instructions_scroll = self.instructions_scroll.saturating_sub(10);
                true
            }
            KeyCode::PageDown => {
                self.instructions_scroll = self
                    .instructions_scroll
                    .saturating_add(10)
                    .min(self.instructions_max_scroll);
                true
            }
            _ => false,
        }
    }

    /// Insert the selected skill as a literal, editable `/name` token.
    fn insert_selected_skill(&mut self) {
        let Some(entry) = self.skills.entries().get(self.skill_selection) else {
            self.error = Some("技能目录为空，没有可填入的技能".to_string());
            return;
        };
        let name = entry.name.clone();
        let token = format!("/{name} ");
        let current = self.drafts.current().text().to_string();
        let text = if current.is_empty() {
            token
        } else if current.ends_with(char::is_whitespace) {
            format!("{current}{token}")
        } else {
            format!("{current} {token}")
        };
        self.drafts.current_mut().set_text(text);
        self.notice = Some(format!(
            "已填入 /{name}；发送后官方注入正文，会计入模型成本"
        ));
        self.close_panel();
    }

    /// Request the selected skill's instruction file, or close an open preview.
    fn toggle_skill_preview(&mut self) {
        if self.skill_preview_target.is_some() {
            self.clear_skill_preview();
            return;
        }
        let Some(entry) = self.skills.entries().get(self.skill_selection) else {
            return;
        };
        let name = entry.name.clone();
        if entry.path.is_none() {
            self.skill_preview_target = Some(name.clone());
            self.skill_preview_error = Some(format!("/{name} 没有文件路径，不能预览"));
            return;
        }
        if !self.ready {
            self.skill_preview_target = Some(name);
            self.skill_preview_error = Some("宿主未就绪，暂时不能预览".to_string());
            return;
        }
        let Some(link) = &self.link else {
            self.skill_preview_target = Some(name);
            self.skill_preview_error = Some("宿主连接已关闭".to_string());
            return;
        };
        let params = json!({ "address": self.model.address().wire_value(), "name": name });
        match link.request("nano.skills.preview", params) {
            Ok(id) => {
                self.skill_preview = None;
                self.skill_preview_error = None;
                self.skill_preview_target = Some(name.clone());
                self.pending.insert(id, Pending::SkillPreview { name });
            }
            Err(message) => {
                self.skill_preview_target = Some(name);
                self.skill_preview_error = Some(message);
            }
        }
    }

    /// Track one submitted `/name` through the official pre-step decision.
    fn observe_invocation(&mut self, payload: &Value) {
        let event_type = payload
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let data = payload.get("data").unwrap_or(&Value::Null);
        match self.invocation.observe(event_type, data) {
            Some(InvocationFact::Injected(name)) => {
                self.notice = Some(format!("技能正文已注入：/{name}"));
            }
            Some(InvocationFact::NotInjected(name)) => {
                self.notice = Some(format!("/{name} 未注入：官方按普通文本处理"));
            }
            None => {}
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

    /// Bottom status: the resident working directory and permission/safety
    /// fact, plus a transient hint. A failed controls read marks the retained
    /// permission value as possibly stale.
    pub fn status_line(&self) -> String {
        let stale = if self.controls.state_error().is_some() {
            "（可能过期）"
        } else {
            ""
        };
        let hint = self.status_hint();
        let base = format!(
            "{} · 权限 {}{stale}",
            self.display_cwd(),
            self.controls.permission_label()
        );
        if hint.is_empty() {
            base
        } else {
            format!("{base} · {hint}")
        }
    }

    /// Basename of the effective working directory.
    pub fn display_cwd(&self) -> String {
        self.cli
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
            .unwrap_or_else(|| ".".to_string())
    }

    /// Status-bar text.
    pub fn status_hint(&self) -> String {
        if self.stop_pending {
            return "停止中：等待官方 idle 事实（草稿保留）".to_string();
        }
        // Errors and notices stay visible even while the task panel owns the
        // keyboard; a failed stop or a malformed projection must not disappear
        // behind a fixed panel hint.
        if let Some(error) = &self.error {
            return error.clone();
        }
        if self.shell_confirm.is_some() {
            return format!("{SHELL_NOTICE} Enter 执行，Esc 取消");
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
        if self.jobs_open {
            if let Some(error) = self.jobs.last_error() {
                return error.to_string();
            }
            if self.jobs.detail_open() {
                return "任务详情（非消费跟随）· ↑↓/PageUp/PageDown 滚动 · k 停止 · r 重读 · Esc 返回"
                    .to_string();
            }
            return "任务面板 · ↑↓ 选择 · Enter 详情 · k 停止 · r 刷新 · Esc 关闭".to_string();
        }
        if !self.scene.focus().input_focused() {
            if self.skills_panel_open() {
                return "技能面板：↑↓ 选择 · Enter 填入 /name · P 预览 · Esc 关闭".to_string();
            }
            if self.instructions_panel_open() {
                return "项目规矩：↑↓ / PageUp / PageDown 滚动 · Esc 关闭".to_string();
            }
            return "队列焦点：↑↓ 选择 · Enter 编辑 · d 删除 · s 插话 · x 清空并停止 · Tab 返回"
                .to_string();
        }
        if let Some(message_id) = &self.edit_target {
            return format!("编辑队列项 {message_id}：Enter 应用，Esc 取消");
        }
        "Enter 发送 · Alt+Enter 插话 · @ 引用文件 · Alt+A 附加 · Alt+←→ 排序 · Alt+Backspace 移除附件 · Alt+K 技能 · Alt+R 规矩 · Ctrl+J 换行 · Esc 停止 · Ctrl+Y 复制原文 · Ctrl+C 退出"
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
        let kinds = self
            .hello
            .as_ref()
            .map(crate::link::subscription_kinds)
            .unwrap_or_else(|| crate::link::SUBSCRIPTION_KINDS.to_vec());
        let (params, replay_truncated) = self.model.resubscribe_params(&kinds);
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
    fn shell_input_keeps_the_draft_when_the_host_is_not_ready() {
        let mut app = app();
        app.drafts.current_mut().set_text("!ls");
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.drafts.current().text(), "!ls");
        assert!(app.error.as_deref().unwrap().contains("宿主未就绪"));
    }

    #[test]
    fn the_first_manual_command_needs_the_user_permission_notice() {
        use nano_jobs::BashState;
        let mut app = app();
        app.ready = true;
        app.drafts.current_mut().set_text("!ls");
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.drafts.current().text(), "!ls");
        assert!(app.shell_confirm.is_some());
        assert!(app.status_hint().contains("不是 Agent 授权"));
        // The second Enter acknowledges and sends. Without a link the approved
        // command cannot be delivered, so it is reported and the draft returns.
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.shell_notice_acked);
        assert_eq!(app.drafts.current().text(), "!ls");
        assert_eq!(app.bash_records.len(), 1);
        assert!(matches!(app.bash_records[0].state, BashState::Failed(_)));
        assert!(!app.bash_records[0].command.is_empty());
    }

    #[test]
    fn escape_cancels_the_manual_command_confirmation() {
        let mut app = app();
        app.ready = true;
        app.drafts.current_mut().set_text("!ls");
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.shell_confirm.is_some());
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.shell_confirm.is_none());
        assert_eq!(app.drafts.current().text(), "!ls");
    }

    #[test]
    fn a_second_manual_command_does_not_ask_again() {
        let mut app = app();
        app.ready = true;
        app.shell_notice_acked = true;
        app.drafts.current_mut().set_text("!pwd");
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.shell_confirm.is_none());
        assert_eq!(app.bash_records.len(), 1);
    }

    #[test]
    fn a_known_shell_result_renders_its_exit_fact() {
        let mut app = app();
        app.bash_counter = 1;
        app.bash_records
            .push(nano_jobs::BashRecord::new(1, "printf hi"));
        app.pending.insert(5, Pending::Shell { record_id: 1 });
        app.handle_request_done(
            5,
            Ok(json!({
                "exitCode": 7,
                "signal": null,
                "timedOut": false,
                "aborted": false,
                "stdout": {"text": "hi", "truncated": false},
                "stderr": {"text": "", "truncated": false},
                "injectedMessageId": "m-bash"
            })),
        );
        let record = &app.bash_records[0];
        assert_eq!(record.outcome_label(), "退出码 7");
        assert_eq!(record.stdout(), "hi");
        assert_eq!(record.injected_message_id.as_deref(), Some("m-bash"));
    }

    #[test]
    fn a_local_shell_failure_restores_the_command_line() {
        let mut app = app();
        app.bash_counter = 1;
        app.bash_records
            .push(nano_jobs::BashRecord::new(1, "false"));
        app.pending.insert(5, Pending::Shell { record_id: 1 });
        app.handle_request_done(
            5,
            Err(RequestError {
                kind: RequestErrorKind::Local,
                code: None,
                message: "not sent".into(),
            }),
        );
        assert_eq!(app.drafts.current().text(), "!false");
    }

    #[test]
    fn a_local_bash_record_is_dropped_when_the_official_message_is_durable() {
        use nano_jobs::BashState;
        let mut app = app();
        let mut record = nano_jobs::BashRecord::new(1, "ls");
        record.state = BashState::Done;
        record.injected_message_id = Some("m-bash".into());
        app.bash_records.push(record);
        app.handle_link(LinkEvent::Host(nano_runtime::HostEvent::Event(
            nano_runtime::protocol::EventParams {
                runtime_id: "rt".into(),
                generation: 1,
                subscription_id: "sub-1".into(),
                sequence: 2,
                kind: "session.event".into(),
                payload: json!({
                    "address": {"rootSessionId": "root"},
                    "seq": 2,
                    "type": "user/message",
                    "data": {
                        "id": "m-bash",
                        "content": [{"type": "text", "text": "<bash-input>ls</bash-input>"}],
                        "source": {"kind": "bash-mode"}
                    }
                }),
            },
        )));
        assert!(app.bash_records.is_empty());
    }

    #[test]
    fn ctrl_t_opens_the_task_panel_and_takes_the_keys() {
        let mut app = app();
        app.ready = true;
        app.drafts.current_mut().set_text("hidden");
        app.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
        assert!(app.jobs_open);
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.drafts.current().text(), "hidden");
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.jobs_open);
    }

    #[test]
    fn a_job_projection_event_updates_the_task_list_without_touching_history() {
        let mut app = app();
        app.handle_link(LinkEvent::Host(nano_runtime::HostEvent::Event(
            nano_runtime::protocol::EventParams {
                runtime_id: "rt".into(),
                generation: 1,
                subscription_id: "sub-1".into(),
                sequence: 3,
                kind: "jobs.event".into(),
                payload: json!({
                    "address": {"rootSessionId": "root"},
                    "type": "registered",
                    "job": {
                        "id": "bash-0123456789abcdef",
                        "kind": "bash",
                        "label": "sleep 30",
                        "status": "running",
                        "startedAt": 1,
                        "output": {"total": 0, "earliest": 0}
                    }
                }),
            },
        )));
        assert_eq!(app.jobs.len(), 1);
        assert_eq!(app.jobs.active_count(), 1);
        assert!(app.model.items().is_empty());
    }

    #[test]
    fn panel_errors_stay_visible_in_the_status_line() {
        let mut app = app();
        app.jobs_open = true;
        app.error = Some("停止失败：示例".to_string());
        assert_eq!(app.status_hint(), "停止失败：示例");
    }

    #[test]
    fn paste_is_ignored_while_the_task_panel_owns_the_keyboard() {
        let mut app = app();
        app.jobs_open = true;
        assert!(!app.handle_paste("hidden".to_string()));
        assert!(app.drafts.current().is_empty());
    }

    #[test]
    fn a_repeated_enter_does_not_satisfy_the_manual_command_gate() {
        let mut app = app();
        app.ready = true;
        app.drafts.current_mut().set_text("!ls");
        app.handle_key(KeyEvent::new_with_kind(
            KeyCode::Enter,
            KeyModifiers::NONE,
            KeyEventKind::Repeat,
        ));
        assert!(app.shell_confirm.is_none());
        assert_eq!(app.drafts.current().text(), "!ls");
    }

    #[test]
    fn host_loss_drops_job_projections_and_marks_running_bash_unknown() {
        let mut app = app();
        app.handle_link(LinkEvent::Host(nano_runtime::HostEvent::Event(
            nano_runtime::protocol::EventParams {
                runtime_id: "rt".into(),
                generation: 1,
                subscription_id: "sub-1".into(),
                sequence: 3,
                kind: "jobs.event".into(),
                payload: json!({
                    "address": {"rootSessionId": "root"},
                    "seq": 1,
                    "type": "registered",
                    "job": {
                        "id": "bash-0123456789abcdef",
                        "kind": "bash",
                        "label": "sleep 30",
                        "status": "running",
                        "startedAt": 1,
                        "output": {"total": 0, "earliest": 0}
                    }
                }),
            },
        )));
        app.bash_counter = 1;
        app.bash_records
            .push(nano_jobs::BashRecord::new(1, "sleep"));
        app.handle_link(LinkEvent::Host(nano_runtime::HostEvent::TransportClosed {
            lost: true,
        }));
        assert!(app.jobs.is_empty());
        assert!(matches!(
            app.bash_records[0].state,
            nano_jobs::BashState::Unknown(_)
        ));
    }

    #[test]
    fn a_not_found_stop_is_reported_by_error_code() {
        let mut app = app();
        app.ready = true;
        app.pending.insert(
            1,
            Pending::JobsKill {
                job_id: "bash-a".into(),
            },
        );
        app.handle_request_done(
            1,
            Err(RequestError {
                kind: RequestErrorKind::Rpc,
                code: Some(1028),
                message: "job not visible".into(),
            }),
        );
        assert!(app.error.as_deref().unwrap().contains("job/not-found"));
    }

    #[test]
    fn an_unknown_stop_result_is_not_reported_as_failure() {
        let mut app = app();
        app.ready = true;
        app.pending.insert(
            1,
            Pending::JobsKill {
                job_id: "bash-a".into(),
            },
        );
        app.handle_request_done(
            1,
            Err(RequestError {
                kind: RequestErrorKind::Unknown,
                code: None,
                message: "transport closed".into(),
            }),
        );
        assert!(app.error.as_deref().unwrap().contains("结果未知"));
    }

    #[test]
    fn an_old_jobs_snapshot_cannot_overwrite_a_newer_terminal_event() {
        let mut app = app();
        app.ready = true;
        app.handle_link(LinkEvent::Host(nano_runtime::HostEvent::Event(
            nano_runtime::protocol::EventParams {
                runtime_id: "rt".into(),
                generation: 1,
                subscription_id: "sub-1".into(),
                sequence: 3,
                kind: "jobs.event".into(),
                payload: json!({
                    "address": {"rootSessionId": "root"},
                    "seq": 5,
                    "type": "settled",
                    "cause": "producer",
                    "awaited": false,
                    "job": {
                        "id": "bash-a",
                        "kind": "bash",
                        "label": "true",
                        "status": "completed",
                        "startedAt": 1,
                        "finishedAt": 2,
                        "output": {"total": 0, "earliest": 0}
                    }
                }),
            },
        )));
        app.pending.insert(1, Pending::JobsSnapshot);
        app.handle_request_done(
            1,
            Ok(json!({
                "seq": 4,
                "jobs": [{
                    "id": "bash-a",
                    "kind": "bash",
                    "label": "true",
                    "status": "running",
                    "startedAt": 1,
                    "output": {"total": 0, "earliest": 0}
                }]
            })),
        );
        assert_eq!(app.jobs.jobs()[0].status(), nano_jobs::JobStatus::Completed);
    }

    #[test]
    fn a_settled_job_keeps_the_natural_completion_status() {
        let mut app = app();
        app.handle_link(LinkEvent::Host(nano_runtime::HostEvent::Event(
            nano_runtime::protocol::EventParams {
                runtime_id: "rt".into(),
                generation: 1,
                subscription_id: "sub-1".into(),
                sequence: 3,
                kind: "jobs.event".into(),
                payload: json!({
                    "address": {"rootSessionId": "root"},
                    "type": "settled",
                    "cause": "producer",
                    "awaited": false,
                    "job": {
                        "id": "bash-0123456789abcdef",
                        "kind": "bash",
                        "label": "true",
                        "status": "completed",
                        "detail": "exit code: 0",
                        "startedAt": 1,
                        "finishedAt": 2,
                        "output": {"total": 10, "earliest": 0}
                    }
                }),
            },
        )));
        assert_eq!(app.jobs.jobs()[0].status(), nano_jobs::JobStatus::Completed);
        assert!(app.notice.as_deref().unwrap().contains("完成"));
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
    fn a_submission_without_a_host_is_marked_unknown() {
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
                code: None,
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
                code: None,
                message: "not sent".into(),
            }),
        );
        assert_eq!(app.drafts.current().text(), "text");
    }

    fn controls_state(value: serde_json::Value) -> App {
        let mut app = app();
        app.ready = true;
        app.controls.set_capabilities(true, true, true);
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
    fn an_unknown_controls_command_keeps_the_draft_and_never_becomes_a_prompt() {
        let mut app = app();
        app.ready = true;
        app.controls.set_capabilities(true, true, true);
        app.drafts.current_mut().set_text("/nope");
        app.dispatch_controls_line("nope", "", "/nope");
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
        assert!(app.status_line().contains("权限 custom"));
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
                code: Some(1019),
                message: "session \"root\" is already owned by an active write handle".into(),
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
                code: None,
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
                code: None,
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
                code: None,
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
    fn a_model_switch_notice_uses_the_official_receipt() {
        let mut app = controls_state(controls_state_value());
        app.pending.insert(
            12,
            Pending::ControlsSelectModel {
                requested: ModelSelection::new("p", "requested-route"),
            },
        );
        app.handle_request_done(
            12,
            Ok(json!({"selected": {"provider": "p", "model": "official-route"}})),
        );
        let notice = app.notice.as_deref().unwrap();
        assert!(notice.contains("official-route"));
        assert!(!notice.contains("requested-route"));
    }

    #[test]
    fn prefill_is_refused_while_editing_a_queued_item() {
        let mut app = controls_state(controls_state_value());
        queue_event(&mut app, "queued text");
        app.begin_queue_edit();
        assert!(app.editing_queue().is_some());
        app.panel.open(PanelMode::Commands);
        // /goal is not in this state; add an argument-taking command.
        let mut state = controls_state_value();
        state["commands"] = json!([
            {"name": "goal", "description": "Set a goal", "inputHint": "<goal>"}
        ]);
        app.controls.apply_state(&state).unwrap();
        app.panel.open(PanelMode::Commands);
        app.activate_panel_row();
        assert!(app.error.as_deref().unwrap().contains("正在编辑队列项"));
        assert_eq!(app.drafts.current().text(), "queued text");
        assert!(app.editing_queue().is_some());
    }

    #[test]
    fn the_status_line_marks_a_failed_control_read_as_stale() {
        let mut app = controls_state(controls_state_value());
        app.controls.note_state_failure("projection unreadable");
        let line = app.status_line();
        assert!(line.contains("权限 workspace-write"));
        assert!(line.contains("可能过期"));
        assert!(line.starts_with(&app.display_cwd()));
    }

    #[test]
    fn a_writer_held_permission_rejection_is_explained() {
        let mut app = controls_state(controls_state_value());
        app.pending.insert(
            13,
            Pending::ControlsSetPermission {
                preset: "danger-full-access".into(),
            },
        );
        app.handle_request_done(
            13,
            Err(RequestError {
                kind: RequestErrorKind::Rpc,
                code: Some(1019),
                message: "session \"root\" is already owned by an active write handle".into(),
            }),
        );
        assert!(app.error.as_deref().unwrap().contains("本窗口不接管"));
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
                code: None,
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
    fn restoring_a_failed_submission_keeps_attachment_cards() {
        let mut app = app();
        app.drafts.current_mut().set_text("look ");
        let cursor = app.drafts.current().cursor();
        app.attachments
            .current_mut()
            .insert(cursor, ready_card("r-img", AttachmentKind::Image));
        let submitted = SubmittedDraft {
            text: "look ".into(),
            attachments: app.attachments.current().clone(),
        };
        app.drafts.current_mut().take();
        app.attachments.current_mut().clear();
        app.submit_backups.insert("r-1".into(), submitted.clone());
        app.restore_draft("r-1", &submitted);
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

    fn skill_entry(name: &str, shadowed: bool) -> Value {
        json!({
            "name": name,
            "description": format!("{name} description"),
            "modelInvocable": true,
            "userInvocable": true,
            "path": format!("/tmp/{name}/SKILL.md"),
            "source": "project-agents",
            "provider": "filesystem",
            "shadowedByCommand": shadowed,
        })
    }

    fn install_catalog(app: &mut App, skills: Value, commands: Value) {
        app.skills
            .apply_list(
                &json!({
                    "address": {"rootSessionId": "root"},
                    "complete": true,
                    "commands": commands,
                    "skills": skills,
                }),
                "root",
            )
            .unwrap();
    }

    fn session_event(app: &mut App, event_type: &str, data: Value) {
        app.handle_link(LinkEvent::Host(nano_runtime::HostEvent::Event(
            nano_runtime::protocol::EventParams {
                runtime_id: "rt".into(),
                generation: 1,
                subscription_id: "sub-1".into(),
                sequence: 1,
                kind: "session.event".into(),
                payload: json!({
                    "address": {"rootSessionId": "root"},
                    "seq": 1,
                    "type": event_type,
                    "data": data,
                }),
            },
        )));
    }

    #[test]
    fn alt_k_opens_the_catalog_and_enter_inserts_a_literal_token() {
        let mut app = app();
        install_catalog(&mut app, json!([skill_entry("hello", false)]), json!([]));
        app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::ALT));
        assert!(app.skills_panel_open());
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.drafts.current().text(), "/hello ");
        assert!(app.scene.focus().input_focused());
        assert!(app.notice.as_deref().unwrap().contains("模型成本"));
    }

    #[test]
    fn a_same_name_command_blocks_the_skill_submission() {
        let mut app = app();
        app.ready = true;
        install_catalog(
            &mut app,
            json!([skill_entry("compact", true)]),
            json!(["compact"]),
        );
        app.drafts.current_mut().set_text("/compact");
        app.submit_draft(Delivery::Queue);
        assert_eq!(app.drafts.current().text(), "/compact");
        assert!(app.error.as_deref().unwrap().contains("命令优先"));
        assert!(app.model.live().echoes.is_empty());
    }

    #[test]
    fn an_unknown_slash_name_is_submitted_literally_and_tracked() {
        let mut app = app();
        app.ready = true;
        install_catalog(&mut app, json!([skill_entry("hello", false)]), json!([]));
        app.drafts.current_mut().set_text("/gone now");
        app.submit_draft(Delivery::Queue);
        assert!(app.drafts.current().is_empty());
        assert_eq!(app.model.live().echoes[0].text, "/gone now");
        assert_eq!(app.skills.resolve("gone"), NameResolution::Unknown);
    }

    #[test]
    fn a_failed_preview_keeps_the_error_visible() {
        let mut app = app();
        app.ready = true;
        install_catalog(&mut app, json!([skill_entry("hello", false)]), json!([]));
        app.open_skills_panel();
        app.skill_preview_target = Some("hello".into());
        app.pending.insert(
            7,
            Pending::SkillPreview {
                name: "hello".into(),
            },
        );
        app.handle_request_done(
            7,
            Err(RequestError {
                kind: RequestErrorKind::Rpc,
                code: None,
                message: "skill path is unreadable".into(),
            }),
        );
        assert!(app.skill_preview_target.is_some());
        assert!(app
            .skill_preview_error
            .as_deref()
            .unwrap()
            .contains("unreadable"));
    }

    #[test]
    fn a_foreign_preview_response_is_rejected() {
        let mut app = app();
        app.ready = true;
        app.skill_preview_target = Some("hello".into());
        app.pending.insert(
            8,
            Pending::SkillPreview {
                name: "hello".into(),
            },
        );
        app.handle_request_done(
            8,
            Ok(json!({
                "address": {"rootSessionId": "other"},
                "name": "hello",
                "path": "/tmp/hello/SKILL.md",
                "text": "body",
                "truncated": false,
                "bytes": 4,
                "limit": 65536,
            })),
        );
        assert!(app.skill_preview.is_none());
        assert!(app
            .skill_preview_error
            .as_deref()
            .unwrap()
            .contains("belongs to root session"));
    }

    #[test]
    fn a_catalog_refresh_drops_a_removed_preview() {
        let mut app = app();
        install_catalog(&mut app, json!([skill_entry("hello", false)]), json!([]));
        app.skill_preview_target = Some("hello".into());
        app.skill_preview = Some(
            SkillPreview::parse(
                &json!({
                    "address": {"rootSessionId": "root"},
                    "name": "hello",
                    "path": "/tmp/hello/SKILL.md",
                    "text": "body",
                    "truncated": false,
                    "bytes": 4,
                    "limit": 65536,
                }),
                "root",
            )
            .unwrap(),
        );
        install_catalog(&mut app, json!([skill_entry("other", false)]), json!([]));
        app.reconcile_skill_preview();
        assert!(app.skill_preview_target.is_none());
        assert!(app.skill_preview.is_none());
    }

    #[test]
    fn a_pathless_skill_is_still_submitted_literally() {
        let mut app = app();
        app.ready = true;
        let mut entry = skill_entry("virtual", false);
        entry.as_object_mut().unwrap().remove("path");
        install_catalog(&mut app, json!([entry]), json!([]));
        app.drafts.current_mut().set_text("/virtual");
        app.submit_draft(Delivery::Queue);
        assert!(app.drafts.current().is_empty());
        assert_eq!(app.model.live().echoes[0].text, "/virtual");
    }

    #[test]
    fn an_official_injection_clears_the_pending_skill_fact() {
        let mut app = app();
        app.ready = true;
        install_catalog(&mut app, json!([skill_entry("hello", false)]), json!([]));
        app.drafts.current_mut().set_text("/hello");
        app.submit_draft(Delivery::Queue);
        let request_id = app.model.live().echoes[0].request_id.clone();
        // The unit environment has no live host link; simulate the accepted
        // submission that would have armed the official-message tracker.
        app.invocation.note_submission("hello", request_id.clone());
        session_event(
            &mut app,
            "user/message",
            json!({"source": {"kind": "user", "rpcId": request_id}, "content": []}),
        );
        session_event(
            &mut app,
            "user/message",
            json!({"source": {"kind": "skill-invocation", "name": "hello"}, "content": []}),
        );
        assert!(app.notice.as_deref().unwrap().contains("已注入"));
        assert_eq!(app.invocation.pending(), None);
    }

    #[test]
    fn a_turn_end_without_injection_reports_not_injected() {
        let mut app = app();
        app.ready = true;
        install_catalog(&mut app, json!([skill_entry("hello", false)]), json!([]));
        app.drafts.current_mut().set_text("/hello");
        app.submit_draft(Delivery::Queue);
        let request_id = app.model.live().echoes[0].request_id.clone();
        app.invocation.note_submission("hello", request_id.clone());
        session_event(
            &mut app,
            "user/message",
            json!({"source": {"kind": "user", "rpcId": request_id}, "content": []}),
        );
        session_event(
            &mut app,
            "turn/end",
            json!({"turn": 1, "reason": {"kind": "completed"}}),
        );
        assert!(app.notice.as_deref().unwrap().contains("未注入"));
    }

    #[test]
    fn page_keys_scroll_the_rules_panel_instead_of_the_feed() {
        let mut app = app();
        app.open_instructions_panel();
        app.instructions_max_scroll = 100;
        let before = app.scroll_back;
        app.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        assert_eq!(app.instructions_scroll, 10);
        assert_eq!(app.scroll_back, before);
        assert!(app.instructions_panel_open());
    }

    #[test]
    fn subscription_kinds_cover_every_contracted_event_kind() {
        let mut ours = crate::link::SUBSCRIPTION_KINDS.to_vec();
        // Job kinds are added by `subscription_kinds` only when the host
        // reports the capability; the full potential set must still cover the
        // whole contract.
        ours.extend(["jobs.event", "jobs.output"]);
        ours.sort_unstable();
        let mut expected = nano_runtime::EVENT_KINDS.to_vec();
        expected.sort_unstable();
        assert_eq!(ours, expected);
    }

    #[test]
    fn a_stream_event_does_not_close_the_skills_panel() {
        let mut app = app();
        app.open_skills_panel();
        app.handle_link(LinkEvent::Host(nano_runtime::HostEvent::Event(
            nano_runtime::protocol::EventParams {
                runtime_id: "rt".into(),
                generation: 1,
                subscription_id: "sub-1".into(),
                sequence: 1,
                kind: "conversation.status".into(),
                payload: json!({"address": {"rootSessionId": "root"}, "status": "idle"}),
            },
        )));
        assert!(app.skills_panel_open());
    }

    #[test]
    fn tab_does_not_close_the_skills_panel() {
        let mut app = app();
        app.open_skills_panel();
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert!(app.skills_panel_open());
    }

    #[test]
    fn alt_r_opens_the_instruction_sources_panel() {
        let mut app = app();
        app.handle_link(LinkEvent::Host(nano_runtime::HostEvent::Event(
            nano_runtime::protocol::EventParams {
                runtime_id: "rt".into(),
                generation: 1,
                subscription_id: "sub-1".into(),
                sequence: 1,
                kind: "instructions.changed".into(),
                payload: json!({
                    "address": {"rootSessionId": "root"},
                    "available": true,
                    "baseline": true,
                    "loadedAt": 1_000_000,
                    "sources": [{"path": "AGENTS.md", "action": "set"}],
                    "budgetNotice": "Workspace instruction budget 65536 bytes: omitted CLAUDE.md",
                }),
            },
        )));
        assert_eq!(app.instructions.sources.len(), 1);
        app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::ALT));
        assert!(app.instructions_panel_open());
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.scene.focus().input_focused());
    }
}
