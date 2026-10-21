//! Host process ownership: spawn with fd 3/4, handshake, requests, events,
//! lifecycle transitions, and the bounded shutdown report.
//!
//! The Rust side owns exactly one child process group. It never scans process
//! names, never shells out to build a command line, and never treats EOF as a
//! successful stream end.

use std::collections::{HashMap, VecDeque};
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::frame::FrameReader;
use crate::lifecycle::{Lifecycle, LifecycleState};
use crate::paths::{
    new_nonce, new_root_session_id, new_runtime_id, write_runtime_patch, Preflight, ProfileInstallSummary,
    RuntimePaths,
};
use crate::protocol::{
    encode_request, parse_incoming, Address, CancelResult, EventParams, FrameRules, Hello, IncomingFrame,
    ProtocolVersion, SessionFactMode, ShutdownReport, StartupErrorParams, StreamEndedParams, SubscribeResult,
};
use crate::protocol_error::{HostError, HostRpcError, ProtocolError};
use crate::{
    GRACEFUL_SHUTDOWN_MS, MAX_PENDING_REQUESTS, PROFILE_NAME, TERM_GRACE_MS,
};

/// Whether this launch creates a new root session or resumes an existing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionMode {
    /// Create the identity supplied by the caller.
    Create,
    /// Resume the exact persisted session.
    Resume,
}

impl SessionMode {
    /// Lowercase spelling used on the wire and in the runtime patch.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Resume => "resume",
        }
    }
}

/// Options for one host launch. Every path can be overridden for tests.
#[derive(Debug, Clone, Default)]
pub struct HostOptions {
    /// Working directory; defaults to the caller's current directory.
    pub cwd: Option<PathBuf>,
    /// Nano data root; defaults to `$HOME/.dsh-tui-nano`.
    pub data_root: Option<PathBuf>,
    /// Official dsh home; defaults to `$DSH_HOME` or `$HOME/.dsh`.
    pub dsh_home: Option<PathBuf>,
    /// Node executable.
    pub node_bin: Option<PathBuf>,
    /// dsh launcher executable.
    pub dsh_bin: Option<PathBuf>,
    /// Bridge package directory.
    pub bridge_package: Option<PathBuf>,
    /// Handshake timeout; defaults to 60 seconds (the mount lock can wait 30).
    pub handshake_timeout: Option<Duration>,
    /// Per-request timeout; defaults to 30 seconds.
    pub request_timeout: Option<Duration>,
    /// Test-only faults forwarded to the bridge config.
    pub test_faults: Option<Value>,
    /// Test-only: force the stdin/stdout nonce fallback transport.
    pub force_fallback: bool,
    /// Optional initial provider route for the root Agent.
    pub provider: Option<String>,
    /// Optional initial model for the root Agent.
    pub model: Option<String>,
    /// Optional initial reasoning effort for the root Agent.
    pub reasoning_effort: Option<String>,
}

/// Events surfaced to the app.
#[derive(Debug, Clone)]
pub enum HostEvent {
    /// A `nano.hello` notification.
    Hello(Hello),
    /// A `nano.startupError` notification.
    StartupError(StartupErrorParams),
    /// A `nano.event` notification.
    Event(EventParams),
    /// A `nano.streamEnded` notification.
    StreamEnded(StreamEndedParams),
    /// The transport ended; `lost` means the process is gone.
    TransportClosed {
        /// Whether the host process exited.
        lost: bool,
    },
    /// The host sent something this build cannot accept.
    ProtocolFailure(String),
}

/// Four-fact shutdown outcome.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShutdownOutcome {
    /// The host answered `nano.shutdown`.
    pub ack: bool,
    /// The child process exited.
    pub process_exited: bool,
    /// Child exit code, when it was reaped.
    pub exit_code: Option<i32>,
    /// TERM or KILL was used.
    pub forced: bool,
    /// Host-reported per-step report, when acknowledged.
    pub report: Option<ShutdownReport>,
    /// Whether the host reported subscriptions and agents stopped.
    pub scope_cleared: Option<bool>,
    /// Whether the host reported session persistence flushed.
    pub session_saved: Option<bool>,
    /// True only when every fact is positive and no forced termination was used.
    pub clean: bool,
}

/// One spawned host process. Cloning shares the same process and transport.
#[derive(Clone)]
pub struct Host {
    inner: Arc<HostInner>,
}

struct HostInner {
    shared: Arc<Shared>,
    child: Arc<ChildState>,
    writer: Mutex<Option<Box<dyn Write + Send>>>,
    inbox: Mutex<mpsc::Receiver<HostEvent>>,
    diagnostics: Arc<Mutex<Diagnostics>>,
    preflight: Preflight,
    patch_path: PathBuf,
    shutdown: Mutex<Option<ShutdownOutcome>>,
    shutdown_lock: Mutex<()>,
    hello: Mutex<Option<Hello>>,
    options: HostOptions,
}

