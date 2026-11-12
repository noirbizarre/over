//! Overlay status (#12): derives per-entry reconciliation status from the
//! same [`DesiredTree`]/[`Plan`] model [`crate::overlays::Overlay::apply`]
//! uses (#107/#13), plus git working-tree state for
//! [`MaterializationIntent::Checkout`] entries (see [`git`]) and virtual
//! checkout blob-hash comparison for
//! [`MaterializationIntent::VirtualCheckout`] entries (#141, see
//! [`virtual_checkout`]).
//!
//! Deliberately read-only: building a [`Report`] never mutates the
//! filesystem or any git repository, exactly like [`Plan::build`].
//! Virtual checkouts read their recorded association from XDG state without
//! creating locks, migrating on disk, or persisting inspection results.
//!
//! Git prerequisites use effective protection and scoped configuration findings,
//! shared with doctor's health summary rather than managed-block bookkeeping.

pub mod configuration;
pub mod git;
mod identity;
pub mod overlay;
pub(crate) mod virtual_checkout;

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
    /// Encrypted content has not been verified because the credential is unavailable.
    Locked,
    /// Target matches desired intent — nothing to do. For a declared
    /// (non-root) `git` entry (#140, ADR-021), this means the repository
    /// is present and its declared revision is applied — regardless of uncommitted changes, local
    /// commits, or other content a plugin manager (or the user) put there.
    Applied,
    /// Target doesn't exist yet. For a declared `git` entry, this means it
    /// still needs provisioning (#140).
    Missing,
    /// A git checkout has uncommitted changes, or a filesystem entry's
    /// content/kind already matches but its permission mode differs (#65,
    /// `Operation::Repair`) — auto-fixable on the next `apply`.
    Modified,
    /// A dangling soft symlink (correctly pointed, but its source
    /// disappeared), or a checkout path that isn't a valid git repo.
    Broken,
    /// Target exists and doesn't match desired intent, a git checkout has
    /// a merge/rebase/cherry-pick in progress, or — for a declared
    /// (non-root) `git` entry (#140) — its declared revision no longer matches.
    /// Supporting Git configuration is reported separately as warning findings.
    Conflict,
    /// A git checkout is ahead of its upstream by this many commits.
    Ahead(usize),
    /// A git checkout is behind its upstream by this many commits.
    Behind(usize),
    /// A git checkout has diverged from its upstream.
    Diverged { ahead: usize, behind: usize },
}

impl Status {
    /// Shared runtime severity; commands retain their own exit thresholds.
    pub fn health_severity(&self) -> Option<configuration::Severity> {
        match self {
            Self::Applied => None,
            Self::Broken | Self::Conflict => Some(configuration::Severity::Error),
            _ => Some(configuration::Severity::Warning),
        }
    }

    fn severity(&self) -> style::Severity {
        match self.health_severity() {
            None => style::Severity::Success,
            Some(configuration::Severity::Error) => style::Severity::Error,
            Some(configuration::Severity::Warning) => style::Severity::Warning,
        }
    }
    /// Whether this status needs attention — used by [`Report::needs_attention`]
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
        let target = style::path(short_path(&self.entry.target.to_string_lossy()));
        let label = |text| style::severity(text, self.status.severity());
        match &self.status {
            Status::Locked => write!(
                f,
                "{} {target} (set OVER_SECRET to verify)",
                label("locked:")
            )?,
            Status::Applied => write!(f, "{} {}", label("✔ applied:"), target)?,
            Status::Missing => write!(f, "{} {}", label("✘ missing:"), target)?,
            Status::Modified => write!(f, "{} {}", label("⚠ modified:"), target)?,
            Status::Broken => write!(f, "{} {}", label("⚠ broken:"), target)?,
            Status::Conflict => write!(f, "{} {}", label("⚠ conflict:"), target)?,
            Status::Ahead(n) => write!(
                f,
                "{} {} {} ({} commit{} ahead)",
                emojis::THREAD,
                label("ahead:"),
                target,
                n,
                if *n == 1 { "" } else { "s" },
            )?,
            Status::Behind(n) => write!(
                f,
                "{} {} {} ({} commit{} behind)",
                emojis::THREAD,
                label("behind:"),
                target,
                n,
                if *n == 1 { "" } else { "s" },
            )?,
            Status::Diverged { ahead, behind } => write!(
                f,
                "{} {} ({ahead} ahead, {behind} behind)",
                label("⚠ diverged:"),
                target,
            )?,
        }

