//! Overlay diff (#109): compares desired rendered content/metadata
//! against actual filesystem (and git) state, so differences are
//! inspectable before reconciliation.
//!
//! Built directly on the same read-only primitives `crate::status` uses —
//! [`Plan::build`] for filesystem entries, [`status::git::inspect`] for
//! [`MaterializationIntent::Checkout`] entries — but answers a different
//! question than `status`: not just *whether* an entry needs attention,
//! but *what exactly differs*, as a structured [`content::ContentDiff`]
//! wherever a meaningful line diff can be produced.
//!
//! [`Change`] is deliberately its own taxonomy, distinct from
//! [`status::Status`], derived directly from the `ActualState` ×
//! `MaterializationIntent` combinations [`crate::materialize::symlink::SymlinkMaterializer::classify`]
//! already produces:
//!
//! - A file-symlink expected, a real file found: both are regular files —
//!   diff their *content* (the "useful content diff" #109 asks for).
//! - Either symlink kind expected, a symlink pointing elsewhere found:
//!   diff the *target path*, never file content (#109: "show target
//!   differences rather than treating the link as ordinary file
//!   content").
//! - Anything else structurally different (a real directory where a
//!   symlink/directory-symlink was expected, and vice versa): nothing
//!   meaningful to diff — reported as [`Change::Unexpected`], the
//!   "unexpected entries" #109 asks for.
//! - Git checkouts: [`status::git::inspect`] reused unchanged; commit/
//!   merge mechanics are #110's job, not this one's.

mod content;

use std::fmt;
use std::fs;
use std::path::Path;

use anyhow::{Context as _, Result};

use crate::actions::partial::{self, BlockState};
use crate::desired::{DesiredEntry, DesiredTree, MaterializationIntent, Provenance};
use crate::materialize::virtual_checkout::git as vc_git;
use crate::materialize::virtual_checkout::state as vc_state;
use crate::plan::actual;
use crate::plan::{ActualState, Operation, Plan};
use crate::status::{self, Status};
use crate::ui::{emojis, style};
use crate::utils::short_path;

pub use content::{ContentDiff, DiffLine, LineTag};

/// What differs (if anything) between a [`DesiredEntry`] and actual
/// state — see the module docs for how each variant is derived.
#[derive(Debug, Clone)]
pub enum Change {
    /// Target already matches desired state — nothing to show.
    Unchanged,
    /// Target doesn't exist yet.
    Missing,
    /// Same kind of entity as desired, but its content or symlink target
    /// differs — carries the structured diff.
    Modified(ContentDiff),
    /// A fundamentally different kind of entity occupies the target than
    /// what's desired (e.g. a real directory where a symlink was
    /// expected) — no meaningful line diff to show.
    Unexpected { actual: ActualState },
    /// A dangling soft symlink (correctly pointed, but its source
    /// disappeared), or a checkout path that isn't a valid git repo.
    Broken,
    /// Git checkout state — dirty / ahead / behind / diverged / a
    /// merge-rebase-cherry-pick in progress. Reuses [`status::Status`]
    /// verbatim: commit/merge mechanics are out of scope here (#110).
    Checkout(Status),
    /// Virtual checkout state (#141): the same aggregate [`status::Status`]
    /// `over status` reports, plus the specific file-level differences
    /// underneath it (each with a content diff when the file was modified,
    /// not just added/deleted) — the "expose the relevant differences"
    /// #141 asks for, computed against the checkout's recorded base
    /// content rather than a real git index.
    VirtualCheckout {
        status: Status,
        files: Vec<VirtualCheckoutFileChange>,
    },
}

/// One file-level difference under a virtual checkout, paired with a
/// content diff when meaningful (mirrors [`FileChangeKind`](vc_git::FileChangeKind),
/// re-exposed here since that type is crate-private plumbing).
#[derive(Debug, Clone)]
pub struct VirtualCheckoutFileChange {
    pub path: std::path::PathBuf,
    pub kind: VirtualCheckoutFileKind,
    /// `Some` for [`VirtualCheckoutFileKind::Modified`] (base content vs.
    /// on-disk content); `None` for `Added`/`Deleted`, where there's no
    /// "other side" to line-diff against.
    pub diff: Option<ContentDiff>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtualCheckoutFileKind {
    Added,
    Modified,
    Deleted,
}

impl fmt::Display for VirtualCheckoutFileChange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let path = self.path.display();
        match self.kind {
            VirtualCheckoutFileKind::Added => write!(f, "  {} added: {path}", emojis::CHECKMARK),
            VirtualCheckoutFileKind::Deleted => {
                write!(f, "  {} deleted: {path}", emojis::CROSSMARK)
            }
            VirtualCheckoutFileKind::Modified => {
                writeln!(f, "  {} modified: {path}", emojis::WARNING)?;
                if let Some(diff) = &self.diff {
                    write!(f, "{diff}")?;
                }
                Ok(())
            }
        }
    }
}

/// One [`DesiredEntry`] paired with what differs about it.
#[derive(Debug, Clone)]
pub struct DiffEntry {
    pub entry: DesiredEntry,
    pub change: Change,
}

impl DiffEntry {
    /// Whether this entry has anything worth showing — used by the CLI
    /// to decide what to print without `--verbose`, same name and
    /// convention as `status::Status::needs_attention`.
    pub fn needs_attention(&self) -> bool {
        !matches!(self.change, Change::Unchanged)
    }
}

