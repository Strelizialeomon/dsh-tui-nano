//! Ordered attachment cards attached to one draft.
//!
//! The text buffer stays the user's original string. Each attachment records a
//! byte offset in that buffer; submission splits the text at those offsets so
//! text and attachments keep the order the user actually produced. Text edits
//! shift the offsets without rewriting the cards.

use std::collections::BTreeMap;

use nano_conversation::{OrderedContent, SessionAddress};

/// Whether an attachment is an image or a generic file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentKind {
    /// Raster image sent as prompt content.
    Image,
    /// Verbatim generic file sent as a durable reference.
    File,
}

impl AttachmentKind {
    /// User-facing label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Image => "图片",
            Self::File => "文件",
        }
    }

    /// Wire begin kind.
    pub fn wire_kind(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::File => "file",
        }
    }
}

/// Upload/admission state of one card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachmentState {
    /// Staged bytes are still crossing the local protocol.
    Uploading {
        /// Bytes accepted by the host.
        sent: u64,
        /// Total source bytes, when known.
        total: u64,
    },
    /// Ready for submission.
    Ready,
    /// The host explicitly refused the upload.
    Failed(String),
    /// The result is unknown; never resend automatically.
    Unknown(String),
}

impl AttachmentState {
    /// Short status label.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Uploading { .. } => "上传中",
            Self::Ready => "已就绪",
            Self::Failed(_) => "失败",
            Self::Unknown(_) => "结果未知",
        }
    }

    /// Whether the card can be submitted.
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }
}

/// One attachment card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentCard {
    /// Client-generated business identity for this upload.
    pub receipt_id: String,
    /// Image or generic file.
    pub kind: AttachmentKind,
    /// Display name stripped of local path information.
    pub name: String,
    /// Source byte length.
    pub bytes: u64,
    /// Declared source media type for images.
    pub media_type: Option<String>,
    /// Intrinsic width for images.
    pub width: Option<u32>,
    /// Intrinsic height for images.
    pub height: Option<u32>,
    /// Lifecycle state.
    pub state: AttachmentState,
}

impl AttachmentCard {
    /// Compact one-line card label.
    pub fn label(&self) -> String {
        let mut label = format!(
            "[{}] {} · {}",
            self.kind.label(),
            self.name,
            human_bytes(self.bytes)
        );
        if let (Some(width), Some(height)) = (self.width, self.height) {
            label.push_str(&format!(" · {width}×{height}"));
        }
        label.push_str(" · ");
        label.push_str(self.state.label());
        if let AttachmentState::Uploading { sent, total } = &self.state {
            if *total > 0 {
                label.push_str(&format!(" {}/{}", human_bytes(*sent), human_bytes(*total)));
            }
        }
        label
    }
}

/// A text offset where one attachment sits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentAnchor {
    /// Byte offset in the draft text.
    pub offset: usize,
    /// Card identity.
    pub receipt_id: String,
}

/// One draft's attachments and their positions in the text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ComposerAttachments {
    anchors: Vec<AttachmentAnchor>,
    cards: Vec<AttachmentCard>,
}

/// Why a draft cannot be submitted yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentBlocker {
    /// Display name of the first blocking card.
    pub name: String,
    /// Its state.
    pub state: AttachmentState,
}

impl AttachmentBlocker {
    /// User-facing reason.
    pub fn message(&self) -> String {
        match &self.state {
            AttachmentState::Uploading { .. } => {
                format!("附件「{}」仍在上传；草稿已保留", self.name)
            }
            AttachmentState::Failed(detail) => {
                format!("附件「{}」上传失败：{detail}；草稿已保留", self.name)
            }
            AttachmentState::Unknown(detail) => {
                format!("附件「{}」结果未知：{detail}；不会自动重发", self.name)
            }
            AttachmentState::Ready => format!("附件「{}」尚未就绪", self.name),
        }
    }
}

impl ComposerAttachments {
    /// All cards in insertion order.
    pub fn cards(&self) -> &[AttachmentCard] {
        &self.cards
    }

    /// Anchors in text order.
    pub fn anchors(&self) -> &[AttachmentAnchor] {
        &self.anchors
    }

