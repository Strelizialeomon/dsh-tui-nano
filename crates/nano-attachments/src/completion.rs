//! Cancellable `@file` completion state.
//!
//! Every input change increments a revision. A slow response may only be
//! applied when its revision is still current, so a late answer can never
//! overwrite newer text or insert at a moved cursor.

use crate::grammar::{
    active_at_token, current_line, format_file_mention, AtToken, FileReferenceCandidate,
};

/// One request the app should send to `nano.fileReferences.list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionRequest {
    /// Revision the response must match.
    pub revision: u64,
    /// Query text following `@`.
    pub query: String,
}

/// What the app should do after one input change.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompletionUpdate {
    /// A new request to send, when the token or query changed.
    pub request: Option<CompletionRequest>,
    /// A previously pending request id to cancel.
    pub cancel: Option<u64>,
}

/// Selection and staleness state for one completion popup.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompletionState {
    revision: u64,
    token: Option<AtToken>,
    token_start: usize,
    candidates: Vec<FileReferenceCandidate>,
    selected: usize,
    visible: bool,
    pending: Option<(u64, u64)>,
    last_query: Option<String>,
    suppressed_query: Option<String>,
}

impl CompletionState {
    /// Revision the next response must carry.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Current active token, if any.
    pub fn token(&self) -> Option<&AtToken> {
        self.token.as_ref()
    }

    /// Candidates currently shown.
    pub fn candidates(&self) -> &[FileReferenceCandidate] {
        &self.candidates
    }

    /// Selection index.
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Whether the popup is visible.
    pub fn visible(&self) -> bool {
        self.visible && !self.candidates.is_empty()
    }

    /// In-flight request id, when one is waiting for a response.
    pub fn pending(&self) -> Option<u64> {
        self.pending.map(|(rpc_id, _)| rpc_id)
    }

    /// Recompute the active token after one input change. Returns a request
    /// when the query is new, and the stale in-flight id when it must be
    /// cancelled.
    pub fn update(&mut self, text: &str, cursor: usize) -> CompletionUpdate {
        let (line_start, line, column) = current_line(text, cursor);
        let token = active_at_token(line, column);
        let query = token.as_ref().map(|token| token.query.clone());
        let next_start = token
            .as_ref()
            .map_or(0, |token| line_start.saturating_add(token.start));
        let changed = token != self.token || (token.is_some() && next_start != self.token_start);
        self.token = token;
        self.token_start = next_start;
        if changed {
            self.revision += 1;
            self.candidates.clear();
            self.visible = false;
            self.selected = 0;
        }
        if query.is_none() {
            self.last_query = None;
            self.suppressed_query = None;
            return CompletionUpdate {
                request: None,
                cancel: self.cancel_pending(),
            };
        }
        if query == self.suppressed_query {
            return CompletionUpdate::default();
        }
        self.suppressed_query = None;
        if query == self.last_query && self.pending.is_some() {
            return CompletionUpdate::default();
        }
        if query == self.last_query && !self.candidates.is_empty() {
            return CompletionUpdate::default();
        }
        self.last_query = query.clone();
        let cancel = self.cancel_pending();
        let revision = self.revision;
        CompletionUpdate {
            request: Some(CompletionRequest {
                revision,
                query: query.unwrap_or_default(),
            }),
            cancel,
        }
    }

    /// Remember the request id that carries `revision`.
    pub fn set_pending(&mut self, rpc_id: u64, revision: u64) {
        self.pending = Some((rpc_id, revision));
    }

    /// Apply one response. Stale responses are ignored.
    pub fn apply(&mut self, revision: u64, candidates: Vec<FileReferenceCandidate>) -> bool {
        if revision != self.revision {
            return false;
        }
        if self.token.is_none() {
            return false;
        }
        self.pending = None;
        self.candidates = candidates;
        self.selected = 0;
        self.visible = !self.candidates.is_empty();
        true
    }

    /// Move the selection by `delta`, clamped.
    pub fn move_selection(&mut self, delta: isize) {
        if self.candidates.is_empty() {
            return;
        }
        let last = self.candidates.len() - 1;
        let next = self.selected as isize + delta;
        self.selected = next.clamp(0, last as isize) as usize;
    }

    /// Dismiss the popup without changing the text.
    pub fn dismiss(&mut self) -> Option<u64> {
        self.suppressed_query = self.token.as_ref().map(|token| token.query.clone());
        self.visible = false;
        self.candidates.clear();
        self.last_query = None;
        self.revision += 1;
        self.cancel_pending()
    }