impl fmt::Display for DiffEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let target = short_path(&self.entry.target.to_string_lossy());
        match &self.change {
            Change::Unchanged => write!(
                f,
                "{} {} {}",
                emojis::CHECKMARK,
                style::white("unchanged:"),
                target
            ),
            Change::Missing => write!(
                f,
                "{} {} {}",
                emojis::CROSSMARK,
                style::white("missing:"),
                target
            ),
            Change::Broken => write!(
                f,
                "{} {} {}",
                emojis::WARNING,
                style::yellow("broken:"),
                target
            ),
            Change::Unexpected { actual } => write!(
                f,
                "{} {} {} (found {})",
                emojis::WARNING,
                style::yellow("unexpected:"),
                target,
                actual,
            ),
            Change::Modified(diff) => {
                writeln!(
                    f,
                    "{} {} {}",
                    emojis::WARNING,
                    style::yellow("modified:"),
                    target
                )?;
                write!(f, "{diff}")
            }
            // Delegate to `status::EntryStatus`'s own `Display` for the
            // ahead/behind/diverged/modified/conflict wording, rather
            // than re-deriving it here.
            Change::Checkout(current) => {
                let entry_status = status::EntryStatus {
                    entry: self.entry.clone(),
                    status: current.clone(),
                };
                write!(f, "{entry_status}")
            }
            Change::VirtualCheckout { status, files } => {
                let entry_status = status::EntryStatus {
                    entry: self.entry.clone(),
                    status: status.clone(),
                };
                writeln!(f, "{entry_status}")?;
                for (i, file) in files.iter().enumerate() {
                    if i > 0 {
                        writeln!(f)?;
                    }
                    write!(f, "{file}")?;
                }
                Ok(())
            }
        }
    }
}

/// The diff of a whole [`DesiredTree`]: one [`DiffEntry`] per
/// [`DesiredEntry`].
#[derive(Debug, Clone, Default)]
pub struct Report {
    entries: Vec<DiffEntry>,
}

impl Report {
    /// Classify every entry in `desired`. Read-only, like [`Plan::build`]
    /// and `status::Report::build`.
    pub fn build(desired: &DesiredTree) -> Result<Self> {
        let plan = Plan::build(desired)?;
        let mut entries = Vec::with_capacity(plan.len());
        for step in plan.steps() {
            let change = if matches!(step.entry.intent, MaterializationIntent::Checkout) {
                classify_checkout(&step.entry)?
            } else if matches!(step.entry.intent, MaterializationIntent::VirtualCheckout) {
                classify_virtual_checkout(&step.entry)?
            } else {
                classify_fs(&step.entry, &step.operation)?
            };
            entries.push(DiffEntry {
                entry: step.entry.clone(),
                change,
            });
        }
        Ok(Self { entries })
    }

