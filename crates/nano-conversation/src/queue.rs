//! Official inbox projection for the QueueDock.
//!
//! The queue is not a local pending list: every item carries the official
//! message id, and edits/removals are sent to the official inbox. Deleting is a
//! durable cancellation; it does not roll back a step that already started.

use serde_json::Value;

/// Which official pending list holds an item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum QueueTarget {
    /// Items consumed at the next turn boundary (`queue` / `followup`).
    NextTurn,
    /// Items consumed at the next step boundary (`steer` / `inject`).
    NextStep,
}

impl QueueTarget {
    /// User-facing label.
    pub fn label(self) -> &'static str {
        match self {
            Self::NextTurn => "待下一回合",
            Self::NextStep => "待下一步",
        }
    }
}

/// One pending item from the official inbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedItem {
    /// Official message id.
    pub id: String,
    /// Concatenated text preview.
    pub text: String,
    /// Official correlation id when the producer supplied one.
    pub rpc_id: Option<String>,
    /// Whether any non-text block is attached.
    pub has_attachment: bool,
    /// Which pending list holds it.
    pub target: QueueTarget,
}

impl QueuedItem {
    /// Build from one official message-shaped value.
    pub fn from_value(value: &Value, target: QueueTarget) -> Option<Self> {
        let id = value.get("id")?.as_str()?.to_string();
        let text = text_of(value.get("content"));
        let rpc_id = rpc_id_of(value.get("source"));
        let has_attachment = value
            .get("content")
            .and_then(Value::as_array)
            .map(|blocks| {
                blocks
                    .iter()
                    .any(|block| block.get("type").and_then(Value::as_str) != Some("text"))
            })
            .unwrap_or(false);
        Some(Self {
            id,
            text,
            rpc_id,
            has_attachment,
            target,
        })
    }

    /// Whether the item can be edited without dropping an attachment.
    pub fn editable(&self) -> bool {
        !self.has_attachment
    }
}

/// Full pending-input projection.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InboxView {
    /// Items waiting for a next turn.
    pub next_turn: Vec<QueuedItem>,
    /// Items waiting for the next step.
    pub next_step: Vec<QueuedItem>,
    /// The live event carried bounded previews; a snapshot is required before
    /// editing. The snapshot response is not truncated.
    pub truncated: bool,
    /// Total items before preview truncation; 0 means unknown/use `len`.
    pub total: usize,
    /// Items omitted from the preview.
    pub omitted: usize,
}

impl InboxView {
    /// Parse the official projection shape.
    pub fn from_value(value: &Value) -> Self {
        Self {
            next_turn: parse_list(value.get("nextTurn"), QueueTarget::NextTurn),
            next_step: parse_list(value.get("nextStep"), QueueTarget::NextStep),
            truncated: false,
            total: 0,
            omitted: 0,
        }
    }

    /// Parse a live event projection and its truncation facts.
    pub fn from_event(inbox: &Value, truncated: bool, total: usize, omitted: usize) -> Self {
        let mut view = Self::from_value(inbox);
        view.truncated = truncated;
        view.total = total;
        view.omitted = omitted;
        view
    }

    /// Whether both pending lists are empty.
    pub fn is_empty(&self) -> bool {
        self.next_turn.is_empty() && self.next_step.is_empty()
    }

    /// Number of items present in this projection.
    pub fn len(&self) -> usize {
        self.next_turn.len() + self.next_step.len()
    }

    /// Total items the official inbox holds, including omitted previews.
    pub fn total_count(&self) -> usize {
        if self.total > 0 {
            self.total
        } else {
            self.len()
        }
    }

    /// All items in official order: next turn, then next step.
    pub fn iter(&self) -> impl Iterator<Item = &QueuedItem> {
        self.next_turn.iter().chain(self.next_step.iter())
    }

    /// Find one item by official message id.
    pub fn find(&self, message_id: &str) -> Option<&QueuedItem> {
        self.iter().find(|item| item.id == message_id)
    }

    /// Find one item by official request correlation id.
    pub fn find_by_request(&self, request_id: &str) -> Option<&QueuedItem> {
        self.iter()
            .find(|item| item.rpc_id.as_deref() == Some(request_id))
    }
}

fn parse_list(value: Option<&Value>, target: QueueTarget) -> Vec<QueuedItem> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| QueuedItem::from_value(item, target))
                .collect()
        })
        .unwrap_or_default()
}

/// Concatenate text blocks; unknown blocks become an explicit placeholder
/// rather than disappearing silently.
pub fn text_of(content: Option<&Value>) -> String {
    let Some(blocks) = content.and_then(Value::as_array) else {
        return String::new();
    };
    let mut text = String::new();
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(value) = block.get("text").and_then(Value::as_str) {
                    text.push_str(value);
                }
            }
            Some("image") => text.push_str("[image]"),
            Some("file") => text.push_str("[file]"),
            Some(other) => {
                text.push('[');
                text.push_str(other);
                text.push(']');
            }
            None => text.push_str("[unknown]"),
        }
    }
    text
}

/// Read the official `user-rpc` correlation id from a message source.
pub fn rpc_id_of(source: Option<&Value>) -> Option<String> {
    let source = source?;
    if source.get("kind").and_then(Value::as_str)? != "user" {
        return None;
    }
    source
        .get("rpcId")
        .and_then(Value::as_str)
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_both_pending_lists_and_identity() {
        let inbox = InboxView::from_value(&json!({
            "nextTurn": [
                {
                    "id": "m-1",
                    "role": "user",
                    "content": [{"type": "text", "text": "queued"}],
                    "source": {"kind": "user", "rpcId": "r-1"}
                }
            ],
            "nextStep": [
                {
                    "id": "m-2",
                    "role": "user",
                    "content": [{"type": "text", "text": "steer"}],
                    "source": {"kind": "user"}
                }
            ]
        }));
        assert_eq!(inbox.len(), 2);
        assert_eq!(inbox.next_turn[0].target, QueueTarget::NextTurn);
        assert_eq!(inbox.next_step[0].target, QueueTarget::NextStep);
        assert_eq!(inbox.find_by_request("r-1").unwrap().id, "m-1");
        assert!(inbox.find("missing").is_none());
    }

    #[test]
    fn attachments_mark_an_item_uneditable() {
        let item = QueuedItem::from_value(
            &json!({
                "id": "m-1",
                "content": [{"type": "text", "text": "x"}, {"type": "image", "attachment": {}}],
                "source": {"kind": "user"}
            }),
            QueueTarget::NextTurn,
        )
        .unwrap();
        assert!(item.has_attachment);
        assert!(!item.editable());
        assert_eq!(item.text, "x[image]");
    }
}
