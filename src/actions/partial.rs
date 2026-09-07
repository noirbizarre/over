//! Partial file management (#66): a managed block injected into an
//! existing (possibly foreign) file, delimited by marker comment lines,
//! discovered from `*.partial.{toml,yaml,yml}` sidecars — mirroring the
//! `.link.*` sidecar convention (`src/actions/symlink.rs`) as closely as
//! possible.
//!
//! Deliberately independent of #61 (full file templating): `content` is
//! used verbatim, never rendered. #65 does resolve a hierarchical
//! `permissions`/`defaults.mode` rule for the target's declared mode (see
//! `crate::overlays::permissions` and ADR-020) — the one hierarchical-rule
//! concern this module honors, applied via [`EnsurePartialBlock::with_permissions`].
//!
//! v1 assumes a `#`-comment target file (shell rc files, ini-style
//! configs, gitconfig, ssh config...) — the marker line format is fixed,
//! not configurable. A target file that can't contain `#` comments (JSON,
//! for instance) simply isn't a good fit for this feature yet.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as AnyhowContext, Result};
use async_trait::async_trait;
use dialoguer::Select;

use noyalib::compat::serde_yaml as serde_yml;
use serde::{Deserialize, Serialize};
use tokio::task::spawn_blocking;

use crate::diff::ContentDiff;
use crate::exec::{Action, Ctx};
use crate::overlays::FileMode;
use crate::plan::actual::{self, ActualState};
use crate::ui;
use crate::ui::style::DialogTheme;
use crate::ui::{emojis, style};
use crate::utils::short_path;

use super::fs::remove_target;

/// A `*.partial.{toml,yaml,yml}` sidecar's contents.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct PartialConfig {
    pub target: String,
    pub content: String,
    /// Identifies this block among possibly several managed in the same
    /// target file. Defaults to the sidecar's own stem (see
    /// `discover_partials`) when absent.
    #[serde(default)]
    pub marker: Option<String>,
}

/// Discover every `*.partial.{toml,yaml,yml}` sidecar under `overlay_root`,
/// returning `(stem, config)` pairs sorted by stem. Mirrors
/// `symlink::discover_symlinks` exactly: same glob, same stem derivation,
/// same TOML > YAML > YML precedence-with-warning on duplicates, same
/// skip-with-warning on an empty `target` (plus, here, an empty `content`
/// too — a managed block with nothing in it is a no-op that shouldn't
/// exist).
pub fn discover_partials(overlay_root: &Path) -> Result<Vec<(String, PartialConfig)>> {
    use globset::GlobBuilder;
    use walkdir::WalkDir;

    let glob = GlobBuilder::new("**/*.partial.{toml,yaml,yml}")
        .literal_separator(true)
        .build()?
        .compile_matcher();

    let mut results: Vec<(String, PartialConfig)> = Vec::new();
    let mut seen_names: HashMap<String, PathBuf> = HashMap::new();

    for entry in WalkDir::new(overlay_root)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        let rel = path.strip_prefix(overlay_root)?;

        if !glob.is_match(rel) {
            continue;
        }

        // Normalize to `/` so partial names are portable identifiers that
        // don't leak the platform's native path separator (`\` on Windows).
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        let stem = rel_str
            .strip_suffix(".partial.toml")
            .or_else(|| rel_str.strip_suffix(".partial.yaml"))
            .or_else(|| rel_str.strip_suffix(".partial.yml"))
            .map(String::from)
            .ok_or_else(|| anyhow::anyhow!("unexpected partial config file: {}", rel.display()))?;

        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read partial config: {}", rel.display()))?;

        let config: PartialConfig = if rel_str.ends_with(".toml") {
            toml::from_str(&raw).with_context(|| format!("failed to parse {}", rel.display()))?
        } else {
            serde_yml::from_str(&raw)
                .with_context(|| format!("failed to parse {}", rel.display()))?
        };

        if config.target.is_empty() {
            tracing::warn!(
                name = %stem,
                path = %rel.display(),
                "empty target in partial config, skipping",
            );
            continue;
        }
        if config.content.is_empty() {
            tracing::warn!(
                name = %stem,
                path = %rel.display(),
                "empty content in partial config, skipping",
            );
            continue;
        }

        if let Some(existing_path) = seen_names.get(&stem) {
            let (toml_path, yaml_path) = if rel.to_string_lossy().ends_with(".toml") {
                (rel.to_path_buf(), existing_path.clone())
            } else {
                (existing_path.clone(), rel.to_path_buf())
            };
            tracing::warn!(
                name = %stem,
                toml = %toml_path.display(),
                yaml = %yaml_path.display(),
                "both TOML and YAML partial configs exist; TOML takes precedence",
            );
            if rel.to_string_lossy().ends_with(".toml") {
                results.retain(|(name, _)| name != &stem);
                seen_names.insert(stem.clone(), rel.to_path_buf());
                results.push((stem, config));
            }
            continue;
        }

        seen_names.insert(stem.clone(), rel.to_path_buf());
        results.push((stem, config));
    }

    results.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(results)
}