struct Shared {
    pending: Mutex<HashMap<String, mpsc::Sender<Result<Value, HostRpcError>>>>,
    lifecycle: Lifecycle,
    exited: AtomicBool,
    exit_code: Mutex<Option<i32>>,
    transport_ended: AtomicBool,
    hello_seen: AtomicBool,
    runtime_id: String,
    root_session_id: String,
    generation: u64,
    session_mode: SessionMode,
    nonce: String,
    require_nonce: bool,
    request_counter: AtomicU64,
}

struct ChildState {
    child: Mutex<Child>,
    pid: u32,
}

#[derive(Debug, Default)]
struct Diagnostics {
    lines: VecDeque<String>,
    bytes: usize,
}

impl Diagnostics {
    fn push_line(&mut self, line: String) {
        let line: String = line.chars().take(4096).collect();
        self.bytes += line.len();
        self.lines.push_back(line);
        while self.lines.len() > 200 || self.bytes > 256 * 1024 {
            if let Some(removed) = self.lines.pop_front() {
                self.bytes = self.bytes.saturating_sub(removed.len());
            } else {
                break;
            }
        }
    }

    fn tail(&self, count: usize) -> Vec<String> {
        self.lines.iter().rev().take(count).rev().cloned().collect()
    }
}

impl Host {
    /// Resolve paths and verify the official version without spawning.
    pub fn preflight(options: &HostOptions) -> Result<Preflight, HostError> {
        Preflight::resolve(
            options.cwd.as_deref(),
            options.data_root.as_deref(),
            options.dsh_home.as_deref(),
            options.node_bin.as_deref(),
            options.dsh_bin.as_deref(),
            options.bridge_package.as_deref(),
        )
    }

    /// Initialize or update the Nano profile. Runs pnpm and verifies composition.
    pub fn install_profile(options: &HostOptions) -> Result<ProfileInstallSummary, HostError> {
        Self::preflight(options)?.install_profile()
    }

    /// Spawn one host generation.
    pub fn spawn(
        runtime_id: impl Into<String>,
        generation: u64,
        root_session_id: impl Into<String>,
        mode: SessionMode,
        options: &HostOptions,
    ) -> Result<Self, HostError> {
        if generation == 0 {
            return Err(HostError::Spawn("generation must be positive".into()));
        }
        let runtime_id = runtime_id.into();
        let root_session_id = root_session_id.into();
        if runtime_id.is_empty() || root_session_id.is_empty() {
            return Err(HostError::Spawn("runtimeId and rootSessionId must be non-empty".into()));
        }
        if options.provider.is_some() != options.model.is_some() {
            return Err(HostError::Spawn(
                "provider and model must be supplied together, or neither".into(),
            ));
        }
        let preflight = Self::preflight(options)?;
        let nonce = new_nonce();
        let patch_path = write_runtime_patch(
            &preflight.paths,
            mode,
            &runtime_id,
            generation,
            &root_session_id,
            &nonce,
            &preflight.official_version,
            options.test_faults.as_ref(),
            options.provider.as_deref(),
            options.model.as_deref(),
            options.reasoning_effort.as_deref(),
        )?;

        let spawned = if options.force_fallback {
            spawn_fallback(&preflight.paths, &patch_path, &preflight.official_version)?
        } else {
            spawn_primary(&preflight.paths, &patch_path, &preflight.official_version)
                .or_else(|_| spawn_fallback(&preflight.paths, &patch_path, &preflight.official_version))?
        };

        let lifecycle = Lifecycle::new();
        lifecycle.transition(LifecycleState::Starting)?;
        let shared = Arc::new(Shared {
            pending: Mutex::new(HashMap::new()),
            lifecycle,
            exited: AtomicBool::new(false),
            exit_code: Mutex::new(None),
            transport_ended: AtomicBool::new(false),
            hello_seen: AtomicBool::new(false),
            runtime_id,
            root_session_id,
            generation,
            session_mode: mode,
            nonce,
            require_nonce: spawned.require_nonce,
            request_counter: AtomicU64::new(1),
        });
        lifecycle_transition(&shared, LifecycleState::Handshaking)?;

        let child = Arc::new(ChildState {
            child: Mutex::new(spawned.child),
            pid: spawned.pid,
        });
        let diagnostics = Arc::new(Mutex::new(Diagnostics::default()));
        let (inbox_tx, inbox_rx) = mpsc::channel();
        let inner = Arc::new(HostInner {
            shared: Arc::clone(&shared),
            child: Arc::clone(&child),
            writer: Mutex::new(Some(spawned.writer)),
            inbox: Mutex::new(inbox_rx),
            diagnostics: Arc::clone(&diagnostics),
            preflight,
            patch_path,
            shutdown: Mutex::new(None),
            shutdown_lock: Mutex::new(()),
            hello: Mutex::new(None),
            options: options.clone(),
        });

        if let Some(stdout) = spawned.stdout {
            spawn_diagnostics(stdout, Arc::clone(&diagnostics));
        }
        if let Some(stderr) = spawned.stderr {
            spawn_diagnostics(stderr, Arc::clone(&diagnostics));
        }
        spawn_reader(spawned.reader, Arc::clone(&shared), inbox_tx);
        spawn_waiter(Arc::clone(&shared), Arc::clone(&child));

        Ok(Self { inner })
    }

