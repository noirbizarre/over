//! Overlay status (#12): derives per-entry reconciliation status from the
//! same [`DesiredTree`]/[`Plan`] model [`crate::overlays::Overlay::apply`]
//! uses (#107/#13), plus git working-tree state for
//! [`MaterializationIntent::Checkout`] entries (see [`git`]).
//!
//! Deliberately read-only: building a [`Report`] never mutates the
//! filesystem or any git repository, exactly like [`Plan::build`].
//! Persistent metadata (`$XDG_STATE_HOME/over`) is not involved — status
//! must stay fully reconstructible from disk/git/config alone, per the
//! issue's own requirement.

pub mod git;

use std::fmt;

use anyhow::Result;

use crate::actions::symlink::LinkType;
use crate::desired::{DesiredEntry, DesiredTree, MaterializationIntent};
use crate::plan::{Operation, Plan};
use crate::ui::{emojis, style};
use crate::utils::short_path;

/// The status of a single [`DesiredEntry`] — a closed set covering every
/// entry kind uniformly (directories, symlinks, and git checkouts alike),
/// matching the minimum set called for by #12.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Target matches desired intent — nothing to do.
    Applied,
    /// Target doesn't exist yet.
    Missing,
    /// A git checkout has uncommitted changes.
    Modified,
    /// A dangling soft symlink (correctly pointed, but its source
    /// disappeared), or a checkout path that isn't a valid git repo.
    Broken,
    /// Target exists and doesn't match desired intent, or a git checkout
    /// has a merge/rebase/cherry-pick in progress.
    Conflict,
    /// A git checkout is ahead of its upstream by this many commits.
    Ahead(usize),
    /// A git checkout is behind its upstream by this many commits.
    Behind(usize),
    /// A git checkout has diverged from its upstream.
    Diverged { ahead: usize, behind: usize },
}

impl Status {
    /// Whether this status needs attention — used by [`Report::has_issues`]
    /// and by the CLI to decide what to show without `--verbose`.
    pub fn needs_attention(&self) -> bool {
        !matches!(self, Status::Applied)
    }
}

/// One [`DesiredEntry`] paired with its reconciliation [`Status`].
#[derive(Debug, Clone)]
pub struct EntryStatus {
    pub entry: DesiredEntry,
    pub status: Status,
}

impl fmt::Display for EntryStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let target = short_path(&self.entry.target.to_string_lossy());
        match &self.status {
            Status::Applied => write!(
                f,
                "{} {} {}",
                emojis::CHECKMARK,
                style::white("applied:"),
                target
            ),
            Status::Missing => write!(
                f,
                "{} {} {}",
                emojis::CROSSMARK,
                style::white("missing:"),
                target
            ),
            Status::Modified => write!(
                f,
                "{} {} {}",
                emojis::WARNING,
                style::yellow("modified:"),
                target
            ),
            Status::Broken => write!(
                f,
                "{} {} {}",
                emojis::WARNING,
                style::yellow("broken:"),
                target
            ),
            Status::Conflict => write!(
                f,
                "{} {} {}",
                emojis::WARNING,
                style::yellow("conflict:"),
                target
            ),
            Status::Ahead(n) => write!(
                f,
                "{} {} {} ({} commit{} ahead)",
                emojis::THREAD,
                style::white("ahead:"),
                target,
                n,
                if *n == 1 { "" } else { "s" },
            ),
            Status::Behind(n) => write!(
                f,
                "{} {} {} ({} commit{} behind)",
                emojis::THREAD,
                style::white("behind:"),
                target,
                n,
                if *n == 1 { "" } else { "s" },
            ),
            Status::Diverged { ahead, behind } => write!(
                f,
                "{} {} {} ({ahead} ahead, {behind} behind)",
                emojis::WARNING,
                style::yellow("diverged:"),
                target,
            ),
        }
    }
}

/// Per-`Status` counts of a [`Report`], in the same order `Status` is
/// declared.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub applied: usize,
    pub missing: usize,
    pub modified: usize,
    pub broken: usize,
    pub conflict: usize,
    pub ahead: usize,
    pub behind: usize,
    pub diverged: usize,
}

