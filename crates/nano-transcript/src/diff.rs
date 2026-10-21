//! File diffs for tool cards.
//!
//! The official `FileDiff`/`FsDiffMeta` payload carries `path`, `oldText`, and
//! `newText`, but no per-line coordinates and no add/delete counters. S03
//! computes the counters locally from the two texts and leaves the 4ch line
//! slot empty because there is no official patch coordinate to print.

use serde_json::Value;

/// Largest pair of changed-line blocks diffed line by line. Beyond this the
/// card keeps the coarse state instead of spending unbounded CPU.
const MAX_DIFF_PRODUCT: usize = 300_000;

/// Largest number of source lines retained per side for a line diff.
const MAX_DIFF_LINES: usize = 2_000;

/// Whether the card shows a not-yet-applied call or an applied result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiffState {
    /// Derivation from call arguments; the tool has not returned yet.
    Pending,
    /// The official result carried applied hunks.
    Applied,
    /// The call failed before the change was applied.
    NotApplied,
    /// The result carried no usable diff metadata.
    Unknown,
}

impl DiffState {
    /// Short user-facing label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "待应用",
            Self::Applied => "已应用",
            Self::NotApplied => "未应用",
            Self::Unknown => "差异未知",
        }
    }
}

/// One rendered diff line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    /// Context, addition, or deletion.
    pub kind: DiffLineKind,
    /// Official old line number, only when a real patch coordinate exists.
    pub old_no: Option<u64>,
    /// Official new line number, only when a real patch coordinate exists.
    pub new_no: Option<u64>,
    /// Line text without its trailing newline.
    pub text: String,
    /// Character range of the changed fragment inside `text`, when a paired
    /// delete/add line gives a local word-level emphasis.
    pub emphasis: Option<(usize, usize)>,
}

/// One line's role in a diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiffLineKind {
    /// Unchanged context.
    Context,
    /// Added line.
    Add,
    /// Deleted line.
    Delete,
}

impl DiffLineKind {
    /// Two-column marker used by the design contract.
    pub fn marker(self) -> &'static str {
        match self {
            Self::Context => " ",
            Self::Add => "+",
            Self::Delete => "-",
        }
    }
}

/// One hunk; a call may change several separate regions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffHunk {
    /// Old source line count in this hunk, used for the separator label.
    pub old_lines: usize,
    /// New source line count in this hunk.
    pub new_lines: usize,
    /// Diff lines in file order.
    pub lines: Vec<DiffLine>,
}

/// One file change card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffView {
    /// Official path, when the result or arguments carry one.
    pub path: Option<String>,
    /// Locally computed added-line count; `None` when the result cannot know
    /// it (for example a `write` overwrite without a before-image).
    pub added: Option<u64>,
    /// Locally computed deleted-line count; `None` when no before-image exists.
    pub deleted: Option<u64>,
    /// Pending or applied.
    pub state: DiffState,
    /// One entry per changed region.
    pub hunks: Vec<DiffHunk>,
    /// Total rendered lines across hunks, excluding separators.
    pub total_lines: usize,
    /// The diff was too large for a local line comparison.
    pub coarse: bool,
}

impl DiffView {
    /// Header statistics: `+N -M`, omitting a count the payload cannot know.
    pub fn stat_label(&self) -> String {
        let mut label = String::new();
        if let Some(added) = self.added {
            label.push('+');
            label.push_str(&added.to_string());
        }
        if let Some(deleted) = self.deleted {
            if !label.is_empty() {
                label.push(' ');
            }
            label.push('-');
            label.push_str(&deleted.to_string());
        }
        if label.is_empty() {
            label.push_str("±?");
        }
        label
    }

    /// Whether any hunk exists to render.
    pub fn is_empty(&self) -> bool {
        self.hunks.iter().all(|hunk| hunk.lines.is_empty())
    }

    /// Derive a pending diff from `write`/`edit` call arguments.
    pub fn from_call_arguments(arguments: &str, name: &str) -> Option<Self> {
        let value: Value = serde_json::from_str(arguments).ok()?;
        match name {
            "write" => {
                let path = value.get("file_path").and_then(Value::as_str)?;
                let content = value.get("content").and_then(Value::as_str)?;
                Some(single_text_diff(
                    Some(path.to_string()),
                    None,
                    Some(content.to_string()),
                    DiffState::Pending,
                ))
            }
            "edit" => {
                let path = value.get("file_path").and_then(Value::as_str)?;
                let old = value.get("old_string").and_then(Value::as_str)?;
                let new = value.get("new_string").and_then(Value::as_str)?;
                Some(single_text_diff(
                    Some(path.to_string()),
                    Some(old.to_string()),
                    Some(new.to_string()),
                    DiffState::Pending,
                ))
            }
            _ => None,
        }
    }

