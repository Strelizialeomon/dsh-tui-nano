//! Multi-line input, bracketed paste, IME composition, and per-address drafts.
//!
//! The buffer stores the user's original text. Display may normalize control
//! characters, but editing, copying, and submission all read this same string.

use std::collections::BTreeMap;

use unicode_width::UnicodeWidthChar;

use crate::address::SessionAddress;

/// Whether the terminal input method currently owns the keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InputMode {
    /// Ordinary typing; Enter sends.
    #[default]
    Normal,
    /// IME composition is active; Enter belongs to the input method.
    Composing,
}

/// One input action after terminal key translation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputEvent {
    /// Insert literal text at the cursor.
    Insert(String),
    /// A bracketed-paste payload; inserted without interpreting its bytes.
    Paste(String),
    /// Insert a newline (Shift+Enter or Ctrl+J).
    Newline,
    /// Plain Enter; sends when the draft is not blank.
    Enter,
    /// Remove the character before the cursor.
    Backspace,
    /// Remove the character after the cursor.
    Delete,
    /// Move one character left.
    Left,
    /// Move one character right.
    Right,
    /// Move one display line up.
    Up,
    /// Move one display line down.
    Down,
    /// Move to the start of the current line.
    Home,
    /// Move to the end of the current line.
    End,
    /// Move to the start of the buffer.
    BufferStart,
    /// Move to the end of the buffer.
    BufferEnd,
    /// The terminal entered or left IME composition.
    SetComposing(bool),
}

/// What the buffer asks the application to do after an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputOutcome {
    /// The buffer changed; redraw.
    Changed,
    /// Submit the current text.
    Send,
    /// Enter was pressed on blank content; the draft is unchanged.
    RejectedEmpty,
    /// The event was swallowed (IME-owned Enter, cursor at a boundary).
    Ignored,
}

/// One editable draft.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InputBuffer {
    text: String,
    cursor: usize,
    mode: InputMode,
}

impl InputBuffer {
    /// Create an empty buffer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a buffer from existing text with the cursor at the end.
    pub fn from_text(text: impl Into<String>) -> Self {
        let text = text.into();
        let cursor = text.len();
        Self {
            text,
            cursor,
            mode: InputMode::Normal,
        }
    }

    /// Original text exactly as the user entered it.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Cursor byte offset, always on a character boundary.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Current IME mode.
    pub fn mode(&self) -> InputMode {
        self.mode
    }

    /// Whether the draft has no non-whitespace text.
    pub fn is_blank(&self) -> bool {
        !self
            .text
            .chars()
            .any(|character| !character.is_whitespace())
    }

    /// Whether the draft is empty.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Replace the draft contents and put the cursor at the end.
    pub fn set_text(&mut self, text: impl Into<String>) {
        let text = text.into();
        self.cursor = text.len();
        self.text = text;
    }

    /// Remove and return the draft text.
    pub fn take(&mut self) -> String {
        self.cursor = 0;
        self.mode = InputMode::Normal;
        std::mem::take(&mut self.text)
    }

    /// Clear the draft.
    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    /// Insert literal text at the cursor.
    pub fn insert_str(&mut self, value: &str) {
        if value.is_empty() {
            return;
        }
        self.text.insert_str(self.cursor, value);
        self.cursor += value.len();
    }

    /// Insert one character at the cursor.
    pub fn insert_char(&mut self, value: char) {
        let mut buffer = [0u8; 4];
        self.insert_str(value.encode_utf8(&mut buffer));
    }

    /// Insert a newline at the cursor.
    pub fn insert_newline(&mut self) {
        self.insert_str("\n");
    }

    /// Delete the character before the cursor.
    pub fn backspace(&mut self) -> bool {
        let Some(previous) = self.previous_boundary(self.cursor) else {
            return false;
        };
        self.text.replace_range(previous..self.cursor, "");
        self.cursor = previous;
        true
    }

    /// Delete the character after the cursor.
    pub fn delete(&mut self) -> bool {
        let Some(next) = self.next_boundary(self.cursor) else {
            return false;
        };
        self.text.replace_range(self.cursor..next, "");
        true
    }

