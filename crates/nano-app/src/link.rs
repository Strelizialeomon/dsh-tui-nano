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
    /// Sanitized description.
    pub message: String,
}

/// One message from the background link.
#[derive(Debug)]
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
}

/// Shared handle to the host worker threads.
pub struct HostLink {
    /// Incoming link events.
    pub events: Receiver<LinkEvent>,
    /// Outgoing request channel.
    commands: Sender<(u64, String, Value)>,
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
        let stop = Arc::new(AtomicBool::new(false));

        let startup_host = host.clone();
        let startup_events = event_tx.clone();
        let startup_stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            let hello = match startup_host.handshake(None) {
                Ok(hello) => hello,
                Err(error) => {
                    let _ = startup_events.send(LinkEvent::Fatal(describe_host_error(&error)));
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
            let kinds = vec![
                "session.event".to_string(),
                "conversation.status".to_string(),
                "conversation.inbox".to_string(),
                "conversation.stream".to_string(),
                "skills.changed".to_string(),
                "instructions.changed".to_string(),
            ];
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
        let command_events = event_tx;
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

        Ok(Self {
            events: event_rx,
            commands: command_tx,
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
    RequestError {
        kind,
        message: describe_host_error(&error),
    }
}
