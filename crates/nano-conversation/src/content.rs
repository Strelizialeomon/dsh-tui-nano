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
    /// A ready image attachment whose staged bytes belong to S06.
    Image {
        /// S06 receipt identity, not a path.
        receipt_id: String,
        /// Display name, stripped of local path information.
        name: String,
        /// Declared source media type.
        media_type: String,
    },
    /// A ready generic-file attachment whose durable receipt belongs to S06.
    File {
        /// S06 receipt identity, not a path.
        receipt_id: String,
        /// Display name, stripped of local path information.
        name: String,
    },
    /// An attachment placeholder that has not completed its S06 upload.
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

    /// Append one ready image attachment.
    pub fn push_image(
        &mut self,
        receipt_id: impl Into<String>,
        name: impl Into<String>,
        media_type: impl Into<String>,
    ) {
        self.parts.push(ContentPart::Image {
            receipt_id: receipt_id.into(),
            name: name.into(),
            media_type: media_type.into(),
        });
    }

    /// Append one ready generic-file attachment.
    pub fn push_file(&mut self, receipt_id: impl Into<String>, name: impl Into<String>) {
        self.parts.push(ContentPart::File {
            receipt_id: receipt_id.into(),
            name: name.into(),
        });
    }

    /// Append one not-ready attachment placeholder.
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
            .any(|part| !matches!(part, ContentPart::Text { .. }))
    }

    /// Whether the list carries non-whitespace text.
    pub fn has_text(&self) -> bool {
        self.parts.iter().any(|part| match part {
            ContentPart::Text { text } => text.chars().any(|character| !character.is_whitespace()),
            _ => false,
        })
    }

    /// UTF-8 byte length of all text parts.
    pub fn text_bytes(&self) -> usize {
        self.parts
            .iter()
            .map(|part| match part {
                ContentPart::Text { text } => text.len(),
                _ => 0,
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
                ContentPart::Image { name, .. } => {
                    text.push_str("[图片 ");
                    text.push_str(name);
                    text.push(']');
                }
                ContentPart::File { name, .. } => {
                    text.push_str("[文件 ");
                    text.push_str(name);
                    text.push(']');
                }
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
                _ => None,
            })
            .collect())
    }

    /// Convert to the wire `content` array for a submission with attachments.
    /// A not-yet-ready placeholder refuses the whole submission.
    pub fn to_wire_blocks(&self) -> Result<Vec<Value>, ContentError> {
        if !self.has_text() && !self.has_attachment() {
            return Err(ContentError::Empty);
        }
        let mut blocks = Vec::with_capacity(self.parts.len());
        for part in &self.parts {
            match part {
                ContentPart::Text { text } => {
                    if !text.is_empty() {
                        blocks.push(json!({ "type": "text", "text": text }));
                    }
                }
                ContentPart::Image { receipt_id, .. } => {
                    blocks.push(json!({ "type": "image", "receiptId": receipt_id }));
                }
                ContentPart::File { receipt_id, .. } => {
                    blocks.push(json!({ "type": "file", "receiptId": receipt_id }));
                }
                ContentPart::Attachment { .. } => return Err(ContentError::AttachmentNotReady),
            }
        }
        if blocks.is_empty() {
            return Err(ContentError::Empty);
        }
        Ok(blocks)
    }

    /// Convert a single text value into ordered content.
    pub fn from_text(text: impl Into<String>) -> Self {
        let mut content = Self::new();
        content.push_text(text);
        content
    }
}