    /// Move one character left.
    pub fn move_left(&mut self) -> bool {
        match self.previous_boundary(self.cursor) {
            Some(previous) => {
                self.cursor = previous;
                true
            }
            None => false,
        }
    }

    /// Move one character right.
    pub fn move_right(&mut self) -> bool {
        match self.next_boundary(self.cursor) {
            Some(next) => {
                self.cursor = next;
                true
            }
            None => false,
        }
    }

    /// Move one display line up, preserving the display column when possible.
    pub fn move_up(&mut self) -> bool {
        let (start, _) = self.line_bounds(self.cursor);
        if start == 0 {
            return false;
        }
        let column = self.display_column(start, self.cursor);
        let previous_end = start - 1;
        let (previous_start, _) = self.line_bounds(previous_end);
        self.cursor = self.byte_at_column(previous_start, previous_end, column);
        true
    }

    /// Move one display line down, preserving the display column when possible.
    pub fn move_down(&mut self) -> bool {
        let (_, end) = self.line_bounds(self.cursor);
        if end >= self.text.len() {
            return false;
        }
        let column = self.display_column(self.line_start(self.cursor), self.cursor);
        let next_start = end + 1;
        let (_, next_end) = self.line_bounds(next_start);
        self.cursor = self.byte_at_column(next_start, next_end, column);
        true
    }

    /// Move to the start of the current display line.
    pub fn move_home(&mut self) -> bool {
        let start = self.line_start(self.cursor);
        let changed = start != self.cursor;
        self.cursor = start;
        changed
    }

    /// Move to the end of the current display line.
    pub fn move_end(&mut self) -> bool {
        let end = self.line_end(self.cursor);
        let changed = end != self.cursor;
        self.cursor = end;
        changed
    }

    /// Move to the start of the buffer.
    pub fn move_buffer_start(&mut self) -> bool {
        let changed = self.cursor != 0;
        self.cursor = 0;
        changed
    }

    /// Move to the end of the buffer.
    pub fn move_buffer_end(&mut self) -> bool {
        let changed = self.cursor != self.text.len();
        self.cursor = self.text.len();
        changed
    }

    /// Apply one translated terminal event.
    pub fn handle(&mut self, event: &InputEvent) -> InputOutcome {
        match event {
            InputEvent::Insert(value) | InputEvent::Paste(value) => {
                if value.is_empty() {
                    return InputOutcome::Ignored;
                }
                self.insert_str(value);
                InputOutcome::Changed
            }
            InputEvent::Newline => {
                self.insert_newline();
                InputOutcome::Changed
            }
            InputEvent::Enter => match self.mode {
                // The input method owns the key while composing; the terminal
                // consumes it before it reaches the application.
                InputMode::Composing => InputOutcome::Ignored,
                InputMode::Normal if self.is_blank() => InputOutcome::RejectedEmpty,
                InputMode::Normal => InputOutcome::Send,
            },
            InputEvent::Backspace => {
                if self.backspace() {
                    InputOutcome::Changed
                } else {
                    InputOutcome::Ignored
                }
            }
            InputEvent::Delete => {
                if self.delete() {
                    InputOutcome::Changed
                } else {
                    InputOutcome::Ignored
                }
            }
            InputEvent::Left => changed(self.move_left()),
            InputEvent::Right => changed(self.move_right()),
            InputEvent::Up => changed(self.move_up()),
            InputEvent::Down => changed(self.move_down()),
            InputEvent::Home => changed(self.move_home()),
            InputEvent::End => changed(self.move_end()),
            InputEvent::BufferStart => changed(self.move_buffer_start()),
            InputEvent::BufferEnd => changed(self.move_buffer_end()),
            InputEvent::SetComposing(active) => {
                self.mode = if *active {
                    InputMode::Composing
                } else {
                    InputMode::Normal
                };
                InputOutcome::Changed
            }
        }
    }

    /// Current (line, display column) for rendering the cursor.
    pub fn cursor_position(&self) -> (usize, usize) {
        let mut line = 0;
        let mut column = 0;
        for character in self.text[..self.cursor].chars() {
            if character == '\n' {
                line += 1;
                column = 0;
            } else {
                column += display_width(character);
            }
        }
        (line, column)
    }