// ── marker lines & pure block operations ────────────────────────────────

fn begin_line(marker: &str) -> String {
    format!("# >>> over: {marker} >>>")
}

fn end_line(marker: &str) -> String {
    format!("# <<< over: {marker} <<<")
}

/// Whether — and how — a managed block for `marker` currently exists in a
/// target file's content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockState<'a> {
    /// No begin or end marker line found — nothing to reconcile against;
    /// inserting a fresh block is purely additive.
    Absent,
    /// A complete, well-formed block found; carries its exact content
    /// (excluding the marker lines themselves).
    Found(&'a str),
    /// A begin marker without a matching end marker (or vice versa) —
    /// never touched automatically; always reported as a conflict.
    Malformed,
}

/// Find the byte range `(start, end)` of the first line in `text` (at or
/// after byte offset `from`) whose content exactly equals `needle`. `end`
/// includes the line's trailing `\n`, if any.
fn find_line(text: &str, needle: &str, from: usize) -> Option<(usize, usize)> {
    let mut pos = from;
    while pos <= text.len() {
        let rest = &text[pos..];
        let line_len = match rest.find('\n') {
            Some(i) => i + 1,
            None => rest.len(),
        };
        if line_len == 0 {
            break; // `rest` is empty: reached the end of `text`.
        }
        let line = &rest[..line_len];
        let trimmed = line.strip_suffix('\n').unwrap_or(line);
        if trimmed == needle {
            return Some((pos, pos + line_len));
        }
        pos += line_len;
    }
    None
}

/// Locate the current managed block for `marker` inside `text`. Read-only,
/// pure — used both by classification (does the file already match?) and
/// by unapply (does it still match what we last wrote, right before
/// removing it?).
pub fn find_block<'a>(text: &'a str, marker: &str) -> BlockState<'a> {
    let begin = begin_line(marker);
    let end = end_line(marker);
    match find_line(text, &begin, 0) {
        None => {
            // An end marker with no matching begin is just as unsafe to
            // touch automatically as the reverse.
            if find_line(text, &end, 0).is_some() {
                BlockState::Malformed
            } else {
                BlockState::Absent
            }
        }
        Some((_, begin_end)) => match find_line(text, &end, begin_end) {
            None => BlockState::Malformed,
            Some((end_start, _)) => {
                let content = &text[begin_end..end_start];
                // `end_start` is the byte offset of the end-marker line
                // itself, so `content` always carries the newline that
                // terminated its own last line — strip exactly one.
                BlockState::Found(content.strip_suffix('\n').unwrap_or(content))
            }
        },
    }
}

/// Render a standalone block (marker lines plus `content`), with no
/// surrounding file content — used to create a brand-new target file.
fn block_only(marker: &str, content: &str) -> String {
    append_block("", marker, content)
}

/// Append a fresh block for `marker` to the end of `text` (additive —
/// only ever called when [`find_block`] reported [`BlockState::Absent`]).
/// Adds a separating newline only if `text` is non-empty and doesn't
/// already end in one, so existing content is never disturbed.
pub fn append_block(text: &str, marker: &str, content: &str) -> String {
    let mut out = text.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&begin_line(marker));
    out.push('\n');
    if !content.is_empty() {
        out.push_str(content);
        if !content.ends_with('\n') {
            out.push('\n');
        }
    }
    out.push_str(&end_line(marker));
    out.push('\n');
    out
}

/// Replace an existing block's content in place, preserving every other
/// byte of `text` untouched. Only meaningful after [`find_block`] reported
/// [`BlockState::Found`] — if the markers can't be found (a defensive
/// check against a caller that didn't verify first), `text` is returned
/// unchanged rather than guessing.
pub fn replace_block(text: &str, marker: &str, content: &str) -> String {
    let begin = begin_line(marker);
    let end = end_line(marker);
    let Some((_, begin_end)) = find_line(text, &begin, 0) else {
        return text.to_string();
    };
    let Some((end_start, _)) = find_line(text, &end, begin_end) else {
        return text.to_string();
    };
    let mut out = String::with_capacity(text.len() + content.len());
    out.push_str(&text[..begin_end]);
    if !content.is_empty() {
        out.push_str(content);
        if !content.ends_with('\n') {
            out.push('\n');
        }
    }
    out.push_str(&text[end_start..]);
    out
}

