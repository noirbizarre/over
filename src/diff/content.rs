//! Structured, line-level text diffs (#109).
//!
//! [`ContentDiff`] is plain data — a tag plus the line content for every
//! line `similar` produces, restricted to changed hunks with a little
//! surrounding context (mirrors `git diff`'s default of 3 context lines)
//! rather than the whole file. It carries no terminal formatting: only
//! its [`fmt::Display`] impl (used by the CLI) knows about colors, so a
//! future machine-readable representation (e.g. JSON output) can reuse
//! [`ContentDiff`] directly instead of re-deriving the diff.

use std::fmt;

use similar::{ChangeTag, TextDiff};

use crate::ui::style;

/// What kind of line-level change a [`DiffLine`] represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineTag {
    /// Present, unchanged, on both sides — kept for surrounding context.
    Equal,
    /// Present only on the "actual" side.
    Delete,
    /// Present only on the "desired" side.
    Insert,
}

/// One line of a [`ContentDiff`], tagged with how it differs.
#[derive(Debug, Clone)]
pub struct DiffLine {
    pub tag: LineTag,
    pub content: String,
}

/// A structured diff between two texts — `None` when either side isn't
/// valid UTF-8, mirroring git's own "binary files differ" fallback rather
/// than diffing raw bytes as if they were text.
#[derive(Debug, Clone)]
pub struct ContentDiff {
    pub lines: Option<Vec<DiffLine>>,
}

impl ContentDiff {
    /// Build a line-level diff of `old` (actual) vs `new` (desired),
    /// grouped into changed hunks with 3 lines of context.
    pub(crate) fn from_texts(old: &str, new: &str) -> Self {
        let diff = TextDiff::from_lines(old, new);
        let mut lines = Vec::new();
        for group in diff.grouped_ops(3) {
            for op in &group {
                for change in diff.iter_changes(op) {
                    lines.push(DiffLine {
                        tag: match change.tag() {
                            ChangeTag::Equal => LineTag::Equal,
                            ChangeTag::Delete => LineTag::Delete,
                            ChangeTag::Insert => LineTag::Insert,
                        },
                        // `similar` keeps the trailing newline in the
                        // change's value for line-mode diffs (so it can
                        // tell whether the last line was newline
                        // terminated) — trim it, our own `Display` adds
                        // one line per `DiffLine` itself.
                        content: change.to_string_lossy().trim_end_matches('\n').to_string(),
                    });
                }
            }
        }
        Self { lines: Some(lines) }
    }

    /// A diff that can't be expressed as text — either side isn't valid
    /// UTF-8.
    pub(crate) fn binary() -> Self {
        Self { lines: None }
    }

    /// Whether this diff actually has anything to show (a binary
    /// fallback, or at least one non-`Equal` line).
    pub fn has_diff(&self) -> bool {
        match &self.lines {
            None => true,
            Some(lines) => lines.iter().any(|l| l.tag != LineTag::Equal),
        }
    }
}

impl fmt::Display for ContentDiff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Some(lines) = &self.lines else {
            return writeln!(f, "  {}", style::white("binary files differ"));
        };
        for line in lines {
            match line.tag {
                LineTag::Equal => writeln!(f, "  {}", line.content)?,
                LineTag::Delete => writeln!(f, "{}", style::red(format!("- {}", line.content)))?,
                LineTag::Insert => writeln!(f, "{}", style::green(format!("+ {}", line.content)))?,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_texts_produce_no_changes() {
        let diff = ContentDiff::from_texts("a\nb\nc\n", "a\nb\nc\n");
        assert!(!diff.has_diff());
        let lines = diff.lines.unwrap();
        assert!(lines.iter().all(|l| l.tag == LineTag::Equal));
    }

    #[test]
    fn changed_line_produces_delete_and_insert() {
        let diff = ContentDiff::from_texts("old content\n", "new content\n");
        assert!(diff.has_diff());
        let lines = diff.lines.unwrap();
        assert!(
            lines
                .iter()
                .any(|l| l.tag == LineTag::Delete && l.content == "old content")
        );
        assert!(
            lines
                .iter()
                .any(|l| l.tag == LineTag::Insert && l.content == "new content")
        );
    }

    #[test]
    fn single_line_without_trailing_newline_diffs_cleanly() {
        // Symlink target paths have no trailing newline — make sure a
        // bare one-line-each diff still works and doesn't leak a stray
        // newline into `content`.
        let diff = ContentDiff::from_texts("/old/target", "/new/target");
        let lines = diff.lines.unwrap();
        assert!(lines.iter().any(|l| l.content == "/old/target"));
        assert!(lines.iter().any(|l| l.content == "/new/target"));
    }

    #[test]
    fn binary_diff_has_no_lines_but_reports_a_diff() {
        let diff = ContentDiff::binary();
        assert!(diff.lines.is_none());
        assert!(diff.has_diff());
    }

    #[test]
    fn display_prints_binary_marker() {
        let diff = ContentDiff::binary();
        let s = format!("{diff}");
        assert!(s.contains("binary files differ"));
    }

    #[test]
    fn display_prints_added_and_removed_lines() {
        let diff = ContentDiff::from_texts("old\n", "new\n");
        let s = format!("{diff}");
        assert!(s.contains("- old"));
        assert!(s.contains("+ new"));
    }

    #[test]
    fn display_prints_context_lines_plain() {
        let diff = ContentDiff::from_texts("same\nold\n", "same\nnew\n");
        let s = format!("{diff}");
        assert!(s.contains("same"));
    }
}