    /// Lines after normalizing only for display. `\r` is shown as nothing so a
    /// pasted CRLF does not draw an extra cursor-moving glyph.
    pub fn display_lines(&self) -> Vec<String> {
        self.text
            .split('\n')
            .map(|line| line.replace('\r', ""))
            .collect()
    }

    fn changed(&mut self) -> InputOutcome {
        InputOutcome::Changed
    }

    fn line_start(&self, index: usize) -> usize {
        self.text[..index]
            .rfind('\n')
            .map(|offset| offset + 1)
            .unwrap_or(0)
    }

    fn line_end(&self, index: usize) -> usize {
        self.text[index..]
            .find('\n')
            .map(|offset| index + offset)
            .unwrap_or(self.text.len())
    }

    fn line_bounds(&self, index: usize) -> (usize, usize) {
        (self.line_start(index), self.line_end(index))
    }

    fn display_column(&self, start: usize, end: usize) -> usize {
        self.text[start..end].chars().map(display_width).sum()
    }

    fn byte_at_column(&self, start: usize, end: usize, column: usize) -> usize {
        let mut width = 0;
        for (offset, character) in self.text[start..end].char_indices() {
            let next = width + display_width(character);
            if next > column {
                // A wide character occupies several cells. When the target
                // column falls inside it, land after it rather than before.
                return if column > width {
                    start + offset + character.len_utf8()
                } else {
                    start + offset
                };
            }
            width = next;
        }
        end
    }

    fn previous_boundary(&self, index: usize) -> Option<usize> {
        if index == 0 {
            return None;
        }
        self.text[..index]
            .char_indices()
            .next_back()
            .map(|(offset, _)| offset)
    }

    fn next_boundary(&self, index: usize) -> Option<usize> {
        if index >= self.text.len() {
            return None;
        }
        self.text[index..]
            .char_indices()
            .nth(1)
            .map(|(offset, _)| index + offset)
            .or(Some(self.text.len()))
    }
}

fn display_width(character: char) -> usize {
    UnicodeWidthChar::width(character).unwrap_or(0)
}

fn changed(value: bool) -> InputOutcome {
    if value {
        InputOutcome::Changed
    } else {
        InputOutcome::Ignored
    }
}

/// Drafts keyed by conversation address. Switching addresses never moves text
/// between them; only the current address's draft is visible.
#[derive(Debug, Clone)]
pub struct DraftStore {
    drafts: BTreeMap<SessionAddress, InputBuffer>,
    current: SessionAddress,
}

impl DraftStore {
    /// Create a store with an empty draft for `current`.
    pub fn new(current: SessionAddress) -> Self {
        let mut drafts = BTreeMap::new();
        drafts.insert(current.clone(), InputBuffer::new());
        Self { drafts, current }
    }

    /// The address whose draft is visible.
    pub fn current_address(&self) -> &SessionAddress {
        &self.current
    }

    /// Switch addresses, keeping the old draft untouched.
    pub fn switch_to(&mut self, address: SessionAddress) -> &mut InputBuffer {
        self.current = address.clone();
        self.drafts.entry(address).or_default()
    }

    /// The visible draft.
    pub fn current(&self) -> &InputBuffer {
        self.drafts
            .get(&self.current)
            .expect("the current address always has a draft")
    }

    /// The visible draft, mutably.
    pub fn current_mut(&mut self) -> &mut InputBuffer {
        self.drafts
            .get_mut(&self.current)
            .expect("the current address always has a draft")
    }

    /// One address's draft.
    pub fn get(&self, address: &SessionAddress) -> Option<&InputBuffer> {
        self.drafts.get(address)
    }

    /// One address's draft, mutably.
    pub fn get_mut(&mut self, address: &SessionAddress) -> Option<&mut InputBuffer> {
        self.drafts.get_mut(address)
    }

    /// Remove one address's draft (for a child conversation that disappeared).
    /// Removing the current address leaves a fresh empty draft in its place so
    /// the visible draft invariant cannot be broken.
    pub fn remove(&mut self, address: &SessionAddress) -> Option<InputBuffer> {
        let removed = self.drafts.remove(address)?;
        if &self.current == address {
            self.drafts.insert(address.clone(), InputBuffer::new());
        }
        Some(removed)
    }

