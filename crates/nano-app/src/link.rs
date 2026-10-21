//! Background link to one host generation.
//!
//! Startup, subscription, event delivery, and requests run off the UI thread so
//! a slow model request never blocks input or drawing.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use nano_runtime::paths::new_runtime_id;
use nano_runtime::protocol::Address;
use nano_runtime::{
    Hello, Host, HostError, HostEvent, HostOptions, SessionMode, MAX_REPLAY_EVENTS,
};
use serde_json::Value;

/// Every event kind this window subscribes to, in one place so the initial
/// subscription and a later rebuild cannot drift apart.
pub const SUBSCRIPTION_KINDS: [&str; 9] = [
    "session.event",
    "conversation.status",
    "conversation.inbox",
    "conversation.stream",
    "controls.model",
    "controls.permission",
    "sessions.context",
    "skills.changed",
    "instructions.changed",
];

/// How a request failed, for deciding whether a result is explicitly failed or
/// merely unknown. Only an RPC error or a local pre-send rejection is a fact;
/// a transport failure may have delivered the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestErrorKind {
    /// The host answered with a JSON-RPC error.
    Rpc,
    /// The request never left this process.
    Local,
    /// The request may or may not have reached the host.
    Unknown,
}

/// One request failure.
#[derive(Debug, Clone)]
pub struct RequestError {
    /// Classification used by the submission state machine.
    pub kind: RequestErrorKind,
    /// JSON-RPC error code, when the host answered with one.
    pub code: Option<i64>,
    /// Sanitized description.
    pub message: String,
}

/// One message from the background link.
pub enum LinkEvent {
    /// Handshake and subscription succeeded.
    Ready {
        /// Accepted handshake.
        hello: Hello,
        /// The replay window could not cover the whole durable log.
        truncated: bool,
    },
    /// A host notification or transport fact.
    Host(HostEvent),
    /// One request finished.
    RequestDone {
        /// Caller-generated request id.
        id: u64,
        /// Result or a classified error.
        result: Result<Value, RequestError>,
    },
    /// The link cannot continue.
    Fatal(String),
    /// The host exited before the handshake with a structured startup error.
    StartupFailed {
        /// Contract startup code (`occupied`, `startup-failed`, ...).
        code: String,
        /// Sanitized message from the host.
        message: String,
        /// Holder pid for a ledger `occupied` rejection.
        holder_pid: Option<u32>,
        /// Repair path for a damaged ledger.
        repair_path: Option<String>,
    },
    /// A controlled restart spawned a new host; its handshake is still pending.
    Switched {
        /// The new generation's link.
        link: HostLink,
    },
    /// A controlled restart could not spawn the new host.
    SwitchFailed {
        /// Sanitized diagnostic.
        message: String,
        /// Startup code when the failure came from the new host.
        code: Option<String>,
        /// Holder pid for an `occupied` rejection.
        holder_pid: Option<u32>,
    },
}

/// Shared handle to the host worker threads.
pub struct HostLink {
    /// Incoming link events.
    pub events: Receiver<LinkEvent>,
    /// Outgoing request channel.
    commands: Sender<(u64, String, Value)>,
    /// Outgoing long-request channel (manual compaction).
    long_commands: Sender<(u64, String, Value, u64)>,
    /// The owning host handle, used for the final ordered shutdown.
    host: Host,
    /// Stops the event loop thread.
    stop: Arc<AtomicBool>,
    counter: AtomicU64,
}