/// Strip a block (marker lines included) from `text`, leaving every other
/// byte untouched. Used only by `unapply`. A no-op (returns `text`
/// unchanged) if the markers can't be found.
pub fn remove_block(text: &str, marker: &str) -> String {
    let begin = begin_line(marker);
    let end = end_line(marker);
    let Some((begin_start, _)) = find_line(text, &begin, 0) else {
        return text.to_string();
    };
    let Some((_, end_end)) = find_line(text, &end, begin_start) else {
        return text.to_string();
    };
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..begin_start]);
    out.push_str(&text[end_end..]);
    out
}

// ── conflict resolution (interactive, force/no_prompt-gated) ────────────

/// Choices for a structural conflict — a directory or symlink sits where
/// a partial-managed file should be. Deliberately smaller than
/// `actions::fs`'s `ConflictChoice`: no "Absorb" (there's no single
/// overlay source file to adopt target content into — the desired content
/// is a literal string in a sidecar) and no "Diff" (nothing file-shaped to
/// diff against yet).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StructuralChoice {
    Skip,
    Overwrite,
}

impl StructuralChoice {
    const ALL: &[StructuralChoice] = &[StructuralChoice::Skip, StructuralChoice::Overwrite];
}

impl fmt::Display for StructuralChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StructuralChoice::Skip => write!(f, "Skip"),
            StructuralChoice::Overwrite => write!(f, "Overwrite (replace with a fresh file)"),
        }
    }
}

fn resolve_structural_conflict(ctx: &Ctx, target: &Path) -> Result<bool> {
    if ctx.force {
        remove_target(target)?;
        return Ok(true);
    }
    if ctx.no_prompt {
        return Err(anyhow::anyhow!(
            "partial-file conflict: '{}' is a directory or symlink, not a plain file \
             (use --force to replace it or run interactively to choose)",
            target.display()
        ));
    }
    // No "loop back and ask again" choice exists here (unlike the block-
    // level conflict below, which has "Diff") — a single prompt suffices.
    let prompt = format!(
        "Conflict: {} exists and isn't a plain file",
        style::yellow(short_path(&target.to_string_lossy())),
    );
    let selection = Select::with_theme(&DialogTheme::default())
        .with_prompt(prompt)
        .default(0)
        .items(StructuralChoice::ALL)
        .interact()
        .map_err(|e| anyhow::anyhow!("prompt failed: {}", e))?;
    match StructuralChoice::ALL[selection] {
        StructuralChoice::Skip => Ok(false),
        StructuralChoice::Overwrite => {
            remove_target(target)?;
            Ok(true)
        }
    }
}

/// Choices for a block-level conflict — the target file exists and has a
/// managed block for this `marker`, but its content doesn't match (or the
/// markers are malformed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockChoice {
    Skip,
    Overwrite,
    Diff,
}

impl BlockChoice {
    const ALL: &[BlockChoice] = &[BlockChoice::Skip, BlockChoice::Overwrite, BlockChoice::Diff];
}

impl fmt::Display for BlockChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BlockChoice::Skip => write!(f, "Skip"),
            BlockChoice::Overwrite => write!(f, "Overwrite (replace the managed block)"),
            BlockChoice::Diff => write!(f, "Diff (show differences, then decide)"),
        }
    }
}

fn resolve_block_conflict(
    ctx: &Ctx,
    target: &Path,
    marker: &str,
    current: &str,
    desired: &str,
) -> Result<bool> {
    if ctx.force {
        return Ok(true);
    }
    if ctx.no_prompt {
        return Err(anyhow::anyhow!(
            "partial-file conflict: managed block '{}' in '{}' doesn't match the overlay's \
             content (use --force to overwrite or run interactively to choose)",
            marker,
            target.display()
        ));
    }
    loop {
        let prompt = format!(
            "Conflict: managed block '{}' in {} doesn't match",
            marker,
            style::yellow(short_path(&target.to_string_lossy())),
        );
        let selection = Select::with_theme(&DialogTheme::default())
            .with_prompt(prompt)
            .default(0)
            .items(BlockChoice::ALL)
            .interact()
            .map_err(|e| anyhow::anyhow!("prompt failed: {}", e))?;
        match BlockChoice::ALL[selection] {
            BlockChoice::Skip => return Ok(false),
            BlockChoice::Overwrite => return Ok(true),
            BlockChoice::Diff => {
                let existing = match find_block(current, marker) {
                    BlockState::Found(x) => x,
                    _ => current,
                };
                ui::info(format!("{}", ContentDiff::from_texts(existing, desired))).ok();
                // Loop back to prompt.
            }
        }
    }
}

// ── the Action itself ────────────────────────────────────────────────────