impl fmt::Display for Counts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} applied, {} missing, {} modified, {} broken, {} conflict(s), \
             {} ahead, {} behind, {} diverged",
            self.applied,
            self.missing,
            self.modified,
            self.broken,
            self.conflict,
            self.ahead,
            self.behind,
            self.diverged,
        )
    }
}

/// The reconciliation status of a whole [`DesiredTree`]: one
/// [`EntryStatus`] per [`DesiredEntry`].
#[derive(Debug, Clone, Default)]
pub struct Report {
    entries: Vec<EntryStatus>,
}

impl Report {
    /// Classify every entry in `desired` — [`Plan::build`] handles
    /// directories/symlinks (never touching the filesystem beyond
    /// read-only inspection), and [`git::inspect`] handles
    /// [`MaterializationIntent::Checkout`] entries (the only ones `Plan`
    /// leaves as [`Operation::Deferred`], since no
    /// [`crate::materialize::Materializer`] claims that intent yet).
    pub fn build(desired: &DesiredTree) -> Result<Self> {
        let plan = Plan::build(desired)?;
        let mut entries = Vec::with_capacity(plan.len());
        for step in plan.steps() {
            let status = match (&step.entry.intent, &step.operation) {
                (MaterializationIntent::Checkout, _) => git::inspect(&step.entry)?,
                (_, Operation::Create) => Status::Missing,
                (_, Operation::Conflict { .. }) => Status::Conflict,
                (intent, Operation::Noop) => classify_noop(intent),
                (_, Operation::Deferred) => {
                    unreachable!("only Checkout entries ever classify as Deferred")
                }
            };
            entries.push(EntryStatus {
                entry: step.entry.clone(),
                status,
            });
        }
        Ok(Self { entries })
    }

    pub fn entries(&self) -> &[EntryStatus] {
        &self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn counts(&self) -> Counts {
        let mut counts = Counts::default();
        for e in &self.entries {
            match e.status {
                Status::Applied => counts.applied += 1,
                Status::Missing => counts.missing += 1,
                Status::Modified => counts.modified += 1,
                Status::Broken => counts.broken += 1,
                Status::Conflict => counts.conflict += 1,
                Status::Ahead(_) => counts.ahead += 1,
                Status::Behind(_) => counts.behind += 1,
                Status::Diverged { .. } => counts.diverged += 1,
            }
        }
        counts
    }

    /// Whether anything in this report needs attention (everything except
    /// `Applied`). Informational only — `over status` never fails the
    /// process just because entries need attention, mirroring `git
    /// status`.
    pub fn has_issues(&self) -> bool {
        self.entries.iter().any(|e| e.status.needs_attention())
    }
}

/// Classify an [`Operation::Noop`] entry more precisely than "applied":
/// a soft symlink that already points where desired but whose source has
/// since disappeared is `Broken`, not `Applied`. Hard links keep their
/// data independently of the source (the link *is* a second name for the
/// same inode), and `Directory` has no such concept, so both stay
/// `Applied`.
///
/// `pub(crate)`: `crate::diff` reuses this exact check for the same
/// `Noop` case instead of re-deriving it (#109).
pub(crate) fn classify_noop(intent: &MaterializationIntent) -> Status {
    match intent {
        MaterializationIntent::SymlinkFile { source, link_type }
        | MaterializationIntent::SymlinkDirectory { source, link_type }
            if *link_type == LinkType::Soft =>
        {
            if source.exists() {
                Status::Applied
            } else {
                Status::Broken
            }
        }
        _ => Status::Applied,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        assert_eq!(report.entries()[0].status, Status::Missing);
        assert_eq!(report.counts().missing, 1);
        assert!(report.has_issues());
    }

    #[tokio::test]
    async fn applied_target_reports_applied() {
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
        crate::plan::Plan::build(&desired)
            .unwrap()
            .execute(ctx.clone())
            .await
            .unwrap();

        let desired2 = DesiredTree::build(&ctx, &overlay).unwrap();
        let report = Report::build(&desired2).unwrap();
        let file_status = report
            .entries()
            .iter()
            .find(|e| e.entry.target == td.path().join("file.txt"))
            .unwrap();
        assert_eq!(file_status.status, Status::Applied);
        assert_eq!(report.counts().missing, 0);
        assert!(!report.has_issues());
    }

    #[rstest]
    fn conflicting_file_reports_conflict() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        overlay_dir.child("file.txt").write_str("overlay").unwrap();
        fs::write(td.path().join("file.txt"), "existing").unwrap();

        let overlay = repo.get("ov").unwrap();
        let ctx = Context::builder()
            .root(td.path().to_path_buf())
            .repository(repo.clone())
            .overlay(overlay.clone())
            .build();

        let desired = DesiredTree::build(&ctx, &overlay).unwrap();
        let report = Report::build(&desired).unwrap();
        let file_status = report
            .entries()
            .iter()
            .find(|e| e.entry.target == td.path().join("file.txt"))
            .unwrap();
        assert_eq!(file_status.status, Status::Conflict);
        assert!(report.has_issues());
    }