impl HostLink {
    /// Spawn the host, handshake, subscribe, and start both workers.
    pub fn start(
        root_session_id: &str,
        mode: SessionMode,
        options: &HostOptions,
    ) -> Result<Self, String> {
        let host = Host::spawn(new_runtime_id(), 1, root_session_id, mode, options)
            .map_err(|error| format!("could not spawn the host: {error}"))?;
        let (event_tx, event_rx) = channel();
        let (command_tx, command_rx) = channel::<(u64, String, Value)>();
        let (long_tx, long_rx) = channel::<(u64, String, Value, u64)>();
        let stop = Arc::new(AtomicBool::new(false));

        let startup_host = host.clone();
        let startup_events = event_tx.clone();
        let startup_stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            let hello = match startup_host.handshake(None) {
                Ok(hello) => hello,
                Err(error) => {
                    let _ = startup_events.send(startup_failure_event(&error));
                    return;
                }
            };
            let seq = hello.session.seq;
            let truncated = seq > MAX_REPLAY_EVENTS;
            let from_seq = if truncated {
                seq - MAX_REPLAY_EVENTS
            } else {
                0
            };
            let address = Address {
                root_session_id: hello.root_session_id.clone(),
                child_session_id: None,
            };
            let kinds = SUBSCRIPTION_KINDS
                .iter()
                .map(|kind| kind.to_string())
                .collect();
            let _subscription = match startup_host.subscribe(address, Some(kinds), Some(from_seq)) {
                Ok(subscription) => subscription,
                Err(error) => {
                    let _ = startup_events.send(LinkEvent::Fatal(format!(
                        "could not subscribe: {}",
                        describe_host_error(&error)
                    )));
                    return;
                }
            };
            let _ = startup_events.send(LinkEvent::Ready { hello, truncated });
            while !startup_stop.load(Ordering::SeqCst) {
                match startup_host.next_event(Some(Duration::from_millis(200))) {
                    Ok(event) => {
                        let closed = matches!(event, HostEvent::TransportClosed { .. });
                        if startup_events.send(LinkEvent::Host(event)).is_err() {
                            return;
                        }
                        if closed {
                            return;
                        }
                    }
                    Err(HostError::Timeout(_)) => {}
                    Err(error) => {
                        let _ = startup_events.send(LinkEvent::Fatal(describe_host_error(&error)));
                        return;
                    }
                }
            }
        });

        let command_host = host.clone();
        let command_events = event_tx.clone();
        std::thread::spawn(move || {
            while let Ok((id, method, params)) = command_rx.recv() {
                let result = command_host
                    .request(&method, params)
                    .map_err(classify_request_error);
                if command_events
                    .send(LinkEvent::RequestDone { id, result })
                    .is_err()
                {
                    return;
                }
            }
        });

        // A long action such as manual compaction must not block the ordinary
        // request worker: prompts submitted while it runs still need to reach
        // the official inbox.
        let long_host = host.clone();
        std::thread::spawn(move || {
            while let Ok((id, method, params, timeout_ms)) = long_rx.recv() {
                let result = long_host
                    .request_with_timeout(&method, params, Duration::from_millis(timeout_ms))
                    .map_err(classify_request_error);
                if event_tx
                    .send(LinkEvent::RequestDone { id, result })
                    .is_err()
                {
                    return;
                }
            }
        });

        Ok(Self {
            events: event_rx,
            commands: command_tx,
            long_commands: long_tx,
            host,
            stop,
            counter: AtomicU64::new(1),
        })
    }

    /// Send one request; returns the local id used in `RequestDone`.
    pub fn request(&self, method: &str, params: Value) -> Result<u64, String> {
        let id = self.counter.fetch_add(1, Ordering::SeqCst);
        self.commands
            .send((id, method.to_string(), params))
            .map_err(|_| "the host request worker is gone".to_string())?;
        Ok(id)
    }

    /// Send one request on the long-action worker with an explicit budget.
    /// Manual compaction waits here so ordinary requests stay responsive.
    pub fn request_long(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<u64, String> {
        let id = self.counter.fetch_add(1, Ordering::SeqCst);
        self.long_commands
            .send((id, method.to_string(), params, timeout.as_millis() as u64))
            .map_err(|_| "the host long-request worker is gone".to_string())?;
        Ok(id)
    }

    /// Run the one ordered shutdown after the terminal is restored.
    pub fn shutdown(&self) -> nano_runtime::ShutdownOutcome {
        self.stop.store(true, Ordering::SeqCst);
        self.host.shutdown()
    }

    /// Host diagnostics, for the status line on startup failure.
    pub fn diagnostics(&self, count: usize) -> Vec<String> {
        self.host.diagnostics_tail(count)
    }
}

fn describe_host_error(error: &HostError) -> String {
    format!("{error}")
}

/// Keep the structured startup facts the UI needs (holder pid, repair path)
/// instead of flattening every pre-hello failure into a diagnostic string.
fn startup_failure_event(error: &HostError) -> LinkEvent {
    match error {
        HostError::Startup {
            code,
            message,
            holder_pid,
            repair_path,
        } => LinkEvent::StartupFailed {
            code: code.clone(),
            message: message.clone(),
            holder_pid: *holder_pid,
            repair_path: repair_path.clone(),
        },
        other => LinkEvent::Fatal(describe_host_error(other)),
    }
}

/// Classify a request failure without losing the fact that it may have been
/// delivered. The app must not restore the draft or claim failure for Unknown.
fn classify_request_error(error: HostError) -> RequestError {
    let kind = match &error {
        HostError::Rpc(_) => RequestErrorKind::Rpc,
        HostError::PendingLimit
        | HostError::Protocol(_)
        | HostError::Lifecycle(_)
        | HostError::UnsupportedVersion(_)
        | HostError::Startup { .. }
        | HostError::Handshake(_) => RequestErrorKind::Local,
        HostError::Timeout(_) | HostError::TransportClosed | HostError::ProcessGone => {
            RequestErrorKind::Unknown
        }
        HostError::Io(_) | HostError::Spawn(_) => RequestErrorKind::Unknown,
    };
    let code = match &error {
        HostError::Rpc(body) => Some(body.code),
        _ => None,
    };
    RequestError {
        kind,
        code,
        message: describe_host_error(&error),
    }
}