    /// Cards in their actual text order, not insertion order.
    pub fn ordered_cards(&self) -> Vec<&AttachmentCard> {
        self.anchors
            .iter()
            .filter_map(|anchor| self.card(&anchor.receipt_id))
            .collect()
    }

    /// Move the card nearest at or before the cursor one position earlier
    /// (`delta < 0`) or later (`delta > 0`). Reordering changes only the
    /// anchors; no bytes are re-uploaded.
    pub fn move_at_cursor(&mut self, cursor: usize, delta: isize) -> Option<String> {
        let index = self
            .anchors
            .iter()
            .rposition(|anchor| anchor.offset <= cursor)?;
        let target = index as isize + delta;
        if target < 0 || target >= self.anchors.len() as isize {
            return None;
        }
        let target = target as usize;
        let moved = self.anchors[index].receipt_id.clone();
        let other = self.anchors[target].receipt_id.clone();
        self.anchors[index].receipt_id = other;
        self.anchors[target].receipt_id = moved.clone();
        Some(moved)
    }

    /// Look up one card.
    pub fn card(&self, receipt_id: &str) -> Option<&AttachmentCard> {
        self.cards.iter().find(|card| card.receipt_id == receipt_id)
    }

    /// Mutably look up one card.
    pub fn card_mut(&mut self, receipt_id: &str) -> Option<&mut AttachmentCard> {
        self.cards
            .iter_mut()
            .find(|card| card.receipt_id == receipt_id)
    }

    /// Whether there are no attachments.
    pub fn is_empty(&self) -> bool {
        self.cards.is_empty()
    }

    /// Number of attachments.
    pub fn len(&self) -> usize {
        self.cards.len()
    }

    /// Insert one card at a text offset (clamped to the end by the caller).
    pub fn insert(&mut self, offset: usize, card: AttachmentCard) {
        if self.card(&card.receipt_id).is_some() {
            return;
        }
        let receipt_id = card.receipt_id.clone();
        self.cards.push(card);
        let position = self
            .anchors
            .iter()
            .position(|anchor| anchor.offset > offset)
            .unwrap_or(self.anchors.len());
        self.anchors
            .insert(position, AttachmentAnchor { offset, receipt_id });
    }

    /// Remove one card and its anchor.
    pub fn remove(&mut self, receipt_id: &str) -> Option<AttachmentCard> {
        self.anchors
            .retain(|anchor| anchor.receipt_id != receipt_id);
        let position = self
            .cards
            .iter()
            .position(|card| card.receipt_id == receipt_id)?;
        Some(self.cards.remove(position))
    }

    /// Remove the nearest card at or before the cursor, if any.
    pub fn remove_before(&mut self, offset: usize) -> Option<AttachmentCard> {
        let target = self
            .anchors
            .iter()
            .rev()
            .find(|anchor| anchor.offset <= offset)
            .map(|anchor| anchor.receipt_id.clone())?;
        self.remove(&target)
    }

    /// Remove the last inserted card.
    pub fn remove_last(&mut self) -> Option<AttachmentCard> {
        let target = self.cards.last()?.receipt_id.clone();
        self.remove(&target)
    }

    /// Shift anchors after one text edit.
    ///
    /// `at` is the byte offset of the edit, `removed` the byte length removed,
    /// and `inserted` the byte length inserted. An anchor inside the removed
    /// range collapses to the edit point.
    pub fn shift_for_edit(&mut self, at: usize, removed: usize, inserted: usize) {
        let end = at.saturating_add(removed);
        for anchor in &mut self.anchors {
            if anchor.offset <= at {
                continue;
            }
            if anchor.offset < end {
                anchor.offset = at.saturating_add(inserted);
            } else {
                anchor.offset = anchor
                    .offset
                    .saturating_sub(removed)
                    .saturating_add(inserted);
            }
        }
        self.anchors.sort_by_key(|anchor| anchor.offset);
    }

    /// Shift every anchor by `delta` bytes.
    pub fn shift_all(&mut self, delta: usize) {
        for anchor in &mut self.anchors {
            anchor.offset = anchor.offset.saturating_add(delta);
        }
    }