    pub fn entries(&self) -> &[DiffEntry] {
        &self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether anything in this report needs attention (has a diff worth
    /// showing) — same name and convention as `status::Report` and
    /// `unapply::Report`.
    pub fn needs_attention(&self) -> bool {
        self.entries.iter().any(DiffEntry::needs_attention)
    }
}

/// Classify a filesystem (non-`Checkout`) entry's `Operation` into a
/// [`Change`].
fn classify_fs(entry: &DesiredEntry, operation: &Operation) -> Result<Change> {
    match operation {
        Operation::Create => Ok(Change::Missing),
        // Reuse `status`'s own dangling-soft-symlink check rather than
        // re-deriving it.
        Operation::Noop => Ok(match status::classify_noop(&entry.intent) {
            Status::Broken => Change::Broken,
            _ => Change::Unchanged,
        }),
        Operation::Conflict { current } => classify_conflict(entry, current),
        // A pending rule-change migration (#129, blocked or not): a
        // fundamentally different kind of entity occupies the target than
        // what's currently desired — exactly what `Unexpected` already
        // means, no meaningful line diff to show either way.
        Operation::Migrate { .. } => Ok(Change::Unexpected {
            actual: actual::inspect(&entry.target)?,
        }),
        // A permission-only drift (#65): content/kind already matches,
        // only the mode differs — reuse the line-diff renderer for a
        // trivial one-line "text" diff (`644` -> `755`), exactly like the
        // symlink-target-path case above.
        Operation::Repair { current, desired } => Ok(Change::Modified(ContentDiff::from_texts(
            &current.to_string(),
            &desired.to_string(),
        ))),
        Operation::Deferred => unreachable!(
            "only Checkout entries ever classify as Deferred, and those are \
             routed to classify_checkout before reaching classify_fs"
        ),
    }
}

/// Classify an `Operation::Conflict` into a [`Change`], per the table in
/// the module docs.
fn classify_conflict(entry: &DesiredEntry, current: &ActualState) -> Result<Change> {
    match (&entry.intent, current) {
        // A file-symlink was expected, a real file occupies the target:
        // both sides are regular files, just not linked yet — diff their
        // content.
        (MaterializationIntent::SymlinkFile { source, .. }, ActualState::File) => {
            Ok(Change::Modified(content_diff_files(&entry.target, source)?))
        }
        // Either symlink kind was expected, a symlink pointing elsewhere
        // is there: diff the target *paths*, never content.
        (
            MaterializationIntent::SymlinkFile { source, .. }
            | MaterializationIntent::SymlinkDirectory { source, .. },
            ActualState::Symlink { points_to },
        ) => Ok(Change::Modified(ContentDiff::from_texts(
            &points_to.to_string_lossy(),
            &source.to_string_lossy(),
        ))),
        (MaterializationIntent::Checkout, _) => {
            unreachable!("Checkout entries never reach Plan::build's Conflict classification")
        }
        // A partial-managed block drifted (or its markers are malformed):
        // diff just the block's content, not the whole file — the rest of
        // the target is never `over`'s concern.
        (MaterializationIntent::PartialFile { content, marker }, ActualState::File) => {
            let current_text = fs::read_to_string(&entry.target)
                .with_context(|| format!("failed to read {}", entry.target.display()))?;
            let existing_block = match partial::find_block(&current_text, marker) {
                BlockState::Found(x) => x.to_string(),
                // Absent/malformed: nothing block-shaped to diff against —
                // fall back to the whole file so at least something useful
                // is shown.
                _ => current_text,
            };
            Ok(Change::Modified(ContentDiff::from_texts(
                &existing_block,
                content,
            )))
        }
        // Everything else structurally mismatched (a directory expected,
        // or a directory-symlink expected but a real file/directory
        // found): nothing meaningful to diff.
        _ => Ok(Change::Unexpected {
            actual: current.clone(),
        }),
    }
}

/// Read both sides of a file-vs-would-be-symlinked-file conflict and diff
/// their content, falling back to a binary marker if either side isn't
/// valid UTF-8 (mirrors git's own "binary files differ").
fn content_diff_files(actual_path: &Path, desired_path: &Path) -> Result<ContentDiff> {
    let old = fs::read(actual_path)
        .with_context(|| format!("failed to read {}", actual_path.display()))?;
    let new = fs::read(desired_path)
        .with_context(|| format!("failed to read {}", desired_path.display()))?;
    Ok(match (String::from_utf8(old), String::from_utf8(new)) {
        (Ok(old), Ok(new)) => ContentDiff::from_texts(&old, &new),
        _ => ContentDiff::binary(),
    })
}

/// Classify a `MaterializationIntent::Checkout` entry, reusing
/// `status::git::inspect` unchanged.
fn classify_checkout(entry: &DesiredEntry) -> Result<Change> {
    Ok(match status::git::inspect(entry)? {
        Status::Applied => Change::Unchanged,
        Status::Missing => Change::Missing,
        Status::Broken => Change::Broken,
        other => Change::Checkout(other),
    })
}

/// Classify a `MaterializationIntent::VirtualCheckout` entry (#141):
/// reuses `status::virtual_checkout::inspect` for the aggregate status,
/// then — when there's something worth showing — builds a content diff
/// per modified file against the checkout's recorded base content.
fn classify_virtual_checkout(entry: &DesiredEntry) -> Result<Change> {
    let status = status::virtual_checkout::inspect(entry)?;
    match status {
        Status::Applied => return Ok(Change::Unchanged),
        Status::Missing => return Ok(Change::Missing),
        Status::Broken => return Ok(Change::Broken),
        _ => {}
    }

    let file_changes = status::virtual_checkout::file_changes(entry)?;
    let files = if file_changes.is_empty() {
        Vec::new()
    } else {
        build_virtual_checkout_file_diffs(entry, &file_changes)?
    };
    Ok(Change::VirtualCheckout { status, files })
}

/// Attach a [`ContentDiff`] to each modified file change (base blob
/// content vs. current on-disk content) — added/deleted files have no
/// "other side" to line-diff against, so their diff stays `None`.
fn build_virtual_checkout_file_diffs(
    entry: &DesiredEntry,
    file_changes: &[vc_git::FileChange],
) -> Result<Vec<VirtualCheckoutFileChange>> {
    let Provenance::Overlay { source, .. } = &entry.provenance else {
        unreachable!(
            "VirtualCheckout entries only ever carry Provenance::Overlay \
             (see desired::tree::{{collect_own_entries,walk_overlay_tree}})"
        );
    };
    let record = vc_state::record_for_blocking(&entry.target)?;

    // Only needed for `Modified` files (base blob content); resolved
    // lazily so a checkout with only added/deleted files never has to
    // discover the source repository at all.
    let mut base_blobs: Option<(
        git2::Repository,
        std::collections::BTreeMap<std::path::PathBuf, git2::Oid>,
    )> = None;

    let mut result = Vec::with_capacity(file_changes.len());
    for change in file_changes {
        let kind = match change.kind {
            vc_git::FileChangeKind::Added => VirtualCheckoutFileKind::Added,
            vc_git::FileChangeKind::Modified => VirtualCheckoutFileKind::Modified,
            vc_git::FileChangeKind::Deleted => VirtualCheckoutFileKind::Deleted,
        };

        let diff = if change.kind == vc_git::FileChangeKind::Modified {
            if base_blobs.is_none() {
                let record = record
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("no recorded virtual checkout association"))?;
                let (repo, managed_path) = vc_git::discover_source(source)?;
                let blobs = {
                    let tree = vc_git::base_tree(&repo, Some(&record.base_oid))?;
                    let subtree = vc_git::managed_subtree(&repo, &tree, &managed_path)?;
                    vc_git::tracked_blobs(&subtree)?
                };
                base_blobs = Some((repo, blobs));
            }
            let (repo, blobs) = base_blobs.as_ref().expect("just populated above");
            match blobs.get(&change.path) {
                Some(oid) => {
                    let base_content = vc_git::read_blob(repo, *oid)?;
                    let on_disk = entry.target.join(&change.path);
                    let actual = fs::read(&on_disk)
                        .with_context(|| format!("failed to read {}", on_disk.display()))?;
                    Some(
                        match (String::from_utf8(base_content), String::from_utf8(actual)) {
                            (Ok(old), Ok(new)) => ContentDiff::from_texts(&old, &new),
                            _ => ContentDiff::binary(),
                        },
                    )
                }
                None => None,
            }
        } else {
            None
        };

        result.push(VirtualCheckoutFileChange {
            path: change.path.clone(),
            kind,
            diff,
        });
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::symlink::LinkType;
    use crate::exec::Context;
    use crate::overlays::Repository;
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    use rstest::rstest;
    use std::fs;

    fn repo_and_root() -> (TempDir, Repository) {
        let td = TempDir::new().unwrap();
        let repo = Repository::new(td.path().to_path_buf());
        (td, repo)
    }

    /// Points `$XDG_STATE_HOME` at a fresh, writable temp dir for the
    /// duration of the returned guard's lifetime — `classify_virtual_checkout`
    /// reads (and `materialize_and_record_virtual_checkout` below writes)
    /// `VirtualCheckoutState` via the real, non-injectable `XdgDirs::new()`,
    /// so every test exercising it must isolate this or it silently
    /// pollutes the real `$XDG_STATE_HOME/over/virtual_checkout.toml`.
    ///
    /// # Safety
    /// `env::set_var` is only unsound when other threads read/write the
    /// process environment concurrently; `cargo nextest` runs each test in
    /// its own process, matching `xdg::tests`' own `set_env` justification.
    fn isolate_xdg_state() -> TempDir {
        let tmp = TempDir::new().unwrap();
        unsafe {
            std::env::set_var("XDG_STATE_HOME", tmp.path());
        }
        tmp
    }

    #[rstest]
    fn missing_target_reports_missing() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~/sub\"")
            .unwrap();
        let overlay = repo.get("ov").unwrap();
        let ctx = Context::builder()
            .root(td.path().to_path_buf())
            .repository(repo.clone())
            .overlay(overlay.clone())
            .build();

        let desired = DesiredTree::build(&ctx, &overlay).unwrap();
        let report = Report::build(&desired).unwrap();

        assert_eq!(report.entries().len(), 1);
        assert!(matches!(report.entries()[0].change, Change::Missing));
        assert!(report.needs_attention());
    }