    #[test]
    fn dangling_soft_symlink_noop_is_broken() {
        // The overlay source no longer exists on disk (deleted after the
        // link was created) — `DesiredTree` itself won't ever produce this
        // entry (its source vanished from `walk_overlay_tree`), so exercise
        // `classify_noop` directly against the intent a stale `Plan::build`
        // classification would still see as `Noop` (the on-disk symlink
        // still points to the recorded `source` path).
        let intent = MaterializationIntent::SymlinkFile {
            source: std::path::PathBuf::from("/does/not/exist/anymore"),
            link_type: LinkType::Soft,
        };
        assert_eq!(classify_noop(&intent), Status::Broken);
    }

    #[test]
    fn empty_desired_tree_produces_empty_report() {
        let report = Report::build(&DesiredTree::default()).unwrap();
        assert!(report.is_empty());
        assert!(!report.has_issues());
    }

    #[test]
    fn hard_link_noop_is_always_applied_even_if_source_missing() {
        let intent = MaterializationIntent::SymlinkFile {
            source: std::path::PathBuf::from("/does/not/exist"),
            link_type: LinkType::Hard,
        };
        assert_eq!(classify_noop(&intent), Status::Applied);
    }

    #[test]
    fn directory_noop_is_always_applied() {
        assert_eq!(
            classify_noop(&MaterializationIntent::Directory),
            Status::Applied
        );
    }

    /// Exercise every `EntryStatus` `Display` arm directly — most of them
    /// (`Applied`, `Modified`, `Broken`, `Ahead`, `Behind`, `Diverged`)
    /// never print by default (`cli::status` only prints entries that
    /// `needs_attention()`, and `Applied` never does), so a report built
    /// through `Report::build` alone wouldn't reach them.
    #[test]
    fn entry_status_display_covers_every_status_variant() {
        fn entry_status(status: Status) -> EntryStatus {
            EntryStatus {
                entry: DesiredEntry {
                    target: std::path::PathBuf::from("/tmp/target"),
                    provenance: crate::desired::Provenance::Overlay {
                        overlay: "ov".to_string(),
                        source: std::path::PathBuf::from("/repo/ov"),
                    },
                    intent: MaterializationIntent::Directory,
                },
                status,
            }
        }

        assert!(format!("{}", entry_status(Status::Applied)).contains("applied:"));
        assert!(format!("{}", entry_status(Status::Missing)).contains("missing:"));
        assert!(format!("{}", entry_status(Status::Modified)).contains("modified:"));
        assert!(format!("{}", entry_status(Status::Broken)).contains("broken:"));
        assert!(format!("{}", entry_status(Status::Conflict)).contains("conflict:"));
        let ahead = format!("{}", entry_status(Status::Ahead(1)));
        assert!(ahead.contains("ahead:") && ahead.contains("1 commit ahead"));
        let ahead_plural = format!("{}", entry_status(Status::Ahead(2)));
        assert!(ahead_plural.contains("2 commits ahead"));
        let behind = format!("{}", entry_status(Status::Behind(1)));
        assert!(behind.contains("behind:") && behind.contains("1 commit behind"));
        let behind_plural = format!("{}", entry_status(Status::Behind(3)));
        assert!(behind_plural.contains("3 commits behind"));
        let diverged = format!(
            "{}",
            entry_status(Status::Diverged {
                ahead: 2,
                behind: 3
            })
        );
        assert!(diverged.contains("diverged:"));
        assert!(diverged.contains("2 ahead, 3 behind"));
    }

