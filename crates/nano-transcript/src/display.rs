//! Display-cell measurement and wrapping.
//!
//! Every visible-line budget in S03 is counted after terminal wrapping, not
//! after source newlines. Wide characters are never split, control characters
//! are neutralised for display, and the original bytes stay in the conversation
//! record. Copying the transcript therefore keeps the real content; only the
//! screen representation is normalised.

use unicode_width::UnicodeWidthChar;

/// Maximum grapheme-less safety bound per source line. Long lines are still
/// wrapped; the bound only keeps a hostile line from producing unbounded rows.
pub const MAX_WRAPPED_ROWS_PER_LINE: usize = 4_000;

/// Display width of one character; control characters count as zero.
pub fn char_width(character: char) -> usize {
    UnicodeWidthChar::width(character).unwrap_or(0)
}

/// Display width of one line after control-character normalisation.
pub fn line_width(line: &str) -> usize {
    line.chars().map(char_width).sum()
}

/// Replace control characters that would move the terminal cursor. Tabs become
/// spaces (their width is then measurable), `\r` is dropped, and other control
/// characters become a visible replacement so a log cannot inject escapes.
pub fn display_text(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\n' => output.push('\n'),
            '\t' => output.push_str("    "),
            '\r' => {}
            character if character.is_control() => output.push('�'),
            character => output.push(character),
        }
    }
    output
}

/// Append source text split into display-width rows, preserving every
/// character. `width` is the usable terminal width; a width of zero is treated
/// as one column.
pub fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let text = display_text(text);
    let mut rows = Vec::new();
    for line in text.split('\n') {
        rows.extend(wrap_line(line, width));
    }
    if rows.is_empty() {
        rows.push(String::new());
    }
    rows
}

/// Wrap one logical line into display rows without splitting a wide character.
pub fn wrap_line(line: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    if line.is_empty() {
        return vec![String::new()];
    }
    let mut rows = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;
    for character in line.chars() {
        let character_width = char_width(character);
        if current_width + character_width > width && !current.is_empty() {
            rows.push(std::mem::take(&mut current));
            current_width = 0;
            if rows.len() >= MAX_WRAPPED_ROWS_PER_LINE {
                break;
            }
        }
        current.push(character);
        current_width += character_width;
        if current_width >= width {
            // Fill the row exactly; the next character starts a new row.
            rows.push(std::mem::take(&mut current));
            current_width = 0;
            if rows.len() >= MAX_WRAPPED_ROWS_PER_LINE {
                break;
            }
        }
    }
    if !current.is_empty() || rows.is_empty() {
        rows.push(current);
    }
    rows
}

/// Number of display rows one source text occupies at a width.
pub fn row_count(text: &str, width: usize) -> usize {
    wrap_text(text, width).len()
}

/// Whether a source text contains at least one non-whitespace character.
pub fn has_visible_text(text: &str) -> bool {
    text.chars().any(|character| !character.is_whitespace())
}

/// Cut a display row to at most `width` columns, appending an ellipsis when it
/// was shortened. Used only for headers and summaries; block bodies wrap.
pub fn elide(text: &str, width: usize) -> String {
    let width = width.max(1);
    let text = display_text(text).replace('\n', " ");
    if line_width(&text) <= width {
        return text;
    }
    let mut output = String::new();
    let mut used = 0usize;
    for character in text.chars() {
        let character_width = char_width(character);
        if used + character_width + 1 > width {
            break;
        }
        output.push(character);
        used += character_width;
    }
    output.push('…');
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_characters_are_never_split() {
        let rows = wrap_line("你好世界", 3);
        assert_eq!(rows, vec!["你", "好", "世", "界"]);
        assert!(rows.iter().all(|row| line_width(row) <= 3));
    }

    #[test]
    fn a_tab_is_measured_as_four_columns() {
        assert_eq!(line_width(&display_text("\t")), 4);
        let rows = wrap_line(&display_text("\tx"), 4);
        assert_eq!(rows, vec!["    ", "x"]);
    }

    #[test]
    fn control_characters_do_not_reach_the_terminal() {
        let text = display_text("a\u{1b}[31mb\r\n\ttail");
        assert!(!text.contains('\u{1b}'));
        assert!(!text.contains('\r'));
        assert!(text.contains("[31m"));
    }

    #[test]
    fn an_empty_source_line_still_occupies_one_row() {
        assert_eq!(wrap_line("", 10), vec![String::new()]);
        assert_eq!(wrap_text("", 10).len(), 1);
    }

    #[test]
    fn a_long_single_line_counts_its_wrapped_rows() {
        assert_eq!(row_count(&"x".repeat(25), 10), 3);
        assert_eq!(row_count("你好", 10), 1);
    }

    #[test]
    fn elide_respects_cell_width() {
        assert_eq!(elide("你好世界", 5), "你好…");
        assert_eq!(elide("abc", 5), "abc");
    }
}
