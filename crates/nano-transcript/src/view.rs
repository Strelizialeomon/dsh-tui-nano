//! D04: the transcript view S12 mounts.
//!
//! One renderer serves both the read-only view (one-shot history and external
//! leaves) and the interactive view (continuable sub-conversations). A consumer
//! mounts the view and feeds the same profile events; it never reads a child
//! session log directly and never creates a second output file.

use std::time::Instant;

use nano_conversation::{ConversationModel, SessionAddress};
use serde_json::Value;

use crate::model::{TranscriptModel, TranscriptSource};
use crate::render::{render, Hit, RenderCache, TranscriptRender};
use crate::scroll::ScrollState;
use crate::tool::QuestionRecord;

/// One mounted transcript view.
#[derive(Debug, Clone)]
pub struct TranscriptView {
    model: TranscriptModel,
    cache: RenderCache,
    scroll: ScrollState,
}

impl TranscriptView {
    /// Mount a view for one address.
    pub fn new(address: SessionAddress, source: TranscriptSource) -> Self {
        Self {
            model: TranscriptModel::new(address, source),
            cache: RenderCache::new(),
            scroll: ScrollState::default(),
        }
    }

    /// Bound address.
    pub fn address(&self) -> &SessionAddress {
        self.model.address()
    }

    /// Read-only or interactive.
    pub fn source(&self) -> TranscriptSource {
        self.model.source()
    }

    /// Apply one profile event. Both view kinds consume the same event feed.
    pub fn apply_event(&mut self, kind: &str, payload: &Value) -> bool {
        self.model.apply_event(kind, payload)
    }

    /// Rebuild entries from the conversation projection.
    pub fn sync(&mut self, conversation: &ConversationModel, now: Instant) -> bool {
        let changed = self.model.sync(conversation, now);
        if let Some(anchor) = self.scroll.anchor().cloned() {
            if let Some(new) = self.model.alias(&anchor.identity) {
                self.scroll.rewrite_anchor(&anchor.identity, new);
            }
        }
        changed
    }

    /// Render the visible window.
    pub fn render(&mut self, width: usize, viewport: usize) -> TranscriptRender {
        render(
            &self.model,
            &mut self.cache,
            &mut self.scroll,
            width,
            viewport,
        )
    }

    /// Scroll state.
    pub fn scroll(&self) -> &ScrollState {
        &self.scroll
    }

    /// Scroll state, mutably (wheel and page keys).
    pub fn scroll_mut(&mut self) -> &mut ScrollState {
        &mut self.scroll
    }

    /// Toggle one entry when the view is interactive.
    pub fn toggle(&mut self, identity: &str) -> bool {
        self.model.toggle(identity)
    }

    /// Reveal an entry when the view is interactive.
    pub fn reveal(&mut self, identity: &str) {
        self.model.reveal(identity);
    }

    /// The recorded question behind a tool call.
    pub fn question(&self, call_id: &str) -> Option<QuestionRecord> {
        self.model.question(call_id)
    }

    /// Entries for diagnostics and tests.
    pub fn entries(&self) -> &[crate::model::Entry] {
        self.model.entries()
    }

    /// Resolve the action for one visible row.
    pub fn hit<'a>(&self, render: &'a TranscriptRender, row: usize) -> Option<&'a Hit> {
        render.hits.get(row).and_then(Option::as_ref)
    }
}