    /// `Report::counts()` only increments a bucket when a matching status
    /// is present — build a synthetic `Report` (private field, but tests
    /// are a child module) covering every variant, since `Report::build`
    /// alone never produces `Modified`/`Broken`/`Ahead`/`Behind`/`Diverged`
    /// without a real git checkout on disk.
    #[test]
    fn counts_tallies_every_status_variant() {
        fn entry_status(status: Status) -> EntryStatus {
            EntryStatus {
                entry: DesiredEntry {
                    target: std::path::PathBuf::from("/tmp/target"),
                    provenance: crate::desired::Provenance::Overlay {
                        overlay: "ov".to_string(),
                        source: std::path::PathBuf::from("/repo/ov"),
                    },
                    intent: MaterializationIntent::Directory,
                },
                status,
            }
        }

        let report = Report {
            entries: vec![
                entry_status(Status::Applied),
                entry_status(Status::Missing),
                entry_status(Status::Modified),
                entry_status(Status::Broken),
                entry_status(Status::Conflict),
                entry_status(Status::Ahead(1)),
                entry_status(Status::Behind(1)),
                entry_status(Status::Diverged {
                    ahead: 1,
                    behind: 1,
                }),
            ],
        };
        let counts = report.counts();
        assert_eq!(
            counts,
            Counts {
                applied: 1,
                missing: 1,
                modified: 1,
                broken: 1,
                conflict: 1,
                ahead: 1,
                behind: 1,
                diverged: 1,
            }
        );
        assert!(report.has_issues());
    }

    #[test]
    fn dangling_soft_symlink_directory_noop_is_broken() {
        // Same as `dangling_soft_symlink_noop_is_broken` but for the
        // `SymlinkDirectory` alternative of `classify_noop`'s pattern.
        let intent = MaterializationIntent::SymlinkDirectory {
            source: std::path::PathBuf::from("/does/not/exist/anymore"),
            link_type: LinkType::Soft,
        };
        assert_eq!(classify_noop(&intent), Status::Broken);
    }

    #[rstest]
    fn git_checkout_entry_dispatches_to_git_inspect() {
        // Not yet cloned: `DesiredTree` only ever describes the checkout
        // (`Overlay::apply` clones it separately, outside `Plan`), so
        // `Report::build` should route it through `git::inspect` and get
        // `Missing` back, exercising the `Checkout` arm of its match.
        // Uses a subpath (not the overlay's own target root, which the
        // fresh `TempDir` already exists as — that would report `Broken`
        // instead, a real directory occupying a git-managed path).
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
        let checkout_status = report
            .entries()
            .iter()
            .find(|e| matches!(e.entry.intent, MaterializationIntent::Checkout))
            .unwrap();
        assert_eq!(checkout_status.status, Status::Missing);
    }

    #[test]
    fn counts_display_lists_every_status() {
        let counts = Counts {
            applied: 1,
            missing: 2,
            modified: 3,
            broken: 4,
            conflict: 5,
            ahead: 6,
            behind: 7,
            diverged: 8,
        };
        let s = format!("{counts}");
        assert!(s.contains("1 applied"));
        assert!(s.contains("2 missing"));
        assert!(s.contains("3 modified"));
        assert!(s.contains("4 broken"));
        assert!(s.contains("5 conflict(s)"));
        assert!(s.contains("6 ahead"));
        assert!(s.contains("7 behind"));
        assert!(s.contains("8 diverged"));
    }
}