    /// Insert every card from `other` at its recorded anchor offset.
    pub fn extend_from(&mut self, other: &ComposerAttachments) {
        for anchor in &other.anchors {
            let Some(card) = other.card(&anchor.receipt_id) else {
                continue;
            };
            self.insert(anchor.offset, card.clone());
        }
    }

    /// Whether every card is ready for submission.
    pub fn ready(&self) -> Result<(), AttachmentBlocker> {
        if let Some(card) = self.cards.iter().find(|card| !card.state.is_ready()) {
            return Err(AttachmentBlocker {
                name: card.name.clone(),
                state: card.state.clone(),
            });
        }
        Ok(())
    }

    /// Build the ordered content for one submission.
    pub fn content(&self, text: &str) -> OrderedContent {
        let mut content = OrderedContent::new();
        let mut last = 0usize;
        for anchor in &self.anchors {
            let offset = anchor.offset.min(text.len());
            if !text.is_char_boundary(offset) {
                continue;
            }
            if offset > last {
                content.push_text(&text[last..offset]);
                last = offset;
            }
            let Some(card) = self.card(&anchor.receipt_id) else {
                continue;
            };
            match card.kind {
                AttachmentKind::Image => content.push_image(
                    card.receipt_id.clone(),
                    card.name.clone(),
                    card.media_type.clone().unwrap_or_default(),
                ),
                AttachmentKind::File => {
                    content.push_file(card.receipt_id.clone(), card.name.clone())
                }
            }
        }
        if last < text.len() {
            content.push_text(&text[last..]);
        }
        content
    }

    /// Display text for a local echo.
    pub fn echo_text(&self, text: &str) -> String {
        self.content(text).text()
    }

    /// Clear every card and anchor.
    pub fn clear(&mut self) {
        self.anchors.clear();
        self.cards.clear();
    }

    /// Number of cards still uploading.
    pub fn uploading(&self) -> usize {
        self.cards
            .iter()
            .filter(|card| matches!(card.state, AttachmentState::Uploading { .. }))
            .count()
    }
}

fn human_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;
    if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

/// Attachment drafts keyed by conversation address.
#[derive(Debug, Clone)]
pub struct AttachmentDraftStore {
    drafts: BTreeMap<SessionAddress, ComposerAttachments>,
    current: SessionAddress,
}

impl AttachmentDraftStore {
    /// Create a store with an empty draft for `current`.
    pub fn new(current: SessionAddress) -> Self {
        let mut drafts = BTreeMap::new();
        drafts.insert(current.clone(), ComposerAttachments::default());
        Self { drafts, current }
    }

    /// The address whose attachments are visible.
    pub fn current_address(&self) -> &SessionAddress {
        &self.current
    }

    /// Switch addresses; the old draft is kept untouched.
    pub fn switch_to(&mut self, address: SessionAddress) -> &mut ComposerAttachments {
        self.current = address.clone();
        self.drafts.entry(address).or_default()
    }

    /// The visible draft.
    pub fn current(&self) -> &ComposerAttachments {
        self.drafts
            .get(&self.current)
            .expect("the current address always has an attachment draft")
    }

    /// The visible draft, mutably.
    pub fn current_mut(&mut self) -> &mut ComposerAttachments {
        self.drafts
            .get_mut(&self.current)
            .expect("the current address always has an attachment draft")
    }

    /// One address's draft.
    pub fn get(&self, address: &SessionAddress) -> Option<&ComposerAttachments> {
        self.drafts.get(address)
    }

    /// One address's draft, mutably.
    pub fn get_mut(&mut self, address: &SessionAddress) -> Option<&mut ComposerAttachments> {
        self.drafts.get_mut(address)
    }

