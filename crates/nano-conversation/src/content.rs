//! Ordered input content.
//!
//! The user's content is an ordered list of text and (from S06) attachment
//! parts. S02 preserves order and the submission boundary; it does not upload
//! or resolve attachments. A submission with an unresolved attachment is
//! refused rather than half-sent.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// One ordered part of a submission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ContentPart {
    /// Literal user text, preserved byte-for-byte.
    Text {
        /// Original text; control characters are not rewritten here.
        text: String,
    },
    /// An attachment whose upload receipt belongs to S06.
    Attachment {
        /// S06 receipt identity, not a path.
        attachment_id: String,
    },
}

/// Why an ordered content list cannot become a wire submission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentError {
    /// Nothing but whitespace (and no attachment) is present.
    Empty,
    /// An attachment has not completed its S06 upload receipt.
    AttachmentNotReady,
}

impl std::fmt::Display for ContentError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => formatter.write_str("content needs non-whitespace text"),
            Self::AttachmentNotReady => {
                formatter.write_str("an attachment has not finished uploading")
            }
        }
    }
}

/// Ordered content for one submission or draft.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OrderedContent {
    parts: Vec<ContentPart>,
}

impl OrderedContent {
    /// Empty content list.
    pub fn new() -> Self {
        Self::default()
    }

    /// Build from already-ordered parts.
    pub fn from_parts(parts: Vec<ContentPart>) -> Self {
        Self { parts }
    }

    /// Append one text part.
    pub fn push_text(&mut self, text: impl Into<String>) {
        self.parts.push(ContentPart::Text { text: text.into() });
    }

    /// Append one attachment placeholder.
    pub fn push_attachment(&mut self, attachment_id: impl Into<String>) {
        self.parts.push(ContentPart::Attachment {
            attachment_id: attachment_id.into(),
        });
    }

    /// Ordered parts.
    pub fn parts(&self) -> &[ContentPart] {
        &self.parts
    }

    /// Whether the list holds no parts.
    pub fn is_empty(&self) -> bool {
        self.parts.is_empty()
    }

    /// Whether any part is an attachment.
    pub fn has_attachment(&self) -> bool {
        self.parts
            .iter()
            .any(|part| matches!(part, ContentPart::Attachment { .. }))
    }

    /// Whether the list carries non-whitespace text.
    pub fn has_text(&self) -> bool {
        self.parts.iter().any(|part| match part {
            ContentPart::Text { text } => text.chars().any(|character| !character.is_whitespace()),
            ContentPart::Attachment { .. } => false,
        })
    }

    /// UTF-8 byte length of all text parts.
    pub fn text_bytes(&self) -> usize {
        self.parts
            .iter()
            .map(|part| match part {
                ContentPart::Text { text } => text.len(),
                ContentPart::Attachment { .. } => 0,
            })
            .sum()
    }

    /// Whether both text and attachment parts are present.
    pub fn mixed(&self) -> bool {
        self.has_attachment() && self.has_text()
    }

    /// Plain concatenation for queue previews and diagnostics.
    pub fn text(&self) -> String {
        let mut text = String::new();
        for part in &self.parts {
            match part {
                ContentPart::Text { text: value } => text.push_str(value),
                ContentPart::Attachment { attachment_id } => {
                    text.push_str("[attachment ");
                    text.push_str(attachment_id);
                    text.push(']');
                }
            }
        }
        text
    }

    /// Convert to the wire `content` array for a text-only submission.
    pub fn to_wire_text_blocks(&self) -> Result<Vec<Value>, ContentError> {
        if self.has_attachment() {
            return Err(ContentError::AttachmentNotReady);
        }
        if !self.has_text() {
            return Err(ContentError::Empty);
        }
        Ok(self
            .parts
            .iter()
            .filter_map(|part| match part {
                ContentPart::Text { text } => Some(json!({ "type": "text", "text": text })),
                ContentPart::Attachment { .. } => None,
            })
            .collect())
    }

    /// Convert a single text value into ordered content.
    pub fn from_text(text: impl Into<String>) -> Self {
        let mut content = Self::new();
        content.push_text(text);
        content
    }
}