    /// Derive an applied diff from the official `tool/result.meta`.
    pub fn from_result_meta(meta: &Value, state: DiffState) -> Option<Self> {
        let diffs = meta.get("diffs")?.as_array()?;
        if diffs.is_empty() {
            return None;
        }
        let mut hunks = Vec::new();
        let mut path = None;
        let mut added = Some(0u64);
        let mut deleted = Some(0u64);
        let mut total_lines = 0usize;
        let mut coarse = false;
        for entry in diffs {
            let entry_path = entry
                .get("path")
                .and_then(Value::as_str)
                .map(str::to_string);
            if path.is_none() {
                path = entry_path;
            }
            let old = entry
                .get("oldText")
                .and_then(Value::as_str)
                .map(str::to_string);
            let new = entry
                .get("newText")
                .and_then(Value::as_str)
                .map(str::to_string);
            let view = single_text_diff(None, old, new, state);
            added = match (added, view.added) {
                (Some(left), Some(right)) => Some(left + right),
                _ => None,
            };
            deleted = match (deleted, view.deleted) {
                (Some(left), Some(right)) => Some(left + right),
                _ => None,
            };
            total_lines += view.total_lines;
            coarse |= view.coarse;
            hunks.extend(view.hunks);
        }
        Some(Self {
            path,
            added,
            deleted,
            state,
            hunks,
            total_lines,
            coarse,
        })
    }
}

/// Build one hunk set from a before/after pair. `old = None` means the before
/// image is unavailable (a create or an overwrite without a prior read).
pub fn single_text_diff(
    path: Option<String>,
    old: Option<String>,
    new: Option<String>,
    state: DiffState,
) -> DiffView {
    let Some(new_text) = new else {
        return DiffView {
            path,
            added: None,
            deleted: None,
            state,
            hunks: Vec::new(),
            total_lines: 0,
            coarse: false,
        };
    };
    let new_lines = source_lines(&new_text);
    match old {
        None => {
            let lines = new_lines
                .iter()
                .map(|line| DiffLine {
                    kind: DiffLineKind::Add,
                    old_no: None,
                    new_no: None,
                    text: (*line).to_string(),
                    emphasis: None,
                })
                .collect::<Vec<_>>();
            let total_lines = lines.len();
            DiffView {
                path,
                // A write overwrite has no before-image: additions are known,
                // deletions are not, and `-0` would be a fabricated fact.
                added: Some(total_lines as u64),
                deleted: None,
                state,
                hunks: vec![DiffHunk {
                    old_lines: 0,
                    new_lines: new_lines.len(),
                    lines,
                }],
                total_lines,
                coarse: false,
            }
        }
        Some(old_text) => {
            let old_lines = source_lines(&old_text);
            let mut added = 0u64;
            let mut deleted = 0u64;
            let mut lines = Vec::new();
            let mut coarse = false;
            if old_lines.len() > MAX_DIFF_LINES || new_lines.len() > MAX_DIFF_LINES {
                coarse = true;
                lines.extend(old_lines.iter().map(|line| DiffLine {
                    kind: DiffLineKind::Delete,
                    old_no: None,
                    new_no: None,
                    text: (*line).to_string(),
                    emphasis: None,
                }));
                lines.extend(new_lines.iter().map(|line| DiffLine {
                    kind: DiffLineKind::Add,
                    old_no: None,
                    new_no: None,
                    text: (*line).to_string(),
                    emphasis: None,
                }));
                added = new_lines.len() as u64;
                deleted = old_lines.len() as u64;
            } else {
                let operations = line_operations(&old_lines, &new_lines);
                for operation in operations {
                    match operation {
                        Operation::Context(text) => lines.push(DiffLine {
                            kind: DiffLineKind::Context,
                            old_no: None,
                            new_no: None,
                            text,
                            emphasis: None,
                        }),
                        Operation::Delete(text) => {
                            deleted += 1;
                            lines.push(DiffLine {
                                kind: DiffLineKind::Delete,
                                old_no: None,
                                new_no: None,
                                text,
                                emphasis: None,
                            });
                        }
                        Operation::Add(text) => {
                            added += 1;
                            lines.push(DiffLine {
                                kind: DiffLineKind::Add,
                                old_no: None,
                                new_no: None,
                                text,
                                emphasis: None,
                            });
                        }
                    }
                }
            }
            mark_emphasis(&mut lines);
            let total_lines = lines.len();
            DiffView {
                path,
                added: Some(added),
                deleted: Some(deleted),
                state,
                hunks: vec![DiffHunk {
                    old_lines: old_lines.len(),
                    new_lines: new_lines.len(),
                    lines,
                }],
                total_lines,
                coarse,
            }
        }
    }
}