pub struct EnsurePartialBlock {
    pub target: PathBuf,
    pub marker: String,
    pub content: String,
    /// Desired permission mode (#65), applied to `target` right after any
    /// write this action performs. `None` (the default via `new`) leaves
    /// permissions entirely unmanaged, matching pre-#65 behavior.
    pub permissions: Option<FileMode>,
}

impl EnsurePartialBlock {
    pub fn new(target: PathBuf, marker: String, content: String) -> Self {
        Self {
            target,
            marker,
            content,
            permissions: None,
        }
    }

    /// Declare the permission mode to enforce on `target` after writing
    /// (#65) — a builder rather than a `new()` parameter so the ~15
    /// existing 3-arg call sites (this action predates #65) stay untouched.
    pub fn with_permissions(mut self, permissions: Option<FileMode>) -> Self {
        self.permissions = permissions;
        self
    }
}

impl fmt::Display for EnsurePartialBlock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {} {} ({})",
            emojis::BLOCK,
            style::white("partial block:"),
            short_path(&self.target.to_string_lossy()),
            self.marker,
        )
    }
}

/// Write `data` to `target`, then apply `permissions` (#65) if declared —
/// factored out since `EnsurePartialBlock::execute` writes the target from
/// several distinct branches, all of which must apply the same declared
/// mode.
fn write_target(
    target: &Path,
    data: impl AsRef<[u8]>,
    permissions: Option<FileMode>,
) -> Result<()> {
    fs::write(target, data).with_context(|| format!("failed to write {}", target.display()))?;
    #[cfg(unix)]
    if let Some(mode) = permissions {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(target, fs::Permissions::from_mode(mode.bits()))
            .with_context(|| format!("failed to set permissions on {}", target.display()))?;
    }
    // Permission bits aren't a meaningful concept to enforce here on
    // non-unix platforms (see ADR-020) — `permissions` is simply unused.
    #[cfg(not(unix))]
    let _ = permissions;
    Ok(())
}