        Ok(())
    }
}

/// One-character-ish marker for a virtual checkout's per-file change kind
/// in `over status`'s listing — mirrors `git status --short`'s single-
/// letter convention (`A`/`M`/`D`) rather than a full word, since these
/// lines are already indented under a labeled aggregate status line.
fn file_change_symbol(
    kind: crate::materialize::virtual_checkout::git::FileChangeKind,
) -> &'static str {
    use crate::materialize::virtual_checkout::git::FileChangeKind;
    match kind {
        FileChangeKind::Added => "A",
        FileChangeKind::Modified => "M",
        FileChangeKind::Deleted => "D",
    }
}

/// Per-`Status` counts of a [`Report`], in the same order `Status` is
/// declared.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub locked: usize,
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
        let categories = [
            (self.applied, "applied", Status::Applied),
            (self.missing, "missing", Status::Missing),
            (self.modified, "modified", Status::Modified),
            (self.broken, "broken", Status::Broken),
            (self.conflict, "conflict(s)", Status::Conflict),
            (self.ahead, "ahead", Status::Ahead(0)),
            (self.behind, "behind", Status::Behind(0)),
            (
                self.diverged,
                "diverged",
                Status::Diverged {
                    ahead: 0,
                    behind: 0,
                },
            ),
            (self.locked, "locked", Status::Locked),
        ]
        .into_iter()
        .filter(|(count, _, _)| *count > 0)
        .map(|(count, label, status)| count_label(count, label, status).to_string())
        .collect::<Vec<_>>();
        if categories.is_empty() {
            return f.write_str("no entries");
        }
        f.write_str(&categories.join(", "))
    }
}

fn count_label(count: usize, label: &str, status: Status) -> console::StyledObject<String> {
    let text = format!("{count} {label}");
    style::severity(text, status.severity())
}

/// The reconciliation status of a whole [`DesiredTree`]: one
/// [`EntryStatus`] per [`DesiredEntry`], plus structured Git prerequisites.
#[derive(Debug, Clone, Default)]
pub struct Report {
    entries: Vec<EntryStatus>,
    git_configuration: Vec<configuration::Issue>,
    inspection_failures: Vec<std::path::PathBuf>,
}

impl Report {
    /// Classify every entry in `desired` — [`Plan::build`] handles
    /// directories/symlinks (never touching the filesystem beyond
    /// read-only inspection), while [`git::inspect`] and
    /// [`virtual_checkout::inspect`] recompute their own, richer status
    /// directly rather than trusting `Plan`'s coarse `Noop`/`Create` for
    /// [`MaterializationIntent::Checkout`]/[`MaterializationIntent::VirtualCheckout`]
    /// entries (both backends deliberately collapse anything but `Missing`
    /// to `Noop`, deferring dirty/ahead/behind/conflict reporting to this
    /// module instead — see `CheckoutMaterializer`/`VirtualCheckoutMaterializer`'s
    /// own docs).
    pub fn build(desired: &DesiredTree) -> Result<Self> {
        Self::build_with_inspector(desired, virtual_checkout::inspect)
    }