    /// Host process id (the dsh host, not the Rust window).
    pub fn pid(&self) -> u32 {
        self.inner.child.pid
    }

    /// Resolved paths for this launch.
    pub fn paths(&self) -> &RuntimePaths {
        &self.inner.preflight.paths
    }

    /// Last diagnostics lines captured from the host's stdout/stderr.
    pub fn diagnostics_tail(&self, count: usize) -> Vec<String> {
        self.inner
            .diagnostics
            .lock()
            .expect("diagnostics mutex poisoned")
            .tail(count)
    }

    /// Current lifecycle state.
    pub fn lifecycle_state(&self) -> LifecycleState {
        self.inner.shared.lifecycle.state()
    }

    /// Wait for and validate `nano.hello`.
    pub fn handshake(&self, timeout: Option<Duration>) -> Result<Hello, HostError> {
        let timeout = timeout.unwrap_or_else(|| {
            self.inner
                .options
                .handshake_timeout
                .unwrap_or(Duration::from_secs(60))
        });
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(self.fail_handshake(HostError::Timeout(format!(
                    "handshake did not complete; diagnostics: {}",
                    self.diagnostics_tail(5).join(" | ")
                ))));
            }
            let event = self.next_event(Some(remaining))?;
            match event {
                HostEvent::Hello(hello) => {
                    hello
                        .validate(&self.inner.shared.runtime_id, self.inner.shared.generation)
                        .map_err(|error| self.fail_handshake(error.into()))?;
                    if hello.root_session_id != self.inner.shared.root_session_id {
                        return Err(self.fail_handshake(HostError::Handshake(format!(
                            "rootSessionId mismatch: expected {}, received {}",
                            self.inner.shared.root_session_id, hello.root_session_id
                        ))));
                    }
                    let expected_mode = match self.inner.shared.session_mode {
                        SessionMode::Create => SessionFactMode::Created,
                        SessionMode::Resume => SessionFactMode::Resumed,
                    };
                    if hello.session.mode != expected_mode {
                        return Err(self.fail_handshake(HostError::Handshake(format!(
                            "session mode mismatch: expected {:?}, received {:?}",
                            expected_mode, hello.session.mode
                        ))));
                    }
                    self.inner.shared.lifecycle.transition(LifecycleState::Ready)?;
                    *self.inner.hello.lock().expect("hello mutex poisoned") = Some(hello.clone());
                    return Ok(hello);
                }
                HostEvent::StartupError(params) => {
                    self.inner
                        .shared
                        .lifecycle
                        .transition_if_allowed(LifecycleState::Failed);
                    self.wait_for_exit(Duration::from_secs(5));
                    let final_state = if self.inner.shared.exited.load(Ordering::SeqCst) {
                        LifecycleState::Stopped
                    } else {
                        LifecycleState::CleanupFailed
                    };
                    let lifecycle = &self.inner.shared.lifecycle;
                    lifecycle.transition_if_allowed(LifecycleState::Stopping);
                    lifecycle.transition_if_allowed(final_state);
                    return Err(HostError::Startup {
                        code: params.code,
                        message: params.message,
                        holder_pid: params.holder_pid,
                        repair_path: params.repair_path,
                    });
                }
                HostEvent::TransportClosed { lost } => {
                    return Err(if lost {
                        HostError::ProcessGone
                    } else {
                        HostError::TransportClosed
                    });
                }
                HostEvent::ProtocolFailure(message) => {
                    return Err(self.fail_handshake(HostError::Handshake(message)));
                }
                other => {
                    return Err(self.fail_handshake(HostError::Handshake(format!(
                        "unexpected frame before hello: {other:?}"
                    ))));
                }
            }
        }
    }

    /// Subscribe to durable session events for the exact root address.
    pub fn subscribe(
        &self,
        address: Address,
        kinds: Option<Vec<String>>,
        from_seq: Option<u64>,
    ) -> Result<SubscribeResult, HostError> {
        let mut params = json!({ "address": address });
        if let Some(kinds) = kinds {
            params["kinds"] = json!(kinds);
        }
        if let Some(from_seq) = from_seq {
            params["fromSeq"] = json!(from_seq);
        }
        let value = self.request("nano.subscribe", params)?;
        serde_json::from_value(value)
            .map_err(|error| HostError::Protocol(ProtocolError::InvalidFrame(error.to_string())))
    }

    /// Cancel one subscription.
    pub fn unsubscribe(&self, subscription_id: &str) -> Result<(), HostError> {
        self.request("nano.unsubscribe", json!({ "subscriptionId": subscription_id }))?;
        Ok(())
    }

    /// Ask the host to cancel one in-flight request.
    pub fn cancel_request(&self, request_id: &str) -> Result<CancelResult, HostError> {
        let value = self.request("nano.cancelRequest", json!({ "id": request_id }))?;
        serde_json::from_value(value)
            .map_err(|error| HostError::Protocol(ProtocolError::InvalidFrame(error.to_string())))
    }

    /// Send one request and wait for its response.
    pub fn request(&self, method: &str, params: Value) -> Result<Value, HostError> {
        let timeout = self
            .inner
            .options
            .request_timeout
            .unwrap_or(Duration::from_secs(30));
        self.request_internal(method, params, timeout, false)
    }

    /// Receive the next host event.
    pub fn next_event(&self, timeout: Option<Duration>) -> Result<HostEvent, HostError> {
        let receiver = self.inner.inbox.lock().expect("inbox mutex poisoned");
        match timeout {
            Some(timeout) => match receiver.recv_timeout(timeout) {
                Ok(event) => Ok(event),
                Err(mpsc::RecvTimeoutError::Timeout) => Err(HostError::Timeout("no host event arrived".into())),
                Err(mpsc::RecvTimeoutError::Disconnected) => Err(HostError::TransportClosed),
            },
            None => receiver.recv().map_err(|_| HostError::TransportClosed),
        }
    }

    /// Close the protocol transport without signalling the host. The host sees
    /// EOF and runs its own cleanup; used to test the crash path.
    pub fn close_transport(&self) {
        *self.inner.writer.lock().expect("writer mutex poisoned") = None;
    }

    /// Send a signal to this host's process group only.
    pub fn terminate_group(&self, signal: i32) {
        #[cfg(unix)]
        {
            let pid = self.inner.child.pid as libc::pid_t;
            if pid > 0 {
                unsafe {
                    libc::kill(-pid, signal);
                }
            }
        }
    }

    /// Run shutdown once and return the four-fact outcome.
    pub fn shutdown(&self) -> ShutdownOutcome {
        let _guard = self.inner.shutdown_lock.lock().expect("shutdown lock poisoned");
        if let Some(outcome) = self.inner.shutdown.lock().expect("shutdown mutex poisoned").clone() {
            return outcome;
        }
        let state = self.inner.shared.lifecycle.state();
        if matches!(
            state,
            LifecycleState::Starting
                | LifecycleState::Handshaking
                | LifecycleState::Failed
                | LifecycleState::Disconnected
        ) {
            // No request can be acknowledged on these paths. Close the transport
            // so a surviving host runs its EOF cleanup, then use the short TERM
            // window instead of the full graceful budget.
            self.inner
                .shared
                .lifecycle
                .transition_if_allowed(LifecycleState::Stopping);
            self.close_transport();
            let forced = self.wait_for_exit_or_terminate(
                Instant::now() + Duration::from_millis(TERM_GRACE_MS),
            );
            let process_exited = self.inner.shared.exited.load(Ordering::SeqCst);
            self.inner
                .shared
                .lifecycle
                .transition_if_allowed(if process_exited && !forced {
                    LifecycleState::Stopped
                } else {
                    LifecycleState::CleanupFailed
                });
            let outcome = ShutdownOutcome {
                ack: false,
                process_exited,
                exit_code: *self.inner.shared.exit_code.lock().expect("exit code mutex poisoned"),
                forced,
                report: None,
                scope_cleared: None,
                session_saved: None,
                clean: false,
            };
            *self.inner.shutdown.lock().expect("shutdown mutex poisoned") = Some(outcome.clone());
            self.remove_patch();
            return outcome;
        }

        let deadline = Instant::now() + Duration::from_millis(GRACEFUL_SHUTDOWN_MS);
        let mut ack = false;
        let mut report = None;

        match state {
            LifecycleState::Ready => {
                self.inner
                    .shared
                    .lifecycle
                    .transition_if_allowed(LifecycleState::Stopping);
                let remaining = deadline.saturating_duration_since(Instant::now());
                match self.request_internal("nano.shutdown", json!({}), remaining, true) {
                    Ok(value) => {
                        match serde_json::from_value::<ShutdownReport>(value) {
                            Ok(parsed) => {
                                ack = true;
                                report = Some(parsed);
                            }
                            Err(error) => {
                                report = None;
                                let _ = error;
                            }
                        }
                    }
                    Err(_) => {}
                }
            }
            LifecycleState::Disconnected
            | LifecycleState::Failed
            | LifecycleState::Lost
            | LifecycleState::Starting
            | LifecycleState::Handshaking => {
                self.inner
                    .shared
                    .lifecycle
                    .transition_if_allowed(LifecycleState::Stopping);
            }
            LifecycleState::Stopping | LifecycleState::Stopped | LifecycleState::CleanupFailed => {}
        }

        let forced = self.wait_for_exit_or_terminate(deadline);
        let process_exited = self.inner.shared.exited.load(Ordering::SeqCst);
        let scope_cleared = report.as_ref().map(scope_cleared_from_report);
        let session_saved = report.as_ref().and_then(session_saved_from_report);
        let clean = ack
            && process_exited
            && report.as_ref().map(|report| report.clean).unwrap_or(false)
            && !forced;
        let final_state = if clean {
            LifecycleState::Stopped
        } else {
            LifecycleState::CleanupFailed
        };
        self.inner
            .shared
            .lifecycle
            .transition_if_allowed(final_state);

        let outcome = ShutdownOutcome {
            ack,
            process_exited,
            exit_code: *self.inner.shared.exit_code.lock().expect("exit code mutex poisoned"),
            forced,
            report,
            scope_cleared,
            session_saved,
            clean,
        };
        *self.inner.shutdown.lock().expect("shutdown mutex poisoned") = Some(outcome.clone());
        self.remove_patch();
        outcome
    }

    /// Install a signal-forwarding thread. The first SIGINT/SIGTERM/SIGHUP runs
    /// the normal shutdown path; a second forces KILL. The thread returns after a
    /// second signal, or once the shutdown outcome is cached and another signal
    /// wakes the iterator; it does not poll in between.
    pub fn forward_signals(self) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
            use signal_hook::iterator::Signals;

            let mut signals = match Signals::new([SIGINT, SIGTERM, SIGHUP]) {
                Ok(signals) => signals,
                Err(_) => return,
            };
            let mut shutdown_started = false;
            let mut shutdown_thread = None;
            loop {
                for _signal in signals.pending() {
                    if !shutdown_started {
                        shutdown_started = true;
                        let host = self.clone();
                        shutdown_thread = Some(thread::spawn(move || {
                            host.shutdown();
                        }));
                    } else {
                        self.terminate_group(libc::SIGKILL);
                        if let Some(handle) = shutdown_thread.take() {
                            let _ = handle.join();
                        }
                        return;
                    }
                }
                if shutdown_started
                    && self
                        .inner
                        .shutdown
                        .lock()
                        .expect("shutdown mutex poisoned")
                        .is_some()
                {
                    if let Some(handle) = shutdown_thread.take() {
                        let _ = handle.join();
                    }
                    return;
                }
                thread::sleep(Duration::from_millis(100));
            }
        })
    }

    /// The accepted handshake, when one has completed.
    pub fn hello(&self) -> Option<Hello> {
        self.inner.hello.lock().expect("hello mutex poisoned").clone()
    }

    fn fail_handshake(&self, error: HostError) -> HostError {
        self.inner
            .shared
            .lifecycle
            .transition_if_allowed(LifecycleState::Failed);
        self.close_transport();
        error
    }

    /// Wait until the graceful deadline, then use TERM and finally KILL. Returns
    /// true when a forced signal was needed.
    fn wait_for_exit_or_terminate(&self, deadline: Instant) -> bool {
        while !self.inner.shared.exited.load(Ordering::SeqCst) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        if self.inner.shared.exited.load(Ordering::SeqCst) {
            return false;
        }
        self.terminate_group(libc::SIGTERM);
        let term_deadline = Instant::now() + Duration::from_millis(TERM_GRACE_MS);
        while !self.inner.shared.exited.load(Ordering::SeqCst) && Instant::now() < term_deadline {
            thread::sleep(Duration::from_millis(20));
        }
        if !self.inner.shared.exited.load(Ordering::SeqCst) {
            self.terminate_group(libc::SIGKILL);
            self.wait_for_exit(Duration::from_secs(2));
        }
        true
    }

    fn request_internal(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
        allow_stopping: bool,
    ) -> Result<Value, HostError> {
        let state = self.inner.shared.lifecycle.state();
        if !(state == LifecycleState::Ready || (allow_stopping && state == LifecycleState::Stopping)) {
            return Err(match state {
                LifecycleState::Lost | LifecycleState::Stopped | LifecycleState::CleanupFailed => {
                    HostError::ProcessGone
                }
                _ => HostError::TransportClosed,
            });
        }
        let id = format!(
            "req-{}",
            self.inner
                .shared
                .request_counter
                .fetch_add(1, Ordering::SeqCst)
        );
        let (sender, receiver) = mpsc::channel();
        {
            let mut pending = self.inner.shared.pending.lock().expect("pending mutex poisoned");
            if pending.len() >= MAX_PENDING_REQUESTS {
                return Err(HostError::PendingLimit);
            }
            pending.insert(id.clone(), sender);
        }
        let encoded = match encode_request(
            &id,
            method,
            params,
            if self.inner.shared.require_nonce {
                Some(self.inner.shared.nonce.as_str())
            } else {
                None
            },
        ) {
            Ok(encoded) => encoded,
            Err(error) => {
                self.inner
                    .shared
                    .pending
                    .lock()
                    .expect("pending mutex poisoned")
                    .remove(&id);
                return Err(error.into());
            }
        };
        if let Err(error) = self.write_frame(&encoded) {
            self.inner
                .shared
                .pending
                .lock()
                .expect("pending mutex poisoned")
                .remove(&id);
            return Err(error);
        }
        match receiver.recv_timeout(timeout) {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => Err(HostError::Rpc(error)),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.inner
                    .shared
                    .pending
                    .lock()
                    .expect("pending mutex poisoned")
                    .remove(&id);
                Err(HostError::Timeout(format!("{method} did not answer")))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(HostError::TransportClosed),
        }
    }

    fn write_frame(&self, bytes: &[u8]) -> Result<(), HostError> {
        let mut writer = self.inner.writer.lock().expect("writer mutex poisoned");
        let writer = writer.as_mut().ok_or(HostError::TransportClosed)?;
        writer
            .write_all(bytes)
            .map_err(|error| {
                if error.kind() == io::ErrorKind::BrokenPipe {
                    HostError::TransportClosed
                } else {
                    HostError::Io(error)
                }
            })?;
        writer.flush().map_err(HostError::Io)
    }

    /// Wait until the host process exits or the timeout expires.
    pub fn wait_for_exit(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while !self.inner.shared.exited.load(Ordering::SeqCst) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        self.inner.shared.exited.load(Ordering::SeqCst)
    }

    fn remove_patch(&self) {
        let _ = std::fs::remove_file(&self.inner.patch_path);
    }
}

