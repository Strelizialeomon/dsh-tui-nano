//! User-invocation facts for `/name`.
//!
//! The official host decides whether a claimed `/name` token injects a skill
//! body; the client must not invent an error. This tracker only reports the two
//! observable outcomes: a durable `skill-invocation` message, or a turn that
//! ended without one.

use serde_json::Value;

/// One observed invocation outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvocationFact {
    /// The official host injected the named skill body.
    Injected(String),
    /// The turn settled without an injection; the line passed as prose.
    NotInjected(String),
}

/// Internal transition decided from one event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Arm,
    Injected,
    NotInjected,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Pending {
    /// Submitted, waiting for the durable user message.
    Waiting { name: String, request_id: String },
    /// The durable user message exists; the pre-step decision is next.
    Armed { name: String, request_id: String },
}

/// Tracks one submitted `/name` line through its turn.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InvocationTracker {
    pending: Option<Pending>,
}

impl InvocationTracker {
    /// Empty tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Remember a submitted slash line. The next submission replaces any
    /// unresolved one; two `/name` lines in flight cannot be told apart by the
    /// official message alone.
    pub fn note_submission(&mut self, name: impl Into<String>, request_id: impl Into<String>) {
        self.pending = Some(Pending::Waiting {
            name: name.into(),
            request_id: request_id.into(),
        });
    }

    /// Forget the pending fact, for example after an explicit submit failure.
    pub fn clear(&mut self) {
        self.pending = None;
    }

    /// Observe one durable session event. Returns a fact only for the submitted
    /// line's own user message, its `skill-invocation` injection, or the
    /// `turn/end` that closes the turn without an injection.
    pub fn observe(&mut self, event_type: &str, data: &Value) -> Option<InvocationFact> {
        let action = match event_type {
            "user/message" => {
                let source = data.get("source");
                let kind = source
                    .and_then(|value| value.get("kind"))
                    .and_then(Value::as_str);
                match (self.pending.as_ref(), kind) {
                    (Some(Pending::Waiting { request_id, .. }), Some("user")) => {
                        let rpc_id = source
                            .and_then(|value| value.get("rpcId"))
                            .and_then(Value::as_str);
                        (rpc_id == Some(request_id.as_str())).then_some(Action::Arm)
                    }
                    (Some(Pending::Armed { name, .. }), Some("skill-invocation")) => {
                        let injected = source
                            .and_then(|value| value.get("name"))
                            .and_then(Value::as_str);
                        (injected == Some(name.as_str())).then_some(Action::Injected)
                    }
                    _ => None,
                }
            }
            "turn/end" => {
                matches!(self.pending, Some(Pending::Armed { .. })).then_some(Action::NotInjected)
            }
            _ => None,
        };
        match action? {
            Action::Arm => {
                if let Some(Pending::Waiting { name, request_id }) = self.pending.take() {
                    self.pending = Some(Pending::Armed { name, request_id });
                }
                None
            }
            Action::Injected => match self.pending.take() {
                Some(Pending::Armed { name, .. }) => Some(InvocationFact::Injected(name)),
                _ => None,
            },
            Action::NotInjected => match self.pending.take() {
                Some(Pending::Armed { name, .. }) => Some(InvocationFact::NotInjected(name)),
                _ => None,
            },
        }
    }

    /// Whether a submission is still unresolved.
    pub fn pending(&self) -> Option<&str> {
        match &self.pending {
            Some(Pending::Waiting { name, .. }) | Some(Pending::Armed { name, .. }) => Some(name),
            None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn user_message(rpc_id: &str) -> Value {
        json!({"source": {"kind": "user", "rpcId": rpc_id}, "content": []})
    }

    #[test]
    fn a_durable_user_message_arms_the_tracker_and_injection_settles_it() {
        let mut tracker = InvocationTracker::new();
        tracker.note_submission("hello", "req-1");
        assert_eq!(
            tracker.observe("user/message", &user_message("other")),
            None
        );
        assert_eq!(
            tracker.observe("user/message", &user_message("req-1")),
            None
        );
        assert_eq!(tracker.pending(), Some("hello"));
        let fact = tracker.observe(
            "user/message",
            &json!({"source": {"kind": "skill-invocation", "name": "hello"}}),
        );
        assert_eq!(fact, Some(InvocationFact::Injected("hello".into())));
        assert_eq!(tracker.pending(), None);
    }

    #[test]
    fn a_turn_end_without_injection_reports_the_prose_fallback() {
        let mut tracker = InvocationTracker::new();
        tracker.note_submission("gone", "req-2");
        tracker.observe("user/message", &user_message("req-2"));
        let fact = tracker.observe(
            "turn/end",
            &json!({"turn": 1, "reason": {"kind": "completed"}}),
        );
        assert_eq!(fact, Some(InvocationFact::NotInjected("gone".into())));
        assert_eq!(tracker.pending(), None);
    }

    #[test]
    fn a_queued_submission_is_not_cleared_by_an_unrelated_turn_end() {
        let mut tracker = InvocationTracker::new();
        tracker.note_submission("queued", "req-3");
        assert_eq!(tracker.observe("turn/end", &json!({"turn": 1})), None);
        assert_eq!(tracker.pending(), Some("queued"));
    }

    #[test]
    fn an_injection_for_another_name_is_ignored() {
        let mut tracker = InvocationTracker::new();
        tracker.note_submission("hello", "req-4");
        tracker.observe("user/message", &user_message("req-4"));
        assert_eq!(
            tracker.observe(
                "user/message",
                &json!({"source": {"kind": "skill-invocation", "name": "other"}}),
            ),
            None
        );
        assert_eq!(tracker.pending(), Some("hello"));
    }
}