    pub(crate) fn build_with_inspector(
        desired: &DesiredTree,
        mut inspect: impl FnMut(&DesiredEntry) -> Result<Status>,
    ) -> Result<Self> {
        // Overlay views retain the inspector's details; other consumers can keep
        // their existing content-discovery policy without a second status model.
        let plan = Plan::build(desired)?;
        let mut entries = Vec::with_capacity(plan.len());
        let mut inspection_failures = Vec::new();
        for (index, step) in plan.steps().iter().enumerate() {
            let inspected = (|| -> Result<Status> {
                // No intent-specific inspector may read through a blocked ancestor.
                if plan.blocker_for_step(index).is_some() {
                    return Ok(Status::Conflict);
                }
                Ok(match (&step.entry.intent, &step.operation) {
                    (_, Operation::NeedsUnlock) => Status::Locked,
                    (MaterializationIntent::Checkout, _) => git::inspect_runtime(&step.entry)?,
                    (
                        MaterializationIntent::VirtualCheckout
                        | MaterializationIntent::ScopedVirtualCheckout(_),
                        _,
                    ) => inspect(&step.entry)?,
                    (_, Operation::Create) => Status::Missing,
                    (_, Operation::Conflict { .. }) => Status::Conflict,
                    (intent, Operation::Noop) => classify_noop(intent),
                    // A pending rule-change migration (#129, blocked or not):
                    // target exists and doesn't match desired intent — `status`
                    // has no dedicated vocabulary for "migration pending", but
                    // `Conflict` is accurate (and an improvement over silently
                    // misclassifying these as `Noop`, as happened before #129).
                    (_, Operation::Migrate { .. }) => Status::Conflict,
                    // A permission-only drift (#65): content/kind already
                    // matches, only the mode needs fixing — auto-fixable, not a
                    // real conflict, so it's reported the same way a dirty git
                    // checkout is.
                    (_, Operation::Repair { .. }) => Status::Modified,
                    (_, Operation::Deferred) => {
                        anyhow::bail!("entry classification is deferred without a scaffold blocker")
                    }
                })
            })();
            let status = match inspected {
                Ok(status) => status,
                Err(_) => {
                    // Keep other entries inspectable, but never expose secret-bearing Git errors.
                    inspection_failures.push(step.entry.target.clone());
                    Status::Broken
                }
            };
            entries.push(EntryStatus {
                entry: step.entry.clone(),
                status,
            });
        }

        Ok(Self {
            entries,
            git_configuration: configuration::inspect(&plan.inspectable_desired())?,
            inspection_failures,
        })
    }

    pub fn entries(&self) -> &[EntryStatus] {
        &self.entries
    }

    pub fn git_configuration(&self) -> &[configuration::Issue] {
        &self.git_configuration
    }

    pub fn inspection_failed(&self, target: &std::path::Path) -> bool {
        self.inspection_failures.iter().any(|path| path == target)
    }