/// Pair delete/add runs and mark the changed middle of each paired line.
fn mark_emphasis(lines: &mut [DiffLine]) {
    let mut index = 0usize;
    while index < lines.len() {
        if lines[index].kind != DiffLineKind::Delete {
            index += 1;
            continue;
        }
        let delete_start = index;
        while index < lines.len() && lines[index].kind == DiffLineKind::Delete {
            index += 1;
        }
        let add_start = index;
        while index < lines.len() && lines[index].kind == DiffLineKind::Add {
            index += 1;
        }
        let delete_count = add_start - delete_start;
        let add_count = index - add_start;
        for offset in 0..delete_count.min(add_count) {
            let old = lines[delete_start + offset].text.clone();
            let new = lines[add_start + offset].text.clone();
            let (prefix, suffix) = common_prefix_suffix(&old, &new);
            let old_chars = old.chars().count();
            let new_chars = new.chars().count();
            let old_end = old_chars.saturating_sub(suffix);
            let new_end = new_chars.saturating_sub(suffix);
            if prefix < old_end {
                lines[delete_start + offset].emphasis = Some((prefix, old_end));
            }
            if prefix < new_end {
                lines[add_start + offset].emphasis = Some((prefix, new_end));
            }
        }
    }
}

fn common_prefix_suffix(old: &str, new: &str) -> (usize, usize) {
    let old_chars: Vec<char> = old.chars().collect();
    let new_chars: Vec<char> = new.chars().collect();
    let mut prefix = 0usize;
    while prefix < old_chars.len()
        && prefix < new_chars.len()
        && old_chars[prefix] == new_chars[prefix]
    {
        prefix += 1;
    }
    let mut suffix = 0usize;
    while suffix < old_chars.len().saturating_sub(prefix)
        && suffix < new_chars.len().saturating_sub(prefix)
        && old_chars[old_chars.len() - 1 - suffix] == new_chars[new_chars.len() - 1 - suffix]
    {
        suffix += 1;
    }
    (prefix, suffix)
}

fn source_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = text.split('\n').collect();
    if text.ends_with('\n') {
        lines.pop();
    }
    lines
}

enum Operation {
    Context(String),
    Add(String),
    Delete(String),
}

/// Longest-common-subsequence line diff with common prefix/suffix trimming.
/// The middle product is bounded; oversized inputs fall back to a whole-block
/// replacement, which is honest but coarse.
fn line_operations(old: &[&str], new: &[&str]) -> Vec<Operation> {
    let mut prefix = 0usize;
    while prefix < old.len() && prefix < new.len() && old[prefix] == new[prefix] {
        prefix += 1;
    }
    let mut suffix = 0usize;
    while suffix < old.len() - prefix
        && suffix < new.len() - prefix
        && old[old.len() - 1 - suffix] == new[new.len() - 1 - suffix]
    {
        suffix += 1;
    }
    let old_middle = &old[prefix..old.len() - suffix];
    let new_middle = &new[prefix..new.len() - suffix];
    let mut operations = Vec::new();
    for line in &old[..prefix] {
        operations.push(Operation::Context((*line).to_string()));
    }
    if old_middle.len().saturating_mul(new_middle.len()) > MAX_DIFF_PRODUCT {
        for line in old_middle {
            operations.push(Operation::Delete((*line).to_string()));
        }
        for line in new_middle {
            operations.push(Operation::Add((*line).to_string()));
        }
    } else {
        operations.extend(lcs_operations(old_middle, new_middle));
    }
    for line in &old[old.len() - suffix..] {
        operations.push(Operation::Context((*line).to_string()));
    }
    operations
}