    #[tokio::test]
    async fn applied_target_reports_unchanged() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        overlay_dir.child("file.txt").write_str("content").unwrap();
        let overlay = repo.get("ov").unwrap();
        let ctx = Context::builder()
            .root(td.path().to_path_buf())
            .repository(repo.clone())
            .overlay(overlay.clone())
            .build();

        let desired = DesiredTree::build(&ctx, &overlay).unwrap();
        Plan::build(&desired)
            .unwrap()
            .execute(ctx.clone())
            .await
            .unwrap();

        let desired2 = DesiredTree::build(&ctx, &overlay).unwrap();
        let report = Report::build(&desired2).unwrap();
        let file_entry = report
            .entries()
            .iter()
            .find(|e| e.entry.target == td.path().join("file.txt"))
            .unwrap();
        assert!(matches!(file_entry.change, Change::Unchanged));
        assert!(!file_entry.needs_attention());
        assert!(!report.needs_attention());
    }

    #[rstest]
    fn existing_file_conflict_produces_content_diff() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        overlay_dir
            .child("file.txt")
            .write_str("desired content\n")
            .unwrap();
        fs::write(td.path().join("file.txt"), "actual content\n").unwrap();

        let overlay = repo.get("ov").unwrap();
        let ctx = Context::builder()
            .root(td.path().to_path_buf())
            .repository(repo.clone())
            .overlay(overlay.clone())
            .build();

        let desired = DesiredTree::build(&ctx, &overlay).unwrap();
        let report = Report::build(&desired).unwrap();
        let file_entry = report
            .entries()
            .iter()
            .find(|e| e.entry.target == td.path().join("file.txt"))
            .unwrap();

        match &file_entry.change {
            Change::Modified(diff) => {
                let s = format!("{diff}");
                assert!(s.contains("actual content"));
                assert!(s.contains("desired content"));
            }
            other => panic!("expected Modified, got {other:?}"),
        }
        assert!(report.needs_attention());
    }

    #[rstest]
    #[cfg(unix)]
    fn existing_file_conflict_reports_context_when_actual_is_unreadable() {
        use std::os::unix::fs::PermissionsExt;

        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        overlay_dir
            .child("file.txt")
            .write_str("desired content\n")
            .unwrap();
        let actual = td.path().join("file.txt");
        fs::write(&actual, "actual content\n").unwrap();
        fs::set_permissions(&actual, fs::Permissions::from_mode(0o000)).unwrap();

        let overlay = repo.get("ov").unwrap();
        let ctx = Context::builder()
            .root(td.path().to_path_buf())
            .repository(repo.clone())
            .overlay(overlay.clone())
            .build();

        let desired = DesiredTree::build(&ctx, &overlay).unwrap();
        let result = Report::build(&desired);

        fs::set_permissions(&actual, fs::Permissions::from_mode(0o644)).unwrap();

        let err = result.unwrap_err();
        assert!(err.to_string().contains("failed to read"));
    }

    #[rstest]
    #[cfg(unix)]
    fn existing_file_conflict_reports_context_when_desired_is_unreadable() {
        use std::os::unix::fs::PermissionsExt;

        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let desired_file = overlay_dir.child("file.txt");
        desired_file.write_str("desired content\n").unwrap();
        fs::set_permissions(desired_file.path(), fs::Permissions::from_mode(0o000)).unwrap();
        fs::write(td.path().join("file.txt"), "actual content\n").unwrap();

        let overlay = repo.get("ov").unwrap();
        let ctx = Context::builder()
            .root(td.path().to_path_buf())
            .repository(repo.clone())
            .overlay(overlay.clone())
            .build();

        let desired = DesiredTree::build(&ctx, &overlay).unwrap();
        let result = Report::build(&desired);

        fs::set_permissions(desired_file.path(), fs::Permissions::from_mode(0o644)).unwrap();

        let err = result.unwrap_err();
        assert!(err.to_string().contains("failed to read"));
    }

    #[test]
    fn drifted_partial_block_produces_block_level_diff() {
        let td = TempDir::new().unwrap();
        let target = td.path().join("file.txt");
        fs::write(
            &target,
            "before\n# >>> over: m >>>\nhand-edited\n# <<< over: m <<<\nafter\n",
        )
        .unwrap();
        let entry = DesiredEntry {
            target: target.clone(),
            provenance: crate::desired::Provenance::PartialSidecar {
                overlay: "ov".to_string(),
                config: std::path::PathBuf::from("/repo/ov/aliases.partial.toml"),
            },
            intent: MaterializationIntent::PartialFile {
                content: "alias x=y".to_string(),
                marker: "m".to_string(),
            },
            permissions: None,
        };
        let change = classify_conflict(&entry, &ActualState::File).unwrap();
        match change {
            Change::Modified(diff) => {
                let s = format!("{diff}");
                assert!(s.contains("hand-edited"));
                assert!(s.contains("alias x=y"));
                assert!(!s.contains("before"));
                assert!(!s.contains("after"));
            }
            other => panic!("expected Modified, got {other:?}"),
        }
    }

    #[test]
    #[cfg(unix)]
    fn drifted_partial_block_reports_context_when_target_is_unreadable() {
        use std::os::unix::fs::PermissionsExt;

        let td = TempDir::new().unwrap();
        let target = td.path().join("file.txt");
        fs::write(
            &target,
            "before\n# >>> over: m >>>\nhand-edited\n# <<< over: m <<<\nafter\n",
        )
        .unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o000)).unwrap();
        let entry = DesiredEntry {
            target: target.clone(),
            provenance: crate::desired::Provenance::PartialSidecar {
                overlay: "ov".to_string(),
                config: std::path::PathBuf::from("/repo/ov/aliases.partial.toml"),
            },
            intent: MaterializationIntent::PartialFile {
                content: "alias x=y".to_string(),
                marker: "m".to_string(),
            },
            permissions: None,
        };
        let result = classify_conflict(&entry, &ActualState::File);

        fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();

        let err = result.unwrap_err();
        assert!(err.to_string().contains("failed to read"));
    }

    #[test]
    fn malformed_partial_block_falls_back_to_whole_file_diff() {
        let td = TempDir::new().unwrap();
        let target = td.path().join("file.txt");
        // Begin marker with no matching end marker: `find_block` reports
        // `Malformed`, which has no block-shaped region to diff against.
        fs::write(&target, "# >>> over: m >>>\nstray, no end marker\n").unwrap();
        let entry = DesiredEntry {
            target: target.clone(),
            provenance: crate::desired::Provenance::PartialSidecar {
                overlay: "ov".to_string(),
                config: std::path::PathBuf::from("/repo/ov/aliases.partial.toml"),
            },
            intent: MaterializationIntent::PartialFile {
                content: "alias x=y".to_string(),
                marker: "m".to_string(),
            },
            permissions: None,
        };
        let change = classify_conflict(&entry, &ActualState::File).unwrap();
        match change {
            Change::Modified(diff) => {
                let s = format!("{diff}");
                assert!(s.contains("stray, no end marker"));
                assert!(s.contains("alias x=y"));
            }
            other => panic!("expected Modified, got {other:?}"),
        }
    }

    #[rstest]
    fn binary_file_conflict_reports_binary_diff() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        fs::write(overlay_dir.path().join("file.bin"), [0xff_u8, 0xfe, 0x00]).unwrap();
        fs::write(td.path().join("file.bin"), [0x00_u8, 0xff, 0xfe]).unwrap();

        let overlay = repo.get("ov").unwrap();
        let ctx = Context::builder()
            .root(td.path().to_path_buf())
            .repository(repo.clone())
            .overlay(overlay.clone())
            .build();

        let desired = DesiredTree::build(&ctx, &overlay).unwrap();
        let report = Report::build(&desired).unwrap();
        let file_entry = report
            .entries()
            .iter()
            .find(|e| e.entry.target == td.path().join("file.bin"))
            .unwrap();

        match &file_entry.change {
            Change::Modified(diff) => assert!(diff.lines.is_none()),
            other => panic!("expected Modified(binary), got {other:?}"),
        }
    }

    #[rstest]
    fn symlink_pointing_elsewhere_produces_target_diff() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        overlay_dir.child("file.txt").write_str("content").unwrap();
        let elsewhere = td.child("elsewhere.txt");
        elsewhere.write_str("elsewhere").unwrap();
        symlink::symlink_file(elsewhere.path(), td.path().join("file.txt")).unwrap();

        let overlay = repo.get("ov").unwrap();
        let ctx = Context::builder()
            .root(td.path().to_path_buf())
            .repository(repo.clone())
            .overlay(overlay.clone())
            .build();

        let desired = DesiredTree::build(&ctx, &overlay).unwrap();
        let report = Report::build(&desired).unwrap();
        let file_entry = report
            .entries()
            .iter()
            .find(|e| e.entry.target == td.path().join("file.txt"))
            .unwrap();

        match &file_entry.change {
            Change::Modified(diff) => {
                let s = format!("{diff}");
                assert!(s.contains("elsewhere.txt"));
                assert!(s.contains("ov/file.txt") || s.contains("file.txt"));
            }
            other => panic!("expected Modified, got {other:?}"),
        }
    }

    #[rstest]
    fn directory_expected_but_file_found_is_unexpected() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        overlay_dir.child("subdir").create_dir_all().unwrap();
        // Occupy the directory's target path with a plain file instead.
        fs::write(td.path().join("subdir"), "not a directory").unwrap();

        let overlay = repo.get("ov").unwrap();
        let ctx = Context::builder()
            .root(td.path().to_path_buf())
            .repository(repo.clone())
            .overlay(overlay.clone())
            .build();

        let desired = DesiredTree::build(&ctx, &overlay).unwrap();
        let report = Report::build(&desired).unwrap();
        let dir_entry = report
            .entries()
            .iter()
            .find(|e| e.entry.target == td.path().join("subdir"))
            .unwrap();

        match &dir_entry.change {
            Change::Unexpected { actual } => assert_eq!(*actual, ActualState::File),
            other => panic!("expected Unexpected, got {other:?}"),
        }
    }

    #[rstest]
    fn link_dir_expected_but_real_directory_found_is_unexpected() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"\nlink_dirs = [\"mydir\"]")
            .unwrap();
        overlay_dir.child("mydir/inner.txt").write_str("x").unwrap();
        fs::create_dir_all(td.path().join("mydir")).unwrap();

        let overlay = repo.get("ov").unwrap();
        let ctx = Context::builder()
            .root(td.path().to_path_buf())
            .repository(repo.clone())
            .overlay(overlay.clone())
            .build();

        let desired = DesiredTree::build(&ctx, &overlay).unwrap();
        let report = Report::build(&desired).unwrap();
        let dir_entry = report
            .entries()
            .iter()
            .find(|e| e.entry.target == td.path().join("mydir"))
            .unwrap();

        match &dir_entry.change {
            Change::Unexpected { actual } => assert_eq!(*actual, ActualState::Directory),
            other => panic!("expected Unexpected, got {other:?}"),
        }
    }

    #[test]
    fn dangling_soft_symlink_noop_is_broken() {
        let entry = DesiredEntry {
            target: std::path::PathBuf::from("/tmp/target"),
            provenance: crate::desired::Provenance::Overlay {
                overlay: "ov".to_string(),
                source: std::path::PathBuf::from("/repo/ov"),
            },
            intent: MaterializationIntent::SymlinkFile {
                source: std::path::PathBuf::from("/does/not/exist/anymore"),
                link_type: LinkType::Soft,
            },
            permissions: None,
        };
        let change = classify_fs(&entry, &Operation::Noop).unwrap();
        assert!(matches!(change, Change::Broken));
    }

    #[test]
    fn migrate_operation_is_unexpected_with_current_actual_state() {
        let td = TempDir::new().unwrap();
        let target = td.path().join("target");
        std::fs::create_dir_all(&target).unwrap();
        let entry = DesiredEntry {
            target: target.clone(),
            provenance: crate::desired::Provenance::Overlay {
                overlay: "ov".to_string(),
                source: std::path::PathBuf::from("/repo/ov"),
            },
            intent: MaterializationIntent::SymlinkDirectory {
                source: std::path::PathBuf::from("/repo/ov"),
                link_type: LinkType::Soft,
            },
            permissions: None,
        };
        let operation = Operation::Migrate {
            from: MaterializationIntent::Checkout,
            to: entry.intent.clone(),
            blocked: None,
        };
        let change = classify_fs(&entry, &operation).unwrap();
        match change {
            Change::Unexpected { actual } => assert_eq!(actual, ActualState::Directory),
            other => panic!("expected Unexpected, got {other:?}"),
        }
    }

    #[test]
    fn empty_desired_tree_produces_empty_report() {
        let report = Report::build(&DesiredTree::default()).unwrap();
        assert!(report.is_empty());
        assert!(!report.needs_attention());
    }

    #[rstest]
    fn git_checkout_not_cloned_yet_reports_missing() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"\n[git]\n\".config/nvim\" = \"https://example.com/nvim.git\"")
            .unwrap();
        let overlay = repo.get("ov").unwrap();
        let ctx = Context::builder()
            .root(td.path().to_path_buf())
            .repository(repo.clone())
            .overlay(overlay.clone())
            .build();

        let desired = DesiredTree::build(&ctx, &overlay).unwrap();
        let report = Report::build(&desired).unwrap();
        let checkout_entry = report
            .entries()
            .iter()
            .find(|e| matches!(e.entry.intent, MaterializationIntent::Checkout))
            .unwrap();
        assert!(matches!(checkout_entry.change, Change::Missing));
    }

    #[test]
    fn dirty_git_checkout_maps_to_checkout_modified() {
        use crate::actions::git::config::GitRepoConfig;
        use git2::{Repository as GitRepository, Signature};

        let td = TempDir::new().unwrap();
        let repo = GitRepository::init(td.path()).unwrap();
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "Test").unwrap();
        cfg.set_str("user.email", "test@test.com").unwrap();
        drop(cfg);
        let sig = Signature::now("Test", "test@test.com").unwrap();
        fs::write(td.path().join("README.md"), "# Test").unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
        fs::write(td.path().join("README.md"), "changed").unwrap();

        let entry = DesiredEntry {
            target: td.path().to_path_buf(),
            provenance: crate::desired::Provenance::Git {
                overlay: "ov".to_string(),
                repo_key: ".".to_string(),
                config: Box::new(GitRepoConfig {
                    url: String::new(),
                    branch: None,
                    tag: None,
                    rev: None,
                    recurse_submodules: false,
                    worktree: false,
                    per_worktree_config: false,
                    worktrees: None,
                    remotes: None,
                    config: None,
                    worktree_config: None,
                }),
            },
            intent: MaterializationIntent::Checkout,
            permissions: None,
        };

        let change = classify_checkout(&entry).unwrap();
        assert!(matches!(change, Change::Checkout(Status::Modified)));
        let display = format!("{}", DiffEntry { entry, change });
        assert!(display.contains("modified:"));
    }

    /// Build a declared (non-root, `repo_key != "."`) `Checkout` entry
    /// backed by a real committed git repo at `path`, whose `origin`
    /// remote is set to `url` — the shared fixture for the two declared-
    /// repo tests below (#140).
    fn declared_checkout_entry(path: &std::path::Path, url: &str) -> DesiredEntry {
        use crate::actions::git::config::GitRepoConfig;
        use git2::{Repository as GitRepository, Signature};

        let repo = GitRepository::init(path).unwrap();
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "Test").unwrap();
        cfg.set_str("user.email", "test@test.com").unwrap();
        drop(cfg);
        let sig = Signature::now("Test", "test@test.com").unwrap();
        fs::write(path.join("README.md"), "# Test").unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
        repo.remote("origin", url).unwrap();

        DesiredEntry {
            target: path.to_path_buf(),
            provenance: crate::desired::Provenance::Git {
                overlay: "ov".to_string(),
                repo_key: ".config/nvim".to_string(),
                config: Box::new(GitRepoConfig {
                    url: url.to_string(),
                    branch: None,
                    tag: None,
                    rev: None,
                    recurse_submodules: false,
                    worktree: false,
                    per_worktree_config: false,
                    worktrees: None,
                    remotes: None,
                    config: None,
                    worktree_config: None,
                }),
            },
            intent: MaterializationIntent::Checkout,
            permissions: None,
        }
    }

    #[test]
    fn declared_repo_with_unrelated_dirty_content_is_unchanged() {
        // #140: unlike the root checkout (see
        // `dirty_git_checkout_maps_to_checkout_modified`), unrelated dirty
        // content in a declared (non-root) repository must never surface
        // as a diff — only its declared configuration matters.
        let td = TempDir::new().unwrap();
        let entry = declared_checkout_entry(td.path(), "https://example.com/nvim.git");
        fs::write(td.path().join("README.md"), "written by the plugin").unwrap();
        fs::write(td.path().join("state.json"), "{}").unwrap();

        let change = classify_checkout(&entry).unwrap();
        assert!(matches!(change, Change::Unchanged));
    }

    fn init_committed_repo(path: &std::path::Path) -> git2::Repository {
        let repo = git2::Repository::init(path).unwrap();
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "Test").unwrap();
        cfg.set_str("user.email", "test@test.com").unwrap();
        drop(cfg);
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        {
            let tree = repo.find_tree(tree_id).unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
                .unwrap();
        }
        repo
    }

    fn virtual_checkout_entry(
        target: std::path::PathBuf,
        source: std::path::PathBuf,
    ) -> DesiredEntry {
        DesiredEntry {
            target,
            provenance: crate::desired::Provenance::Overlay {
                overlay: "ov".to_string(),
                source,
            },
            intent: MaterializationIntent::VirtualCheckout,
            permissions: None,
        }
    }

    async fn materialize_and_record_virtual_checkout(
        source_root: &std::path::Path,
        target: &std::path::Path,
    ) {
        let repo = git2::Repository::open(source_root).unwrap();
        fs::create_dir_all(target).unwrap();
        let head_tree = repo.head().unwrap().peel_to_tree().unwrap();
        crate::materialize::virtual_checkout::git::checkout_subtree(&repo, &head_tree, target)
            .unwrap();
        let base_oid = repo
            .head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .id()
            .to_string();
        crate::materialize::virtual_checkout::state::persist(
            target,
            crate::materialize::virtual_checkout::state::VirtualCheckoutRecord {
                overlay: "ov".to_string(),
                managed_path: std::path::PathBuf::new(),
                base_oid,
                created_at: 0,
                last_commit_at: None,
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn virtual_checkout_clean_reports_unchanged() {
        let _xdg = isolate_xdg_state();
        let td = TempDir::new().unwrap();
        td.child("a.txt").write_str("a").unwrap();
        init_committed_repo(td.path());
        let target = td.child("target");
        materialize_and_record_virtual_checkout(td.path(), target.path()).await;

        let entry = virtual_checkout_entry(target.path().to_path_buf(), td.path().to_path_buf());
        let change = classify_virtual_checkout(&entry).unwrap();
        assert!(matches!(change, Change::Unchanged));
    }

    #[test]
    fn virtual_checkout_missing_reports_missing() {
        let _xdg = isolate_xdg_state();
        let td = TempDir::new().unwrap();
        td.child("a.txt").write_str("a").unwrap();
        init_committed_repo(td.path());

        let entry =
            virtual_checkout_entry(td.path().join("does-not-exist"), td.path().to_path_buf());
        let change = classify_virtual_checkout(&entry).unwrap();
        assert!(matches!(change, Change::Missing));
    }

    #[test]
    fn virtual_checkout_broken_reports_broken() {
        let _xdg = isolate_xdg_state();
        let td = TempDir::new().unwrap();
        td.child("a.txt").write_str("a").unwrap();
        init_committed_repo(td.path());
        // A real directory with no recorded XDG association.
        let target = td.child("target");
        target.create_dir_all().unwrap();

        let entry = virtual_checkout_entry(target.path().to_path_buf(), td.path().to_path_buf());
        let change = classify_virtual_checkout(&entry).unwrap();
        assert!(matches!(change, Change::Broken));
    }

    #[tokio::test]
    async fn virtual_checkout_modified_file_produces_content_diff() {
        let _xdg = isolate_xdg_state();
        let td = TempDir::new().unwrap();
        td.child("a.txt").write_str("original\n").unwrap();
        init_committed_repo(td.path());
        let target = td.child("target");
        materialize_and_record_virtual_checkout(td.path(), target.path()).await;

        fs::write(target.path().join("a.txt"), "edited locally\n").unwrap();

        let entry = virtual_checkout_entry(target.path().to_path_buf(), td.path().to_path_buf());
        let change = classify_virtual_checkout(&entry).unwrap();
        match change {
            Change::VirtualCheckout { status, files } => {
                assert_eq!(status, Status::Modified);
                assert_eq!(files.len(), 1);
                assert_eq!(files[0].kind, VirtualCheckoutFileKind::Modified);
                let diff_text = format!("{}", files[0].diff.as_ref().unwrap());
                assert!(diff_text.contains("original"));
                assert!(diff_text.contains("edited locally"));
            }
            other => panic!("expected VirtualCheckout, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn virtual_checkout_added_and_deleted_files_have_no_diff() {
        let _xdg = isolate_xdg_state();
        let td = TempDir::new().unwrap();
        td.child("a.txt").write_str("a").unwrap();
        td.child("b.txt").write_str("b").unwrap();
        init_committed_repo(td.path());
        let target = td.child("target");
        materialize_and_record_virtual_checkout(td.path(), target.path()).await;

        fs::remove_file(target.path().join("b.txt")).unwrap();
        fs::write(target.path().join("new.txt"), "new").unwrap();

        let entry = virtual_checkout_entry(target.path().to_path_buf(), td.path().to_path_buf());
        let change = classify_virtual_checkout(&entry).unwrap();
        match change {
            Change::VirtualCheckout { files, .. } => {
                assert_eq!(files.len(), 2);
                let added = files
                    .iter()
                    .find(|f| f.kind == VirtualCheckoutFileKind::Added)
                    .unwrap();
                assert!(added.diff.is_none());
                let deleted = files
                    .iter()
                    .find(|f| f.kind == VirtualCheckoutFileKind::Deleted)
                    .unwrap();
                assert!(deleted.diff.is_none());

                // Both `Display` arms, and the multi-entry separator
                // (`i > 0`) in `DiffEntry`'s own `Display`.
                let diff_entry = DiffEntry {
                    entry: entry.clone(),
                    change: Change::VirtualCheckout {
                        status: Status::Modified,
                        files,
                    },
                };
                let s = format!("{diff_entry}");
                assert!(s.contains("added:"));
                assert!(s.contains("deleted:"));
            }
            other => panic!("expected VirtualCheckout, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn virtual_checkout_binary_modification_falls_back_to_binary_diff() {
        let _xdg = isolate_xdg_state();
        let td = TempDir::new().unwrap();
        fs::write(td.path().join("bin.dat"), [0xff_u8, 0xfe, 0x00]).unwrap();
        init_committed_repo(td.path());
        let target = td.child("target");
        materialize_and_record_virtual_checkout(td.path(), target.path()).await;

        fs::write(target.path().join("bin.dat"), [0x00_u8, 0xff, 0xfe]).unwrap();

        let entry = virtual_checkout_entry(target.path().to_path_buf(), td.path().to_path_buf());
        let change = classify_virtual_checkout(&entry).unwrap();
        match change {
            Change::VirtualCheckout { files, .. } => {
                assert_eq!(files.len(), 1);
                assert!(files[0].diff.as_ref().unwrap().lines.is_none());
            }
            other => panic!("expected VirtualCheckout, got {other:?}"),
        }
    }

    #[test]
    fn declared_repo_config_drift_is_reported_as_checkout_conflict() {
        let td = TempDir::new().unwrap();
        let mut entry = declared_checkout_entry(td.path(), "https://example.com/nvim.git");
        // Declared config now points elsewhere than what's actually cloned.
        if let crate::desired::Provenance::Git { config, .. } = &mut entry.provenance {
            config.url = "https://example.com/different-plugin.git".to_string();
        }

        let change = classify_checkout(&entry).unwrap();
        assert!(matches!(change, Change::Checkout(Status::Conflict)));
    }
}