    /// Stable issue identities let doctor count a shared dependency once while
    /// still marking every composition that depends on it as unhealthy.
    pub fn health_issues(&self) -> Vec<(String, configuration::Severity)> {
        let mut issues = Vec::new();
        for entry in self
            .entries
            .iter()
            .filter(|entry| entry.status.needs_attention())
        {
            let Some(severity) = entry.status.health_severity() else {
                continue;
            };
            let claim = identity::ClaimIdentity::new(&entry.entry);
            issues.push((format!("{claim:?}:{:?}", entry.status), severity));
        }
        issues.extend(
            self.git_configuration
                .iter()
                .map(|issue| (format!("{issue:?}"), issue.severity)),
        );
        issues
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn counts(&self) -> Counts {
        let mut counts = Counts::default();
        for e in &self.entries {
            match e.status {
                Status::Locked => counts.locked += 1,
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
    /// `Applied`, plus any Git prerequisite problem). The canonical status CLI
    /// uses this to choose its exit status.
    pub fn needs_attention(&self) -> bool {
        self.entries.iter().any(|e| e.status.needs_attention())
            || !self.git_configuration.is_empty()
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

/// Short, human-readable reason a checkout isn't safe to migrate away from
/// yet — used by [`Operation::Migrate`](crate::plan::Operation)'s `blocked`
/// field (#129). `unapply`'s own `status_word` stays separate (its own
/// taxonomy, ADR-013); this is a full phrase, not a single word.
pub(crate) fn describe(status: &Status) -> String {
    match status {
        Status::Locked => "requires unlocking to verify encrypted content".to_string(),
        Status::Applied => "is applied".to_string(),
        Status::Missing => "is missing".to_string(),
        Status::Modified => "has uncommitted changes".to_string(),
        Status::Broken => "isn't a valid git repository".to_string(),
        Status::Conflict => "has a merge/rebase/cherry-pick in progress".to_string(),
        Status::Ahead(n) => format!("is {n} commit{} ahead of its upstream", plural(*n)),
        Status::Behind(n) => format!("is {n} commit{} behind its upstream", plural(*n)),
        Status::Diverged { ahead, behind } => {
            format!("has diverged from its upstream ({ahead} ahead, {behind} behind)")
        }
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
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

    /// Points `$XDG_STATE_HOME` at a fresh, writable temp dir for the
    /// duration of the returned guard's lifetime — needed only by the one
    /// test here that persists a `VirtualCheckoutState` record via the
    /// real, non-injectable `XdgDirs::new()`, so it never pollutes the
    /// real `$XDG_STATE_HOME/over/virtual_checkout.toml`.
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
        assert_eq!(report.entries()[0].status, Status::Missing);
        assert_eq!(report.counts().missing, 1);
        assert!(report.needs_attention());
    }

    #[rstest]
    #[case(false, "file")]
    #[case(false, "virtual")]
    #[case(false, "git")]
    #[case(true, "file")]
    #[case(true, "virtual")]
    #[case(true, "git")]
    fn blocked_children_are_reported_without_inspecting_through_the_scaffold(
        #[case] link: bool,
        #[case] kind: &str,
    ) {
        let (td, repo) = repo_and_root();
        td.child("ov/over.toml")
            .write_str("target='~/blocked'")
            .unwrap();
        td.child("ov/nested/file").write_str("managed").unwrap();
        td.child("home").create_dir_all().unwrap();
        if link {
            td.child("foreign/nested").create_dir_all().unwrap();
            // A foreign repository must never become a diagnostic prerequisite.
            git2::Repository::init(td.path().join("foreign/nested/file")).unwrap();
            symlink::symlink_dir(td.path().join("foreign"), td.path().join("home/blocked"))
                .unwrap();
        } else {
            td.child("home/blocked").write_str("obstruction").unwrap();
        }
        let overlay = repo.get("ov").unwrap();
        let ctx = Context::builder()
            .root(td.path().join("home"))
            .repository(repo)
            .build();
        let mut entries = DesiredTree::build(&ctx, &overlay)
            .unwrap()
            .entries()
            .to_vec();
        let child = entries
            .iter_mut()
            .find(|entry| entry.target.ends_with("nested/file"))
            .unwrap();
        if kind == "virtual" {
            child.intent = MaterializationIntent::VirtualCheckout;
        } else if kind == "git" {
            child.intent = MaterializationIntent::Checkout;
            child.provenance = crate::desired::Provenance::Git {
                overlay: "ov".into(),
                repo_key: "nested/file".into(),
                config: Box::new(serde_json::from_str("\"unused\"").unwrap()),
            };
        }
        let mut unrelated = entries[0].clone();
        unrelated.target = td.path().join("home/unrelated");
        unrelated.intent = MaterializationIntent::Directory;
        entries.push(unrelated);
        let desired = DesiredTree::from_entries(entries);
        let report =
            Report::build_with_inspector(&desired, |_| panic!("blocked inspector called")).unwrap();
        assert!(
            report
                .entries()
                .iter()
                .filter(|entry| entry.entry.target.ends_with("nested/file"))
                .all(|entry| entry.status == Status::Conflict)
        );
        assert!(
            report
                .entries()
                .iter()
                .any(|entry| entry.entry.target.ends_with("unrelated")
                    && entry.status == Status::Missing)
        );
        assert!(
            report
                .git_configuration
                .iter()
                .all(|issue| !issue.repository.starts_with(td.path().join("foreign")))
        );
        let error = crate::diff::Report::build(&desired).unwrap_err();
        assert!(error.to_string().contains("scaffold"));
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
        assert!(!report.needs_attention());
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
        assert!(report.needs_attention());
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
        assert!(!report.needs_attention());
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
                    permissions: None,
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
                    permissions: None,
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
            git_configuration: Vec::new(),
            inspection_failures: Vec::new(),
        };
        let counts = report.counts();
        assert_eq!(
            counts,
            Counts {
                locked: 0,
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
        assert!(report.needs_attention());
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

    #[tokio::test]
    async fn virtual_checkout_display_is_pure_and_details_are_available_during_inspection() {
        use crate::materialize::virtual_checkout::{git as vc_git, state as vc_state};

        let _xdg = isolate_xdg_state();
        let td = TempDir::new().unwrap();
        td.child("a.txt").write_str("original\n").unwrap();
        let source_repo = git2::Repository::init(td.path()).unwrap();
        let mut cfg = source_repo.config().unwrap();
        cfg.set_str("user.name", "Test").unwrap();
        cfg.set_str("user.email", "test@test.com").unwrap();
        drop(cfg);
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();
        let mut index = source_repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        {
            let tree = source_repo.find_tree(tree_id).unwrap();
            source_repo
                .commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
                .unwrap();
        }

        let target = td.child("target");
        fs::create_dir_all(target.path()).unwrap();
        let head_tree = source_repo.head().unwrap().peel_to_tree().unwrap();
        vc_git::checkout_subtree(&source_repo, &head_tree, target.path()).unwrap();
        let base_oid = source_repo
            .head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .id()
            .to_string();
        vc_state::persist(
            target.path(),
            crate::materialize::virtual_checkout::state::VirtualCheckoutRecord {
                overlay: "ov".to_string(),
                managed_path: std::path::PathBuf::new(),
                base_oid,
                created_at: 0,
                last_commit_at: None,
                ..Default::default()
            },
        )
        .await
        .unwrap();

        fs::write(target.path().join("a.txt"), "edited\n").unwrap();

        let entry = DesiredEntry {
            target: target.path().to_path_buf(),
            provenance: crate::desired::Provenance::Overlay {
                overlay: "ov".to_string(),
                source: td.path().to_path_buf(),
            },
            intent: MaterializationIntent::VirtualCheckout,
            permissions: None,
        };
        let entry_status = EntryStatus {
            entry: entry.clone(),
            status: virtual_checkout::inspect(&entry).unwrap(),
        };
        assert_eq!(entry_status.status, Status::Modified);
        let display = format!("{entry_status}");
        assert!(display.contains("modified:"));
        let changes = virtual_checkout::unstaged_changes(&entry).unwrap();
        assert_eq!(changes[0].path, std::path::Path::new("a.txt"));
        // Rendering remains valid after inspection resources disappear.
        drop(head_tree);
        drop(source_repo);
        fs::remove_dir_all(td.path()).unwrap();
        assert_eq!(format!("{entry_status}"), display);
    }

    #[test]
    fn describe_covers_every_status_variant() {
        assert_eq!(describe(&Status::Applied), "is applied");
        assert_eq!(describe(&Status::Missing), "is missing");
        assert_eq!(describe(&Status::Modified), "has uncommitted changes");
        assert_eq!(describe(&Status::Broken), "isn't a valid git repository");
        assert_eq!(
            describe(&Status::Conflict),
            "has a merge/rebase/cherry-pick in progress"
        );
        assert_eq!(
            describe(&Status::Ahead(1)),
            "is 1 commit ahead of its upstream"
        );
        assert_eq!(
            describe(&Status::Ahead(2)),
            "is 2 commits ahead of its upstream"
        );
        assert_eq!(
            describe(&Status::Behind(1)),
            "is 1 commit behind its upstream"
        );
        assert_eq!(
            describe(&Status::Diverged {
                ahead: 2,
                behind: 3
            }),
            "has diverged from its upstream (2 ahead, 3 behind)"
        );
    }

    #[test]
    fn counts_display_lists_every_status() {
        let counts = Counts {
            locked: 9,
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
        assert!(s.contains("9 locked"));
    }

    #[rstest::rstest]
    #[case(Status::Modified, "modified", 33)]
    #[case(Status::Conflict, "conflict(s)", 31)]
    #[case(Status::Broken, "broken", 31)]
    #[case(Status::Applied, "applied", 32)]
    fn nonzero_counts_use_the_entry_severity(
        #[case] status: Status,
        #[case] label: &str,
        #[case] color: u8,
    ) {
        assert_eq!(
            count_label(1, label, status)
                .force_styling(true)
                .to_string(),
            format!("\x1b[{color}m1 {label}\x1b[0m"),
        );
    }

    #[rstest::rstest]
    #[case(Counts::default(), "no entries")]
    #[case(Counts { applied: 2, ..Counts::default() }, "2 applied")]
    #[case(Counts { broken: 1, conflict: 2, locked: 3, ..Counts::default() }, "1 broken, 2 conflict(s), 3 locked")]
    fn reconciliation_counts_omit_zero_categories_without_dangling_separators(
        #[case] counts: Counts,
        #[case] expected: &str,
    ) {
        let output = counts.to_string();
        assert_eq!(console::strip_ansi_codes(&output), expected);
    }

    // ── exclude_problems: #148 ──────────────────────────────────────────

    #[test]
    fn report_build_surfaces_a_missing_exclude_block_for_a_managed_target() {
        use crate::desired::Provenance;

        let td = TempDir::new().unwrap();
        git2::Repository::init(td.path()).unwrap();
        let target = td.path().join("dotfile");
        fs::write(&target, "content").unwrap();

        let desired = DesiredTree::from_entries(vec![DesiredEntry {
            target,
            provenance: Provenance::Overlay {
                overlay: "demo".to_string(),
                source: std::path::PathBuf::new(),
            },
            intent: MaterializationIntent::SymlinkFile {
                source: std::path::PathBuf::from("/dev/null"),
                link_type: LinkType::Soft,
            },
            permissions: None,
        }]);

        let report = Report::build(&desired).unwrap();

        assert_eq!(report.git_configuration().len(), 1);
        assert!(
            report.git_configuration()[0]
                .message
                .contains("exclude missing: dotfile")
        );
        assert!(report.needs_attention());
    }

    #[test]
    fn report_build_reports_ok_when_the_exclude_block_already_matches() {
        use crate::desired::Provenance;
        use crate::git_exclude::{self, ManagedTarget};

        let td = TempDir::new().unwrap();
        git2::Repository::init(td.path()).unwrap();
        let target = td.path().join("dotfile");
        fs::write(&target, "content").unwrap();

        let desired = DesiredTree::from_entries(vec![DesiredEntry {
            target: target.clone(),
            provenance: Provenance::Overlay {
                overlay: "demo".to_string(),
                source: std::path::PathBuf::new(),
            },
            intent: MaterializationIntent::SymlinkFile {
                source: std::path::PathBuf::from("/dev/null"),
                link_type: LinkType::Soft,
            },
            permissions: None,
        }]);

        // Pre-reconcile so the block already matches what `diagnose` would
        // expect to find.
        git_exclude::reconcile(&[ManagedTarget {
            target,
            overlay: "demo".to_string(),
        }])
        .unwrap();

        let report = Report::build(&desired).unwrap();

        assert!(report.git_configuration().is_empty());
    }

    #[test]
    fn report_build_without_any_managed_target_has_no_exclude_problems() {
        let report = Report::build(&DesiredTree::default()).unwrap();
        assert!(report.git_configuration().is_empty());
    }
}