impl Drop for HostInner {
    fn drop(&mut self) {
        // Closing the transport asks the host to run its own EOF cleanup. If it
        // does not exit promptly, terminate only this host's process group.
        *self.writer.lock().expect("writer mutex poisoned") = None;
        if !self.shared.exited.load(Ordering::SeqCst) {
            #[cfg(unix)]
            unsafe {
                let pid = self.child.pid as libc::pid_t;
                if pid > 0 {
                    libc::kill(-pid, libc::SIGTERM);
                }
            }
            let deadline = Instant::now() + Duration::from_secs(2);
            while !self.shared.exited.load(Ordering::SeqCst) && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(20));
            }
        }
        if !self.shared.exited.load(Ordering::SeqCst) {
            #[cfg(unix)]
            unsafe {
                let pid = self.child.pid as libc::pid_t;
                if pid > 0 {
                    libc::kill(-pid, libc::SIGKILL);
                }
            }
            let deadline = Instant::now() + Duration::from_secs(1);
            while !self.shared.exited.load(Ordering::SeqCst) && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(20));
            }
        }
        if let Ok(mut child) = self.child.child.try_lock() {
            let _ = child.try_wait();
        }
        let _ = std::fs::remove_file(&self.patch_path);
    }
}

struct SpawnedHost {
    child: Child,
    pid: u32,
    writer: Box<dyn Write + Send>,
    reader: Box<dyn Read + Send>,
    stdout: Option<Box<dyn Read + Send>>,
    stderr: Option<Box<dyn Read + Send>>,
    require_nonce: bool,
}

