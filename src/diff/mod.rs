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

use anyhow::Result;

use crate::desired::{DesiredEntry, DesiredTree, MaterializationIntent};
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
}

/// One [`DesiredEntry`] paired with what differs about it.
#[derive(Debug, Clone)]
pub struct DiffEntry {
    pub entry: DesiredEntry,
    pub change: Change,
}

impl DiffEntry {
    /// Whether this entry has anything worth showing — used by the CLI
    /// to decide what to print without `--verbose`, same convention as
    /// `status::Status::needs_attention`.
    pub fn has_diff(&self) -> bool {
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

    /// Whether anything in this report has a diff worth showing.
    pub fn has_changes(&self) -> bool {
        self.entries.iter().any(DiffEntry::has_diff)
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
    let old = fs::read(actual_path)?;
    let new = fs::read(desired_path)?;
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
        assert!(report.has_changes());
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
        assert!(!file_entry.has_diff());
        assert!(!report.has_changes());
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
        assert!(report.has_changes());
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
        };
        let change = classify_fs(&entry, &Operation::Noop).unwrap();
        assert!(matches!(change, Change::Broken));
    }

    #[test]
    fn empty_desired_tree_produces_empty_report() {
        let report = Report::build(&DesiredTree::default()).unwrap();
        assert!(report.is_empty());
        assert!(!report.has_changes());
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
        };

        let change = classify_checkout(&entry).unwrap();
        assert!(matches!(change, Change::Checkout(Status::Modified)));
        let display = format!("{}", DiffEntry { entry, change });
        assert!(display.contains("modified:"));
    }
}