    /// Remove one address's draft.
    pub fn remove(&mut self, address: &SessionAddress) -> Option<ComposerAttachments> {
        let removed = self.drafts.remove(address)?;
        if &self.current == address {
            self.drafts
                .insert(address.clone(), ComposerAttachments::default());
        }
        Some(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(receipt_id: &str, name: &str) -> AttachmentCard {
        AttachmentCard {
            receipt_id: receipt_id.into(),
            kind: AttachmentKind::Image,
            name: name.into(),
            bytes: 2048,
            media_type: Some("image/png".into()),
            width: Some(10),
            height: Some(10),
            state: AttachmentState::Ready,
        }
    }

    fn file(receipt_id: &str, name: &str) -> AttachmentCard {
        AttachmentCard {
            receipt_id: receipt_id.into(),
            kind: AttachmentKind::File,
            name: name.into(),
            bytes: 5,
            media_type: None,
            width: None,
            height: None,
            state: AttachmentState::Ready,
        }
    }

    #[test]
    fn text_and_attachments_keep_user_order() {
        let mut draft = ComposerAttachments::default();
        draft.insert(4, image("i-1", "a.png"));
        draft.insert(4, file("f-1", "b.txt"));
        let content = draft.content("look  and more");
        let parts = content.parts();
        assert!(matches!(
            parts[0],
            nano_conversation::ContentPart::Text { .. }
        ));
        assert!(matches!(
            parts[1],
            nano_conversation::ContentPart::Image { .. }
        ));
        assert!(matches!(
            parts[2],
            nano_conversation::ContentPart::File { .. }
        ));
        assert!(matches!(
            parts[3],
            nano_conversation::ContentPart::Text { .. }
        ));
        assert_eq!(content.text(), "look[图片 a.png][文件 b.txt]  and more");
    }

    #[test]
    fn anchors_shift_after_text_edits() {
        let mut draft = ComposerAttachments::default();
        draft.insert(4, image("i-1", "a.png"));
        draft.shift_for_edit(0, 0, 3);
        assert_eq!(draft.anchors()[0].offset, 7);
        draft.shift_for_edit(5, 2, 0);
        assert_eq!(draft.anchors()[0].offset, 5);
        draft.shift_for_edit(0, 10, 0);
        assert_eq!(draft.anchors()[0].offset, 0);
    }

    #[test]
    fn not_ready_cards_block_submission() {
        let mut draft = ComposerAttachments::default();
        let mut card = image("i-1", "a.png");
        card.state = AttachmentState::Uploading {
            sent: 10,
            total: 100,
        };
        draft.insert(0, card);
        let blocker = draft.ready().unwrap_err();
        assert!(blocker.message().contains("上传"));
    }

    #[test]
    fn reordering_at_the_boundaries_is_a_no_op() {
        let mut single = ComposerAttachments::default();
        single.insert(0, image("i-1", "a.png"));
        assert_eq!(single.move_at_cursor(0, -1), None);
        assert_eq!(single.move_at_cursor(0, 1), None);
        assert_eq!(single.ordered_cards()[0].receipt_id, "i-1");

        let mut pair = ComposerAttachments::default();
        pair.insert(0, image("i-1", "a.png"));
        pair.insert(4, file("f-1", "b.txt"));
        assert_eq!(pair.move_at_cursor(0, -1), None);
        assert_eq!(pair.move_at_cursor(4, 1), None);
    }

    #[test]
    fn reordering_moves_only_the_anchor() {
        let mut draft = ComposerAttachments::default();
        draft.insert(0, image("i-1", "a.png"));
        draft.insert(4, file("f-1", "b.txt"));
        assert_eq!(draft.move_at_cursor(4, -1).as_deref(), Some("f-1"));
        let ordered = draft.ordered_cards();
        assert_eq!(ordered[0].receipt_id, "f-1");
        assert_eq!(ordered[1].receipt_id, "i-1");
        assert_eq!(draft.anchors()[0].offset, 0);
        assert_eq!(draft.anchors()[1].offset, 4);
    }

    #[test]
    fn remove_before_cursor_removes_the_nearest_card() {
        let mut draft = ComposerAttachments::default();
        draft.insert(2, image("i-1", "a.png"));
        draft.insert(8, file("f-1", "b.txt"));
        assert_eq!(draft.remove_before(5).unwrap().receipt_id, "i-1");
        assert_eq!(draft.cards().len(), 1);
    }
}
