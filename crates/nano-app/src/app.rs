//! Application state and key handling.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};
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
    CommandRouter, ConversationItem, ConversationModel, ConversationStatus, Delivery, DraftStore,
    FeatureContribution, FeatureRegistry, FocusId, InputBuffer, InputEvent, InputMode,
    InputOutcome, InputRoute, MessageKind, SceneId, SceneState, SessionAddress, SubmitError,
};
use nano_decisions::{
    answers_to_wire, DecisionAction, DecisionClick, DecisionKey, DecisionTop, DecisionsModel,
};
use nano_jobs::{BashRecord, JobsModel, SHELL_NOTICE};
use nano_monitor::MonitorModel;
use nano_runtime::{CapabilityStatus, Hello, SessionMode};
use nano_schedule::{ScheduleAction, ScheduleModel};
use nano_sessions::{
    CompactionOutcome, ContextMeter, RenameOutcome, SessionSummary, SwitchMachine, SwitchTarget,
    TargetMode,
};
use nano_skills::{
    InstructionState, InvocationFact, InvocationTracker, NameResolution, SkillCatalog, SkillPreview,
};
use nano_subagents::{AgentMode, ComposerGate, ExitTarget, SubagentState};
use nano_terminal::{
    is_close_key, is_toggle_key, Command as TerminalCommand, Failure, RequestKind, TerminalState,
    TerminalStore,
};
use nano_transcript::{Hit, HitKind, RecordedQuestionState, TranscriptSource, TranscriptView};
use ratatui::layout::Rect;
use serde_json::{json, Value};

use crate::cli::Cli;
use crate::link::{HostLink, LinkEvent, RequestError, RequestErrorKind, SHELL_REQUEST_TIMEOUT};

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

/// What the activity row above the composer reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityKind {
    /// A turn is running.
    Running,
    /// The root conversation is waiting for approval.
    AwaitingApproval,
    /// The root conversation is waiting for a question answer.
    AwaitingQuestion,
}