#[async_trait]
impl Action for EnsurePartialBlock {
    async fn execute(&self, ctx: Ctx) -> Result<()> {
        if ctx.dry_run {
            return Ok(());
        }
        let target = self.target.clone();
        let marker = self.marker.clone();
        let content = self.content.clone();
        let permissions = self.permissions;
        let ctx2 = ctx.clone();
        spawn_blocking(move || -> Result<()> {
            match actual::inspect(&target)? {
                ActualState::Missing => {
                    if let Some(parent) = target.parent() {
                        fs::create_dir_all(parent).with_context(|| {
                            format!(
                                "failed to create parent directories for {}",
                                target.display()
                            )
                        })?;
                    }
                    write_target(&target, block_only(&marker, &content), permissions)?;
                    Ok(())
                }
                ActualState::Directory | ActualState::Symlink { .. } => {
                    if !resolve_structural_conflict(&ctx2, &target)? {
                        return Ok(()); // Skipped.
                    }
                    write_target(&target, block_only(&marker, &content), permissions)?;
                    Ok(())
                }
                ActualState::File => {
                    let current = fs::read_to_string(&target)
                        .with_context(|| format!("failed to read {}", target.display()))?;
                    match find_block(&current, &marker) {
                        BlockState::Absent => {
                            write_target(
                                &target,
                                append_block(&current, &marker, &content),
                                permissions,
                            )?;
                        }
                        BlockState::Found(existing) if existing == content => {
                            // Already correct — `materialize` is only ever
                            // invoked for `Create`/`Conflict` steps, so this
                            // is a defensive no-op against a classify→
                            // execute race, not the expected path. A
                            // permission-only drift is never routed through
                            // here either (`Operation::Repair` bypasses
                            // `EnsurePartialBlock` entirely — see
                            // `materialize::partial`).
                        }
                        BlockState::Found(_) | BlockState::Malformed => {
                            if !resolve_block_conflict(&ctx2, &target, &marker, &current, &content)?
                            {
                                return Ok(()); // Skipped.
                            }
                            let updated = match find_block(&current, &marker) {
                                BlockState::Found(_) => replace_block(&current, &marker, &content),
                                // Malformed: never delete the stray marker
                                // line(s) automatically — append a fresh,
                                // well-formed block after them instead.
                                _ => append_block(&current, &marker, &content),
                            };
                            write_target(&target, updated, permissions)?;
                        }
                    }
                    Ok(())
                }
            }
        })
        .await?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    use rstest::rstest;

    fn init_test_tracing() {
        let _ = tracing_subscriber::fmt()
            .with_test_writer()
            .with_max_level(tracing::Level::TRACE)
            .try_init();
    }

    // ── find_block ───────────────────────────────────────────────────────

    #[test]
    fn find_block_absent_in_empty_text() {
        assert_eq!(find_block("", "m"), BlockState::Absent);
    }

    #[test]
    fn find_block_absent_when_unrelated_content_present() {
        assert_eq!(
            find_block("just some file\ncontent\n", "m"),
            BlockState::Absent
        );
    }

    #[test]
    fn find_block_found_returns_exact_content() {
        let text = "before\n# >>> over: m >>>\nalias x=y\n# <<< over: m <<<\nafter\n";
        assert_eq!(find_block(text, "m"), BlockState::Found("alias x=y"));
    }

    #[test]
    fn find_block_found_with_multiline_content() {
        let text = "# >>> over: m >>>\nline1\nline2\n# <<< over: m <<<\n";
        assert_eq!(find_block(text, "m"), BlockState::Found("line1\nline2"));
    }

    #[test]
    fn find_block_found_empty_content() {
        let text = "# >>> over: m >>>\n# <<< over: m <<<\n";
        assert_eq!(find_block(text, "m"), BlockState::Found(""));
    }

    #[test]
    fn find_block_malformed_begin_without_end() {
        let text = "# >>> over: m >>>\nalias x=y\n";
        assert_eq!(find_block(text, "m"), BlockState::Malformed);
    }

    #[test]
    fn find_block_malformed_end_without_begin() {
        let text = "alias x=y\n# <<< over: m <<<\n";
        assert_eq!(find_block(text, "m"), BlockState::Malformed);
    }

    #[test]
    fn find_block_distinguishes_different_markers() {
        let text = "# >>> over: a >>>\nfor-a\n# <<< over: a <<<\n# >>> over: b >>>\nfor-b\n# <<< over: b <<<\n";
        assert_eq!(find_block(text, "a"), BlockState::Found("for-a"));
        assert_eq!(find_block(text, "b"), BlockState::Found("for-b"));
    }

    // ── append_block ─────────────────────────────────────────────────────

    #[test]
    fn append_block_to_empty_text() {
        let out = append_block("", "m", "alias x=y");
        assert_eq!(out, "# >>> over: m >>>\nalias x=y\n# <<< over: m <<<\n");
    }

    #[test]
    fn append_block_adds_separating_newline() {
        let out = append_block("existing content", "m", "alias x=y");
        assert_eq!(
            out,
            "existing content\n# >>> over: m >>>\nalias x=y\n# <<< over: m <<<\n"
        );
    }

    #[test]
    fn append_block_does_not_duplicate_trailing_newline() {
        let out = append_block("existing content\n", "m", "alias x=y");
        assert_eq!(
            out,
            "existing content\n# >>> over: m >>>\nalias x=y\n# <<< over: m <<<\n"
        );
    }

    #[test]
    fn append_block_with_empty_content() {
        let out = append_block("", "m", "");
        assert_eq!(out, "# >>> over: m >>>\n# <<< over: m <<<\n");
    }

    // ── replace_block ────────────────────────────────────────────────────

    #[test]
    fn replace_block_preserves_surrounding_content() {
        let text = "before\n# >>> over: m >>>\nold\n# <<< over: m <<<\nafter\n";
        let out = replace_block(text, "m", "new");
        assert_eq!(
            out,
            "before\n# >>> over: m >>>\nnew\n# <<< over: m <<<\nafter\n"
        );
    }

    #[test]
    fn replace_block_no_op_when_markers_absent() {
        let text = "no markers here\n";
        assert_eq!(replace_block(text, "m", "new"), text);
    }

    #[test]
    fn replace_block_no_op_when_end_marker_missing() {
        let text = "before\n# >>> over: m >>>\nold, no end marker\n";
        assert_eq!(replace_block(text, "m", "new"), text);
    }

    #[test]
    fn replace_block_with_empty_content() {
        let text = "before\n# >>> over: m >>>\nold\n# <<< over: m <<<\nafter\n";
        let out = replace_block(text, "m", "");
        assert_eq!(out, "before\n# >>> over: m >>>\n# <<< over: m <<<\nafter\n");
    }

    // ── remove_block ─────────────────────────────────────────────────────

    #[test]
    fn remove_block_strips_markers_and_content() {
        let text = "before\n# >>> over: m >>>\nold\n# <<< over: m <<<\nafter\n";
        assert_eq!(remove_block(text, "m"), "before\nafter\n");
    }

    #[test]
    fn remove_block_leaves_only_content_when_block_is_everything() {
        let text = "# >>> over: m >>>\nold\n# <<< over: m <<<\n";
        assert_eq!(remove_block(text, "m"), "");
    }

    #[test]
    fn remove_block_no_op_when_markers_absent() {
        let text = "no markers here\n";
        assert_eq!(remove_block(text, "m"), text);
    }

    #[test]
    fn remove_block_no_op_when_end_marker_missing() {
        let text = "before\n# >>> over: m >>>\nold, no end marker\n";
        assert_eq!(remove_block(text, "m"), text);
    }

    // ── discover_partials ────────────────────────────────────────────────

    #[test]
    fn discover_partials_finds_files() {
        let td = TempDir::new().unwrap();
        td.child("aliases.partial.toml")
            .write_str("target = \"/home/user/.zshrc\"\ncontent = \"alias x=y\"")
            .unwrap();

        let results = discover_partials(td.path()).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "aliases");
        assert_eq!(results[0].1.target, "/home/user/.zshrc");
        assert_eq!(results[0].1.content, "alias x=y");
    }