    /// Number of retained drafts.
    pub fn len(&self) -> usize {
        self.drafts.len()
    }

    /// Whether no draft exists.
    pub fn is_empty(&self) -> bool {
        self.drafts.is_empty()
    }
}

impl InputBuffer {
    /// Convenience for tests and callers that only need `changed`.
    pub fn apply(&mut self, event: &InputEvent) -> InputOutcome {
        self.handle(event)
    }

    /// Mark the buffer as changed without a semantic event.
    pub fn touch(&mut self) -> InputOutcome {
        self.changed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiline_editing_preserves_text_and_cursor() {
        let mut input = InputBuffer::new();
        input.handle(&InputEvent::Insert("第一行".into()));
        input.handle(&InputEvent::Newline);
        input.handle(&InputEvent::Insert("second".into()));
        assert_eq!(input.text(), "第一行\nsecond");
        assert!(input.move_up());
        let (line, _) = input.cursor_position();
        assert_eq!(line, 0);
        input.handle(&InputEvent::Insert("X".into()));
        assert_eq!(input.text(), "第一行X\nsecond");
    }

    #[test]
    fn bracketed_paste_keeps_newlines_and_carriage_returns() {
        let mut input = InputBuffer::new();
        let pasted = "a\r\nb\nc";
        assert_eq!(
            input.handle(&InputEvent::Paste(pasted.into())),
            InputOutcome::Changed
        );
        assert_eq!(input.text(), pasted);
        assert_eq!(input.display_lines(), vec!["a", "b", "c"]);
    }

    #[test]
    fn ime_enter_never_sends() {
        let mut input = InputBuffer::from_text("拼");
        input.handle(&InputEvent::SetComposing(true));
        assert_eq!(input.handle(&InputEvent::Enter), InputOutcome::Ignored);
        assert_eq!(input.text(), "拼");
        input.handle(&InputEvent::SetComposing(false));
        assert_eq!(input.handle(&InputEvent::Enter), InputOutcome::Send);
    }

    #[test]
    fn blank_enter_is_rejected_and_keeps_text() {
        let mut input = InputBuffer::from_text(" \n\t");
        assert_eq!(
            input.handle(&InputEvent::Enter),
            InputOutcome::RejectedEmpty
        );
        assert_eq!(input.text(), " \n\t");
    }

    #[test]
    fn very_long_single_line_round_trips() {
        let long = "x".repeat(200_000);
        let mut input = InputBuffer::new();
        input.handle(&InputEvent::Insert(long.clone()));
        assert_eq!(input.text(), long);
        assert!(input.move_buffer_start());
        assert_eq!(input.cursor(), 0);
        assert!(input.move_buffer_end());
        assert_eq!(input.cursor(), long.len());
    }

    #[test]
    fn drafts_are_per_address_and_do_not_leak() {
        let root = SessionAddress::root("root");
        let child =
            SessionAddress::child("root", "root", "child", crate::SubagentMode::Continuable);
        let mut store = DraftStore::new(root.clone());
        store.current_mut().insert_str("main draft");
        store.switch_to(child.clone());
        assert!(store.current().is_empty());
        store.current_mut().insert_str("child draft");
        store.switch_to(root.clone());
        assert_eq!(store.current().text(), "main draft");
        store.switch_to(child);
        assert_eq!(store.current().text(), "child draft");
    }

    #[test]
    fn removing_the_visible_draft_leaves_an_empty_one() {
        let root = SessionAddress::root("root");
        let mut store = DraftStore::new(root.clone());
        store.current_mut().insert_str("draft");
        let removed = store.remove(&root).unwrap();
        assert_eq!(removed.text(), "draft");
        assert!(store.current().is_empty());
    }

    #[test]
    fn cursor_movement_handles_cjk_width() {
        let mut input = InputBuffer::from_text("你好\nabc");
        assert!(input.move_up());
        let (line, column) = input.cursor_position();
        assert_eq!(line, 0);
        // "abc" is three columns; the first line is four columns wide, so the
        // preserved column lands at the end of the first line.
        assert_eq!(column, 4);
    }
}
