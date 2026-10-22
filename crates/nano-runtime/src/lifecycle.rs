//! Client lifecycle state machine.
//!
//! The table is mirrored from `protocol/nano-protocol.json`; the contract test
//! compares every edge in both directions.

use std::sync::Mutex;

use crate::protocol_error::HostError;
use crate::LIFECYCLE_TRANSITIONS;

/// One lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleState {
    /// No host is running.
    Stopped,
    /// A host process was spawned.
    Starting,
    /// The transport is open and the handshake is awaited.
    Handshaking,
    /// `nano.hello` was accepted.
    Ready,
    /// Transport broke while the host process is still alive.
    Disconnected,
    /// A shutdown is in progress.
    Stopping,
    /// Startup failed.
    Failed,
    /// The host process died unexpectedly.
    Lost,
    /// Shutdown finished but at least one cleanup fact is unknown or failed.
    CleanupFailed,
}

impl LifecycleState {
    /// Contract spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::Starting => "starting",
            Self::Handshaking => "handshaking",
            Self::Ready => "ready",
            Self::Disconnected => "disconnected",
            Self::Stopping => "stopping",
            Self::Failed => "failed",
            Self::Lost => "lost",
            Self::CleanupFailed => "cleanup-failed",
        }
    }

    /// Parse a contract spelling.
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "stopped" => Self::Stopped,
            "starting" => Self::Starting,
            "handshaking" => Self::Handshaking,
            "ready" => Self::Ready,
            "disconnected" => Self::Disconnected,
            "stopping" => Self::Stopping,
            "failed" => Self::Failed,
            "lost" => Self::Lost,
            "cleanup-failed" => Self::CleanupFailed,
            _ => return None,
        })
    }
}

/// Whether the contract permits `from -> to`.
pub fn is_allowed(from: LifecycleState, to: LifecycleState) -> bool {
    LIFECYCLE_TRANSITIONS
        .iter()
        .any(|(edge_from, edge_to)| *edge_from == from.as_str() && *edge_to == to.as_str())
}

/// Thread-safe holder for one lifecycle state.
#[derive(Debug)]
pub struct Lifecycle {
    state: Mutex<LifecycleState>,
}

impl Default for Lifecycle {
    fn default() -> Self {
        Self::new()
    }
}

impl Lifecycle {
    /// Start in `stopped`.
    pub fn new() -> Self {
        Self {
            state: Mutex::new(LifecycleState::Stopped),
        }
    }

    /// Current state.
    pub fn state(&self) -> LifecycleState {
        *self.state.lock().expect("lifecycle mutex poisoned")
    }

    /// Apply a contract transition, or reject it.
    pub fn transition(&self, next: LifecycleState) -> Result<LifecycleState, HostError> {
        let mut state = self.state.lock().expect("lifecycle mutex poisoned");
        if !is_allowed(*state, next) {
            return Err(HostError::Lifecycle(format!(
                "{} -> {}",
                state.as_str(),
                next.as_str()
            )));
        }
        *state = next;
        Ok(next)
    }

    /// Transition if the edge exists; return the current state otherwise.
    ///
    /// Used by transport-failure paths that must not turn a stopping host back
    /// into a ready or failed one.
    pub fn transition_if_allowed(&self, next: LifecycleState) -> LifecycleState {
        let mut state = self.state.lock().expect("lifecycle mutex poisoned");
        if is_allowed(*state, next) {
            *state = next;
        }
        *state
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LIFECYCLE_STATES;

    #[test]
    fn every_contract_edge_is_allowed() {
        for (from, to) in LIFECYCLE_TRANSITIONS {
            let from = LifecycleState::parse(from).unwrap();
            let to = LifecycleState::parse(to).unwrap();
            assert!(is_allowed(from, to), "{} -> {}", from.as_str(), to.as_str());
        }
    }

    #[test]
    fn no_extra_edges() {
        for from in LIFECYCLE_STATES {
            for to in LIFECYCLE_STATES {
                let from = LifecycleState::parse(from).unwrap();
                let to = LifecycleState::parse(to).unwrap();
                let expected = LIFECYCLE_TRANSITIONS.iter().any(|(edge_from, edge_to)| {
                    *edge_from == from.as_str() && *edge_to == to.as_str()
                });
                assert_eq!(is_allowed(from, to), expected);
            }
        }
    }

    #[test]
    fn rejects_undefined_transitions() {
        let lifecycle = Lifecycle::new();
        assert!(lifecycle.transition(LifecycleState::Ready).is_err());
        lifecycle.transition(LifecycleState::Starting).unwrap();
        lifecycle.transition(LifecycleState::Handshaking).unwrap();
        lifecycle.transition(LifecycleState::Ready).unwrap();
        assert!(lifecycle.transition(LifecycleState::Stopped).is_err());
    }
}