fn lcs_operations(old: &[&str], new: &[&str]) -> Vec<Operation> {
    let rows = old.len() + 1;
    let columns = new.len() + 1;
    let mut table = vec![0u32; rows * columns];
    for i in (0..old.len()).rev() {
        for j in (0..new.len()).rev() {
            table[i * columns + j] = if old[i] == new[j] {
                table[(i + 1) * columns + j + 1] + 1
            } else {
                table[(i + 1) * columns + j].max(table[i * columns + j + 1])
            };
        }
    }
    let mut operations = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < old.len() && j < new.len() {
        if old[i] == new[j] {
            operations.push(Operation::Context(old[i].to_string()));
            i += 1;
            j += 1;
        } else if table[(i + 1) * columns + j] >= table[i * columns + j + 1] {
            operations.push(Operation::Delete(old[i].to_string()));
            i += 1;
        } else {
            operations.push(Operation::Add(new[j].to_string()));
            j += 1;
        }
    }
    while i < old.len() {
        operations.push(Operation::Delete(old[i].to_string()));
        i += 1;
    }
    while j < new.len() {
        operations.push(Operation::Add(new[j].to_string()));
        j += 1;
    }
    operations
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn stats_are_computed_locally_from_before_and_after() {
        let diff = single_text_diff(
            Some("a.rs".into()),
            Some("one\ntwo\nthree".into()),
            Some("one\nTWO\nthree\nfour".into()),
            DiffState::Pending,
        );
        assert_eq!(diff.added, Some(2));
        assert_eq!(diff.deleted, Some(1));
        assert_eq!(diff.stat_label(), "+2 -1");
        assert_eq!(diff.state, DiffState::Pending);
    }

    #[test]
    fn a_write_without_a_before_image_never_fabricates_a_deletion_count() {
        let diff = single_text_diff(None, None, Some("a\nb\n".into()), DiffState::Applied);
        assert_eq!(diff.added, Some(2));
        assert_eq!(diff.deleted, None);
        assert_eq!(diff.stat_label(), "+2");
        assert!(diff
            .hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .all(|line| line.kind == DiffLineKind::Add));
    }

    #[test]
    fn official_diff_metadata_parses_and_keeps_no_fake_coordinates() {
        let meta = json!({
            "operation": "update",
            "diffs": [{
                "path": "src/lib.rs",
                "oldText": "a\nb\nc\nd\n",
                "newText": "a\nB\nc\nd\n"
            }]
        });
        let diff = DiffView::from_result_meta(&meta, DiffState::Applied).unwrap();
        assert_eq!(diff.state, DiffState::Applied);
        assert_eq!(diff.path.as_deref(), Some("src/lib.rs"));
        assert_eq!(diff.added, Some(1));
        assert_eq!(diff.deleted, Some(1));
        assert!(diff
            .hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .all(|line| line.old_no.is_none() && line.new_no.is_none()));
    }

    #[test]
    fn a_null_old_text_in_result_metadata_marks_deletions_unknown() {
        let meta = json!({"diffs": [{"path": "new.txt", "oldText": null, "newText": "x\n"}]});
        let diff = DiffView::from_result_meta(&meta, DiffState::Applied).unwrap();
        assert_eq!(diff.added, Some(1));
        assert_eq!(diff.deleted, None);
    }

    #[test]
    fn edit_arguments_become_a_pending_diff() {
        let diff = DiffView::from_call_arguments(
            r#"{"file_path":"a.txt","old_string":"old","new_string":"new"}"#,
            "edit",
        )
        .unwrap();
        assert_eq!(diff.state, DiffState::Pending);
        assert_eq!(diff.path.as_deref(), Some("a.txt"));
        assert_eq!(diff.added, Some(1));
        assert_eq!(diff.deleted, Some(1));
    }

    #[test]
    fn oversized_inputs_fall_back_to_coarse_instead_of_unbounded_work() {
        let old = "a\n".repeat(MAX_DIFF_LINES + 10);
        let new = "b\n".repeat(MAX_DIFF_LINES + 10);
        let diff = single_text_diff(None, Some(old), Some(new), DiffState::Pending);
        assert!(diff.coarse);
        assert_eq!(diff.added, Some((MAX_DIFF_LINES + 10) as u64));
        assert_eq!(diff.deleted, Some((MAX_DIFF_LINES + 10) as u64));
    }

    #[test]
    fn an_empty_write_has_no_added_line() {
        let diff = single_text_diff(
            Some("empty.txt".into()),
            None,
            Some(String::new()),
            DiffState::Applied,
        );
        assert_eq!(diff.added, Some(0));
        assert_eq!(diff.deleted, None);
        assert_eq!(diff.total_lines, 0);
    }

    #[test]
    fn an_edited_pair_marks_the_changed_fragment() {
        let diff = single_text_diff(
            None,
            Some("hello world".into()),
            Some("hello there".into()),
            DiffState::Pending,
        );
        let lines = &diff.hunks[0].lines;
        let delete = lines
            .iter()
            .find(|line| line.kind == DiffLineKind::Delete)
            .unwrap();
        let add = lines
            .iter()
            .find(|line| line.kind == DiffLineKind::Add)
            .unwrap();
        assert_eq!(delete.emphasis, Some((6, 11)));
        assert_eq!(add.emphasis, Some((6, 11)));
    }

    #[test]
    fn common_prefix_and_suffix_are_context_lines() {
        let diff = single_text_diff(
            None,
            Some("a\nb\nc".into()),
            Some("a\nx\nc".into()),
            DiffState::Pending,
        );
        let kinds: Vec<_> = diff.hunks[0].lines.iter().map(|line| line.kind).collect();
        assert_eq!(
            kinds,
            vec![
                DiffLineKind::Context,
                DiffLineKind::Delete,
                DiffLineKind::Add,
                DiffLineKind::Context
            ]
        );
    }
}
