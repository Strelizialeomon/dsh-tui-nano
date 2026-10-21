//! `@file` mention grammar shared with the official provider.
//!
//! An `@path` token triggers completion only at the start of a line or after
//! whitespace; an `@` inside another token, such as an email address, does not.
//! `@"path with spaces` keeps the quote open while a directory is being
//! descended. Selecting a candidate only inserts prompt text; it never reads
//! the file.

/// One active completion token at the cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtToken {
    /// Byte offset where the token starts (including `@` and any quote).
    pub start: usize,
    /// The exact token prefix, such as `@` or `@"`.
    pub prefix: String,
    /// Text following the prefix, up to the cursor.
    pub query: String,
    /// Whether the token is explicitly quoted.
    pub quoted: bool,
}

/// One path-only candidate returned by the official provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateKind {
    /// A regular file; selecting it finishes the mention.
    File,
    /// A directory; selecting it keeps the mention open for another level.
    Directory,
}

/// One path-only completion candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileReferenceCandidate {
    /// User-facing path accepted by normal prompts and filesystem tools.
    pub path: String,
    /// File or directory.
    pub kind: CandidateKind,
}

/// Find the active `@` token ending at `cursor_col` within `line`.
///
/// This mirrors the official browser-safe grammar: a quoted token wins over a
/// plain one, and the token must be at the start of the line or after
/// whitespace.
pub fn active_at_token(line: &str, cursor_col: usize) -> Option<AtToken> {
    if cursor_col > line.len() || !line.is_char_boundary(cursor_col) {
        return None;
    }
    let before = &line[..cursor_col];
    if let Some(token) = quoted_token(before) {
        return Some(token);
    }
    plain_token(before)
}

/// Format a selected path as prompt text. Whitespace uses the quoted grammar;
/// a directory keeps the quote open so completion can descend another level.
/// Returns `None` for a path the editor grammar cannot represent safely.
pub fn format_file_mention(
    candidate: &FileReferenceCandidate,
    preserve_quote: bool,
) -> Option<String> {
    let path = match candidate.kind {
        CandidateKind::Directory => format!("{}/", candidate.path),
        CandidateKind::File => candidate.path.clone(),
    };
    if path
        .chars()
        .any(|character| character.is_control() || character == '"')
    {
        return None;
    }
    let needs_quote = preserve_quote || path.chars().any(char::is_whitespace);
    if !needs_quote {
        return Some(format!("@{path}"));
    }
    match candidate.kind {
        CandidateKind::Directory => Some(format!("@\"{path}")),
        CandidateKind::File => Some(format!("@\"{path}\"")),
    }
}

/// Find an unclosed quoted token at the end of `before`.
fn quoted_token(before: &str) -> Option<AtToken> {
    let mut search_from = before.len();
    while let Some(relative) = before[..search_from].rfind("@\"") {
        let start = relative;
        let after_prefix = &before[start + 2..];
        if !after_prefix.contains('"') {
            let preceded_ok = start == 0
                || before[..start]
                    .chars()
                    .next_back()
                    .is_some_and(char::is_whitespace);
            if preceded_ok {
                return Some(AtToken {
                    start,
                    prefix: "@\"".to_string(),
                    query: after_prefix.to_string(),
                    quoted: true,
                });
            }
        }
        search_from = relative;
        if search_from == 0 {
            break;
        }
        // Step one character back so an earlier `@"` can still match.
        search_from -= before[..search_from]
            .chars()
            .next_back()
            .map(char::len_utf8)
            .unwrap_or(1);
    }
    None
}

/// Find a plain `@token` at the end of `before`.
fn plain_token(before: &str) -> Option<AtToken> {
    let start = before
        .char_indices()
        .rev()
        .find(|(_, character)| character.is_whitespace())
        .map(|(offset, character)| offset + character.len_utf8())
        .unwrap_or(0);
    let token = &before[start..];
    let rest = token.strip_prefix('@')?;
    if rest.starts_with('"') {
        return None;
    }
    Some(AtToken {
        start,
        prefix: "@".to_string(),
        query: rest.to_string(),
        quoted: false,
    })
}

/// Find the line containing `cursor` and its column, returning both slices.
pub fn current_line(text: &str, cursor: usize) -> (usize, &str, usize) {
    let cursor = cursor.min(text.len());
    let start = text[..cursor].rfind('\n').map_or(0, |offset| offset + 1);
    let end = text[cursor..]
        .find('\n')
        .map_or(text.len(), |offset| cursor + offset);
    (start, &text[start..end], cursor - start)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(line: &str, cursor: usize) -> Option<(usize, String, bool)> {
        active_at_token(line, cursor).map(|token| (token.start, token.query, token.quoted))
    }

    #[test]
    fn triggers_at_start_and_after_whitespace_only() {
        assert_eq!(token("@src", 4), Some((0, "src".to_string(), false)));
        assert_eq!(token("see @src", 8), Some((4, "src".to_string(), false)));
        assert_eq!(token("mail@example.com", 16), None);
        assert_eq!(token("see mail@example", 15), None);
        assert_eq!(token("  ", 2), None);
    }

    #[test]
    fn quoted_token_keeps_spaces_and_wins_over_plain() {
        assert_eq!(
            token("@\"path with", 11),
            Some((0, "path with".to_string(), true))
        );
        assert_eq!(token("see @\"a b", 9), Some((4, "a b".to_string(), true)));
        assert_eq!(token("@\"closed\"", 9), None);
    }

    #[test]
    fn directory_candidates_keep_the_quote_open() {
        let directory = FileReferenceCandidate {
            path: "src/my dir".to_string(),
            kind: CandidateKind::Directory,
        };
        assert_eq!(
            format_file_mention(&directory, false).as_deref(),
            Some("@\"src/my dir/")
        );
        let plain = FileReferenceCandidate {
            path: "src".to_string(),
            kind: CandidateKind::Directory,
        };
        assert_eq!(format_file_mention(&plain, false).as_deref(), Some("@src/"));
        assert_eq!(
            format_file_mention(&plain, true).as_deref(),
            Some("@\"src/")
        );
    }

    #[test]
    fn absolute_paths_are_preserved() {
        let candidate = FileReferenceCandidate {
            path: "/tmp/src/main.rs".to_string(),
            kind: CandidateKind::File,
        };
        assert_eq!(
            format_file_mention(&candidate, false).as_deref(),
            Some("@/tmp/src/main.rs")
        );
    }

    #[test]
    fn unsafe_paths_are_refused() {
        for path in ["bad\u{7}", "bad\"quote", "bad\u{85}"] {
            let candidate = FileReferenceCandidate {
                path: path.to_string(),
                kind: CandidateKind::File,
            };
            assert!(format_file_mention(&candidate, false).is_none());
        }
    }

    #[test]
    fn a_quoted_mention_can_descent_into_a_directory() {
        assert_eq!(token("@\"src/", 6), Some((0, "src/".to_string(), true)));
        assert_eq!(
            token("@\"src/my dir/", 13),
            Some((0, "src/my dir/".to_string(), true))
        );
    }

    #[test]
    fn current_line_uses_byte_offsets() {
        let text = "first\nsee @sr\nlast";
        let cursor = text.find("@sr").unwrap() + 3;
        let (start, line, column) = current_line(text, cursor);
        assert_eq!(start, 6);
        assert_eq!(line, "see @sr");
        assert_eq!(column, 7);
        assert_eq!(active_at_token(line, column).unwrap().query, "sr");
    }
}