    #[test]
    fn discover_partials_yaml_also_found() {
        let td = TempDir::new().unwrap();
        td.child("aliases.partial.yaml")
            .write_str("target: /home/user/.zshrc\ncontent: alias x=y")
            .unwrap();

        let results = discover_partials(td.path()).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "aliases");
    }

    #[test]
    fn discover_partials_toml_takes_precedence_over_yaml() {
        init_test_tracing();
        let td = TempDir::new().unwrap();
        td.child("aliases.partial.toml")
            .write_str("target = \"/from-toml\"\ncontent = \"x\"")
            .unwrap();
        td.child("aliases.partial.yaml")
            .write_str("target: /from-yaml\ncontent: x")
            .unwrap();

        let results = discover_partials(td.path()).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].1.target, "/from-toml");
    }

    #[test]
    fn discover_partials_yaml_first_toml_second_toml_wins() {
        // Same assertion as the test above, but with the files written in
        // the opposite order — `WalkDir` doesn't sort, so which file is
        // encountered first (and therefore which side of the
        // seen-names/duplicate branch each one exercises) depends on the
        // OS's own directory order, not write order. Mirrors
        // `symlink::discover_symlinks_yaml_first_toml_second_toml_wins`.
        init_test_tracing();
        let td = TempDir::new().unwrap();
        td.child("aliases.partial.yaml")
            .write_str("target: /from-yaml\ncontent: x")
            .unwrap();
        td.child("aliases.partial.toml")
            .write_str("target = \"/from-toml\"\ncontent = \"x\"")
            .unwrap();

        let results = discover_partials(td.path()).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].1.target, "/from-toml");
    }

    #[test]
    fn discover_partials_skips_empty_target() {
        init_test_tracing();
        let td = TempDir::new().unwrap();
        td.child("bad.partial.toml")
            .write_str("target = \"\"\ncontent = \"x\"")
            .unwrap();

        let results = discover_partials(td.path()).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn discover_partials_skips_empty_content() {
        init_test_tracing();
        let td = TempDir::new().unwrap();
        td.child("bad.partial.toml")
            .write_str("target = \"/foo\"\ncontent = \"\"")
            .unwrap();

        let results = discover_partials(td.path()).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn discover_partials_optional_marker_field() {
        let td = TempDir::new().unwrap();
        td.child("aliases.partial.toml")
            .write_str("target = \"/foo\"\ncontent = \"x\"\nmarker = \"custom\"")
            .unwrap();

        let results = discover_partials(td.path()).unwrap();
        assert_eq!(results[0].1.marker, Some("custom".to_string()));
    }

    #[test]
    fn discover_partials_empty_dir() {
        let td = TempDir::new().unwrap();
        let results = discover_partials(td.path()).unwrap();
        assert!(results.is_empty());
    }

    // ── EnsurePartialBlock ───────────────────────────────────────────────

    fn ctx_force(force: bool, no_prompt: bool, dry_run: bool) -> Ctx {
        crate::exec::Context::builder()
            .force(force)
            .no_prompt(no_prompt)
            .dry_run(dry_run)
            .build()
    }

    #[tokio::test]
    async fn ensure_partial_block_creates_missing_file() {
        let td = TempDir::new().unwrap();
        let target = td.path().join("nested/dir/.zshrc");
        let action =
            EnsurePartialBlock::new(target.clone(), "m".to_string(), "alias x=y".to_string());
        action
            .execute(ctx_force(false, false, false))
            .await
            .unwrap();

        assert_eq!(
            fs::read_to_string(&target).unwrap(),
            "# >>> over: m >>>\nalias x=y\n# <<< over: m <<<\n"
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn ensure_partial_block_reports_context_when_parent_cannot_be_created() {
        use std::os::unix::fs::PermissionsExt;

        let td = TempDir::new().unwrap();
        // `target` itself is missing (so `actual::inspect` reports
        // `Missing` rather than erroring), but its read-only parent
        // rejects creating the still-missing `nested/` component —
        // exercises the `.with_context` wrapping added to name the
        // offending path.
        let readonly = td.path().join("readonly");
        fs::create_dir_all(&readonly).unwrap();
        let target = readonly.join("nested/.zshrc");
        fs::set_permissions(&readonly, fs::Permissions::from_mode(0o555)).unwrap();

        let action =
            EnsurePartialBlock::new(target.clone(), "m".to_string(), "alias x=y".to_string());
        let result = action.execute(ctx_force(false, false, false)).await;

        fs::set_permissions(&readonly, fs::Permissions::from_mode(0o755)).unwrap();

        let err = result.unwrap_err();
        assert!(
            err.to_string()
                .contains("failed to create parent directories")
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn ensure_partial_block_reports_context_when_write_fails() {
        use std::os::unix::fs::PermissionsExt;

        let td = TempDir::new().unwrap();
        let dir = td.path().join("readonly");
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join(".zshrc");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();

        let action =
            EnsurePartialBlock::new(target.clone(), "m".to_string(), "alias x=y".to_string());
        let result = action.execute(ctx_force(false, false, false)).await;

        // Restore permissions so the `TempDir` can clean up regardless of
        // the assertion outcome.
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();

        let err = result.unwrap_err();
        assert!(err.to_string().contains("failed to write"));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn ensure_partial_block_reports_context_when_read_fails() {
        use std::os::unix::fs::PermissionsExt;

        let td = TempDir::new().unwrap();
        let target = td.path().join(".zshrc");
        fs::write(&target, "export FOO=bar\n").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o000)).unwrap();

        let action =
            EnsurePartialBlock::new(target.clone(), "m".to_string(), "alias x=y".to_string());
        let result = action.execute(ctx_force(false, false, false)).await;

        fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();

        let err = result.unwrap_err();
        assert!(err.to_string().contains("failed to read"));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn ensure_partial_block_reports_context_when_append_write_fails() {
        use std::os::unix::fs::PermissionsExt;

        let td = TempDir::new().unwrap();
        let target = td.path().join(".zshrc");
        // Readable (so the block-detection read succeeds and finds no
        // existing block) but not writable, so appending the new block
        // fails.
        fs::write(&target, "export FOO=bar\n").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o444)).unwrap();

        let action =
            EnsurePartialBlock::new(target.clone(), "m".to_string(), "alias x=y".to_string());
        let result = action.execute(ctx_force(false, false, false)).await;

        fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();

        let err = result.unwrap_err();
        assert!(err.to_string().contains("failed to write"));
    }

    #[tokio::test]
    async fn ensure_partial_block_appends_to_existing_file() {
        let td = TempDir::new().unwrap();
        let target = td.path().join(".zshrc");
        fs::write(&target, "export FOO=bar\n").unwrap();

        let action =
            EnsurePartialBlock::new(target.clone(), "m".to_string(), "alias x=y".to_string());
        action
            .execute(ctx_force(false, false, false))
            .await
            .unwrap();

        assert_eq!(
            fs::read_to_string(&target).unwrap(),
            "export FOO=bar\n# >>> over: m >>>\nalias x=y\n# <<< over: m <<<\n"
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn ensure_partial_block_applies_permissions_to_a_new_file() {
        use std::os::unix::fs::PermissionsExt;

        let td = TempDir::new().unwrap();
        let target = td.path().join(".zshrc");
        let action =
            EnsurePartialBlock::new(target.clone(), "m".to_string(), "alias x=y".to_string())
                .with_permissions(Some(FileMode::parse("600").unwrap()));
        action
            .execute(ctx_force(false, false, false))
            .await
            .unwrap();

        let mode = fs::metadata(&target).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode, 0o600);
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn ensure_partial_block_applies_permissions_when_appending_to_an_existing_file() {
        use std::os::unix::fs::PermissionsExt;

        let td = TempDir::new().unwrap();
        let target = td.path().join(".zshrc");
        fs::write(&target, "export FOO=bar\n").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();

        let action =
            EnsurePartialBlock::new(target.clone(), "m".to_string(), "alias x=y".to_string())
                .with_permissions(Some(FileMode::parse("600").unwrap()));
        action
            .execute(ctx_force(false, false, false))
            .await
            .unwrap();

        let mode = fs::metadata(&target).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode, 0o600);
    }

    #[tokio::test]
    async fn ensure_partial_block_without_declared_permissions_leaves_default_mode() {
        // No `.with_permissions(...)` call at all — must behave exactly
        // like before #65 (no permission management whatsoever).
        let td = TempDir::new().unwrap();
        let target = td.path().join(".zshrc");
        let action =
            EnsurePartialBlock::new(target.clone(), "m".to_string(), "alias x=y".to_string());
        action
            .execute(ctx_force(false, false, false))
            .await
            .unwrap();
        assert!(target.exists());
    }

    #[tokio::test]
    async fn ensure_partial_block_dry_run_does_not_touch_filesystem() {
        let td = TempDir::new().unwrap();
        let target = td.path().join(".zshrc");
        let action =
            EnsurePartialBlock::new(target.clone(), "m".to_string(), "alias x=y".to_string());
        action.execute(ctx_force(false, false, true)).await.unwrap();
        assert!(!target.exists());
    }

    #[tokio::test]
    async fn ensure_partial_block_force_overwrites_drifted_block() {
        let td = TempDir::new().unwrap();
        let target = td.path().join(".zshrc");
        fs::write(
            &target,
            "before\n# >>> over: m >>>\nhand-edited\n# <<< over: m <<<\nafter\n",
        )
        .unwrap();

        let action =
            EnsurePartialBlock::new(target.clone(), "m".to_string(), "alias x=y".to_string());
        action.execute(ctx_force(true, false, false)).await.unwrap();

        assert_eq!(
            fs::read_to_string(&target).unwrap(),
            "before\n# >>> over: m >>>\nalias x=y\n# <<< over: m <<<\nafter\n"
        );
    }

    #[tokio::test]
    async fn ensure_partial_block_no_prompt_errors_on_drift() {
        let td = TempDir::new().unwrap();
        let target = td.path().join(".zshrc");
        fs::write(
            &target,
            "# >>> over: m >>>\nhand-edited\n# <<< over: m <<<\n",
        )
        .unwrap();

        let action =
            EnsurePartialBlock::new(target.clone(), "m".to_string(), "alias x=y".to_string());
        let result = action.execute(ctx_force(false, true, false)).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn ensure_partial_block_force_replaces_directory_in_the_way() {
        let td = TempDir::new().unwrap();
        let target = td.path().join("blocked");
        fs::create_dir_all(&target).unwrap();

        let action =
            EnsurePartialBlock::new(target.clone(), "m".to_string(), "alias x=y".to_string());
        action.execute(ctx_force(true, false, false)).await.unwrap();

        assert!(target.is_file());
        assert_eq!(
            fs::read_to_string(&target).unwrap(),
            "# >>> over: m >>>\nalias x=y\n# <<< over: m <<<\n"
        );
    }

    #[tokio::test]
    async fn ensure_partial_block_no_prompt_errors_on_structural_conflict() {
        let td = TempDir::new().unwrap();
        let target = td.path().join("blocked");
        fs::create_dir_all(&target).unwrap();

        let action =
            EnsurePartialBlock::new(target.clone(), "m".to_string(), "alias x=y".to_string());
        let result = action.execute(ctx_force(false, true, false)).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn ensure_partial_block_already_correct_is_a_no_op() {
        let td = TempDir::new().unwrap();
        let target = td.path().join(".zshrc");
        let original = "# >>> over: m >>>\nalias x=y\n# <<< over: m <<<\n";
        fs::write(&target, original).unwrap();

        let action =
            EnsurePartialBlock::new(target.clone(), "m".to_string(), "alias x=y".to_string());
        action
            .execute(ctx_force(false, false, false))
            .await
            .unwrap();

        assert_eq!(fs::read_to_string(&target).unwrap(), original);
    }

    #[tokio::test]
    async fn ensure_partial_block_force_appends_after_malformed_markers() {
        let td = TempDir::new().unwrap();
        let target = td.path().join(".zshrc");
        fs::write(&target, "# >>> over: m >>>\nstray\n").unwrap();

        let action =
            EnsurePartialBlock::new(target.clone(), "m".to_string(), "alias x=y".to_string());
        action.execute(ctx_force(true, false, false)).await.unwrap();

        assert_eq!(
            fs::read_to_string(&target).unwrap(),
            "# >>> over: m >>>\nstray\n# >>> over: m >>>\nalias x=y\n# <<< over: m <<<\n"
        );
    }

    // ── Display ──────────────────────────────────────────────────────────

    #[test]
    fn structural_choice_display() {
        assert_eq!(format!("{}", StructuralChoice::Skip), "Skip");
        assert!(format!("{}", StructuralChoice::Overwrite).contains("Overwrite"));
    }

    #[test]
    fn block_choice_display() {
        assert_eq!(format!("{}", BlockChoice::Skip), "Skip");
        assert!(format!("{}", BlockChoice::Overwrite).contains("Overwrite"));
        assert!(format!("{}", BlockChoice::Diff).contains("Diff"));
    }

    #[rstest]
    fn ensure_partial_block_display() {
        let action = EnsurePartialBlock::new(
            PathBuf::from("/home/user/.zshrc"),
            "aliases".to_string(),
            "alias x=y".to_string(),
        );
        let display = format!("{}", action);
        assert!(display.contains("partial block:"));
        assert!(display.contains(".zshrc"));
        assert!(display.contains("aliases"));
    }
}