fn base_command(paths: &RuntimePaths, patch_path: &Path) -> Command {
    let mut command = Command::new(&paths.node);
    command
        .arg(&paths.dsh)
        .arg("--profile")
        .arg(PROFILE_NAME)
        .arg("--patch")
        .arg(patch_path)
        .current_dir(&paths.cwd)
        .env("DSH_HOME", &paths.dsh_home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command
}

#[cfg(unix)]
fn spawn_primary(paths: &RuntimePaths, patch_path: &Path, _official: &str) -> Result<SpawnedHost, HostError> {
    use std::os::unix::io::AsRawFd;
    use std::os::unix::process::CommandExt;

    let (to_child_read, to_child_write) = os_pipe::pipe()
        .map_err(|error| HostError::Spawn(format!("could not create fd 3 pipe: {error}")))?;
    let (from_child_read, from_child_write) = os_pipe::pipe()
        .map_err(|error| HostError::Spawn(format!("could not create fd 4 pipe: {error}")))?;
    let mut command = base_command(paths, patch_path);
    command.env("NANO_TRANSPORT", "fd");
    let in_fd = to_child_read.as_raw_fd();
    let out_fd = from_child_write.as_raw_fd();
    unsafe {
        command.pre_exec(move || {
            let dup_in = libc::fcntl(in_fd, libc::F_DUPFD, 20);
            if dup_in < 0 {
                return Err(io::Error::last_os_error());
            }
            let dup_out = libc::fcntl(out_fd, libc::F_DUPFD, 20);
            if dup_out < 0 {
                libc::close(dup_in);
                return Err(io::Error::last_os_error());
            }
            let result_in = libc::dup2(dup_in, 3);
            let result_out = libc::dup2(dup_out, 4);
            libc::close(dup_in);
            libc::close(dup_out);
            if result_in < 0 || result_out < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command
        .spawn()
        .map_err(|error| HostError::Spawn(format!("could not spawn dsh with fd 3/4: {error}")))?;
    let pid = child.id();
    let stdout = child.stdout.take().map(|stdout| Box::new(stdout) as Box<dyn Read + Send>);
    let stderr = child.stderr.take().map(|stderr| Box::new(stderr) as Box<dyn Read + Send>);
    // The child-side ends are dropped here; the child keeps its dup2 copies.
    drop(to_child_read);
    drop(from_child_write);
    Ok(SpawnedHost {
        child,
        pid,
        writer: Box::new(to_child_write),
        reader: Box::new(from_child_read),
        stdout,
        stderr,
        require_nonce: false,
    })
}

#[cfg(not(unix))]
fn spawn_primary(_paths: &RuntimePaths, _patch_path: &Path, _official: &str) -> Result<SpawnedHost, HostError> {
    Err(HostError::Spawn("fd 3/4 transport is Unix-only".into()))
}

fn spawn_fallback(
    paths: &RuntimePaths,
    patch_path: &Path,
    _official: &str,
) -> Result<SpawnedHost, HostError> {
    let mut command = base_command(paths, patch_path);
    command.env("NANO_TRANSPORT", "stdio");
    command.stdin(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| HostError::Spawn(format!("could not spawn dsh with stdio fallback: {error}")))?;
    let pid = child.id();
    let stdin = child.stdin.take().ok_or_else(|| HostError::Spawn("missing child stdin".into()))?;
    let stdout = child.stdout.take().ok_or_else(|| HostError::Spawn("missing child stdout".into()))?;
    let stderr = child.stderr.take().map(|stderr| Box::new(stderr) as Box<dyn Read + Send>);
    Ok(SpawnedHost {
        child,
        pid,
        writer: Box::new(stdin),
        reader: Box::new(stdout),
        stdout: None,
        stderr,
        require_nonce: true,
    })
}

fn spawn_reader(reader: Box<dyn Read + Send>, shared: Arc<Shared>, inbox: mpsc::Sender<HostEvent>) {
    thread::spawn(move || {
        let mut frames = FrameReader::new(BufReader::new(reader));
        loop {
            let frame = match frames.read_frame() {
                Ok(Some(frame)) => frame,
                Ok(None) => break,
                Err(error) => {
                    let _ = inbox.send(HostEvent::ProtocolFailure(error.to_string()));
                    break;
                }
            };
            let rules = FrameRules {
                require_nonce: shared.require_nonce,
                nonce: shared.nonce.as_str(),
            };
            match parse_incoming(&frame, rules) {
                Ok(IncomingFrame::Response { id, result, error }) => {
                    let Some(id) = id else {
                        // A parse-error report has no request id; it cannot be
                        // correlated and is already visible in diagnostics.
                        continue;
                    };
                    let sender = shared.pending.lock().expect("pending mutex poisoned").remove(&id);
                    if let Some(sender) = sender {
                        let outcome = match error {
                            Some(error) => Err(HostRpcError {
                                code: error.code,
                                message: error.message,
                                data: error.data,
                            }),
                            None => Ok(result.unwrap_or(Value::Null)),
                        };
                        let _ = sender.send(outcome);
                    }
                }
                Ok(IncomingFrame::Notification { method, params }) => {
                    if !handle_notification(&method, params, &shared, &inbox) {
                        break;
                    }
                }
                Err(error) => {
                    let _ = inbox.send(HostEvent::ProtocolFailure(error.to_string()));
                    break;
                }
            }
            if shared.transport_ended.load(Ordering::SeqCst) {
                break;
            }
        }
        finish_transport(&shared, &inbox);
    });
}

fn handle_notification(
    method: &str,
    params: Value,
    shared: &Arc<Shared>,
    inbox: &mpsc::Sender<HostEvent>,
) -> bool {
    match method {
        "nano.hello" => match serde_json::from_value::<Hello>(params) {
            Ok(hello) => {
                shared.hello_seen.store(true, Ordering::SeqCst);
                let _ = inbox.send(HostEvent::Hello(hello));
                true
            }
            Err(error) => {
                let _ = inbox.send(HostEvent::ProtocolFailure(format!("invalid hello: {error}")));
                false
            }
        },
        "nano.startupError" => {
            if shared.hello_seen.load(Ordering::SeqCst) {
                let _ = inbox.send(HostEvent::ProtocolFailure(
                    "nano.startupError arrived after nano.hello".into(),
                ));
                return false;
            }
            match serde_json::from_value::<StartupErrorParams>(params) {
                Ok(params) => match params.validate() {
                    Ok(()) => {
                        let _ = inbox.send(HostEvent::StartupError(params));
                        true
                    }
                    Err(error) => {
                        let _ = inbox.send(HostEvent::ProtocolFailure(error.to_string()));
                        false
                    }
                },
                Err(error) => {
                    let _ = inbox.send(HostEvent::ProtocolFailure(format!(
                        "invalid startupError: {error}"
                    )));
                    false
                }
            }
        }
        "nano.event" => match serde_json::from_value::<EventParams>(params) {
            Ok(params) => match params.validate(&shared.runtime_id, shared.generation) {
                Ok(()) => {
                    let _ = inbox.send(HostEvent::Event(params));
                    true
                }
                Err(error) => {
                    let _ = inbox.send(HostEvent::ProtocolFailure(error.to_string()));
                    false
                }
            },
            Err(error) => {
                let _ = inbox.send(HostEvent::ProtocolFailure(format!("invalid event: {error}")));
                false
            }
        },
        "nano.streamEnded" => match serde_json::from_value::<StreamEndedParams>(params) {
            Ok(params) => match params.validate(&shared.runtime_id, shared.generation) {
                Ok(()) => {
                    let _ = inbox.send(HostEvent::StreamEnded(params));
                    true
                }
                Err(error) => {
                    let _ = inbox.send(HostEvent::ProtocolFailure(error.to_string()));
                    false
                }
            },
            Err(error) => {
                let _ = inbox.send(HostEvent::ProtocolFailure(format!("invalid streamEnded: {error}")));
                false
            }
        },
        other => {
            let _ = inbox.send(HostEvent::ProtocolFailure(format!(
                "unknown host notification {other}"
            )));
            false
        }
    }
}

fn finish_transport(shared: &Arc<Shared>, inbox: &mpsc::Sender<HostEvent>) {
    if shared.transport_ended.swap(true, Ordering::SeqCst) {
        return;
    }
    thread::sleep(Duration::from_millis(50));
    let lost = shared.exited.load(Ordering::SeqCst);
    let state = shared.lifecycle.state();
    match state {
        LifecycleState::Ready => {
            shared.lifecycle.transition_if_allowed(if lost {
                LifecycleState::Lost
            } else {
                LifecycleState::Disconnected
            });
        }
        LifecycleState::Handshaking | LifecycleState::Starting => {
            shared.lifecycle.transition_if_allowed(LifecycleState::Failed);
        }
        _ => {}
    }
    let mut pending = shared.pending.lock().expect("pending mutex poisoned");
    for (_, sender) in pending.drain() {
        let _ = sender.send(Err(HostRpcError {
            code: 1009,
            message: "host transport closed".into(),
            data: None,
        }));
    }
    drop(pending);
    let _ = inbox.send(HostEvent::TransportClosed { lost });
}

fn spawn_waiter(shared: Arc<Shared>, child: Arc<ChildState>) {
    thread::spawn(move || loop {
        {
            let mut guard = child.child.lock().expect("child mutex poisoned");
            match guard.try_wait() {
                Ok(Some(status)) => {
                    *shared.exit_code.lock().expect("exit code mutex poisoned") = status.code();
                    shared.exited.store(true, Ordering::SeqCst);
                    if shared.lifecycle.state() == LifecycleState::Disconnected {
                        shared
                            .lifecycle
                            .transition_if_allowed(LifecycleState::Lost);
                    }
                    break;
                }
                Ok(None) => {}
                Err(_) => break,
            }
        }
        thread::sleep(Duration::from_millis(25));
    });
}

fn spawn_diagnostics(stream: Box<dyn Read + Send>, diagnostics: Arc<Mutex<Diagnostics>>) {
    thread::spawn(move || {
        let mut reader = stream;
        let mut buffer = [0u8; 4096];
        let mut pending = Vec::new();
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    pending.extend_from_slice(&buffer[..count]);
                    while let Some(index) = pending.iter().position(|byte| *byte == b'\n') {
                        let line: Vec<u8> = pending.drain(..=index).collect();
                        let text = String::from_utf8_lossy(&line[..line.len().saturating_sub(1)]).to_string();
                        diagnostics.lock().expect("diagnostics mutex poisoned").push_line(text);
                    }
                    if pending.len() > 64 * 1024 {
                        pending.clear();
                    }
                }
                Err(_) => break,
            }
        }
        if !pending.is_empty() {
            let text = String::from_utf8_lossy(&pending).to_string();
            diagnostics.lock().expect("diagnostics mutex poisoned").push_line(text);
        }
    });
}

fn lifecycle_transition(shared: &Arc<Shared>, next: LifecycleState) -> Result<(), HostError> {
    shared.lifecycle.transition(next).map(|_| ())
}

fn scope_cleared_from_report(report: &ShutdownReport) -> bool {
    let mut saw_scope = false;
    for step in &report.steps {
        if step.name == "subscriptions" || step.name == "agents" || step.name == "dispose" {
            saw_scope = true;
            if step.status != crate::protocol::StepStatus::Stopped {
                return false;
            }
        }
    }
    saw_scope
}

fn session_saved_from_report(report: &ShutdownReport) -> Option<bool> {
    report
        .steps
        .iter()
        .find(|step| step.name == "persistence")
        .map(|step| step.status == crate::protocol::StepStatus::Stopped)
}

/// Generate a fresh runtime identity.
pub fn generate_runtime_id() -> String {
    new_runtime_id()
}

/// Generate a fresh pre-assigned root session identity.
pub fn generate_root_session_id() -> String {
    new_root_session_id()
}

/// Protocol version implemented by this crate.
pub fn protocol_version() -> ProtocolVersion {
    ProtocolVersion {
        major: crate::PROTOCOL_MAJOR,
        minor: crate::PROTOCOL_MINOR,
    }
}

#[allow(dead_code)]
fn json_object(value: Value) -> Value {
    if value.is_object() {
        value
    } else {
        json!({})
    }
}
