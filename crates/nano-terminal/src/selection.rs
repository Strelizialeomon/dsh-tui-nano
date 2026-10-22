//! Visible-screen selection and copy.
//!
//! Selection coordinates are viewport cells (row, column). Copy uses the VT
//! screen's logical cells, so wide and combining characters stay intact and a
//! soft-wrapped row does not gain an extra newline.

/// One active mouse selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    anchor: (u16, u16),
    cursor: (u16, u16),
    moved: bool,
}

impl Selection {
    /// Begin a selection at one cell.
    pub fn start(anchor: (u16, u16)) -> Self {
        Self {
            anchor,
            cursor: anchor,
            moved: false,
        }
    }

    /// Move the selection endpoint.
    pub fn update(&mut self, cursor: (u16, u16)) {
        if cursor != self.anchor {
            self.moved = true;
        }
        self.cursor = cursor;
    }

    /// Whether the pointer moved after the press.
    pub fn moved(&self) -> bool {
        self.moved
    }

    /// Normalized top-left cell.
    pub fn start_cell(&self) -> (u16, u16) {
        if self.anchor <= self.cursor {
            self.anchor
        } else {
            self.cursor
        }
    }

    /// Normalized bottom-right cell.
    pub fn end_cell(&self) -> (u16, u16) {
        if self.anchor <= self.cursor {
            self.cursor
        } else {
            self.anchor
        }
    }

    /// Whether one viewport cell is inside the selection (reading order).
    pub fn contains(&self, row: u16, column: u16) -> bool {
        let (start_row, start_col) = self.start_cell();
        let (end_row, end_col) = self.end_cell();
        if start_row == end_row {
            return row == start_row && column >= start_col && column <= end_col;
        }
        if row == start_row {
            return column >= start_col;
        }
        if row == end_row {
            return column <= end_col;
        }
        row > start_row && row < end_row
    }

    /// Copy the selected visible text exactly as the VT screen stores it.
    pub fn text(&self, screen: &vt100::Screen) -> Option<String> {
        if !self.moved {
            return None;
        }
        let (start_row, start_col) = self.start_cell();
        let (end_row, end_col) = self.end_cell();
        if start_row == end_row && start_col == end_col {
            return None;
        }
        // `contents_between` takes an exclusive end column. Clamp both ends to
        // the parser width: a read-only attachment can be narrower than the
        // viewport when the outer window grows.
        let (_, columns) = screen.size();
        let last_column = columns.saturating_sub(1);
        let start_column = start_col.min(last_column);
        let end_column = end_col.min(last_column).saturating_add(1);
        if start_row == end_row && start_column >= end_column {
            return None;
        }
        let text = screen.contents_between(start_row, start_column, end_row, end_column);
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(text: &str) -> vt100::Parser {
        let mut parser = vt100::Parser::new(4, 20, 0);
        parser.process(text.as_bytes());
        parser
    }

    #[test]
    fn same_row_selection_is_ordered() {
        let parser = screen("hello world");
        let mut selection = Selection::start((0, 9));
        selection.update((0, 1));
        assert_eq!(selection.start_cell(), (0, 1));
        assert_eq!(selection.end_cell(), (0, 9));
        assert_eq!(selection.text(parser.screen()).unwrap(), "ello worl");
    }

    #[test]
    fn multi_row_selection_keeps_terminal_semantics() {
        let parser = screen("hello 世界\r\nsecond line");
        let mut selection = Selection::start((0, 0));
        selection.update((1, 5));
        let text = selection.text(parser.screen()).unwrap();
        assert!(text.contains("hello 世界"), "{text:?}");
        assert!(text.contains("second"), "{text:?}");
    }

    #[test]
    fn a_click_without_drag_copies_nothing() {
        let parser = screen("hello");
        let selection = Selection::start((0, 1));
        assert_eq!(selection.text(parser.screen()), None);
    }

    #[test]
    fn contains_follows_reading_order() {
        let mut selection = Selection::start((0, 4));
        selection.update((2, 2));
        assert!(selection.contains(0, 4));
        assert!(selection.contains(0, 10));
        assert!(selection.contains(1, 0));
        assert!(selection.contains(2, 2));
        assert!(!selection.contains(0, 3));
        assert!(!selection.contains(2, 3));
    }
}

#[cfg(test)]
mod clamp_tests {
    use super::*;

    #[test]
    fn selection_columns_are_clamped_across_rows() {
        let mut parser = vt100::Parser::new(4, 10, 0);
        parser.process(b"123456789X\r\nabcdefghij");
        let mut selection = Selection::start((0, 14));
        selection.update((1, 14));
        let text = selection.text(parser.screen()).expect("clamped selection");
        assert!(text.contains('X'), "{text:?}");
        assert!(text.contains("abcdefghij"), "{text:?}");
    }

    #[test]
    fn a_single_row_selection_beyond_the_width_copies_the_last_cell() {
        let mut parser = vt100::Parser::new(4, 10, 0);
        parser.process(b"123456789X");
        let mut selection = Selection::start((0, 9));
        selection.update((0, 14));
        assert_eq!(selection.text(parser.screen()).as_deref(), Some("X"));
    }
}