impl ActivityKind {
    /// Text shown after the 2ch indicator.
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "正在运行… · Esc 可停止",
            Self::AwaitingApproval => "等待你的授权",
            Self::AwaitingQuestion => "等待你的选择",
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

/// Reduced-motion opt-out. S13 owns the final detection contract; until it is
/// frozen, the environment switch is the only supported input.
fn reduced_motion_env() -> bool {
    matches!(
        std::env::var("NANO_REDUCED_MOTION").as_deref(),
        Ok("1") | Ok("true")
    )
}

/// Read a complete child wire address from one event payload. A missing field
/// means this is not a child event; partial addresses are ignored rather than
/// guessed.
fn child_address_from_payload(payload: &Value) -> Option<SessionAddress> {
    let address = payload.get("address")?;
    let root = address.get("rootSessionId").and_then(Value::as_str)?;
    let child = address.get("childSessionId").and_then(Value::as_str)?;
    let parent = address.get("parentSessionId").and_then(Value::as_str)?;
    let mode = address.get("mode").and_then(Value::as_str)?;
    let mode = match mode {
        "continuable" => nano_conversation::SubagentMode::Continuable,
        "one-shot" => nano_conversation::SubagentMode::OneShot,
        "unknown" => nano_conversation::SubagentMode::Unknown,
        _ => return None,
    };
    Some(SessionAddress::child(root, parent, child, mode))
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
    /// Read the official session list.
    SessionsList,
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
    /// Read the monitor list.
    MonitorList,
    /// Create one monitor for a job.
    MonitorCreate {
        /// Official job id.
        job_id: String,
        /// Unread bytes the form showed before the request.
        unread_bytes: u64,
    },
    /// Stop monitor delivery only.
    MonitorStop {
        /// Official job id.
        job_id: String,
    },
    /// Reset one monitor's budget.
    MonitorReset {
        /// Official job id.
        job_id: String,
    },
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
    /// Read pending approval/question cards after a reconnect.
    DecisionsSnapshot,
    /// One answer to a decision card.
    Decision {
        /// The action that was sent; used to roll the card back on failure.
        action: DecisionAction,
        /// Short label for status and diagnostics.
        label: String,
    },
    /// Read the schedule catalog.
    ScheduleCatalog,
    /// Create one schedule task.
    ScheduleCreate,
    /// Update one schedule task.
    ScheduleUpdate,
    /// Delete one schedule task.
    ScheduleDelete,
    /// Read one delivery-history page.
    ScheduleHistory {
        /// Exclusive cursor for an older page.
        before: Option<String>,
    },
    /// One S09 terminal request and the epoch it belongs to.
    Terminal {
        /// Pending identity.
        kind: RequestKind,
        /// Operation epoch; stale responses are ignored.
        epoch: u64,
    },
    /// Read the direct-child snapshot.
    SubagentsSnapshot,
    /// Read the recursive descendant list.
    SubagentsList,
    /// Resolve one child's mode, liveness, timing, and sequence.
    SubagentsDescribe {
        /// Child being described.
        child_id: String,
    },
    /// One human continuation prompt.
    SubagentPrompt {
        /// Business identity used to restore the draft on failure.
        request_id: String,
    },
    /// Stop one child's current turn.
    SubagentInterrupt {
        /// Child being stopped.
        child_id: String,
    },
    /// Open the child transcript subscription.
    SubagentSubscribe {
        /// Address key the subscription serves.
        address_key: String,
        /// Subscription generation; stale responses are released immediately.
        generation: u64,
    },
    /// Release a child transcript subscription.
    SubagentUnsubscribe,
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
/// One monitor attach form. The unread byte count is shown before the user
/// chooses between "new output" and "from the retained beginning".
#[derive(Debug, Clone)]
pub struct MonitorForm {
    /// Observed official job id.
    pub job_id: String,
    /// Job label for the form title.
    pub job_label: String,
    /// Bytes already retained when the form opened.
    pub unread_bytes: u64,
    /// Replay the retained head from `earliest`.
    pub from_earliest: bool,
    /// `quiet` uses `inject`; otherwise `notify` uses `followup`.
    pub quiet: bool,
    /// Optional literal/regex pattern.
    pub filter: String,
    /// Treat the pattern as a regular expression.
    pub filter_regex: bool,
    /// Whether typed characters edit the filter.
    pub editing_filter: bool,
}

/// One in-progress Up/Down recall over submitted prompts. The snapshot is
/// taken when browsing starts so later durable messages cannot shift the
/// entry under the cursor; the displaced draft is restored on the way back.
#[derive(Debug, Clone)]
pub struct HistoryBrowse {
    address: SessionAddress,
    entries: Vec<String>,
    /// Distance from the newest entry: 0 is the latest prompt.
    index: usize,
    saved_text: String,
    saved_cursor: usize,
}

/// Main application state.
pub struct App {
    /// Launch options.
    pub cli: Cli,
    /// Conversation state.
    pub model: ConversationModel,
    /// Per-address drafts.
    pub drafts: DraftStore,
    /// Persistent root prompt history, resolved when the host link attaches.
    pub history_path: Option<PathBuf>,
    /// Up/Down history recall for the active composer.
    pub history_browse: Option<HistoryBrowse>,
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
    /// Last measured feed viewport height.
    pub feed_height: usize,
    /// Reading view (transcript, line budgets, expansion, scroll).
    pub view: TranscriptView,
    /// Question and approval cards.
    pub decisions: DecisionsModel,
    /// Whether the transcript entries need rebuilding before the next draw.
    pub transcript_dirty: bool,
    /// Feed hit targets from the last draw.
    pub feed_hits: Vec<Option<Hit>>,
    /// Feed region from the last draw.
    pub feed_area: Rect,
    /// "回到最新" button region from the last draw.
    pub jump_area: Rect,
    /// Decision card region from the last draw.
    pub decision_area: Rect,
    /// Decision click targets from the last draw.
    pub decision_hits: Vec<Option<DecisionClick>>,
    /// Whether the 2ch activity indicator is in its alternate phase.
    pub activity_phase: bool,
    /// Next 500ms indicator transition, when a turn is running.
    pub activity_at: Option<Instant>,
    /// Composer send button region from the last draw.
    pub send_area: Rect,
    /// Pointer press position, used to distinguish a click from a drag.
    pub mouse_down: Option<(u16, u16)>,
    /// A drag happened between press and release.
    pub mouse_dragged: bool,
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
    /// Schedule overlay state.
    pub schedule: ScheduleModel,
    /// Hello capability detail when the scheduler is not usable.
    pub schedule_unsupported: Option<String>,
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
    /// Explicit main-session switch phase.
    pub switch: SwitchMachine,
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
    /// Official S10 monitor projections, never a second listener.
    pub monitor: MonitorModel,
    /// Whether the monitor sub-view owns the task panel.
    pub monitor_open: bool,
    /// The attach form, when the user is choosing a monitor's scope.
    pub monitor_form: Option<MonitorForm>,
    /// A monitor list request is in flight.
    pub monitor_snapshot_pending: bool,
    /// Select this job's monitor after its projection arrives.
    pub monitor_select_after_refresh: Option<String>,
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
    /// Latest official context-occupancy reference.
    pub context: Option<ContextMeter>,
    /// Why the last context read failed, when it did.
    pub context_error: Option<String>,
    /// Official session list panel. Named distinctly from S04's command panel.
    pub sessions_panel: SessionPanel,
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
    /// S09 terminal feature, created when the host link exposes its data root.
    pub terminal: Option<TerminalState>,
    /// Screen area used by terminal mouse coordinates, set by the draw pass.
    pub terminal_screen_area: Rect,
    /// Resolved Nano data root, kept across host restarts for terminal rebinding.
    pub terminal_data_root: Option<PathBuf>,
    /// S12 official child catalog, dock, navigation, and input gates.
    pub subagents: SubagentState,
    /// Child conversation models keyed by address key.
    pub child_models: HashMap<String, ConversationModel>,
    /// Child transcript views keyed by address key; the mounted one lives in
    /// `view` while `parent_view` holds the parent transcript.
    pub child_views: HashMap<String, TranscriptView>,
    /// Parent transcript kept aside while a child view is mounted.
    pub parent_view: Option<TranscriptView>,
    /// Address key of the mounted child view.
    pub active_child_key: Option<String>,
    /// Open child session subscription.
    pub subagent_subscription: Option<String>,
    /// Monotonic generation for in-flight child subscriptions.
    pub subagent_subscribe_generation: u64,
    /// Child prompt drafts kept per request id for address-correct restore.
    pub subagent_backups: HashMap<String, (SessionAddress, SubmittedDraft)>,
    /// A snapshot request is in flight.
    pub subagents_snapshot_pending: bool,
    /// A snapshot was requested while one was in flight.
    pub subagents_snapshot_queued: bool,
    /// A recursive list request is in flight.
    pub subagent_list_loading: bool,
    /// Next blink transition to service in `tick`.
    pub subagent_blink_at: Option<Instant>,
    /// Reduced motion requested by the environment; blinking becomes static.
    pub reduced_motion: bool,
    /// Screen area of the subagent dock, set by the draw pass.
    pub subagent_dock_area: Rect,
    /// Dock rows as (index, screen row), including the overflow entry.
    pub subagent_dock_hits: Vec<(usize, u16)>,
    /// Screen area of the child-view exit button.
    pub subagent_exit_area: Rect,
    /// Screen area of the all-agents list, set by the draw pass.
    pub subagent_list_area: Rect,
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
        registry.register(
            FeatureContribution::new("terminal", SceneId::new("terminal")).with_key("f12"),
        );
        let mut scene = SceneState::conversation();
        scene.set_address(address.clone());
        Self {
            cli,
            model: ConversationModel::new(address.clone()),
            drafts: DraftStore::new(address.clone()),
            history_path: None,
            history_browse: None,
            attachments: AttachmentDraftStore::new(address.clone()),
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
            feed_height: 10,
            view: TranscriptView::new(address.clone(), TranscriptSource::Interactive),
            decisions: DecisionsModel::new(address.clone()),
            transcript_dirty: true,
            feed_hits: Vec::new(),
            feed_area: Rect::default(),
            jump_area: Rect::default(),
            decision_area: Rect::default(),
            decision_hits: Vec::new(),
            activity_phase: false,
            activity_at: None,
            send_area: Rect::default(),
            mouse_down: None,
            mouse_dragged: false,
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
            schedule: ScheduleModel::new(),
            schedule_unsupported: None,
            attachment_limits: None,
            completion: CompletionState::default(),
            completion_cancel: None,
            picker: None,
            uploads: HashMap::new(),
            switch: SwitchMachine::new(),
            jobs: JobsModel::new(jobs_root),
            jobs_open: false,
            jobs_scroll_back: 0,
            following: std::collections::HashSet::new(),
            follow_dirty: std::collections::HashSet::new(),
            jobs_snapshot_pending: false,
            jobs_detail_lines: 0,
            monitor: MonitorModel::new(address.root_session_id.clone()),
            monitor_open: false,
            monitor_form: None,
            monitor_snapshot_pending: false,
            monitor_select_after_refresh: None,
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
            context: None,
            context_error: None,
            sessions_panel: SessionPanel::default(),
            restart_rx: None,
            compact: None,
            compact_in_flight: false,
            compact_request_id: None,
            compact_maybe_running: false,
            terminal: None,
            terminal_screen_area: Rect::new(0, 0, 0, 0),
            terminal_data_root: None,
            subagents: SubagentState::new(address.root_session_id.clone()),
            child_models: HashMap::new(),
            child_views: HashMap::new(),
            parent_view: None,
            active_child_key: None,
            subagent_subscription: None,
            subagent_subscribe_generation: 0,
            subagent_backups: HashMap::new(),
            subagents_snapshot_pending: false,
            subagents_snapshot_queued: false,
            subagent_list_loading: false,
            subagent_blink_at: None,
            reduced_motion: reduced_motion_env(),
            subagent_dock_area: Rect::default(),
            subagent_dock_hits: Vec::new(),
            subagent_exit_area: Rect::default(),
            subagent_list_area: Rect::default(),
        }
    }

    /// Attach the host link and open the terminal identity store under the
    /// resolved Nano data root.
    pub fn attach_link(&mut self, link: HostLink) {
        let data_root = link.data_root();
        self.history_path = Some(data_root.join("history.jsonl"));
        self.terminal_data_root = Some(data_root);
        let root_session_id = self.model.address().root_session_id.clone();
        self.bind_terminal(root_session_id);
        self.link = Some(link);
    }

    /// Rebuild the terminal feature for one root session. S07 owns the switch;
    /// the old host shuts its terminals down, and the view must never carry the
    /// old screen or attachment into the new session.
    fn bind_terminal(&mut self, root_session_id: String) {
        let Some(data_root) = self.terminal_data_root.clone() else {
            self.terminal = None;
            return;
        };
        self.terminal = Some(match TerminalStore::open(&data_root, &root_session_id) {
            Ok(store) => {
                let mut terminal = TerminalState::new(store, (80, 24));
                terminal.set_root_session_id(root_session_id);
                terminal
            }
            Err(error) => TerminalState::unavailable(error, (80, 24)),
        });
        self.terminal_screen_area = Rect::new(0, 0, 0, 0);
    }

    /// Whether the terminal scene is the active main scene.
    pub fn terminal_active(&self) -> bool {
        self.scene.active() == &SceneId::new("terminal")
    }

    /// Whether a subagent view currently owns the conversation surface.
    pub fn subagent_active(&self) -> bool {
        self.active_child_key.is_some()
    }

    /// Active conversation model: the mounted child, otherwise the root.
    pub fn active_model(&self) -> &ConversationModel {
        self.active_child_key
            .as_ref()
            .and_then(|key| self.child_models.get(key))
            .unwrap_or(&self.model)
    }

    /// Ask the host for the direct-child snapshot.
    pub fn refresh_subagents(&mut self) {
        if !self.ready || !self.capability_supported("subagents") {
            return;
        }
        if self.subagents_snapshot_pending {
            self.subagents_snapshot_queued = true;
            return;
        }
        let Some(link) = &self.link else {
            return;
        };
        match link.request("nano.subagents.snapshot", json!({})) {
            Ok(id) => {
                self.subagents_snapshot_pending = true;
                self.pending.insert(id, Pending::SubagentsSnapshot);
            }
            Err(message) => self.error = Some(format!("子代理目录读取失败：{message}")),
        }
    }

    /// Ask the host for the recursive descendant list.
    pub fn refresh_subagents_list(&mut self) {
        if !self.ready || !self.capability_supported("subagents") || self.subagent_list_loading {
            return;
        }
        let Some(link) = &self.link else {
            return;
        };
        match link.request("nano.subagents.list", json!({})) {
            Ok(id) => {
                self.subagent_list_loading = true;
                self.pending.insert(id, Pending::SubagentsList);
            }
            Err(message) => self.error = Some(format!("子代理列表读取失败：{message}")),
        }
    }

    /// Ask the host to resolve one child's mode, liveness, timing, and seq.
    fn request_subagent_describe(&mut self, child_id: &str) {
        let Some(mounted) = self.subagents.mounted() else {
            return;
        };
        let Some(child) = &mounted.address.child else {
            return;
        };
        if self.subagents.describing.as_deref() == Some(child_id) {
            return;
        }
        let Some(link) = &self.link else {
            return;
        };
        let params = json!({
            "parentSessionId": child.parent_session_id,
            "childSessionId": child.child_session_id,
        });
        match link.request("nano.subagents.describe", params) {
            Ok(id) => {
                self.subagents.describing = Some(child_id.to_string());
                self.pending.insert(
                    id,
                    Pending::SubagentsDescribe {
                        child_id: child_id.to_string(),
                    },
                );
            }
            Err(message) => self.error = Some(format!("子代理历史读取失败：{message}")),
        }
    }

    /// Open one dock row. The view is mounted immediately and the official
    /// describe confirms mode and history before input is enabled.
    pub fn enter_subagent(&mut self, index: usize) -> bool {
        if !self.subagents.enter_dock(index) {
            return false;
        }
        self.mount_child_surface();
        true
    }

    /// Open the overflow row, which is the all-agents list.
    pub fn enter_subagent_overflow(&mut self) {
        self.subagents.enter_overflow();
        self.refresh_subagents_list();
    }

    /// Open one row from the all-agents list.
    pub fn enter_subagent_list(&mut self, index: usize) -> bool {
        if !self.subagents.enter_list(index) {
            return false;
        }
        self.mount_child_surface();
        true
    }

    /// Swap the parent transcript aside and install a placeholder child view.
    fn mount_child_surface(&mut self) {
        let Some(mounted) = self.subagents.mounted() else {
            return;
        };
        let address = mounted.address.clone();
        let child_id = address
            .child
            .as_ref()
            .map(|child| child.child_session_id.clone());
        let key = address.key();
        let placeholder = TranscriptView::new(address, TranscriptSource::ReadOnly);
        if self.parent_view.is_none() {
            self.parent_view = Some(std::mem::replace(&mut self.view, placeholder));
        } else {
            self.view = placeholder;
        }
        self.active_child_key = Some(key);
        self.transcript_dirty = true;
        self.completion.clear();
        // A child view owns the conversation surface: hidden root panels and
        // focus owners must not keep consuming keys underneath it.
        self.scene.focus_mut().reset(FocusId::input());
        self.sessions_panel.close();
        self.jobs_open = false;
        self.panel.close();
        self.picker = None;
        self.schedule.close_view();
        self.shell_confirm = None;
        self.edit_target = None;
        self.edit_backup = None;
        if !self.ready {
            // No live host means no live exact parent. Keep the view readable
            // and the composer read-only until a host is available again.
            if let Some(mounted) = self.subagents.nav_mut().mounted_mut() {
                mounted.parent_live = false;
                mounted.activity = nano_subagents::Activity::Inactive;
            }
        }
        if let Some(child_id) = child_id {
            self.request_subagent_describe(&child_id);
        }
    }

    /// Apply one `nano.subagents.describe` result and mount its real view.
    pub(crate) fn apply_subagent_description(&mut self, value: &Value) -> bool {
        let row = match self.subagents.apply_description(value) {
            Ok(row) => row,
            Err(message) => {
                self.error = Some(format!("子代理描述无效：{message}"));
                return true;
            }
        };
        let Some(mounted) = self.subagents.mounted().cloned() else {
            return true;
        };
        let Some(child) = mounted.address.child.as_ref() else {
            return true;
        };
        if child.child_session_id != row.id {
            // A describe for an old mount arrived after the user returned.
            return true;
        }
        let address = mounted.address.clone();
        let key = address.key();
        let source = if mounted.mode == AgentMode::Continuable {
            TranscriptSource::Interactive
        } else {
            TranscriptSource::ReadOnly
        };
        if !self.child_models.contains_key(&key) {
            let mut model = ConversationModel::new(address.clone());
            model.apply_replay_baseline(row.seq.unwrap_or(0), false);
            self.child_models.insert(key.clone(), model);
        }
        if self.active_child_key.as_deref() != Some(key.as_str()) {
            let view = self
                .child_views
                .remove(&key)
                .unwrap_or_else(|| TranscriptView::new(address.clone(), source));
            let old = std::mem::replace(&mut self.view, view);
            if let Some(old_key) = self.active_child_key.take() {
                self.child_views.insert(old_key, old);
            }
            self.active_child_key = Some(key.clone());
        }
        self.drafts.switch_to(address.clone());
        self.attachments.switch_to(address.clone());
        self.completion.clear();
        self.transcript_dirty = true;
        // A cached child model already holds everything up to its last
        // applied seq. Re-subscribing from the bounded window would replay
        // those events and duplicate transcript rows; resume at the next seq.
        let from = self
            .child_models
            .get(&key)
            .and_then(|model| model.last_applied_seq())
            .map(|seq| seq.saturating_add(1))
            .unwrap_or_else(|| {
                row.seq
                    .unwrap_or(0)
                    .saturating_sub(nano_conversation::MAX_REPLAY_EVENTS)
            });
        self.subscribe_child(&address, from);
        true
    }

    /// Open the child transcript subscription with a bounded replay window.
    fn subscribe_child(&mut self, address: &nano_conversation::SessionAddress, from_seq: u64) {
        if self.subagent_subscription.is_some() {
            self.unsubscribe_child();
        }
        let Some(link) = &self.link else {
            return;
        };
        let params = json!({
            "address": address.wire_value(),
            "kinds": ["session.event", "conversation.stream"],
            "fromSeq": from_seq,
        });
        self.subagent_subscribe_generation = self.subagent_subscribe_generation.wrapping_add(1);
        let generation = self.subagent_subscribe_generation;
        match link.request("nano.subscribe", params) {
            Ok(id) => {
                self.pending.insert(
                    id,
                    Pending::SubagentSubscribe {
                        address_key: address.key(),
                        generation,
                    },
                );
            }
            Err(message) => self.error = Some(format!("子代理历史订阅失败：{message}")),
        }
    }

    /// Release the child subscription. The child itself keeps running.
    fn unsubscribe_child(&mut self) {
        let Some(subscription_id) = self.subagent_subscription.take() else {
            return;
        };
        if let Some(link) = &self.link {
            let id = link.request(
                "nano.unsubscribe",
                json!({ "subscriptionId": subscription_id }),
            );
            if let Ok(id) = id {
                self.pending.insert(id, Pending::SubagentUnsubscribe);
            }
        }
    }

    /// Leave the child view and return to its exact origin.
    pub fn leave_subagent(&mut self) -> bool {
        let target = self.subagents.exit();
        self.subagents.describing = None;
        self.unsubscribe_child();
        if let Some(parent) = self.parent_view.take() {
            let child = std::mem::replace(&mut self.view, parent);
            if let Some(key) = self.active_child_key.take() {
                if self.child_views.len() >= 16 {
                    self.child_views.clear();
                }
                self.child_views.insert(key, child);
            }
        } else {
            self.active_child_key = None;
        }
        let root = self.model.address().clone();
        self.drafts.switch_to(root.clone());
        self.attachments.switch_to(root);
        self.completion.clear();
        self.transcript_dirty = true;
        match target {
            Some(ExitTarget::Dock { index, child_id }) => {
                let (rows, overflow) = self.subagents.dock();
                let count = rows.len() + usize::from(overflow > 0);
                let restored = rows.iter().position(|row| row.id == child_id);
                match restored {
                    Some(index) if index < count => {
                        self.subagents.nav_mut().dock_selected = index;
                    }
                    None if index < count => {
                        self.subagents.nav_mut().dock_selected = index;
                    }
                    _ => self.subagents.nav_mut().dock_focus = false,
                }
            }
            Some(ExitTarget::List { child_id }) => {
                match self
                    .subagents
                    .list_rows()
                    .iter()
                    .position(|row| row.id == child_id)
                {
                    Some(index) => self.subagents.nav_mut().list_selected = index,
                    None => {
                        // The entry disappeared while it was open. Do not strand
                        // the list on a stale row; fall back to the input.
                        self.subagents.nav_mut().close_list();
                        self.subagents.nav_mut().dock_focus = false;
                    }
                }
            }
            None => {}
        }
        true
    }

    /// Send the mounted child's draft through the official continuation path.
    pub fn submit_subagent_draft(&mut self, delivery: Delivery) {
        self.history_browse = None;
        let Some(mounted) = self.subagents.mounted().cloned() else {
            return;
        };
        let gate = self
            .subagents
            .mounted_gate()
            .unwrap_or(ComposerGate::Resolving);
        if !gate.can_send() {
            self.notice = Some(gate.hint().to_string());
            return;
        }
        let text = self.drafts.current().text().to_string();
        let has_attachments = !self.attachments.current().is_empty();
        if text.trim().is_empty() && !has_attachments {
            self.error = Some("内容为空，草稿已保留".to_string());
            return;
        }
        if let Err(blocker) = self.attachments.current().ready() {
            self.error = Some(blocker.message());
            return;
        }
        let content = self.attachments.current().content(&text);
        let blocks = match content.to_wire_blocks() {
            Ok(blocks) => blocks,
            Err(error) => {
                self.error = Some(error.to_string());
                return;
            }
        };
        if blocks
            .iter()
            .any(|block| block.get("type").and_then(Value::as_str) == Some("file"))
        {
            self.error = Some("子代理只接受图片附件；通用文件已被拒绝，草稿已保留".to_string());
            return;
        }
        let request_id = nano_conversation::new_request_id();
        let params =
            nano_subagents::prompt_params(&mounted.address, &request_id, delivery, &blocks);
        let snapshot = SubmittedDraft {
            text: text.clone(),
            attachments: self.attachments.current().clone(),
        };
        if let Some(model) = self.child_models.get_mut(&mounted.address.key()) {
            model.track_submission(&request_id, &text, delivery);
        }
        self.drafts.current_mut().take();
        self.attachments.current_mut().clear();
        self.completion.clear();
        self.subagent_backups
            .insert(request_id.clone(), (mounted.address.clone(), snapshot));
        let Some(link) = &self.link else {
            self.restore_subagent_draft(&request_id);
            if let Some(model) = self.child_models.get_mut(&mounted.address.key()) {
                model.submit_failed(&request_id, "宿主连接已关闭");
            }
            self.error = Some("宿主连接已关闭；子代理消息未发送".to_string());
            return;
        };
        match link.request("nano.subagents.prompt", params) {
            Ok(id) => {
                self.pending.insert(
                    id,
                    Pending::SubagentPrompt {
                        request_id: request_id.clone(),
                    },
                );
            }
            Err(message) => {
                self.restore_subagent_draft(&request_id);
                if let Some(model) = self.child_models.get_mut(&mounted.address.key()) {
                    model.submit_failed(&request_id, message.clone());
                }
                self.error = Some(format!("子代理发送失败：{message}"));
            }
        }
    }

    /// Restore one failed child prompt to the address that sent it, even if
    /// the user has already left that child view.
    fn restore_subagent_draft(&mut self, request_id: &str) {
        let Some((address, snapshot)) = self.subagent_backups.remove(request_id) else {
            return;
        };
        let mut restored = false;
        if let Some(draft) = self.drafts.get_mut(&address) {
            let current = draft.text().to_string();
            if current.is_empty() {
                draft.set_text(snapshot.text.clone());
            } else if !current.contains(&snapshot.text) {
                draft.set_text(format!("{}\n\n{current}", snapshot.text));
            }
            restored = true;
        }
        if let Some(attachments) = self.attachments.get_mut(&address) {
            if attachments.is_empty() {
                *attachments = snapshot.attachments.clone();
            }
            restored = true;
        }
        if !restored {
            // The address was never switched in (rare teardown race); keep the
            // text visible on the current draft rather than losing it.
            if self.drafts.current().is_empty() {
                self.drafts.current_mut().set_text(snapshot.text.clone());
            }
        }
    }

    /// Stop the mounted child's current turn. Queued messages and descendants
    /// stay alive; `dispose` is never used.
    pub fn stop_mounted_subagent(&mut self) {
        let Some(mounted) = self.subagents.mounted().cloned() else {
            return;
        };
        let gate = self
            .subagents
            .mounted_gate()
            .unwrap_or(ComposerGate::Resolving);
        if !gate.can_stop() {
            self.notice = Some("当前子代理没有可停止的回合".to_string());
            return;
        }
        self.interrupt_subagent_address(&mounted.address);
    }

    /// Stop one dock row's current turn.
    fn stop_selected_subagent(&mut self) {
        let index = self.subagents.nav().dock_selected;
        let (rows, _) = self.subagents.dock();
        let Some(row) = rows.get(index) else {
            return;
        };
        if row.mode != AgentMode::Continuable {
            self.notice = Some("一次性或外部子代理没有可停止的回合".to_string());
            return;
        }
        let address = self.subagents.address_for(row);
        self.interrupt_subagent_address(&address);
    }

    fn interrupt_subagent_address(&mut self, address: &nano_conversation::SessionAddress) {
        let Some(link) = &self.link else {
            return;
        };
        let child_id = address
            .child
            .as_ref()
            .map(|child| child.child_session_id.clone())
            .unwrap_or_default();
        match link.request(
            "nano.subagents.interrupt",
            nano_subagents::interrupt_params(address),
        ) {
            Ok(id) => {
                self.pending
                    .insert(id, Pending::SubagentInterrupt { child_id });
            }
            Err(message) => self.error = Some(format!("子代理停止失败：{message}")),
        }
    }

    /// Apply terminal side effects in issue order.
    fn dispatch_terminal(&mut self, commands: Vec<TerminalCommand>) {
        let mut commands: std::collections::VecDeque<TerminalCommand> = commands.into();
        while let Some(command) = commands.pop_front() {
            match command {
                TerminalCommand::Request {
                    kind,
                    method,
                    params,
                    epoch,
                } => match self.link.as_ref().map(|link| link.request(method, params)) {
                    Some(Ok(id)) => {
                        self.pending.insert(id, Pending::Terminal { kind, epoch });
                    }
                    Some(Err(message)) => {
                        if let Some(terminal) = self.terminal.as_mut() {
                            commands.extend(terminal.on_response_epoch(
                                epoch,
                                kind,
                                Err(Failure::unknown(message)),
                            ));
                        }
                    }
                    None => {
                        if let Some(terminal) = self.terminal.as_mut() {
                            commands.extend(terminal.on_response_epoch(
                                epoch,
                                kind,
                                Err(Failure::unknown("宿主未就绪")),
                            ));
                        }
                    }
                },
                TerminalCommand::Notify { method, params } => {
                    if let Some(link) = &self.link {
                        if let Err(message) = link.notify(method, params) {
                            self.error = Some(format!("终端通信失败：{message}"));
                            if let Some(terminal) = self.terminal.as_mut() {
                                commands.extend(terminal.disconnect("宿主通信已断开"));
                            }
                        }
                    }
                }
                TerminalCommand::Clipboard(text) => {
                    write_clipboard(&text);
                }
                TerminalCommand::Status { error, message } => {
                    if error {
                        self.error = Some(message);
                    } else {
                        self.notice = Some(message);
                    }
                }
            }
        }
    }

    /// Toggle into the terminal scene and start its attach flow.
    pub fn open_terminal(&mut self) {
        self.scene.switch(SceneId::new("terminal"));
        let commands = self
            .terminal
            .as_mut()
            .map(TerminalState::open)
            .unwrap_or_default();
        self.dispatch_terminal(commands);
    }

    /// Leave the terminal scene without stopping the process.
    pub fn hide_terminal(&mut self) {
        self.scene.switch(SceneId::conversation());
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
                self.error = Some(format!(
                    "模型目录刷新失败：{message}（保留上次值，可能过期）"
                ));
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
                    self.error = Some("正在编辑队列项；先应用或取消再选择命令".to_string());
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
                    self.error = Some(if self.skills.resolve(name) == NameResolution::Command {
                        format!("/{name} 是宿主命令，命令优先；{reason}")
                    } else {
                        reason
                    });
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
        if self.terminal_active() {
            if is_toggle_key(&key) {
                self.hide_terminal();
                return true;
            }
            if is_close_key(&key) {
                let commands = self
                    .terminal
                    .as_mut()
                    .map(TerminalState::close_terminal)
                    .unwrap_or_default();
                self.dispatch_terminal(commands);
                return true;
            }
            self.clear_transient_status();
            let commands = self
                .terminal
                .as_mut()
                .map(|terminal| terminal.handle_key(key))
                .unwrap_or_default();
            self.dispatch_terminal(commands);
            return true;
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
        if is_toggle_key(&key) {
            self.open_terminal();
            return true;
        }
        if self.schedule.open {
            if key.modifiers.contains(KeyModifiers::CONTROL)
                && matches!(key.code, KeyCode::Char('g') | KeyCode::Char('G'))
            {
                self.schedule.close_view();
                return true;
            }
            let action = self.schedule.handle_key(key);
            self.apply_schedule_action(action);
            return true;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('g') | KeyCode::Char('G'))
        {
            self.toggle_schedule();
            return true;
        }
        if self.sessions_panel.open {
            return self.handle_sessions_panel_key(key);
        }
        if self.subagents.nav().list_open {
            return self.handle_subagent_list_key(key);
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
        if let Some(handled) = self.handle_child_key(key) {
            return handled;
        }
        if key.modifiers.contains(KeyModifiers::ALT)
            && matches!(key.code, KeyCode::Char('l') | KeyCode::Char('L'))
            && !self.subagent_active()
            && self.drafts.current().is_blank()
        {
            // Always reachable, even after every dock entry has aged out.
            self.enter_subagent_overflow();
            return true;
        }
        if !self.subagent_active() && self.decisions.top().is_some() {
            if let Some(mapped) = decision_key(key) {
                self.clear_transient_status();
                self.decisions.handle_key(mapped);
                self.drain_decisions();
                return true;
            }
            return false;
        }
        if matches!(key.code, KeyCode::PageUp) {
            self.view.scroll_mut().page_up(self.feed_height);
            return true;
        }
        if matches!(key.code, KeyCode::PageDown) {
            self.view.scroll_mut().page_down(self.feed_height);
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
        if self.edit_target.is_none() {
            if let Some(handled) = self.handle_history_key(key) {
                return handled;
            }
            if let Some(handled) = self.handle_dock_key(key) {
                return handled;
            }
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('u') => {
                    self.history_browse = None;
                    self.drafts.current_mut().clear();
                    self.attachments.current_mut().clear();
                    self.completion.clear();
                    return true;
                }
                KeyCode::Char('j') => {
                    let old = self.drafts.current().text().to_string();
                    self.history_browse = None;
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
                    self.view.scroll_mut().to_bottom();
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
            if self.compact_in_flight || self.compact_maybe_running {
                self.cancel_compact();
            } else if self.shell_confirm.take().is_some() {
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
                self.history_browse = None;
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
            let mutates = matches!(
                &event,
                InputEvent::Insert(_)
                    | InputEvent::Paste(_)
                    | InputEvent::Newline
                    | InputEvent::Backspace
                    | InputEvent::Delete
            );
            let old = self.drafts.current().text().to_string();
            let outcome = self.drafts.current_mut().handle(&event);
            if mutates {
                // Editing turns the recalled prompt into an ordinary draft.
                self.history_browse = None;
            }
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
        if self.terminal_active() {
            let commands = self
                .terminal
                .as_mut()
                .map(|terminal| terminal.handle_paste(text))
                .unwrap_or_default();
            self.dispatch_terminal(commands);
            return true;
        }
        if self.subagent_active()
            && !self
                .subagents
                .mounted_gate()
                .unwrap_or(ComposerGate::Resolving)
                .can_send()
        {
            return true;
        }
        if !self.subagent_active() && self.decision_owns_seat() {
            self.clear_transient_status();
            return self.decisions.paste(&text);
        }
        if self.schedule.open {
            if let Some(form) = self.schedule.form.as_mut() {
                if let Some(field) = form.focused_text() {
                    field.insert_str(&text);
                    return true;
                }
            }
            return false;
        }
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
                self.attachment_limits = nano_attachments::ImageLimits::from_hello(&hello);
                self.controls.set_capabilities(
                    capability_supported(&hello, "modelSelection"),
                    capability_supported(&hello, "permissionPresets"),
                    capability_supported(&hello, "commands"),
                );
                self.claim_controls_commands();
                self.hello = Some(hello);
                self.schedule_unsupported = match self
                    .hello
                    .as_ref()
                    .and_then(|hello| hello.capabilities.get("schedule"))
                {
                    Some(capability) if capability.status == CapabilityStatus::Supported => None,
                    Some(capability) => Some(
                        capability
                            .detail
                            .clone()
                            .unwrap_or_else(|| "宿主未挂载官方调度服务".to_string()),
                    ),
                    None => Some("宿主没有声明 schedule 能力".to_string()),
                };
                if self.switch.busy() {
                    if let Some(target) = self.switch.succeed() {
                        self.notice = Some(format!("已切换到会话 {}", target.label()));
                    }
                } else {
                    self.notice = Some("已连接官方会话".to_string());
                }
                self.refresh_snapshot();
                self.refresh_decisions();
                self.transcript_dirty = true;
                self.refresh_context();
                self.refresh_jobs_snapshot();
                self.refresh_controls_state();
                self.refresh_skills();
                self.refresh_instructions();
                self.refresh_subagents();
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
                self.mark_uploads_unknown("宿主连接已关闭");
                self.handle_host_loss();
                self.controls.reset();
                self.claim_controls_commands();
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
                self.terminal_data_root = Some(link.data_root());
                let root_session_id = self.model.address().root_session_id.clone();
                self.bind_terminal(root_session_id);
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
        if let Some(terminal) = self.terminal.as_mut() {
            let commands = terminal.disconnect("会话切换失败；终端进程状态未知");
            self.dispatch_terminal(commands);
        }
        self.switch.fail(code, holder_pid, message.clone());
        let label = self.switch.state().label();
        self.error = Some(format!("{label}：{message}；当前没有可用宿主"));
        self.model.mark_disconnected();
    }

    fn handle_host_event(&mut self, event: nano_runtime::HostEvent) -> bool {
        use nano_runtime::HostEvent;
        match event {
            HostEvent::Event(params) => {
                if params.kind == "session.event" || params.kind == "conversation.stream" {
                    if let Some(address) = child_address_from_payload(&params.payload) {
                        return self.handle_child_event(&params.kind, &params.payload, address);
                    }
                }
                self.transcript_dirty = true;
                if params.kind == "session.event" || params.kind.starts_with("decisions.") {
                    self.view.apply_event(&params.kind, &params.payload);
                }
                let mut decision_changed = false;
                if params.kind.starts_with("decisions.") {
                    decision_changed = self.decisions.apply_event(&params.kind, &params.payload);
                    self.drain_decisions();
                }
                if params.kind == "sessions.context" {
                    let changed = self.apply_context_value(&params.payload);
                    return changed || decision_changed;
                }
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
                if params.kind == "schedule.changed" && self.schedule.open && self.ready {
                    self.refresh_schedule_catalog();
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
                    "subagents.event" => self.handle_subagents_event(&params.payload),
                    "monitor.updated" => {
                        let previous_notice = self.monitor.last_notice().map(str::to_string);
                        let previous_error = self.monitor.last_error().map(str::to_string);
                        let changed = self.monitor.apply_event(&params.kind, &params.payload);
                        let notice = self.monitor.last_notice().map(str::to_string);
                        let error = self.monitor.last_error().map(str::to_string);
                        let notice_changed = notice != previous_notice;
                        let error_changed = error != previous_error;
                        if notice_changed {
                            self.notice = notice;
                        }
                        if error_changed {
                            self.error = error;
                        }
                        if notice_changed || error_changed {
                            // Copy both diagnostics before clearing, otherwise a
                            // projected notice would erase a simultaneous error.
                            self.monitor.clear_diagnostics();
                        }
                        if changed {
                            self.select_pending_monitor();
                        }
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
            HostEvent::TerminalNotification { method, params } => {
                let commands = self
                    .terminal
                    .as_mut()
                    .map(|terminal| terminal.on_notification(&method, &params))
                    .unwrap_or_default();
                self.dispatch_terminal(commands);
                true
            }
            HostEvent::StreamEnded(params) => {
                // A closed child subscription is rebuilt from the child's own
                // last applied seq; the root rebuild path is unrelated.
                if self.subagent_subscription.as_deref() == Some(params.subscription_id.as_str()) {
                    self.subagent_subscription = None;
                    if let Some(mounted) = self.subagents.mounted().cloned() {
                        let key = mounted.address.key();
                        let from = self
                            .child_models
                            .get(&key)
                            .and_then(|model| model.last_applied_seq())
                            .map(|seq| seq.saturating_add(1))
                            .unwrap_or(0);
                        self.subscribe_child(&mounted.address, from);
                    }
                    return true;
                }
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

    /// Apply one child-addressed durable event or stream frame. The child
    /// model keeps the official address and never leaks into the root model.
    fn handle_child_event(&mut self, kind: &str, payload: &Value, address: SessionAddress) -> bool {
        let key = address.key();
        let model = self
            .child_models
            .entry(key.clone())
            .or_insert_with(|| ConversationModel::new(address));
        let changed = model.apply_event(kind, payload);
        if self.active_child_key.as_deref() == Some(key.as_str()) && changed {
            self.transcript_dirty = true;
        }
        // A child turn boundary changes the official activity and timing
        // projections. Refresh the direct catalog so the dock lamp and the
        // header do not stay on a stale running state.
        if payload.get("type").and_then(Value::as_str) == Some("turn/end") {
            self.subagents.snapshot_due = true;
            self.refresh_subagents();
        }
        changed
    }

    /// Apply one official subagent lifecycle event and refresh the catalog.
    fn handle_subagents_event(&mut self, payload: &Value) -> bool {
        match self.subagents.apply_event(payload) {
            Ok(changed) => {
                if self.subagents.snapshot_due {
                    self.refresh_subagents();
                }
                if self.subagents.nav().list_open {
                    self.refresh_subagents_list();
                }
                if self.subagent_blink_at.is_none() {
                    self.subagent_blink_at = Some(Instant::now());
                }
                // If the mounted child's direct parent settled, its liveness
                // may have changed; re-read the official description instead
                // of keeping a stale enabled composer.
                if matches!(
                    payload.get("type").and_then(Value::as_str),
                    Some("start") | Some("end")
                ) {
                    let description = self.subagents.mounted().and_then(|mounted| {
                        let child = mounted.address.child.as_ref()?;
                        Some((
                            child.parent_session_id.clone(),
                            child.child_session_id.clone(),
                        ))
                    });
                    if let Some((parent, child)) = description {
                        if payload.get("childId").and_then(Value::as_str) == Some(parent.as_str()) {
                            self.request_subagent_describe(&child);
                        }
                    }
                }
                changed
            }
            Err(message) => {
                self.error = Some(format!("子代理事件无效：{message}"));
                true
            }
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
                    | Pending::ScheduleCatalog
                    | Pending::ScheduleCreate
                    | Pending::ScheduleUpdate
                    | Pending::ScheduleDelete
                    | Pending::ScheduleHistory { .. }
                    | Pending::SubagentsSnapshot
                    | Pending::SubagentsList
                    | Pending::SubagentsDescribe { .. }
                    | Pending::SubagentPrompt { .. }
                    | Pending::SubagentInterrupt { .. }
                    | Pending::SubagentSubscribe { .. }
                    | Pending::SubagentUnsubscribe
                    | Pending::MonitorList
                    | Pending::MonitorCreate { .. }
                    | Pending::MonitorStop { .. }
                    | Pending::MonitorReset { .. }
            )
        {
            self.resubscribing = false;
            self.clear_stop_remaining = 0;
            self.schedule.fail("宿主连接已关闭；结果未知");
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
                        self.refresh_context();
                        // Jobs and monitor events are live-only, so re-read the
                        // official baselines after a gap instead of trusting
                        // the old lists.
                        self.refresh_jobs_snapshot();
                        self.refresh_monitor_snapshot();
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
            Pending::DecisionsSnapshot => match result {
                Ok(value) => {
                    self.decisions.apply_snapshot(&value);
                    let address = self.model.address().wire_value();
                    if let Some(projection) = value.get("projection") {
                        let payload = json!({
                            "address": address.clone(),
                            "active": projection.get("active").cloned().unwrap_or_else(|| json!([])),
                            "settled": projection.get("settled").cloned().unwrap_or_else(|| json!([])),
                        });
                        self.view.apply_event("decisions.projection", &payload);
                    }
                    if let Some(approvals) = value.get("approvals").and_then(Value::as_array) {
                        for approval in approvals {
                            let mut payload = approval.clone();
                            if let Some(object) = payload.as_object_mut() {
                                object.insert("address".into(), address.clone());
                                object.insert("state".into(), json!("pending"));
                            }
                            self.view.apply_event("decisions.approval", &payload);
                        }
                    }
                    if let Some(questions) = value.get("questions").and_then(Value::as_array) {
                        for question in questions {
                            if question.get("callId").is_none() {
                                continue;
                            }
                            let mut payload = question.clone();
                            if let Some(object) = payload.as_object_mut() {
                                object.insert("address".into(), address.clone());
                                object.insert("state".into(), json!("pending"));
                            }
                            self.view.apply_event("decisions.question", &payload);
                        }
                    }
                    self.transcript_dirty = true;
                    self.sync_decision_focus();
                }
                Err(error) => {
                    self.notice = Some(format!("读取决策状态失败：{}", error.message));
                }
            },
            Pending::Decision { action, label } => match result {
                Ok(value) => {
                    let accepted = value
                        .get("accepted")
                        .and_then(Value::as_bool)
                        .unwrap_or(true);
                    let mode = value.get("mode").and_then(Value::as_str);
                    let reason = value.get("reason").and_then(Value::as_str);
                    let not_accepted = !accepted
                        || matches!(
                            reason,
                            Some("not-continued")
                                | Some("already-continued")
                                | Some("no-live-wait")
                        );
                    if not_accepted {
                        let detail = reason.unwrap_or("请求已失效");
                        self.decisions
                            .answer_not_accepted(&action, format!("{label}未被接受：{detail}"));
                        self.error = Some(format!("{label}未被接受：{detail}"));
                        if reason == Some("no-live-wait") {
                            self.refresh_decisions();
                        }
                    } else if mode == Some("continued")
                        && matches!(action, DecisionAction::QuestionExpire { .. })
                    {
                        self.decisions.mark_question_continued(&action);
                    }
                }
                Err(error) => {
                    self.decisions
                        .action_failed(&action, format!("{label}失败：{}", error.message));
                    self.error = Some(format!("{label}失败：{}", error.message));
                    if error.message.contains("already settled")
                        || error.message.contains("unknown")
                    {
                        self.refresh_decisions();
                    }
                }
            },
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
            Pending::SessionsList => {
                self.sessions_panel.loading = false;
                match result {
                    Ok(value) => match SessionSummary::parse_result(&value) {
                        Ok(rows) => {
                            self.sessions_panel.rows = rows;
                            self.sessions_panel.error = None;
                            self.sessions_panel.clamp();
                        }
                        Err(error) => {
                            self.sessions_panel.error = Some(format!("会话列表无效：{error}"));
                        }
                    },
                    Err(error) => {
                        self.sessions_panel.error =
                            Some(format!("读取会话列表失败：{}", error.message));
                    }
                }
            }
            Pending::Rename { session_id, title } => match result {
                Ok(value) => match RenameOutcome::parse(&value) {
                    Ok(outcome) => {
                        if let Some(row) = self
                            .sessions_panel
                            .rows
                            .iter_mut()
                            .find(|row| row.session_id == session_id)
                        {
                            row.title = Some(outcome.title.clone());
                        }
                        self.sessions_panel.rename = None;
                        self.sessions_panel.error = None;
                        self.notice = Some(format!("会话已重命名为「{}」", outcome.title));
                    }
                    Err(error) => {
                        self.sessions_panel.error = Some(format!("改名结果无效：{error}"));
                    }
                },
                Err(error)
                    if matches!(error.kind, RequestErrorKind::Rpc | RequestErrorKind::Local) =>
                {
                    let message = format!("改名失败：{}", error.message);
                    if self.sessions_panel.open {
                        self.sessions_panel.error = Some(message);
                        // Restore the editor only while its panel is still open;
                        // a late response must not revive a closed panel.
                        self.sessions_panel.rename = Some(RenameInput {
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
                    self.sessions_panel.rename = None;
                    if self.sessions_panel.open {
                        self.sessions_panel.error = Some(message);
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
            Pending::Terminal { kind, epoch } => {
                let result = result.map_err(|error| Failure {
                    definite: !matches!(error.kind, RequestErrorKind::Unknown),
                    message: error.message,
                });
                let commands = self
                    .terminal
                    .as_mut()
                    .map(|terminal| terminal.on_response_epoch(epoch, kind, result))
                    .unwrap_or_default();
                self.dispatch_terminal(commands);
            }
            Pending::SubagentsSnapshot => {
                self.subagents_snapshot_pending = false;
                match result {
                    Ok(value) => {
                        if let Err(message) = self.subagents.apply_snapshot(&value) {
                            self.error = Some(format!("子代理目录无效：{message}"));
                        }
                    }
                    Err(error) => {
                        self.error = Some(format!("子代理目录读取失败：{}", error.message));
                    }
                }
                if self.subagents_snapshot_queued {
                    self.subagents_snapshot_queued = false;
                    self.refresh_subagents();
                }
            }
            Pending::SubagentsList => {
                self.subagent_list_loading = false;
                match result {
                    Ok(value) => {
                        if let Err(message) = self.subagents.apply_list(&value) {
                            self.error = Some(format!("子代理列表无效：{message}"));
                        }
                    }
                    Err(error) => {
                        self.error = Some(format!("子代理列表读取失败：{}", error.message));
                    }
                }
            }
            Pending::SubagentsDescribe { child_id } => match result {
                Ok(value) => {
                    self.apply_subagent_description(&value);
                }
                Err(error) => {
                    self.subagents.describing = None;
                    self.error = Some(format!("子代理历史读取失败：{}", error.message));
                    if self
                        .subagents
                        .mounted()
                        .and_then(|mounted| mounted.address.child.as_ref())
                        .is_some_and(|child| child.child_session_id == child_id)
                    {
                        self.notice = Some("无法确认子代理模式；输入保持禁用".to_string());
                    }
                }
            },
            Pending::SubagentPrompt { request_id } => match result {
                Ok(value) => {
                    let accepted = value
                        .get("accepted")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    let message_id = value
                        .get("messageId")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let address = self
                        .subagent_backups
                        .get(&request_id)
                        .map(|(address, _)| address.clone());
                    if accepted {
                        self.subagent_backups.remove(&request_id);
                        if let Some(address) = &address {
                            if let Some(model) = self.child_models.get_mut(&address.key()) {
                                model.submit_accepted(&request_id, &message_id, false);
                            }
                        }
                        self.notice =
                            Some("子代理消息已被官方收件箱接受（回执不等于已执行）".to_string());
                    } else {
                        if let Some(address) = &address {
                            if let Some(model) = self.child_models.get_mut(&address.key()) {
                                model.submit_failed(&request_id, "官方未接受");
                            }
                        }
                        self.restore_subagent_draft(&request_id);
                        self.error = Some("官方未接受子代理消息；草稿已恢复".to_string());
                    }
                }
                Err(error)
                    if matches!(error.kind, RequestErrorKind::Rpc | RequestErrorKind::Local) =>
                {
                    let address = self
                        .subagent_backups
                        .get(&request_id)
                        .map(|(address, _)| address.clone());
                    self.restore_subagent_draft(&request_id);
                    if let Some(address) = address {
                        if let Some(model) = self.child_models.get_mut(&address.key()) {
                            model.submit_failed(&request_id, error.message.clone());
                        }
                    }
                    self.error = Some(format!("子代理发送失败：{}", error.message));
                }
                Err(error) => {
                    // The prompt may have reached the child. Keep the draft
                    // cleared and never retry with a new requestId.
                    let address = self
                        .subagent_backups
                        .remove(&request_id)
                        .map(|(address, _)| address);
                    if let Some(address) = address {
                        if let Some(model) = self.child_models.get_mut(&address.key()) {
                            model.submit_unknown(&request_id, error.message);
                        }
                    }
                    self.notice =
                        Some("子代理发送结果未知；草稿未回滚，也不会自动重发".to_string());
                }
            },
            Pending::SubagentInterrupt { child_id } => match result {
                Ok(_) => {
                    self.notice = Some(format!(
                        "已请求停止子代理 {child_id} 的当前回合；排队消息与后代保留"
                    ));
                }
                Err(error) => self.error = Some(format!("子代理停止失败：{}", error.message)),
            },
            Pending::SubagentSubscribe {
                address_key,
                generation,
            } => match result {
                Ok(value) => {
                    let subscription_id = value
                        .get("subscriptionId")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    let current = generation == self.subagent_subscribe_generation
                        && self.active_child_key.as_deref() == Some(address_key.as_str());
                    if !current {
                        // The view moved or re-subscribed while this request was
                        // in flight. Release the stale subscription instead of
                        // leaking a second live stream.
                        if let Some(subscription_id) = subscription_id {
                            if let Some(link) = &self.link {
                                if let Ok(id) = link.request(
                                    "nano.unsubscribe",
                                    json!({ "subscriptionId": subscription_id }),
                                ) {
                                    self.pending.insert(id, Pending::SubagentUnsubscribe);
                                }
                            }
                        }
                        return;
                    }
                    if let Some(model) = self.child_models.get_mut(&address_key) {
                        let seq = value.get("seq").and_then(Value::as_u64).unwrap_or(0);
                        model.apply_replay_baseline(seq, false);
                    }
                    self.subagent_subscription = subscription_id;
                    self.transcript_dirty = true;
                }
                Err(error) => {
                    self.error = Some(format!("子代理历史订阅失败：{}", error.message));
                }
            },
            Pending::SubagentUnsubscribe => {}
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
                        if self
                            .controls
                            .catalog()
                            .is_some_and(|catalog| catalog.is_empty())
                        {
                            self.notice = Some("官方模型目录为空；保留上次选择与原因".to_string());
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
                        self.error =
                            Some("模型切换结果未知；正在重读官方投影，不自动重发".to_string());
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
                            "权限写入结果未知；正在重读官方投影（可能已变、可能未变）".to_string(),
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
                        self.error =
                            Some(format!("命令 {name} 结果未知（可能已执行）；不自动重发"));
                    }
                    _ => {
                        self.error = Some(format!("命令 {name} 失败：{}", error.message));
                    }
                },
            },
            Pending::ScheduleCatalog => match result {
                Ok(value) => {
                    if let Err(message) = self.schedule.apply_catalog(&value) {
                        self.schedule
                            .fail_catalog(format!("任务列表无效：{message}"));
                    }
                }
                Err(error) => self
                    .schedule
                    .fail_catalog(format!("读取任务列表失败：{}", error.message)),
            },
            Pending::ScheduleCreate => match result {
                Ok(value) => {
                    if let Err(message) = self.schedule.apply_create(&value) {
                        self.schedule.fail(format!("创建回执无效：{message}"));
                    }
                }
                Err(error) => self.report_schedule_failure("创建", error),
            },
            Pending::ScheduleUpdate => match result {
                Ok(value) => {
                    let inner = value.get("result").unwrap_or(&Value::Null);
                    if let Err(message) = self.schedule.apply_update(inner) {
                        self.schedule.fail(format!("修改回执无效：{message}"));
                    }
                }
                Err(error) => self.report_schedule_failure("修改", error),
            },
            Pending::ScheduleDelete => match result {
                Ok(value) => {
                    let inner = value.get("result").unwrap_or(&Value::Null);
                    if let Err(message) = self.schedule.apply_delete(inner) {
                        self.schedule.fail(format!("删除回执无效：{message}"));
                    }
                }
                Err(error) => self.report_schedule_failure("删除", error),
            },
            Pending::ScheduleHistory { before } => match result {
                Ok(value) => {
                    let inner = value.get("result").unwrap_or(&Value::Null);
                    if let Err(message) = self.schedule.apply_history(inner, before.is_some()) {
                        self.schedule.fail(format!("投递历史无效：{message}"));
                    }
                }
                Err(error) => {
                    if let Some(history) = self.schedule.history.as_mut() {
                        history.loading_older = false;
                        history.error = Some(error.message.clone());
                    }
                    self.schedule
                        .fail(format!("读取投递历史失败：{}", error.message));
                }
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
            }
            Pending::MonitorList => {
                self.monitor_snapshot_pending = false;
                if !self.ready {
                    return;
                }
                match result {
                    Ok(value) => {
                        if let Err(message) = self.monitor.apply_list(&value) {
                            self.error = Some(format!("监听列表无效：{message}"));
                        } else {
                            self.select_pending_monitor();
                        }
                    }
                    Err(error) => {
                        if matches!(error.kind, RequestErrorKind::Rpc | RequestErrorKind::Local) {
                            self.error = Some(format!("读取监听列表失败：{}", error.message));
                        }
                    }
                }
            }
            Pending::MonitorCreate {
                job_id,
                unread_bytes,
            } => {
                if !self.ready {
                    return;
                }
                match result {
                    Ok(value) => {
                        let existing = value
                            .get("existing")
                            .and_then(Value::as_bool)
                            .unwrap_or(false);
                        let skipped = value
                            .get("monitor")
                            .and_then(|monitor| monitor.get("skippedBytes"))
                            .and_then(Value::as_u64)
                            .unwrap_or(unread_bytes);
                        self.monitor_form = None;
                        if !self.monitor.select_job(&job_id) {
                            // The control response may overtake the live event;
                            // select once the projection arrives.
                            self.monitor_select_after_refresh = Some(job_id.clone());
                        } else {
                            self.monitor_select_after_refresh = None;
                        }
                        self.monitor_open = true;
                        self.notice = Some(if existing {
                            "已有活动监听；未重复创建".to_string()
                        } else if skipped > unread_bytes {
                            format!(
                                "监听已创建；等待期间又新增 {} 字节未读，已按最新游标跳过",
                                skipped - unread_bytes
                            )
                        } else {
                            "监听已创建".to_string()
                        });
                        self.refresh_monitor_snapshot();
                    }
                    Err(error) => {
                        let message = match error.code {
                            Some(1028) => "任务不存在或不可见（job/not-found）".to_string(),
                            Some(1057) => {
                                "当前宿主不支持 Monitor（monitor/unavailable）".to_string()
                            }
                            Some(1059) => "过滤器无效（monitor/filter-invalid）".to_string(),
                            Some(1060) => "预算参数无效（monitor/budget-invalid）".to_string(),
                            Some(1061) => "监听数量已达上限（monitor/limit-reached）".to_string(),
                            _ => format!("创建监听失败：{}", error.message),
                        };
                        self.error = Some(message);
                    }
                }
            }
            Pending::MonitorStop { job_id } => {
                if !self.ready {
                    return;
                }
                match result {
                    Ok(_value) => {
                        self.notice = Some("监听已停止；被观察任务未受影响".to_string());
                        self.refresh_monitor_snapshot();
                    }
                    Err(error) => {
                        let message = match error.code {
                            Some(1057) => {
                                "当前宿主不支持 Monitor（monitor/unavailable）".to_string()
                            }
                            Some(1058) => "监听记录不存在（monitor/not-found）".to_string(),
                            _ => format!("停止监听失败（{}）：{}", job_id, error.message),
                        };
                        self.error = Some(message);
                    }
                }
            }
            Pending::MonitorReset { job_id } => {
                if !self.ready {
                    return;
                }
                match result {
                    Ok(_value) => {
                        self.notice = Some("监听预算已重置".to_string());
                        self.refresh_monitor_snapshot();
                    }
                    Err(error) => {
                        let message = match error.code {
                            Some(1057) => {
                                "当前宿主不支持 Monitor（monitor/unavailable）".to_string()
                            }
                            Some(1058) => "监听记录不存在（monitor/not-found）".to_string(),
                            Some(1060) => "预算参数无效（monitor/budget-invalid）".to_string(),
                            _ => format!("重置监听预算失败（{}）：{}", job_id, error.message),
                        };
                        self.error = Some(message);
                    }
                }
            }
        }
        if self.schedule.take_refresh() {
            self.refresh_schedule_catalog();
        }
    }

    /// Toggle the schedule overlay. Opening always reads the official catalog
    /// unless the hello capability says the scheduler is unusable.
    pub fn toggle_schedule(&mut self) {
        if self.schedule.open {
            self.schedule.close_view();
            return;
        }
        let unsupported = self.schedule_unsupported.clone();
        self.schedule.open_view(unsupported);
        if self.schedule.take_refresh() {
            self.refresh_schedule_catalog();
        }
    }

    /// Read the complete catalog (active and ended). This is the read path the
    /// list uses; the official `list` path is reserved for active-only callers.
    pub fn refresh_schedule_catalog(&mut self) {
        if !self.schedule.open {
            return;
        }
        if !self.ready {
            self.schedule.fail("宿主未就绪；连接后按 r 重试");
            return;
        }
        let params = serde_json::json!({ "address": self.schedule_address() });
        self.send_schedule_request("nano.schedule.catalog", params, Pending::ScheduleCatalog);
    }

    fn schedule_address(&self) -> Value {
        serde_json::json!({ "rootSessionId": self.model.address().root_session_id })
    }

    fn send_schedule_request(&mut self, method: &str, params: Value, pending: Pending) {
        let Some(link) = &self.link else {
            self.schedule.fail("宿主连接已关闭；结果未知");
            return;
        };
        match link.request(method, params) {
            Ok(id) => {
                self.pending.insert(id, pending);
            }
            Err(message) => {
                self.schedule.fail(format!("请求未能发出：{message}"));
            }
        }
    }

    fn report_schedule_failure(&mut self, action: &str, error: RequestError) {
        // A timeout or lost response may still have committed the official
        // write. Never claim failure for that case; refresh and let the catalog
        // decide.
        if matches!(error.kind, RequestErrorKind::Unknown) {
            self.schedule.fail(format!(
                "{action}结果未知（可能已提交）：{}；按 r 刷新确认，不要盲目重试",
                error.message
            ));
        } else {
            self.schedule
                .fail(format!("{action}失败：{}", error.message));
        }
    }

    fn apply_schedule_action(&mut self, action: ScheduleAction) {
        match action {
            ScheduleAction::None | ScheduleAction::Redraw | ScheduleAction::Close => {}
            ScheduleAction::Refresh => self.refresh_schedule_catalog(),
            ScheduleAction::Create(request) => {
                if !self.ready {
                    self.schedule.fail("宿主未就绪");
                    return;
                }
                let params = serde_json::json!({
                    "address": self.schedule_address(),
                    "request": request,
                });
                self.send_schedule_request("nano.schedule.create", params, Pending::ScheduleCreate);
            }
            ScheduleAction::Update {
                id,
                expected,
                fields,
            } => {
                if !self.ready {
                    self.schedule.fail("宿主未就绪");
                    return;
                }
                let mut params = serde_json::json!({
                    "address": self.schedule_address(),
                    "id": id,
                    "expected": expected,
                });
                if let (Some(target), Some(update)) = (params.as_object_mut(), fields.as_object()) {
                    for (key, value) in update {
                        target.insert(key.clone(), value.clone());
                    }
                }
                self.send_schedule_request("nano.schedule.update", params, Pending::ScheduleUpdate);
            }
            ScheduleAction::Delete { id } => {
                if !self.ready {
                    self.schedule.fail("宿主未就绪");
                    return;
                }
                let params = serde_json::json!({
                    "address": self.schedule_address(),
                    "id": id,
                });
                self.send_schedule_request("nano.schedule.delete", params, Pending::ScheduleDelete);
            }
            ScheduleAction::History { id, before } => {
                if !self.ready {
                    self.schedule.fail("宿主未就绪");
                    return;
                }
                let mut params = serde_json::json!({
                    "address": self.schedule_address(),
                    "id": id,
                    "limit": 50,
                });
                if let (Some(target), Some(cursor)) = (params.as_object_mut(), before.as_ref()) {
                    target.insert("before".into(), Value::String(cursor.clone()));
                }
                self.send_schedule_request(
                    "nano.schedule.history",
                    params,
                    Pending::ScheduleHistory { before },
                );
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
        if let Some(terminal) = self.terminal.as_mut() {
            let commands = terminal.disconnect("宿主已断开；终端进程状态未知");
            self.dispatch_terminal(commands);
        }
        self.decisions.clear();
        self.scene.focus_mut().reset(FocusId::input());
        self.transcript_dirty = true;
        self.jobs.clear();
        self.jobs_snapshot_pending = false;
        self.monitor.clear();
        self.monitor_snapshot_pending = false;
        self.monitor_open = false;
        self.monitor_form = None;
        self.monitor_select_after_refresh = None;
        self.following.clear();
        self.follow_dirty.clear();
        self.jobs_scroll_back = 0;
        self.jobs_detail_lines = 0;
        for record in &mut self.bash_records {
            if matches!(record.state, nano_jobs::BashState::Running) {
                record.unknown("宿主已断开，命令可能已执行");
            }
        }
        // Without a live host the exact parent is gone. Keep the child view
        // readable, but never leave a composer that would fail on send.
        if let Some(mounted) = self.subagents.nav_mut().mounted_mut() {
            mounted.parent_live = false;
            mounted.activity = nano_subagents::Activity::Inactive;
        }
    }

    /// Address value for S06 request params. The mounted child owns its own
    /// image drafts; the root address is used everywhere else.
    fn address_value(&self) -> Value {
        if let Some(mounted) = self.subagents.mounted() {
            return mounted.address.wire_value();
        }
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
        if self.subagent_active() {
            // Child prompts do not expose the root-only file-reference path.
            self.cancel_completion();
            return;
        }
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
        if self.subagent_active()
            && !self
                .subagents
                .mounted_gate()
                .unwrap_or(ComposerGate::Resolving)
                .can_send()
        {
            self.notice = Some("当前子代理没有可输入内容的附件入口".to_string());
            return;
        }
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
        if self.subagent_active() {
            return Err("子代理只接受图片附件；通用文件已被拒绝".to_string());
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
            self.monitor_open = false;
            self.monitor_form = None;
            self.jobs.close_detail();
            self.jobs_scroll_back = 0;
            self.notice = Some("任务面板已关闭".to_string());
        } else {
            self.jobs_open = true;
            self.monitor_open = false;
            self.monitor_form = None;
            self.refresh_jobs_snapshot();
            self.refresh_monitor_snapshot();
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
        if self.monitor_form.is_some() {
            return self.handle_monitor_form_key(key);
        }
        if self.monitor_open {
            return self.handle_monitor_view_key(key);
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
                KeyCode::Char('m') | KeyCode::Char('M') => {
                    self.begin_monitor_for_detail();
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
                self.monitor_open = false;
                self.monitor_form = None;
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
            KeyCode::Char('m') | KeyCode::Char('M') => {
                self.begin_monitor_for_selected();
                true
            }
            KeyCode::Char('r') | KeyCode::Char('R') => {
                self.refresh_jobs_snapshot();
                self.refresh_monitor_snapshot();
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

    /// Whether the connected host reports the S10 Monitor surface.
    fn monitor_supported(&self) -> bool {
        self.hello
            .as_ref()
            .is_some_and(crate::link::monitor_supported)
    }

    /// Read the monitor list. The result is an absolute projection; the model
    /// rejects it when a newer live event already landed.
    fn refresh_monitor_snapshot(&mut self) {
        if !self.ready || self.monitor_snapshot_pending || !self.monitor_supported() {
            return;
        }
        let params = json!({ "address": self.model.address().wire_value() });
        let Some(link) = &self.link else {
            return;
        };
        match link.request("nano.monitor.list", params) {
            Ok(id) => {
                self.monitor_snapshot_pending = true;
                self.pending.insert(id, Pending::MonitorList);
            }
            Err(message) => {
                self.error = Some(format!("读取监听列表失败：{message}"));
            }
        }
    }

    /// Start a monitor flow for the selected task row.
    fn begin_monitor_for_selected(&mut self) {
        let Some(job) = self.jobs.selected_job() else {
            return;
        };
        let job_id = job.id.clone();
        let label = job.label.clone();
        let unread = job.output.total.saturating_sub(job.output.earliest);
        self.begin_monitor_for_job(&job_id, &label, unread);
    }

    /// Start a monitor flow for the open task detail.
    fn begin_monitor_for_detail(&mut self) {
        let Some(job) = self.jobs.detail_job() else {
            return;
        };
        let job_id = job.id.clone();
        let label = job.label.clone();
        let unread = job.output.total.saturating_sub(job.output.earliest);
        self.begin_monitor_for_job(&job_id, &label, unread);
    }

    /// Open the existing monitor, or the attach form when none is active.
    fn begin_monitor_for_job(&mut self, job_id: &str, label: &str, unread: u64) {
        if !self.ready {
            self.error = Some("宿主未就绪".to_string());
            return;
        }
        if !self.jobs_supported() || !self.monitor_supported() {
            self.error = Some("当前宿主不支持 Monitor".to_string());
            return;
        }
        if let Some(existing) = self.monitor.monitor_for_job(job_id) {
            if !existing.stopped_by_user {
                self.monitor.select_job(job_id);
                self.monitor_open = true;
                self.monitor_form = None;
                self.jobs_scroll_back = 0;
                self.jobs.close_detail();
                return;
            }
            // A user-stopped monitor is an inactive record. Starting a new
            // one is an explicit fresh attach, not a silent resume.
        }
        self.open_monitor_form(job_id, label, unread);
    }

    /// Open the attach form. The unread byte count is always visible first.
    fn open_monitor_form(&mut self, job_id: &str, label: &str, unread: u64) {
        self.monitor_open = false;
        self.jobs_scroll_back = 0;
        self.monitor_form = Some(MonitorForm {
            job_id: job_id.to_string(),
            job_label: label.to_string(),
            unread_bytes: unread,
            from_earliest: false,
            quiet: false,
            filter: String::new(),
            filter_regex: false,
            editing_filter: false,
        });
    }

    /// Keys while the monitor attach form owns the task panel.
    fn handle_monitor_form_key(&mut self, key: KeyEvent) -> bool {
        let Some(form) = self.monitor_form.as_mut() else {
            return false;
        };
        if form.editing_filter {
            return match key.code {
                KeyCode::Esc | KeyCode::Enter => {
                    form.editing_filter = false;
                    true
                }
                KeyCode::Backspace => {
                    form.filter.pop();
                    true
                }
                KeyCode::Char(character) => {
                    if !character.is_control() && form.filter.len() + character.len_utf8() <= 512 {
                        form.filter.push(character);
                    }
                    true
                }
                _ => false,
            };
        }
        match key.code {
            KeyCode::Esc => {
                self.monitor_form = None;
                true
            }
            KeyCode::Enter => {
                self.submit_monitor_form();
                true
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                form.from_earliest = false;
                true
            }
            KeyCode::Char('a') | KeyCode::Char('A') => {
                form.from_earliest = true;
                true
            }
            KeyCode::Char('t') | KeyCode::Char('T') => {
                form.quiet = !form.quiet;
                true
            }
            KeyCode::Char('f') | KeyCode::Char('F') => {
                form.editing_filter = true;
                true
            }
            KeyCode::Char('r') | KeyCode::Char('R') => {
                form.filter_regex = !form.filter_regex;
                true
            }
            _ => false,
        }
    }

    /// Select a created monitor once its projection has arrived.
    fn select_pending_monitor(&mut self) {
        let Some(job_id) = self.monitor_select_after_refresh.clone() else {
            return;
        };
        if self.monitor.select_job(&job_id) {
            self.monitor_select_after_refresh = None;
        }
    }

    /// Send the attach request with the exact visible choices. The form stays
    /// open until the host answers, so a rejection does not lose the choices.
    fn submit_monitor_form(&mut self) {
        let Some(form) = self.monitor_form.clone() else {
            return;
        };
        if self
            .pending
            .values()
            .any(|pending| matches!(pending, Pending::MonitorCreate { .. }))
        {
            return;
        }
        if !self.ready {
            self.error = Some("宿主未就绪".to_string());
            return;
        }
        let mut params = json!({
            "address": self.model.address().wire_value(),
            "jobId": form.job_id.clone(),
            "from": if form.from_earliest { "earliest" } else { "new" },
            "mode": if form.quiet { "quiet" } else { "notify" },
            "filter": if form.filter.is_empty() {
                Value::Null
            } else {
                json!({ "pattern": form.filter.clone(), "regex": form.filter_regex })
            },
        });
        if form.filter.is_empty() {
            if let Some(object) = params.as_object_mut() {
                object.remove("filter");
            }
        }
        let Some(link) = &self.link else {
            return;
        };
        match link.request("nano.monitor.create", params) {
            Ok(id) => {
                self.pending.insert(
                    id,
                    Pending::MonitorCreate {
                        job_id: form.job_id.clone(),
                        unread_bytes: form.unread_bytes,
                    },
                );
                self.notice = Some("正在创建监听…".to_string());
            }
            Err(message) => {
                self.error = Some(format!("创建监听失败：{message}"));
            }
        }
    }

    /// Keys while the monitor detail owns the task panel.
    fn handle_monitor_view_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc => {
                self.monitor_open = false;
                true
            }
            KeyCode::Up => {
                self.monitor.select_previous();
                true
            }
            KeyCode::Down => {
                self.monitor.select_next();
                true
            }
            KeyCode::Char('s') | KeyCode::Char('S') => {
                self.stop_monitor();
                true
            }
            KeyCode::Char('b') | KeyCode::Char('B') => {
                self.reset_monitor_budget();
                true
            }
            KeyCode::Char('r') | KeyCode::Char('R') => {
                self.refresh_monitor_snapshot();
                true
            }
            KeyCode::PageUp => {
                // The monitor panel draws from the top, so PageUp moves toward
                // the beginning (smaller offset).
                self.jobs_scroll_back = self.jobs_scroll_back.saturating_sub(10);
                true
            }
            KeyCode::PageDown => {
                self.jobs_scroll_back = self.jobs_scroll_back.saturating_add(10);
                true
            }
            _ => false,
        }
    }

    /// Stop monitor delivery only; the observed task is untouched.
    fn stop_monitor(&mut self) {
        let Some(monitor) = self.monitor.selected_monitor() else {
            return;
        };
        let job_id = monitor.job_id.clone();
        let params = json!({
            "address": self.model.address().wire_value(),
            "jobId": job_id.clone(),
        });
        if self.send_monitor_request("nano.monitor.stop", params, Pending::MonitorStop { job_id }) {
            self.notice = Some("正在停止监听…".to_string());
        }
    }

    /// Reset one monitor's budget by explicit user action.
    fn reset_monitor_budget(&mut self) {
        let Some(monitor) = self.monitor.selected_monitor() else {
            return;
        };
        let job_id = monitor.job_id.clone();
        let params = json!({
            "address": self.model.address().wire_value(),
            "jobId": job_id.clone(),
        });
        if self.send_monitor_request(
            "nano.monitor.resetBudget",
            params,
            Pending::MonitorReset { job_id },
        ) {
            self.notice = Some("正在重置监听预算…".to_string());
        }
    }

    /// Shared send path for monitor requests.
    fn send_monitor_request(&mut self, method: &str, params: Value, pending: Pending) -> bool {
        if !self.ready {
            self.error = Some("宿主未就绪".to_string());
            return false;
        }
        let Some(link) = &self.link else {
            return false;
        };
        match link.request(method, params) {
            Ok(id) => {
                self.pending.insert(id, pending);
                true
            }
            Err(message) => {
                self.error = Some(format!("监听请求未能发出：{message}"));
                false
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
        self.history_browse = None;
        let text = self.drafts.current().text().to_string();
        let has_attachments = !self.attachments.current().is_empty();
        if text.trim().is_empty() && !has_attachments {
            self.error = Some("内容为空，草稿已保留".to_string());
            return;
        }
        // Every non-empty root submission is remembered, including slash and
        // `!` commands; a failed send still counts, matching Claude Code.
        if !text.trim().is_empty() {
            self.record_history_entry(&text);
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

    /// Whether a decision card currently owns the input seat.
    pub fn decision_owns_seat(&self) -> bool {
        self.decisions.top().is_some()
            && !self.picker.is_some()
            && self.shell_confirm.is_none()
            && !self.panel.is_open()
            && !self.jobs_open
            && !self.sessions_panel.open
            && !self.skills_panel_open()
            && !self.instructions_panel_open()
            && !self.schedule.open
            && self.schedule.busy.is_none()
    }

    /// Apply one pointer event. Returns whether the screen needs a redraw.
    pub fn handle_mouse(&mut self, mouse: crossterm::event::MouseEvent) -> bool {
        if self.terminal_active() {
            let area = self.terminal_screen_area;
            let commands = self
                .terminal
                .as_mut()
                .map(|terminal| terminal.handle_mouse(mouse, area))
                .unwrap_or_default();
            self.dispatch_terminal(commands);
            return true;
        }
        use crossterm::event::{MouseButton, MouseEventKind};
        match mouse.kind {
            MouseEventKind::ScrollUp => self.scroll_at(mouse.column, mouse.row, -1),
            MouseEventKind::ScrollDown => self.scroll_at(mouse.column, mouse.row, 1),
            MouseEventKind::Down(MouseButton::Left) => {
                self.mouse_down = Some((mouse.column, mouse.row));
                self.mouse_dragged = false;
                false
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                self.mouse_dragged = true;
                false
            }
            MouseEventKind::Up(MouseButton::Left) => {
                let down = self.mouse_down.take();
                let dragged = std::mem::take(&mut self.mouse_dragged);
                if dragged {
                    return false;
                }
                match down {
                    Some((column, row)) if column == mouse.column && row == mouse.row => {
                        self.click_at(column, row)
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }

    fn scroll_at(&mut self, column: u16, row: u16, direction: isize) -> bool {
        if self.decision_area.contains((column, row).into()) {
            self.decisions.scroll(direction);
            return true;
        }
        if self.feed_area.contains((column, row).into()) {
            return self.view.scroll_mut().wheel(direction, Instant::now());
        }
        false
    }

    /// Send button click: exactly Enter's submit path, including the mounted
    /// child address. A root decision card never blocks a child composer.
    pub fn click_send(&mut self) -> bool {
        if self.subagent_active() {
            let gate = self
                .subagents
                .mounted_gate()
                .unwrap_or(ComposerGate::Resolving);
            if !gate.can_send() {
                return false;
            }
            self.submit_subagent_draft(Delivery::Queue);
            return true;
        }
        if self.decision_owns_seat() {
            return false;
        }
        if self.edit_target.is_some() {
            self.apply_queue_edit();
        } else {
            self.submit_draft(Delivery::Queue);
        }
        true
    }

    fn click_at(&mut self, column: u16, row: u16) -> bool {
        let point = (column, row);
        if self.send_area.contains(point.into()) {
            return self.click_send();
        }
        if self.jump_area.contains(point.into()) {
            self.view.scroll_mut().to_bottom();
            self.transcript_dirty = true;
            return true;
        }
        if self.subagent_active() && self.subagent_exit_area.contains(point.into()) {
            return self.leave_subagent();
        }
        if !self.subagent_active() && self.subagent_dock_area.contains(point.into()) {
            if let Some((index, _)) = self
                .subagent_dock_hits
                .iter()
                .find(|(_, hit_row)| *hit_row == row)
            {
                let index = *index;
                let (rows, _) = self.subagents.dock();
                self.subagents.nav_mut().dock_focus = true;
                if index < rows.len() {
                    self.subagents.nav_mut().dock_selected = index;
                    return self.enter_subagent(index);
                }
                self.enter_subagent_overflow();
                return true;
            }
        }
        if self.decision_area.contains(point.into()) {
            let offset = row.saturating_sub(self.decision_area.y) as usize;
            let Some(Some(click)) = self.decision_hits.get(offset).cloned() else {
                return false;
            };
            self.decisions.click(click);
            self.drain_decisions();
            return true;
        }
        if self.feed_area.contains(point.into()) {
            let offset = row.saturating_sub(self.feed_area.y) as usize;
            let Some(Some(hit)) = self.feed_hits.get(offset).cloned() else {
                return false;
            };
            match hit.kind {
                HitKind::Toggle => return self.view.toggle(&hit.identity),
                HitKind::OpenQuestion { call_id } => {
                    if self.subagent_active() {
                        // Child approvals/questions are pinned to `never` by
                        // the official policy; never offer an answer surface.
                        self.notice =
                            Some("子代理审批由官方钉死为拒绝，不能从此处放行".to_string());
                        return true;
                    }
                    return self.open_question(&call_id);
                }
            }
        }
        false
    }

    fn open_question(&mut self, call_id: &str) -> bool {
        let Some(record) = self.view.question(call_id) else {
            return false;
        };
        let continued = record.state == RecordedQuestionState::Continued;
        let answerable = record.answerable();
        let opened = self.decisions.open_question(
            call_id.to_string(),
            record.questions,
            record.answers,
            answerable,
            continued,
        );
        if opened {
            self.sync_decision_focus();
        }
        opened
    }

    fn sync_decision_focus(&mut self) {
        let decision = FocusId::new("decision");
        if self.decisions.top().is_some() {
            self.scene.focus_mut().push(decision);
        } else if self.scene.focus().top() == Some(&decision) {
            self.scene.focus_mut().reset(FocusId::input());
            self.decisions.clear_notice();
        }
    }

    fn drain_decisions(&mut self) {
        for action in self.decisions.take_actions() {
            self.send_decision_action(action);
        }
        if let Some(notice) = self.decisions.notice().map(str::to_string) {
            self.notice = Some(notice);
        }
        if let Some(error) = self.decisions.error().map(str::to_string) {
            self.error = Some(error);
        }
        self.sync_decision_focus();
        self.transcript_dirty = true;
    }

    fn send_decision_action(&mut self, action: DecisionAction) {
        let address = self.address_value();
        let (method, params, label) = match &action {
            DecisionAction::Approval {
                request_id,
                outcome,
            } => (
                "nano.approval.answer",
                json!({
                    "address": address,
                    "requestId": request_id,
                    "outcome": outcome.as_str(),
                }),
                "审批答复".to_string(),
            ),
            DecisionAction::QuestionAnswer {
                request_id,
                call_id,
                answers,
            } => {
                let mut params = serde_json::Map::new();
                params.insert("address".into(), address);
                if let Some(id) = request_id {
                    params.insert("requestId".into(), Value::String(id.clone()));
                }
                if let Some(id) = call_id {
                    params.insert("callId".into(), Value::String(id.clone()));
                }
                params.insert("answers".into(), answers_to_wire(answers));
                (
                    "nano.question.answer",
                    Value::Object(params),
                    "答卷".to_string(),
                )
            }
            DecisionAction::QuestionExpire {
                request_id,
                call_id,
            } => {
                let mut params = serde_json::Map::new();
                params.insert("address".into(), address);
                if let Some(id) = request_id {
                    params.insert("requestId".into(), Value::String(id.clone()));
                }
                if let Some(id) = call_id {
                    params.insert("callId".into(), Value::String(id.clone()));
                }
                params.insert("expire".into(), Value::Bool(true));
                (
                    "nano.question.answer",
                    Value::Object(params),
                    "问卷到期".to_string(),
                )
            }
            DecisionAction::QuestionCancel { request_id } => {
                let mut params = serde_json::Map::new();
                params.insert("address".into(), address);
                if let Some(id) = request_id {
                    params.insert("requestId".into(), Value::String(id.clone()));
                }
                params.insert("cancel".into(), Value::Bool(true));
                (
                    "nano.question.answer",
                    Value::Object(params),
                    "取消问卷".to_string(),
                )
            }
            DecisionAction::QuestionRequestChanges {
                request_id,
                call_id,
            } => {
                let mut params = serde_json::Map::new();
                params.insert("address".into(), address);
                if let Some(id) = request_id {
                    params.insert("requestId".into(), Value::String(id.clone()));
                }
                if let Some(id) = call_id {
                    params.insert("callId".into(), Value::String(id.clone()));
                }
                params.insert("cancel".into(), Value::Bool(true));
                (
                    "nano.question.answer",
                    Value::Object(params),
                    "请求修改".to_string(),
                )
            }
        };
        if !self.ready {
            self.decisions
                .action_failed(&action, "宿主未就绪；卡片已保留");
            self.error = Some("宿主未就绪；卡片已保留".to_string());
            return;
        }
        let Some(link) = &self.link else {
            self.decisions.action_failed(&action, "宿主连接已关闭");
            return;
        };
        match link.request(method, params) {
            Ok(id) => {
                self.pending.insert(id, Pending::Decision { action, label });
            }
            Err(message) => {
                self.decisions.action_failed(&action, message.clone());
                self.error = Some(format!("{label}发送失败：{message}"));
            }
        }
    }

    /// Read pending decision cards after a reconnect.
    pub fn refresh_decisions(&mut self) {
        if !self.ready {
            return;
        }
        let Some(link) = &self.link else {
            return;
        };
        let params = json!({ "address": self.address_value() });
        if let Ok(id) = link.request("nano.decisions.snapshot", params) {
            self.pending.insert(id, Pending::DecisionsSnapshot);
        }
    }

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
        self.history_browse = None;
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

    /// Route one key while a child conversation is mounted. Returns `None`
    /// when the key should keep its ordinary meaning (scrolling, global
    /// shortcuts, or plain typing while input is enabled).
    fn handle_child_key(&mut self, key: KeyEvent) -> Option<bool> {
        let gate = self.subagents.mounted_gate()?;
        let composing = self.drafts.current().mode() == InputMode::Composing;
        let blank = self.drafts.current().is_blank();
        if key.code == KeyCode::Esc && key.modifiers.is_empty() {
            // The input method owns Esc while composing; exiting is a separate
            // physical action.
            if composing {
                return Some(false);
            }
            return Some(self.leave_subagent());
        }
        if key.code == KeyCode::Left
            && key.modifiers.is_empty()
            && blank
            && self.scene.focus().input_focused()
            && !composing
        {
            return Some(self.leave_subagent());
        }
        if key.code == KeyCode::Char('k')
            && key.modifiers.contains(KeyModifiers::ALT)
            && gate.can_stop()
        {
            self.stop_mounted_subagent();
            return Some(true);
        }
        if key.code == KeyCode::Enter {
            if key.kind == KeyEventKind::Repeat {
                return Some(true);
            }
            if composing {
                return Some(false);
            }
            if key.modifiers.contains(KeyModifiers::SHIFT) {
                if gate.can_send() {
                    let old = self.drafts.current().text().to_string();
                    self.drafts.current_mut().handle(&InputEvent::Newline);
                    let new = self.drafts.current().text().to_string();
                    self.shift_attachment_anchors(&old, &new);
                }
                return Some(true);
            }
            if key.modifiers.contains(KeyModifiers::ALT) {
                self.submit_subagent_draft(Delivery::Steer);
                return Some(true);
            }
            if key.modifiers.is_empty() {
                if gate.can_send() {
                    self.submit_subagent_draft(Delivery::Queue);
                } else {
                    self.notice = Some(gate.hint().to_string());
                }
                return Some(true);
            }
            return Some(false);
        }
        // A read-only or stopped child owns no editable draft. Swallow editing
        // keys so a phantom draft cannot be built and later sent by accident.
        if !gate.can_send() && key.modifiers.is_empty() {
            match key.code {
                KeyCode::Char(_)
                | KeyCode::Backspace
                | KeyCode::Delete
                | KeyCode::Left
                | KeyCode::Right
                | KeyCode::Up
                | KeyCode::Down
                | KeyCode::Home
                | KeyCode::End
                | KeyCode::Tab => return Some(true),
                _ => {}
            }
        }
        // The parent queue and its shortcuts do not belong to a child view.
        if key.modifiers.contains(KeyModifiers::ALT) {
            match key.code {
                KeyCode::Char('e' | 'E' | 'd' | 'D' | 's' | 'S' | 'x' | 'X') => return Some(true),
                // A read-only child has no attachment entry either.
                KeyCode::Char('a' | 'A') if !gate.can_send() => return Some(true),
                _ => {}
            }
        }
        if key.code == KeyCode::Tab {
            return Some(true);
        }
        None
    }

    /// Route one key while the all-agents list owns the keyboard.
    fn handle_subagent_list_key(&mut self, key: KeyEvent) -> bool {
        let rows = self.subagents.list_rows().len();
        match key.code {
            KeyCode::Esc => {
                self.subagents.nav_mut().close_list();
                true
            }
            KeyCode::Up => self.subagents.nav_mut().move_list(-1, rows),
            KeyCode::Down => self.subagents.nav_mut().move_list(1, rows),
            KeyCode::PageUp => {
                self.subagents.nav_mut().move_list(-8, rows);
                true
            }
            KeyCode::PageDown => {
                self.subagents.nav_mut().move_list(8, rows);
                true
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('f') => {
                let index = self.subagents.nav().list_selected;
                self.enter_subagent_list(index)
            }
            KeyCode::Char(' ') if key.modifiers.is_empty() => {
                let index = self.subagents.nav().list_selected;
                self.enter_subagent_list(index)
            }
            KeyCode::Char('r') if key.modifiers.is_empty() => {
                self.refresh_subagents_list();
                true
            }
            _ => false,
        }
    }

    /// Canonical project identity for history entries. The host cwd is
    /// canonical; the CLI cwd is the pre-handshake fallback.
    fn history_project(&self) -> String {
        self.hello
            .as_ref()
            .map(|hello| hello.cwd.clone())
            .or_else(|| {
                self.cli
                    .cwd
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned())
            })
            .or_else(|| {
                std::env::current_dir()
                    .ok()
                    .map(|path| path.to_string_lossy().into_owned())
            })
            .unwrap_or_default()
    }

    /// Current root session identity, used to put this session's prompts first.
    fn history_session_id(&self) -> String {
        self.subagents.root_session_id().to_string()
    }

    /// Persist one submitted root prompt. A write failure never blocks the
    /// submission and is not surfaced in the UI.
    fn record_history_entry(&self, text: &str) {
        let Some(path) = self.history_path.as_ref() else {
            return;
        };
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or(0);
        let value = json!({
            "display": text,
            "timestamp": timestamp,
            "project": self.history_project(),
            "sessionId": self.history_session_id(),
        });
        let Ok(line) = serde_json::to_string(&value) else {
            return;
        };
        let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)
        else {
            return;
        };
        let _ = file.write_all(line.as_bytes());
        let _ = file.write_all(b"\n");
    }

    /// Read enough of the file's tail to cover the newest matching prompts.
    /// The newest 100 project entries are always within the last 256 lines;
    /// reading backwards keeps a long history file from being loaded whole.
    fn history_tail(path: &std::path::Path) -> String {
        const CHUNK: u64 = 64 * 1024;
        const LINE_BUDGET: usize = 256;
        let Ok(mut file) = File::open(path) else {
            return String::new();
        };
        let Ok(length) = file.metadata().map(|metadata| metadata.len()) else {
            return String::new();
        };
        let mut tail: Vec<u8> = Vec::new();
        let mut position = length;
        loop {
            let chunk = position.min(CHUNK);
            position -= chunk;
            let mut buffer = vec![0u8; chunk as usize];
            if file.seek(SeekFrom::Start(position)).is_err()
                || file.read_exact(&mut buffer).is_err()
            {
                return String::new();
            }
            buffer.extend_from_slice(&tail);
            tail = buffer;
            let newlines = tail.iter().filter(|byte| **byte == b'\n').count();
            if position == 0 || newlines >= LINE_BUDGET {
                break;
            }
        }
        String::from_utf8_lossy(&tail).into_owned()
    }

    /// Root history: the project's submitted prompts from the file, newest
    /// first, this session before other sessions, capped at 100, then reversed
    /// to the oldest-first order the browser expects.
    fn root_input_history(&self) -> Vec<String> {
        let Some(path) = self.history_path.as_ref() else {
            return Vec::new();
        };
        let text = Self::history_tail(path);
        if text.is_empty() {
            return Vec::new();
        }
        let project = self.history_project();
        let session = self.history_session_id();
        let mut window: Vec<(bool, String)> = Vec::new();
        for line in text.lines().rev() {
            let Ok(value) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if value.get("project").and_then(Value::as_str) != Some(project.as_str()) {
                continue;
            }
            let Some(display) = value.get("display").and_then(Value::as_str) else {
                continue;
            };
            if display.trim().is_empty() {
                continue;
            }
            let current = value.get("sessionId").and_then(Value::as_str) == Some(session.as_str());
            window.push((current, display.to_string()));
            if window.len() >= 100 {
                break;
            }
        }
        let mut ordered: Vec<String> = window
            .iter()
            .filter(|(current, _)| *current)
            .map(|(_, text)| text.clone())
            .collect();
        ordered.extend(
            window
                .iter()
                .filter(|(current, _)| !*current)
                .map(|(_, text)| text.clone()),
        );
        ordered.reverse();
        ordered
    }

    /// Child history: this session's user prompts for the mounted address,
    /// filtered from the bounded durable projection plus live echoes.
    fn child_input_history(&self) -> Vec<String> {
        let model = self.active_model();
        let mut entries: Vec<String> = model
            .history()
            .items()
            .iter()
            .filter_map(|item| match item {
                ConversationItem::Message(message) if message.kind == MessageKind::User => {
                    Some(message.text.clone())
                }
                _ => None,
            })
            .filter(|text| !text.trim().is_empty())
            .collect();
        for echo in model.live().echoes {
            if !echo.text.trim().is_empty() && entries.last() != Some(&echo.text) {
                entries.push(echo.text);
            }
        }
        if entries.len() > 100 {
            entries = entries.split_off(entries.len() - 100);
        }
        entries
    }

    /// Submitted prompts for the active composer, oldest first. Root prompts
    /// come from the project history file; child prompts come from the child's
    /// own bounded projection. Injected notices, bash records, and monitor
    /// lines never count.
    fn input_history(&self) -> Vec<String> {
        if self.subagent_active() {
            self.child_input_history()
        } else {
            self.root_input_history()
        }
    }

    /// Replace the draft with one recalled prompt. History entries carry no
    /// attachments, so anchors are shifted against the new text.
    fn recall_history_text(&mut self, text: String, cursor_start: bool) {
        let old = self.drafts.current().text().to_string();
        self.drafts.current_mut().set_text(text.clone());
        if cursor_start {
            self.drafts.current_mut().set_cursor(0);
        }
        self.shift_attachment_anchors(&old, &text);
        self.refresh_completion();
    }

    /// Up-arrow history. Stops at the oldest entry; a mid-draft cursor keeps
    /// its ordinary line movement.
    fn history_up(&mut self) -> Option<bool> {
        if self.subagents.nav().dock_focus {
            return None;
        }
        if self.subagent_active()
            && !self
                .subagents
                .mounted_gate()
                .unwrap_or(ComposerGate::Resolving)
                .can_send()
        {
            return None;
        }
        let address = self.drafts.current_address().clone();
        if self
            .history_browse
            .as_ref()
            .is_some_and(|browse| browse.address != address)
        {
            self.history_browse = None;
        }
        let (line, _) = self.drafts.current().cursor_position();
        if line != 0 {
            return None;
        }
        if let Some(browse) = self.history_browse.as_ref() {
            if browse.index + 1 < browse.entries.len() {
                let index = browse.index + 1;
                let text = browse.entries[browse.entries.len() - 1 - index].clone();
                if let Some(browse) = self.history_browse.as_mut() {
                    browse.index = index;
                }
                self.recall_history_text(text, true);
            }
            return Some(true);
        }
        let entries = self.input_history();
        let Some(text) = entries.last().cloned() else {
            return None;
        };
        let saved_text = self.drafts.current().text().to_string();
        let saved_cursor = self.drafts.current().cursor();
        self.history_browse = Some(HistoryBrowse {
            address,
            entries,
            index: 0,
            saved_text,
            saved_cursor,
        });
        self.recall_history_text(text, true);
        Some(true)
    }

    /// Down-arrow history. Returns false at the newest entry so the ordinary
    /// cursor movement (and the dock's blank-input focus) keeps its meaning.
    fn history_down(&mut self) -> Option<bool> {
        if self.subagents.nav().dock_focus {
            return None;
        }
        if self.subagent_active()
            && !self
                .subagents
                .mounted_gate()
                .unwrap_or(ComposerGate::Resolving)
                .can_send()
        {
            return None;
        }
        let address = self.drafts.current_address().clone();
        let Some(browse) = self.history_browse.as_ref() else {
            return None;
        };
        if browse.address != address {
            self.history_browse = None;
            return None;
        }
        let (line, _) = self.drafts.current().cursor_position();
        let last_line = self.drafts.current().text().matches('\n').count();
        if line != last_line {
            return None;
        }
        if browse.index > 0 {
            let index = browse.index - 1;
            let text = browse.entries[browse.entries.len() - 1 - index].clone();
            if let Some(browse) = self.history_browse.as_mut() {
                browse.index = index;
            }
            self.recall_history_text(text, false);
            return Some(true);
        }
        let (saved_text, saved_cursor) = (browse.saved_text.clone(), browse.saved_cursor);
        self.history_browse = None;
        let old = self.drafts.current().text().to_string();
        self.drafts.current_mut().set_text(saved_text.clone());
        self.drafts.current_mut().set_cursor(saved_cursor);
        self.shift_attachment_anchors(&old, &saved_text);
        self.refresh_completion();
        Some(true)
    }

    /// Route one unmodified Up/Down to history before the dock or the input
    /// buffer sees it.
    fn handle_history_key(&mut self, key: KeyEvent) -> Option<bool> {
        if !key.modifiers.is_empty() {
            return None;
        }
        match key.code {
            KeyCode::Up => self.history_up(),
            KeyCode::Down => self.history_down(),
            _ => None,
        }
    }

    /// Route one key to the dock while the input is blank. Returns `None` when
    /// the key keeps its ordinary meaning.
    fn handle_dock_key(&mut self, key: KeyEvent) -> Option<bool> {
        if self.subagents.nav().is_mounted()
            || !self.scene.focus().input_focused()
            || !self.drafts.current().is_blank()
        {
            return None;
        }
        let (rows, overflow) = self.subagents.dock();
        if rows.is_empty() {
            return None;
        }
        let dock_focus = self.subagents.nav().dock_focus;
        if !dock_focus {
            match key.code {
                KeyCode::Up | KeyCode::Down if key.modifiers.is_empty() => {
                    self.subagents.nav_mut().dock_focus = true;
                    self.subagents
                        .nav_mut()
                        .clamp_dock(rows.len(), overflow > 0);
                    if key.code == KeyCode::Up {
                        let last = rows.len() + usize::from(overflow > 0) - 1;
                        self.subagents.nav_mut().dock_selected = last;
                    }
                    return Some(true);
                }
                _ => return None,
            }
        }
        match key.code {
            KeyCode::Esc => {
                self.subagents.nav_mut().dock_focus = false;
                Some(true)
            }
            KeyCode::Up => Some(
                self.subagents
                    .nav_mut()
                    .move_dock(-1, rows.len(), overflow > 0),
            ),
            KeyCode::Down => Some(
                self.subagents
                    .nav_mut()
                    .move_dock(1, rows.len(), overflow > 0),
            ),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('f') => {
                let index = self.subagents.nav().dock_selected;
                if index < rows.len() {
                    Some(self.enter_subagent(index))
                } else {
                    self.enter_subagent_overflow();
                    Some(true)
                }
            }
            KeyCode::Char(' ') if key.modifiers.is_empty() => {
                let index = self.subagents.nav().dock_selected;
                if index < rows.len() {
                    Some(self.enter_subagent(index))
                } else {
                    self.enter_subagent_overflow();
                    Some(true)
                }
            }
            KeyCode::Char('k') if key.modifiers.is_empty() => {
                self.stop_selected_subagent();
                Some(true)
            }
            KeyCode::Char('l') if key.modifiers.is_empty() => {
                self.enter_subagent_overflow();
                Some(true)
            }
            KeyCode::Char(_) => {
                // Typing returns to the input and keeps the ordinary meaning.
                self.subagents.nav_mut().dock_focus = false;
                None
            }
            _ => {
                self.subagents.nav_mut().dock_focus = false;
                None
            }
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

    /// Which activity message the row above the composer owns, if any.
    pub fn activity_kind(&self) -> Option<ActivityKind> {
        if !self.subagent_active() {
            match self.decisions.top() {
                Some(DecisionTop::Approval(_)) => return Some(ActivityKind::AwaitingApproval),
                Some(DecisionTop::Question(card)) if card.state().interactive() => {
                    return Some(ActivityKind::AwaitingQuestion);
                }
                _ => {}
            }
        }
        if self.active_model().status() == ConversationStatus::Running {
            Some(ActivityKind::Running)
        } else {
            None
        }
    }

    /// Path shown in the header and at the right of the status bar: relative to
    /// home when possible, otherwise the tail of a foreign path.
    pub fn display_path(&self) -> String {
        let path = self
            .cli
            .cwd
            .clone()
            .or_else(|| std::env::current_dir().ok());
        let Some(path) = path else {
            return ".".to_string();
        };
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            if let Ok(rest) = path.strip_prefix(&home) {
                let rest = rest.to_string_lossy();
                return if rest.is_empty() {
                    "~".to_string()
                } else {
                    format!("~/{}", rest.trim_end_matches('/'))
                };
            }
        }
        path.file_name()
            .map(|name| format!("…/{}", name.to_string_lossy()))
            .unwrap_or_else(|| "…".to_string())
    }

    /// Left status-bar slot: the permission fact. In a child view the official
    /// pin is the only honest permission statement.
    pub fn status_permission(&self) -> String {
        if self.subagent_active() {
            return "权限：子代理审批由官方拒绝".to_string();
        }
        let stale = if self.controls.state_error().is_some() {
            "（可能过期）"
        } else {
            ""
        };
        format!("权限：{}{stale}", self.controls.permission_label())
    }

    /// Middle status-bar slot: connection and turn transients, truncated
    /// before either end slot when space is short.
    pub fn status_transient(&self) -> String {
        // A concrete error is more useful than the connection label it
        // implies; reserving the narrow middle slot for it keeps it visible.
        if let Some(error) = &self.error {
            return error.clone();
        }
        let connection = match &self.connection {
            Connection::Ready => "",
            Connection::Starting => "启动中",
            Connection::Disconnected => "已断开",
            Connection::Failed(_) => "连接失败",
        };
        let hint = self.status_hint();
        match (connection.is_empty(), hint.is_empty()) {
            (true, true) => String::new(),
            (true, false) => hint,
            (false, true) => connection.to_string(),
            (false, false) => format!("{connection} · {hint}"),
        }
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
        if self.subagent_active()
            && !self.schedule.open
            && !self.sessions_panel.open
            && !self.jobs_open
            && !self.panel.is_open()
            && !self.skills_panel_open()
            && !self.instructions_panel_open()
        {
            if let Some(error) = &self.error {
                return format!("子代理错误：{error}");
            }
            if let Some(notice) = &self.notice {
                return notice.clone();
            }
            if self.decisions.top().is_some() {
                return "根会话有待处理审批；Esc 返回后处理".to_string();
            }
            if let Some(gate) = self.subagents.mounted_gate() {
                if !gate.can_send() {
                    return format!("子代理 · {}", gate.hint());
                }
            }
            // The enabled child composer needs no resident shortcut string;
            // its identity and state live in the header row.
            return String::new();
        }
        if self.schedule.open {
            if let Some(busy) = &self.schedule.busy {
                return busy.clone();
            }
            if let Some(error) = &self.schedule.error {
                return format!("定时任务：{error}");
            }
            if let Some(notice) = &self.schedule.notice {
                return format!("定时任务：{notice}");
            }
            return "定时任务 · Ctrl+G 或 Esc 关闭".to_string();
        }
        if self.sessions_panel.open {
            return "会话列表：↑↓ 选择 · Enter 切换 · n 改名 · c 压缩 · r 刷新 · Esc 关闭"
                .to_string();
        }
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
        if self.decision_owns_seat() {
            return match self.decisions.top() {
                Some(nano_decisions::DecisionTop::Approval(_)) => {
                    "审批：↑↓ 切换 · Enter 确认 · 1 选允许 · 2 选拒绝 · Esc 取消（不会允许）"
                        .to_string()
                }
                Some(nano_decisions::DecisionTop::Question(card)) => {
                    if card.state() == nano_decisions::QuestionCardState::ReadOnly {
                        "问卷回看：↑↓ 滚动 · Esc 关闭".to_string()
                    } else if card.plan_review().is_some() {
                        "计划审阅：↑↓ 选择 · Enter 确认 · 1 展开/收起 · Esc 取消".to_string()
                    } else {
                        "问卷：↑↓ 移动 · Enter 选择/提交 · 空格勾选 · 数字直选 · Alt+N 跳过 · Alt+T 停止倒计时 · Esc 收起"
                            .to_string()
                    }
                }
                None => String::new(),
            };
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
        String::new()
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
        self.sessions_panel.open = true;
        self.sessions_panel.error = None;
        self.sessions_panel.selected = 0;
        self.scene.focus_mut().push(FocusId::new("sessions"));
        self.refresh_sessions();
    }

    /// Close the panel and return focus to the input editor.
    pub fn close_session_panel(&mut self) {
        self.sessions_panel.close();
        if self.scene.focus().top() == Some(&FocusId::new("sessions")) {
            self.scene.focus_mut().pop();
        }
    }

    /// Session panel keys never fall through to the conversation input.
    fn handle_sessions_panel_key(&mut self, key: KeyEvent) -> bool {
        if let Some(rename) = &mut self.sessions_panel.rename {
            match key.code {
                KeyCode::Esc => {
                    self.sessions_panel.rename = None;
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
                if self.sessions_panel.selected > 0 {
                    self.sessions_panel.selected -= 1;
                }
                true
            }
            KeyCode::Down => {
                if self.sessions_panel.selected < self.sessions_panel.rows.len() {
                    self.sessions_panel.selected += 1;
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
            self.sessions_panel.error = Some("宿主未就绪".to_string());
            return;
        }
        if self.sessions_panel.loading {
            return;
        }
        let Some(link) = &self.link else {
            self.sessions_panel.error = Some("宿主连接已关闭".to_string());
            return;
        };
        match link.request("nano.sessions.list", json!({})) {
            Ok(id) => {
                self.sessions_panel.loading = true;
                self.sessions_panel.error = None;
                self.pending.insert(id, Pending::SessionsList);
            }
            Err(message) => {
                self.sessions_panel.error = Some(format!("读取会话列表失败：{message}"));
            }
        }
    }

    /// Switch to the selected row, or create a fresh session at row zero.
    fn activate_selected_session(&mut self) {
        if self.sessions_panel.selected == 0 {
            self.begin_switch(SwitchTarget::new(
                nano_runtime::paths::new_root_session_id(),
                TargetMode::Create,
            ));
            return;
        }
        let Some(row) = self.sessions_panel.selected_row().cloned() else {
            return;
        };
        if !row.switchable() {
            self.sessions_panel.error = Some("子代理会话由 S12 接管，不能作为主会话".to_string());
            return;
        }
        if let Some(cwd) = self.hello.as_ref().map(|hello| hello.cwd.as_str()) {
            if !row.switchable_in(cwd) {
                self.sessions_panel.error = Some(format!(
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
        let Some(row) = self.sessions_panel.selected_row().cloned() else {
            self.sessions_panel.error = Some("「新建会话」没有标题".to_string());
            return;
        };
        if !row.current {
            self.sessions_panel.error =
                Some("冷会话改名会先 resume 并拿写锁；先打开该会话再改名".to_string());
            return;
        }
        if !self.ready {
            self.sessions_panel.error = Some("宿主未就绪".to_string());
            return;
        }
        self.sessions_panel.rename = Some(RenameInput {
            session_id: row.session_id.clone(),
            buffer: InputBuffer::from_text(row.title.clone().unwrap_or_default()),
        });
        self.sessions_panel.error = None;
    }

    /// Submit the rename; the official service decides normalization and errors.
    fn apply_rename(&mut self) {
        let Some(rename) = self.sessions_panel.rename.clone() else {
            return;
        };
        let title = rename.buffer.text().to_string();
        let Some(link) = &self.link else {
            self.sessions_panel.error = Some("宿主连接已关闭".to_string());
            return;
        };
        match link.request(
            "nano.sessions.rename",
            json!({"sessionId": rename.session_id, "title": title}),
        ) {
            Ok(id) => {
                self.sessions_panel.rename = None;
                self.pending.insert(
                    id,
                    Pending::Rename {
                        session_id: rename.session_id,
                        title,
                    },
                );
            }
            Err(message) => {
                self.sessions_panel.error = Some(format!("改名请求未能发出：{message}"));
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
        self.attachments.switch_to(address.clone());
        self.view = TranscriptView::new(address.clone(), TranscriptSource::Interactive);
        self.subagents = SubagentState::new(address.root_session_id.clone());
        self.child_models.clear();
        self.child_views.clear();
        self.parent_view = None;
        self.active_child_key = None;
        self.subagent_subscription = None;
        self.subagent_subscribe_generation = 0;
        self.subagent_backups.clear();
        self.subagents_snapshot_pending = false;
        self.subagents_snapshot_queued = false;
        self.subagent_list_loading = false;
        self.subagent_blink_at = None;
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
        self.view.scroll_mut().to_bottom();
        self.replay_truncated = false;
        self.sessions_panel.close();
        self.monitor = MonitorModel::new(address.root_session_id.clone());
        self.monitor_open = false;
        self.monitor_form = None;
        self.monitor_snapshot_pending = false;
        self.monitor_select_after_refresh = None;
        let root_session_id = address.root_session_id.clone();
        self.bind_terminal(root_session_id);
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
        let now = Instant::now();
        let mut changed = self.poll_restart_events();
        if self.terminal_active() {
            let commands = self
                .terminal
                .as_mut()
                .map(TerminalState::tick)
                .unwrap_or_default();
            if !commands.is_empty() {
                self.dispatch_terminal(commands);
                changed = true;
            }
        }
        if self.decisions.tick(now) {
            self.drain_decisions();
            changed = true;
        }
        if !self.active_model().previews().is_empty() {
            self.transcript_dirty = true;
            changed = true;
        }
        if self.view.scroll().recent_wheel(now) {
            return changed;
        }
        if self.subagents.cleanup(now) {
            changed = true;
        }
        if self.subagents.snapshot_due && !self.subagents_snapshot_pending {
            self.refresh_subagents();
            changed = true;
        }
        let activity_active = self.activity_kind().is_some();
        if activity_active && !self.reduced_motion {
            match self.activity_at {
                None => {
                    self.activity_at = Some(now);
                    self.activity_phase = false;
                    changed = true;
                }
                Some(next) if now >= next => {
                    self.activity_phase = !self.activity_phase;
                    self.activity_at = Some(now + Duration::from_millis(500));
                    changed = true;
                }
                Some(_) => {}
            }
        } else if self.activity_at.take().is_some() || self.activity_phase {
            self.activity_phase = false;
            changed = true;
        }
        if self.subagents.any_running() && !self.reduced_motion {
            match self.subagent_blink_at {
                None => {
                    self.subagent_blink_at = Some(now);
                    changed = true;
                }
                Some(next) if now >= next => {
                    self.subagent_blink_at = Some(now + nano_subagents::BLINK_PERIOD / 2);
                    changed = true;
                }
                Some(_) => {}
            }
        } else {
            self.subagent_blink_at = None;
        }
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

/// Whether the handshake reports one official capability as usable now.
fn capability_supported(hello: &Hello, name: &str) -> bool {
    hello
        .capabilities
        .get(name)
        .is_some_and(|capability| capability.status == CapabilityStatus::Supported)
}

/// Put selected terminal text on the clipboard through OSC 52. This does not
/// send any key to the process and works in Ghostty without an extra crate.
fn write_clipboard(text: &str) {
    use std::io::Write;
    let encoded = base64(text.as_bytes());
    let mut stdout = std::io::stdout();
    let _ = write!(stdout, "\x1b]52;c;{encoded}\x07");
    let _ = stdout.flush();
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

/// Map one terminal key to the decision-card vocabulary.
fn decision_key(key: KeyEvent) -> Option<DecisionKey> {
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return None;
    }
    match key.code {
        KeyCode::Up => Some(DecisionKey::Up),
        KeyCode::Down => Some(DecisionKey::Down),
        KeyCode::Left => Some(DecisionKey::Left),
        KeyCode::Right => Some(DecisionKey::Right),
        KeyCode::Tab if shift => Some(DecisionKey::BackTab),
        KeyCode::Tab => Some(DecisionKey::Tab),
        KeyCode::BackTab => Some(DecisionKey::BackTab),
        KeyCode::Enter => Some(DecisionKey::Enter),
        KeyCode::Esc => Some(DecisionKey::Esc),
        KeyCode::Backspace => Some(DecisionKey::Backspace),
        KeyCode::PageUp => Some(DecisionKey::PageUp),
        KeyCode::PageDown => Some(DecisionKey::PageDown),
        KeyCode::Char(' ') => Some(DecisionKey::Space),
        KeyCode::Char('n') | KeyCode::Char('N') if alt => Some(DecisionKey::Skip),
        KeyCode::Char('t') | KeyCode::Char('T') if alt => Some(DecisionKey::MoreTime),
        KeyCode::Char(character) if !alt => Some(DecisionKey::Char(character)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;
    use serde_json::json;

    fn app() -> App {
        App::new(Cli::default(), "root".to_string(), false)
    }

    #[test]
    fn the_activity_indicator_toggles_every_500ms() {
        let mut app = app();
        app.decisions.apply_event(
            "decisions.approval",
            &json!({
                "address": {"rootSessionId": "root"},
                "state": "pending",
                "requestId": "a-1",
                "toolName": "bash"
            }),
        );
        assert_eq!(app.activity_kind(), Some(ActivityKind::AwaitingApproval));

        app.tick();
        assert!(
            !app.activity_phase,
            "the first tick arms the timer off-screen"
        );
        app.activity_at = Some(Instant::now() - Duration::from_millis(1));
        app.tick();
        assert!(
            app.activity_phase,
            "the phase flips when the deadline passes"
        );
        let next = app.activity_at.expect("next transition is scheduled");
        assert!(
            next > Instant::now() + Duration::from_millis(400),
            "the next transition is about 500ms away"
        );

        app.reduced_motion = true;
        app.tick();
        assert!(!app.activity_phase);
        assert!(app.activity_at.is_none());
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

    /// A handshake with the official jobs registry and the S10 Monitor surface.
    fn hello_with_monitor() -> Hello {
        use nano_runtime::protocol::Capability;
        let mut hello = hello_with_attachments(0);
        for name in ["jobs", "monitor"] {
            hello.capabilities.insert(
                name.to_string(),
                Capability {
                    status: CapabilityStatus::Supported,
                    mode: None,
                    detail: None,
                    required: None,
                },
            );
        }
        hello
    }

    /// One official job projection with retained output bytes.
    fn monitor_job(id: &str, total: u64, earliest: u64) -> Value {
        json!({
            "id": id,
            "kind": "bash",
            "label": "sleep 30",
            "owner": "root",
            "status": "running",
            "startedAt": 1,
            "output": {"total": total, "earliest": earliest}
        })
    }

    /// One full monitor projection for live-event tests.
    fn monitor_projection(job_id: &str, stopped_by_user: bool) -> Value {
        json!({
            "id": job_id,
            "jobId": job_id,
            "mode": "notify",
            "listening": !stopped_by_user,
            "stoppedByUser": stopped_by_user,
            "from": "new",
            "initialOffset": 100,
            "offset": 120,
            "unreadBytes": 100,
            "skippedBytes": 100,
            "filter": null,
            "mergeWindowMs": 800,
            "budgets": {
                "maxNotifications": 50,
                "minIntervalMs": 1000,
                "maxLinesPerNotification": 20,
                "maxNotificationBytes": 8192,
                "maxNotificationTokens": 2048,
                "expiresAt": 1000000
            },
            "used": {"notifications": 1, "lines": 3, "bytes": 120, "estimatedTokens": 30},
            "exhausted": false,
            "stream": "watching",
            "jobStatus": "running",
            "lossy": false,
            "gaps": 0,
            "heldMatches": 0,
            "heldBytes": 0,
            "droppedMatches": 0,
            "deliveries": [],
            "role": "user",
            "source": "monitor",
            "createdAt": 1,
            "updatedAt": 2
        })
    }

    #[test]
    fn m_on_a_job_shows_the_unread_bytes_before_choosing_a_range() {
        let mut app = app();
        app.ready = true;
        app.hello = Some(hello_with_monitor());
        app.jobs
            .apply_snapshot(&json!({
                "seq": 1,
                "jobs": [monitor_job("bash-a", 200, 50)]
            }))
            .unwrap();
        app.jobs_open = true;
        app.handle_key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE));
        let form = app.monitor_form.as_ref().expect("attach form opened");
        assert_eq!(form.unread_bytes, 150);
        assert!(!form.from_earliest);
        assert!(!form.quiet);
        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        assert!(app.monitor_form.as_ref().unwrap().from_earliest);
        app.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE));
        assert!(app.monitor_form.as_ref().unwrap().quiet);
    }

    #[test]
    fn a_user_stopped_monitor_reopens_the_attach_form_instead_of_resuming() {
        let mut app = app();
        app.ready = true;
        app.hello = Some(hello_with_monitor());
        app.jobs
            .apply_snapshot(&json!({
                "seq": 1,
                "jobs": [monitor_job("bash-a", 200, 50)]
            }))
            .unwrap();
        app.monitor.apply_event(
            "monitor.updated",
            &json!({
                "address": {"rootSessionId": "root"},
                "seq": 1,
                "monitor": monitor_projection("bash-a", true)
            }),
        );
        app.begin_monitor_for_selected();
        assert!(app.monitor_form.is_some());
        assert!(!app.monitor_open);
    }

    #[test]
    fn a_live_monitor_projection_updates_the_panel_without_touching_history() {
        let mut app = app();
        app.handle_link(LinkEvent::Host(nano_runtime::HostEvent::Event(
            nano_runtime::protocol::EventParams {
                runtime_id: "rt".into(),
                generation: 1,
                subscription_id: "sub-1".into(),
                sequence: 2,
                kind: "monitor.updated".into(),
                payload: json!({
                    "address": {"rootSessionId": "root"},
                    "seq": 2,
                    "monitor": monitor_projection("bash-a", false)
                }),
            },
        )));
        assert_eq!(app.monitor.len(), 1);
        let monitor = app.monitor.selected_monitor().unwrap();
        assert_eq!(monitor.mode, nano_monitor::MonitorMode::Notify);
        assert_eq!(monitor.used.notifications, 1);
        assert!(monitor
            .identity_label()
            .contains("role=user / source=monitor"));
        assert!(app.model.items().is_empty());
    }

    #[test]
    fn a_simultaneous_monitor_notice_and_error_are_both_kept() {
        let mut app = app();
        let mut projection = monitor_projection("bash-a", false);
        projection["exhausted"] = json!(true);
        projection["exhaustReason"] = json!("notifications");
        projection["deliveries"] = json!([{
            "messageId": "message-1",
            "mode": "notify",
            "state": "failed",
            "at": 2,
            "lines": 1,
            "bytes": 20,
            "estimatedTokens": 5,
            "firstOffset": 0,
            "lastOffset": 0,
            "omittedLines": 0,
            "textPreview": "[monitor] job=bash-a matched=1",
            "detail": "投递调用失败"
        }]);
        app.handle_link(LinkEvent::Host(nano_runtime::HostEvent::Event(
            nano_runtime::protocol::EventParams {
                runtime_id: "rt".into(),
                generation: 1,
                subscription_id: "sub-1".into(),
                sequence: 2,
                kind: "monitor.updated".into(),
                payload: json!({
                    "address": {"rootSessionId": "root"},
                    "seq": 2,
                    "monitor": projection
                }),
            },
        )));
        assert!(app
            .notice
            .as_deref()
            .is_some_and(|notice| notice.contains("预算已用尽")));
        assert!(app
            .error
            .as_deref()
            .is_some_and(|error| error.contains("监听投递")));
    }

    #[test]
    fn monitor_filter_editing_respects_the_utf8_byte_cap() {
        let mut app = app();
        app.jobs_open = true;
        app.monitor_form = Some(MonitorForm {
            job_id: "bash-a".into(),
            job_label: "sleep 30".into(),
            unread_bytes: 0,
            from_earliest: false,
            quiet: false,
            filter: "a".repeat(511),
            filter_regex: false,
            editing_filter: true,
        });
        app.handle_key(KeyEvent::new(KeyCode::Char('€'), KeyModifiers::NONE));
        assert_eq!(app.monitor_form.as_ref().unwrap().filter.len(), 511);
    }

    #[test]
    fn monitor_page_keys_match_the_top_anchored_view() {
        let mut app = app();
        app.jobs_open = true;
        app.monitor_open = true;
        app.jobs_scroll_back = 25;
        app.handle_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
        assert_eq!(app.jobs_scroll_back, 15);
        app.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        assert_eq!(app.jobs_scroll_back, 25);
    }

    #[test]
    fn a_pending_monitor_selection_survives_an_event_arriving_after_create() {
        let mut app = app();
        app.monitor_select_after_refresh = Some("bash-a".to_string());
        app.handle_link(LinkEvent::Host(nano_runtime::HostEvent::Event(
            nano_runtime::protocol::EventParams {
                runtime_id: "rt".into(),
                generation: 1,
                subscription_id: "sub-1".into(),
                sequence: 2,
                kind: "monitor.updated".into(),
                payload: json!({
                    "address": {"rootSessionId": "root"},
                    "seq": 2,
                    "monitor": monitor_projection("bash-a", false)
                }),
            },
        )));
        assert_eq!(
            app.monitor
                .selected_monitor()
                .map(|monitor| monitor.job_id.as_str()),
            Some("bash-a")
        );
        assert!(app.monitor_select_after_refresh.is_none());
    }

    #[test]
    fn host_loss_drops_monitor_projections() {
        let mut app = app();
        app.monitor.apply_event(
            "monitor.updated",
            &json!({
                "address": {"rootSessionId": "root"},
                "seq": 1,
                "monitor": monitor_projection("bash-a", false)
            }),
        );
        assert_eq!(app.monitor.len(), 1);
        app.handle_host_loss();
        assert!(app.monitor.is_empty());
        assert!(!app.monitor_open);
        assert!(app.monitor_form.is_none());
    }

    #[test]
    fn switching_root_sessions_drops_monitor_projections() {
        let mut app = app();
        app.monitor.apply_event(
            "monitor.updated",
            &json!({
                "address": {"rootSessionId": "root"},
                "seq": 1,
                "monitor": monitor_projection("bash-a", false)
            }),
        );
        app.monitor_open = true;
        app.reset_for_target(&SessionAddress::root("other".to_string()));
        assert!(app.monitor.is_empty());
        assert!(!app.monitor_open);
        assert!(app.monitor_form.is_none());
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
    fn an_unknown_schedule_result_is_not_reported_as_failure() {
        let mut app = app();
        app.schedule.open_view(None);
        app.schedule.busy = Some("正在创建…".into());
        app.report_schedule_failure(
            "创建",
            RequestError {
                kind: RequestErrorKind::Unknown,
                code: None,
                message: "transport closed".into(),
            },
        );
        let error = app.schedule.error.as_deref().unwrap();
        assert!(error.contains("结果未知"));
        assert!(!error.contains("创建失败"));
        assert!(app.schedule.busy.is_none());
    }

    #[test]
    fn a_definitive_schedule_rejection_is_reported_as_failure() {
        let mut app = app();
        app.schedule.open_view(None);
        app.report_schedule_failure(
            "创建",
            RequestError {
                kind: RequestErrorKind::Rpc,
                code: Some(1044),
                message: "invalid_rule: bad cron".into(),
            },
        );
        assert!(app.schedule.error.as_deref().unwrap().contains("创建失败"));
    }

    #[test]
    fn control_g_opens_and_closes_the_schedule_overlay() {
        let mut app = app();
        assert!(app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL)));
        assert!(app.schedule.open);
        assert!(app.status_hint().contains("定时任务"));
        assert!(app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(!app.schedule.open);
    }

    #[test]
    fn an_unsupported_schedule_capability_is_shown_and_blocks_actions() {
        let mut app = app();
        app.schedule_unsupported = Some("调度未挂载".into());
        app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL));
        assert!(app.schedule.open);
        assert_eq!(app.schedule.unsupported.as_deref(), Some("调度未挂载"));
        app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        assert!(app.schedule.form.is_none());
        let rows: Vec<String> = app
            .schedule
            .rows()
            .into_iter()
            .map(|row| row.text)
            .collect();
        assert!(rows.iter().any(|row| row.contains("调度未挂载")));
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
        assert!(app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL)));
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
        app.drafts
            .current_mut()
            .set_text("/permission danger-full-access");
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
        assert!(app.status_permission().contains("权限：custom"));
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
        assert!(!app.notice.as_deref().unwrap_or_default().contains("已完成"));
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
        let line = app.status_permission();
        assert!(line.contains("权限：workspace-write"));
        assert!(line.contains("可能过期"));
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
        let before = app.view.scroll().back();
        app.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        assert_eq!(app.instructions_scroll, 10);
        assert_eq!(app.view.scroll().back(), before);
        assert!(app.instructions_panel_open());
    }

    #[test]
    fn subscription_kinds_cover_every_contracted_event_kind() {
        let mut ours = crate::link::SUBSCRIPTION_KINDS.to_vec();
        // Job and monitor kinds are added by `subscription_kinds` only when the
        // host reports the capability; the full potential set must still cover
        // the whole contract.
        ours.extend(["jobs.event", "jobs.output", "monitor.updated"]);
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

    #[test]
    fn the_session_panel_requires_a_ready_host() {
        let mut app = app();
        app.open_session_panel();
        assert!(!app.sessions_panel.open);
        assert!(app.error.as_deref().unwrap().contains("宿主未就绪"));
    }

    #[test]
    fn a_session_list_result_replaces_the_rows_and_clamps_the_selection() {
        let mut app = app();
        app.sessions_panel.open = true;
        app.sessions_panel.selected = 9;
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
        assert_eq!(app.sessions_panel.rows.len(), 2);
        assert_eq!(app.sessions_panel.selected, 2);
        assert_eq!(app.sessions_panel.selected_row().unwrap().session_id, "old");
        assert!(!app.sessions_panel.loading);
    }

    #[test]
    fn a_failed_rename_restores_the_editor_and_reports_the_official_error() {
        let mut app = app();
        app.sessions_panel.open = true;
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
                code: None,
                message: "nano/sessions/title-invalid".into(),
            }),
        );
        assert!(app
            .sessions_panel
            .error
            .as_deref()
            .unwrap()
            .contains("title-invalid"));
        assert_eq!(
            app.sessions_panel.rename.as_ref().unwrap().buffer.text(),
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
        app.sessions_panel.open = true;
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
                code: None,
                message: "transport closed".into(),
            }),
        );
        assert!(app
            .sessions_panel
            .error
            .as_deref()
            .unwrap()
            .contains("未知"));
        assert!(
            app.sessions_panel.rename.is_none(),
            "a lost receipt does not reopen the editor"
        );
    }

    #[test]
    fn esc_while_a_compact_wait_is_in_flight_cancels_the_wait() {
        let mut app = app();
        app.sessions_panel.open = true;
        app.compact_in_flight = true;
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.sessions_panel.open, "the panel stays open");
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
                code: None,
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
        app.sessions_panel.open = true;
        app.sessions_panel.rows.push(
            SessionSummary::parse(&json!({
                "sessionId": "old",
                "createdAt": 1,
                "agentAvailable": false,
                "running": false,
                "current": true
            }))
            .unwrap(),
        );
        app.sessions_panel.selected = 1;
        app.sessions_panel.close();
        assert!(app.sessions_panel.rows.is_empty());
        assert_eq!(app.sessions_panel.selected, 0);
    }
    #[test]
    fn an_approval_event_takes_the_input_seat_and_enter_rejects() {
        let mut app = app();
        app.handle_link(LinkEvent::Host(nano_runtime::HostEvent::Event(
            nano_runtime::protocol::EventParams {
                runtime_id: "rt".into(),
                generation: 1,
                subscription_id: "sub-1".into(),
                sequence: 1,
                kind: "decisions.approval".into(),
                payload: json!({
                    "address": {"rootSessionId": "root"},
                    "state": "pending",
                    "requestId": "a-1",
                    "toolName": "bash",
                    "reason": "sandbox"
                }),
            },
        )));
        assert!(app.decisions.approval().is_some());
        assert!(!app.scene.focus().input_focused());
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.decisions.approval().is_some());
        assert!(app.error.as_deref().unwrap().contains("宿主未就绪"));
    }

    #[test]
    fn a_timed_question_takes_the_seat_and_esc_hides_without_answering() {
        let mut app = app();
        app.handle_link(LinkEvent::Host(nano_runtime::HostEvent::Event(
            nano_runtime::protocol::EventParams {
                runtime_id: "rt".into(),
                generation: 1,
                subscription_id: "sub-1".into(),
                sequence: 1,
                kind: "decisions.question".into(),
                payload: json!({
                    "address": {"rootSessionId": "root"},
                    "state": "pending",
                    "requestId": "q-1",
                    "callId": "c-1",
                    "timed": true,
                    "remainingMs": 60_000,
                    "questions": [{"id": "q1", "question": "Pick", "options": [{"label": "A"}]}]
                }),
            },
        )));
        let card = app.decisions.question().unwrap();
        assert_eq!(card.call_id(), Some("c-1"));
        assert!(card.timed().remaining_ms.is_some());
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.decisions.top().is_none());
        assert!(app.scene.focus().input_focused());
    }

    #[test]
    fn decision_events_for_another_address_are_ignored() {
        let mut app = app();
        app.handle_link(LinkEvent::Host(nano_runtime::HostEvent::Event(
            nano_runtime::protocol::EventParams {
                runtime_id: "rt".into(),
                generation: 1,
                subscription_id: "sub-1".into(),
                sequence: 1,
                kind: "decisions.approval".into(),
                payload: json!({
                    "address": {"rootSessionId": "other"},
                    "state": "pending",
                    "requestId": "a-1",
                    "toolName": "bash"
                }),
            },
        )));
        assert!(app.decisions.top().is_none());
    }
}

#[cfg(test)]
mod terminal_tests {
    use super::*;
    use crossterm::event::KeyEvent;
    use serde_json::json;

    fn app() -> App {
        App::new(Cli::default(), "root".to_string(), false)
    }

    fn terminal_app() -> App {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "nano-app-terminal-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        let store = nano_terminal::TerminalStore::open(&root, "root").expect("terminal store");
        let mut app = App::new(Cli::default(), "root".to_string(), false);
        app.terminal = Some(nano_terminal::TerminalState::new(store, (80, 24)));
        app
    }

    #[test]
    fn terminal_scene_toggles_and_owns_the_keyboard() {
        let mut app = terminal_app();
        let toggle = KeyEvent::new(KeyCode::F(12), KeyModifiers::NONE);
        assert!(app.handle_key(toggle));
        assert!(app.terminal_active());
        assert!(app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)));
        assert!(!app.should_quit);
        assert!(app.handle_key(toggle));
        assert!(!app.terminal_active());
    }

    #[test]
    fn switching_root_sessions_rebinds_the_terminal_view() {
        let mut app = terminal_app();
        app.terminal_data_root = Some(
            std::env::temp_dir().join(format!("nano-app-terminal-switch-{}", std::process::id())),
        );
        if let Some(terminal) = app.terminal.as_mut() {
            terminal.set_root_session_id("root".to_string());
        }
        app.reset_for_target(&SessionAddress::root("other".to_string()));
        let terminal = app.terminal.as_ref().expect("terminal rebound");
        assert_eq!(terminal.root_session_id(), "other");
        assert_eq!(terminal.phase(), nano_terminal::Phase::Idle);
        assert!(!app.terminal_active());
    }

    #[test]
    fn terminal_paste_does_not_touch_the_conversation_draft() {
        let mut app = terminal_app();
        app.open_terminal();
        app.handle_paste("ls -la".to_string());
        assert_eq!(app.drafts.current().text(), "");
        app.hide_terminal();
        app.handle_paste("hello".to_string());
        assert_eq!(app.drafts.current().text(), "hello");
    }

    fn install_subagents(app: &mut App, agents: Value) {
        app.subagents
            .apply_snapshot(&json!({
                "agents": agents,
                "diagnostics": [],
                "parentLive": true
            }))
            .expect("valid snapshot");
    }

    fn subagent_row(id: &str, mode: &str) -> Value {
        json!({
            "id": id,
            "parentId": "root",
            "depth": 1,
            "mode": mode,
            "label": format!("task {id}"),
            "activity": "inactive",
            "hasChildren": false
        })
    }

    fn describe_child(id: &str, mode: &str) -> Value {
        json!({
            "id": id,
            "parentId": "root",
            "mode": mode,
            "label": format!("task {id}"),
            "activity": "inactive",
            "parentLive": true,
            "seq": 1
        })
    }

    #[test]
    fn dock_enter_and_escape_restore_parent_and_child_drafts() {
        let mut app = app();
        install_subagents(&mut app, json!([subagent_row("child", "continuable")]));
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert!(app.subagents.nav().dock_focus);
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.subagent_active());
        app.apply_subagent_description(&describe_child("child", "continuable"));
        assert_eq!(app.subagents.mounted_gate(), Some(ComposerGate::Enabled));
        app.drafts.current_mut().set_text("child draft");
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.subagent_active());
        assert_eq!(app.drafts.current().text(), "");
        // Re-entering the same address restores its draft.
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        app.apply_subagent_description(&describe_child("child", "continuable"));
        assert_eq!(app.drafts.current().text(), "child draft");
    }

    #[test]
    fn describe_window_never_sends_or_shows_the_parent_draft() {
        let mut app = app();
        install_subagents(&mut app, json!([subagent_row("child", "continuable")]));
        let root = SessionAddress::root("root".to_string());
        app.drafts.current_mut().set_text("parent draft");
        assert!(app.enter_subagent(0));
        assert_eq!(app.subagents.mounted_gate(), Some(ComposerGate::Resolving));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        // The child address is not usable before its history is confirmed.
        assert_eq!(app.drafts.current().text(), "parent draft");
        assert!(app.subagent_backups.is_empty());
        app.apply_subagent_description(&describe_child("child", "continuable"));
        assert_eq!(app.subagents.mounted_gate(), Some(ComposerGate::Enabled));
        assert_eq!(app.drafts.current().text(), "");
        assert_eq!(
            app.drafts.get(&root).map(|draft| draft.text()),
            Some("parent draft")
        );
    }

    #[test]
    fn child_k_types_normally_and_alt_k_stops() {
        let mut app = app();
        install_subagents(&mut app, json!([subagent_row("child", "continuable")]));
        assert!(app.enter_subagent(0));
        app.apply_subagent_description(&describe_child("child", "continuable"));
        app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));
        assert_eq!(app.drafts.current().text(), "k");
        // Alt+K is the independent stop; with no host it is a no-op and must
        // not consume or alter the draft.
        app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::ALT));
        assert_eq!(app.drafts.current().text(), "k");
    }

    #[test]
    fn child_status_line_surfaces_errors_and_hides_root_permission() {
        let mut app = app();
        install_subagents(&mut app, json!([subagent_row("child", "continuable")]));
        assert!(app.enter_subagent(0));
        app.apply_subagent_description(&describe_child("child", "continuable"));
        app.error = Some("官方拒绝".to_string());
        let hint = app.status_hint();
        assert!(hint.contains("子代理错误"));
        assert!(hint.contains("官方拒绝"));
        app.error = None;
        let line = app.status_permission();
        assert!(line.contains("子代理审批由官方拒绝"));
        assert!(!line.contains("workspace-write"));
    }

    #[test]
    fn alt_l_opens_the_all_agents_list_even_when_the_dock_is_empty() {
        let mut app = app();
        app.handle_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::ALT));
        assert!(app.subagents.nav().list_open);
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.subagents.nav().list_open);
    }

    #[test]
    fn a_stale_subscribe_response_is_released() {
        let mut app = app();
        app.subagent_subscribe_generation = 2;
        app.active_child_key = Some("child:root:root:child:continuable".to_string());
        app.pending.insert(
            7,
            Pending::SubagentSubscribe {
                address_key: "old".to_string(),
                generation: 1,
            },
        );
        app.handle_request_done(7, Ok(json!({ "subscriptionId": "stale" })));
        assert!(app.subagent_subscription.is_none());
    }

    #[test]
    fn failed_child_prompt_restores_the_sending_address_draft() {
        let mut app = app();
        install_subagents(&mut app, json!([subagent_row("child", "continuable")]));
        assert!(app.enter_subagent(0));
        app.apply_subagent_description(&describe_child("child", "continuable"));
        let address = app.subagents.mounted().unwrap().address.clone();
        app.drafts.current_mut().set_text("lost text");
        app.subagent_backups.insert(
            "req-1".to_string(),
            (
                address.clone(),
                SubmittedDraft {
                    text: "lost text".to_string(),
                    attachments: ComposerAttachments::default(),
                },
            ),
        );
        app.drafts.current_mut().take();
        assert!(app.leave_subagent());
        app.restore_subagent_draft("req-1");
        assert_eq!(
            app.drafts.get(&address).map(|draft| draft.text()),
            Some("lost text")
        );
        assert_eq!(app.drafts.current().text(), "");
    }

    #[test]
    fn mounting_a_child_clears_hidden_root_focus_owners() {
        let mut app = app();
        install_subagents(&mut app, json!([subagent_row("child", "continuable")]));
        app.jobs_open = true;
        app.sessions_panel.open = true;
        assert!(app.enter_subagent(0));
        assert!(app.scene.focus().input_focused());
        assert!(!app.jobs_open);
        assert!(!app.sessions_panel.open);
    }

    #[test]
    fn one_shot_child_is_read_only_and_swallows_typing() {
        let mut app = app();
        install_subagents(&mut app, json!([subagent_row("old", "one-shot")]));
        assert!(app.enter_subagent(0));
        app.apply_subagent_description(&describe_child("old", "one-shot"));
        assert_eq!(app.subagents.mounted_gate(), Some(ComposerGate::ReadOnly));
        app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(app.drafts.current().text(), "");
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.subagent_active());
    }

    #[test]
    fn unknown_mode_stays_read_only_until_history_resolves_it() {
        let mut app = app();
        install_subagents(&mut app, json!([subagent_row("mystery", "unknown")]));
        assert!(app.enter_subagent(0));
        assert_eq!(app.subagents.mounted_gate(), Some(ComposerGate::Resolving));
        app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(app.drafts.current().text(), "");
        app.apply_subagent_description(&describe_child("mystery", "continuable"));
        assert_eq!(app.subagents.mounted_gate(), Some(ComposerGate::Enabled));
        app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(app.drafts.current().text(), "x");
    }

    #[test]
    fn external_child_never_opens_a_detail_view() {
        let mut app = app();
        install_subagents(&mut app, json!([subagent_row("remote", "external")]));
        assert!(!app.enter_subagent(0));
        assert!(!app.subagent_active());
    }

    #[test]
    fn child_session_events_never_enter_the_root_model() {
        let mut app = app();
        install_subagents(&mut app, json!([subagent_row("child", "continuable")]));
        assert!(app.enter_subagent(0));
        app.apply_subagent_description(&describe_child("child", "continuable"));
        app.handle_link(LinkEvent::Host(nano_runtime::HostEvent::Event(
            nano_runtime::protocol::EventParams {
                runtime_id: "rt".into(),
                generation: 1,
                subscription_id: "sub-child".into(),
                sequence: 1,
                kind: "session.event".into(),
                payload: json!({
                    "address": {
                        "rootSessionId": "root",
                        "parentSessionId": "root",
                        "childSessionId": "child",
                        "mode": "continuable"
                    },
                    "seq": 1,
                    "type": "user/message",
                    "data": {
                        "id": "message-1",
                        "role": "user",
                        "content": [{"type": "text", "text": "child only"}],
                        "source": {"kind": "user", "rpcId": "r-1"}
                    }
                }),
            },
        )));
        assert_eq!(app.model.history().len(), 0);
        let key = app.active_child_key.clone().expect("child mounted");
        assert_eq!(app.child_models[&key].history().len(), 1);
    }

    #[test]
    fn terminal_cleanup_hides_the_dock_entry_but_not_the_open_view() {
        let mut app = app();
        install_subagents(&mut app, json!([subagent_row("child", "one-shot")]));
        assert!(app.enter_subagent(0));
        app.apply_subagent_description(&describe_child("child", "one-shot"));
        app.subagents
            .apply_event(&json!({
                "type": "end",
                "childId": "child",
                "stopReason": "completed"
            }))
            .expect("valid lifecycle event");
        assert!(app
            .subagents
            .cleanup(Instant::now() + nano_subagents::CLEANUP_DELAY * 2));
        assert!(app.subagents.dock().0.is_empty());
        // The mounted detail is not closed by the dock timer.
        assert!(app.subagent_active());
    }

    #[test]
    fn list_navigation_returns_to_the_selected_row() {
        let mut app = app();
        app.subagents
            .apply_list(&json!({
                "entries": [
                    {"id": "a", "parentId": "root", "depth": 1, "mode": "continuable", "activity": "inactive", "hasChildren": true},
                    {"id": "c", "parentId": "a", "depth": 2, "mode": "one-shot", "activity": "inactive", "hasChildren": false}
                ],
                "diagnostics": [
                    {"id": "broken", "parentId": "a", "depth": 2, "reason": "corrupt"}
                ]
            }))
            .expect("valid list");
        app.enter_subagent_overflow();
        assert!(app.subagents.nav().list_open);
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.subagent_active());
        app.apply_subagent_description(&json!({
            "id": "c",
            "parentId": "a",
            "mode": "one-shot",
            "activity": "inactive",
            "parentLive": false,
            "seq": 1
        }));
        assert_eq!(app.subagents.mounted_gate(), Some(ComposerGate::ReadOnly));
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.subagents.nav().list_open);
        assert_eq!(app.subagents.nav().list_selected, 1);
    }

    #[test]
    fn the_send_button_in_a_child_view_submits_to_the_child_address() {
        let mut app = app();
        install_subagents(&mut app, json!([subagent_row("child", "continuable")]));
        assert!(app.enter_subagent(0));
        app.apply_subagent_description(&describe_child("child", "continuable"));
        app.drafts.current_mut().set_text("child draft message");
        // A root decision card must not block the child composer either.
        app.decisions.apply_event(
            "decisions.approval",
            &json!({
                "address": {"rootSessionId": "root"},
                "state": "pending",
                "requestId": "a-1",
                "toolName": "bash"
            }),
        );
        assert!(app.click_send());
        assert!(app.error.as_deref().unwrap().contains("子代理消息未发送"));
        assert_eq!(app.drafts.current().text(), "child draft message");
    }

    #[test]
    fn clicking_the_jump_button_returns_to_the_bottom() {
        let mut app = app();
        app.view.scroll_mut().to_top(100, 10);
        assert!(!app.view.scroll().sticky());
        app.jump_area = Rect::new(10, 5, 10, 1);
        assert!(app.click_at(12, 5));
        assert!(app.view.scroll().sticky());
    }

    #[test]
    fn ctrl_end_returns_to_the_bottom() {
        let mut app = app();
        app.view.scroll_mut().to_top(100, 10);
        assert!(!app.view.scroll().sticky());
        app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL));
        assert!(app.view.scroll().sticky());
        assert_eq!(app.view.scroll().back(), 0);
    }

    fn temp_history_path(prefix: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "nano-history-{prefix}-{}-{stamp}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("temp history dir");
        dir.join("history.jsonl")
    }

    fn append_history(path: &std::path::Path, project: &str, session: &str, display: &str) {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let line = json!({
            "display": display,
            "timestamp": 0,
            "project": project,
            "sessionId": session,
        })
        .to_string();
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)
            .expect("history file");
        writeln!(file, "{line}").expect("history line");
    }

    fn remove_history(path: &std::path::Path) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }

    #[test]
    fn root_history_reads_the_project_file_current_session_first() {
        let path = temp_history_path("order");
        let project = "/tmp/nano-history-project-a";
        let other_project = "/tmp/nano-history-project-b";
        append_history(&path, project, "other-session", "old-other");
        append_history(&path, project, "root", "current-new");
        append_history(&path, other_project, "root", "wrong-project");
        {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .expect("history file");
            file.write_all(b"{bad\n").expect("bad line");
        }
        append_history(&path, project, "root", "current-newest");

        let mut app = app();
        app.history_path = Some(path.clone());
        app.cli.cwd = Some(PathBuf::from(project));
        assert_eq!(
            app.root_input_history(),
            vec!["old-other", "current-new", "current-newest"]
        );
        remove_history(&path);
    }

    #[test]
    fn root_history_is_capped_at_the_newest_hundred_project_entries() {
        let path = temp_history_path("cap");
        let project = "/tmp/nano-history-cap";
        for index in 0..150 {
            append_history(&path, project, "root", &format!("p{index:03}"));
        }
        let mut app = app();
        app.history_path = Some(path.clone());
        app.cli.cwd = Some(PathBuf::from(project));
        let entries = app.root_input_history();
        assert_eq!(entries.len(), 100);
        assert_eq!(entries.first().map(String::as_str), Some("p050"));
        assert_eq!(entries.last().map(String::as_str), Some("p149"));
        remove_history(&path);
    }

    #[test]
    fn a_submitted_prompt_is_appended_with_private_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let path = temp_history_path("write");
        let project = "/tmp/nano-history-write";
        let mut app = app();
        app.history_path = Some(path.clone());
        app.cli.cwd = Some(PathBuf::from(project));
        app.record_history_entry("remember me");
        let text = std::fs::read_to_string(&path).expect("history file");
        let value: Value = serde_json::from_str(text.trim()).expect("valid line");
        assert_eq!(value["display"], "remember me");
        assert_eq!(value["project"], project);
        assert_eq!(value["sessionId"], "root");
        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        remove_history(&path);
    }

    #[test]
    fn up_recalls_file_history_and_down_restores_the_draft() {
        let path = temp_history_path("keys");
        let project = "/tmp/nano-history-keys";
        append_history(&path, project, "root", "first");
        append_history(&path, project, "root", "second");
        let mut app = app();
        app.history_path = Some(path.clone());
        app.cli.cwd = Some(PathBuf::from(project));
        app.drafts.current_mut().set_text("draft");
        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.drafts.current().text(), "second");
        assert_eq!(app.drafts.current().cursor(), 0);
        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.drafts.current().text(), "first");
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.drafts.current().text(), "second");
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.drafts.current().text(), "draft");
        assert_eq!(app.drafts.current().cursor(), 5);
        // Editing a recalled entry ends the browse but keeps the text.
        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(app.drafts.current().text(), "xsecond");
        assert!(app.history_browse.is_none());
        remove_history(&path);
    }

    #[test]
    fn blank_up_without_history_focuses_the_dock() {
        let mut app = app();
        install_subagents(&mut app, json!([subagent_row("child", "continuable")]));
        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert!(app.subagents.nav().dock_focus);
    }

    #[test]
    fn child_history_uses_only_user_messages_from_the_child_projection() {
        let mut app = app();
        install_subagents(&mut app, json!([subagent_row("child", "continuable")]));
        assert!(app.enter_subagent(0));
        app.apply_subagent_description(&describe_child("child", "continuable"));
        for (seq, text, source) in [
            (1, "child prompt", "user"),
            (2, "injected notice", "system"),
        ] {
            app.handle_link(LinkEvent::Host(nano_runtime::HostEvent::Event(
                nano_runtime::protocol::EventParams {
                    runtime_id: "rt".into(),
                    generation: 1,
                    subscription_id: "sub-child".into(),
                    sequence: seq,
                    kind: "session.event".into(),
                    payload: json!({
                        "address": {
                            "rootSessionId": "root",
                            "parentSessionId": "root",
                            "childSessionId": "child",
                            "mode": "continuable"
                        },
                        "seq": seq,
                        "type": "user/message",
                        "data": {
                            "id": format!("message-{seq}"),
                            "role": "user",
                            "content": [{"type": "text", "text": text}],
                            "source": {"kind": source, "rpcId": format!("r-{seq}")}
                        }
                    }),
                },
            )));
        }
        assert_eq!(app.input_history(), vec!["child prompt"]);
    }
}