    /// Format the selected candidate and replace the active token.
    pub fn accept(&mut self, text: &str, cursor: usize) -> Option<AcceptedMention> {
        let token = self.token.clone()?;
        let candidate = self.candidates.get(self.selected)?.clone();
        let mention = format_file_mention(&candidate, token.quoted)?;
        let cursor = cursor.min(text.len());
        if self.token_start > cursor {
            return None;
        }
        let directory = candidate.kind == crate::grammar::CandidateKind::Directory;
        let mut next = String::with_capacity(text.len() + mention.len());
        next.push_str(&text[..self.token_start]);
        next.push_str(&mention);
        next.push_str(&text[cursor..]);
        let cursor = self.token_start + mention.len();
        self.visible = false;
        self.candidates.clear();
        self.last_query = None;
        self.suppressed_query = None;
        self.revision += 1;
        Some(AcceptedMention {
            text: next,
            cursor,
            directory,
        })
    }

    /// Cancel the in-flight request, if any.
    pub fn cancel_pending(&mut self) -> Option<u64> {
        self.pending.take().map(|(rpc_id, _)| rpc_id)
    }

    /// Clear all state.
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

/// Result of inserting one selected candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedMention {
    /// New draft text.
    pub text: String,
    /// Cursor byte offset after the inserted mention.
    pub cursor: usize,
    /// Whether the candidate was a directory that kept its quote open.
    pub directory: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammar::CandidateKind;

    fn candidate(path: &str, kind: CandidateKind) -> FileReferenceCandidate {
        FileReferenceCandidate {
            path: path.into(),
            kind,
        }
    }

    #[test]
    fn a_late_response_never_overwrites_newer_input() {
        let mut state = CompletionState::default();
        let update = state.update("see @sr", 7);
        let revision = update.request.unwrap().revision;
        state.update("see @src/m", 11);
        assert!(!state.apply(revision, vec![candidate("src/old", CandidateKind::File)]));
        assert!(!state.visible());
    }

    #[test]
    fn moving_the_cursor_away_cancels_completion() {
        let mut state = CompletionState::default();
        let update = state.update("@sr", 3);
        state.set_pending(7, update.request.unwrap().revision);
        let moved = state.update("@sr and more", 13);
        assert_eq!(moved.cancel, Some(7));
        assert!(state.token().is_none());
        assert!(!state.visible());
    }

    #[test]
    fn accepting_in_a_multiline_draft_replaces_only_the_token() {
        let mut state = CompletionState::default();
        let text = "first line\nsee @sr";
        let cursor = text.len();
        let update = state.update(text, cursor);
        let revision = update.request.unwrap().revision;
        state.apply(revision, vec![candidate("src/lib.rs", CandidateKind::File)]);
        let accepted = state.accept(text, cursor).unwrap();
        assert_eq!(accepted.text, "first line\nsee @src/lib.rs");
        assert_eq!(accepted.cursor, "first line\nsee @src/lib.rs".len());
    }

    #[test]
    fn accepting_a_file_replaces_only_the_token() {
        let mut state = CompletionState::default();
        let update = state.update("see @sr here", 7);
        let revision = update.request.unwrap().revision;
        state.apply(revision, vec![candidate("src/lib.rs", CandidateKind::File)]);
        let accepted = state.accept("see @sr here", 7).unwrap();
        assert_eq!(accepted.text, "see @src/lib.rs here");
        assert_eq!(accepted.cursor, "see @src/lib.rs".len());
        assert!(!accepted.directory);
    }

    #[test]
    fn accepting_a_directory_keeps_the_quote_open() {
        let mut state = CompletionState::default();
        let update = state.update("@\"my d", 6);
        let revision = update.request.unwrap().revision;
        state.apply(
            revision,
            vec![candidate("my dir", CandidateKind::Directory)],
        );
        let accepted = state.accept("@\"my d", 6).unwrap();
        assert_eq!(accepted.text, "@\"my dir/");
        assert!(accepted.directory);
    }

    #[test]
    fn selection_is_clamped() {
        let mut state = CompletionState::default();
        let update = state.update("@", 1);
        let revision = update.request.unwrap().revision;
        state.apply(
            revision,
            vec![
                candidate("a", CandidateKind::File),
                candidate("b", CandidateKind::File),
            ],
        );
        state.move_selection(5);
        assert_eq!(state.selected(), 1);
        state.move_selection(-5);
        assert_eq!(state.selected(), 0);
    }
}
