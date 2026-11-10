//! `over unapply` (#64): reverses `over apply` by reconciling actual
//! filesystem/git state back toward "nothing" — the same `DesiredTree`/
//! [`Plan`] vocabulary `apply`/`status`/`diff`/`sync` already share
//! (ADR-011).
//!
//! Re-walk current desired state and recheck ownership before removal.
//! Ordinary scaffolds are only removed empty, links must still match their
//! source, and partial blocks preserve unrelated bytes. Virtual checkouts use
//! their verified association/scope and retain dirty files. Real checkouts
//! need a disposal-specific assessment and explicit confirmation or
//! `--discard-checkout`: clean Git status does not prove recoverability of
//! ignored files, other refs, reflogs or repository-local configuration.
//!
//! Reports distinguish proposed operations from executed/retained outcomes.
//! No removal is a general promise of rollback or historical restoration.

use std::fmt;
use std::fs;
use std::io;
use std::path::PathBuf;

use anyhow::{Context as _, Result};
use tokio::task::spawn_blocking;

use crate::actions::partial::{self, BlockState};
use crate::desired::{DesiredEntry, DesiredTree, MaterializationIntent};
use crate::exec::Ctx;
use crate::plan::actual::{self, ActualState};
use crate::plan::{Operation, Plan};
use crate::status::{self, Status};
use crate::ui::{emojis, style};
use crate::utils::short_path;

/// What happened (or would happen) to a single [`DesiredEntry`] when
/// reconciling it back toward "nothing". Its own taxonomy, not a reuse of
/// [`status::Status`]/`crate::diff::Change` — per ADR-013's precedent, each
/// command answers a different question about the same underlying
/// `DesiredTree`/[`Plan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Target still matches exactly what this overlay would materialize
    /// there — safe to remove (or already removed, under `--dry-run`).
    Removed,
    /// Owned virtual-checkout files were removed, but protected contents keep
    /// the container directory in place. The projection is successfully gone.
    RemovedProjectionRetainedDirectory,
    /// Nothing exists at the target — already unapplied, or never applied.
    AlreadyAbsent,
    /// Target exists but doesn't match what this overlay would create
    /// there (a foreign file, a symlink pointing elsewhere, a type
    /// mismatch...). Never touched.
    NotOwned { current: ActualState },
    /// A git checkout that isn't fully in sync with its upstream
    /// (uncommitted changes, unpushed commits, ahead/behind/diverged, a
    /// merge in progress, or not a valid repo). Never touched — resolve
    /// with `over sync` or plain git first.
    CheckoutNotClean { status: Status },
    /// Real checkout removal requires its own explicit authorization.
    CheckoutDisposal { reason: String },
    /// Nonrecursive scaffold cleanup preserved unrelated contents.
    RetainedDirectory,
}

impl Outcome {
    /// Whether this outcome needs the user's attention — something was
    /// deliberately left untouched. Used by the CLI to decide what to show
    /// without `--verbose`, and to decide the process exit code.
    pub fn needs_attention(&self) -> bool {
        matches!(
            self,
            Outcome::NotOwned { .. }
                | Outcome::CheckoutNotClean { .. }
                | Outcome::CheckoutDisposal { .. }
        )
    }
}

/// Short, human-readable word for a checkout [`Status`] — `Status` itself
/// has no `Display` impl (only `status::EntryStatus` does, tied to its own
/// message shape), so this is a small local rendering instead of reaching
/// into that module's formatting.
fn status_word(status: &Status) -> String {
    match status {
        Status::Applied => "applied".to_string(),
        Status::Missing => "missing".to_string(),
        Status::Modified => "modified".to_string(),
        Status::Broken => "broken".to_string(),
        Status::Conflict => "conflict".to_string(),
        Status::Ahead(n) => format!("ahead {n}"),
        Status::Behind(n) => format!("behind {n}"),
        Status::Diverged { ahead, behind } => format!("diverged +{ahead}/-{behind}"),
    }
}

/// One [`DesiredEntry`] paired with its unapply [`Outcome`].
#[derive(Debug, Clone)]
pub struct EntryOutcome {
    pub entry: DesiredEntry,
    pub outcome: Outcome,
}

impl fmt::Display for EntryOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let target = short_path(&self.entry.target.to_string_lossy());
        match &self.outcome {
            Outcome::Removed => write!(
                f,
                "{} {} {}{}",
                emojis::TRASH,
                style::white("remove:"),
                target,
                if matches!(self.entry.intent, MaterializationIntent::Checkout) {
                    " (recursive removal, including local files and Git history)"
                } else {
                    ""
                }
            ),
            Outcome::AlreadyAbsent => write!(
                f,
                "{} {} {}",
                emojis::CHECKMARK,
                style::white("already absent:"),
                target
            ),
            Outcome::NotOwned { current } => write!(
                f,
                "{} {} {} ({current} — not created by this overlay, left untouched)",
                emojis::WARNING,
                style::yellow("skip:"),
                target,
            ),
            Outcome::CheckoutNotClean { status } => write!(
                f,
                "{} {} {} ({} — run `over sync` or resolve manually first)",
                emojis::WARNING,
                style::yellow("skip:"),
                target,
                status_word(status),
            ),
            Outcome::CheckoutDisposal { reason } => {
                write!(f, "skip: {target} (checkout disposal: {reason})")
            }
            Outcome::RetainedDirectory => write!(f, "retain nonempty directory: {target}"),
            Outcome::RemovedProjectionRetainedDirectory => {
                write!(f, "removed projection; retained directory: {target}")
            }
        }
    }
}

/// Per-`Outcome` counts of a [`Report`], in the same order [`Outcome`] is
/// declared.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    /// Removed entries/projections, including those whose container remains.
    pub removed: usize,
    pub already_absent: usize,
    pub not_owned: usize,
    pub checkout_needs_attention: usize,
}

impl fmt::Display for Counts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} to remove, {} already absent, {} not owned, {} checkout(s) need attention",
            self.removed, self.already_absent, self.not_owned, self.checkout_needs_attention,
        )
    }
}

/// The unapply reconciliation of a whole [`DesiredTree`]: one
/// [`EntryOutcome`] per [`DesiredEntry`], in the same order `DesiredTree`
/// produces them (ascending by target — parents before children).
#[derive(Debug, Clone, Default)]
pub struct Report {
    entries: Vec<EntryOutcome>,
    guards: Vec<crate::materialize::disposal::PathGuard>,
}

impl Report {
    /// Classify every entry in `desired` against current state. Read-only —
    /// reuses [`Plan::build`]'s own classification for
    /// `Directory`/`SymlinkFile`/`SymlinkDirectory` intents (`Noop` means
    /// "still exactly what this overlay would create here", safe to
    /// remove; `Conflict` means "something else is there", never touched).
    ///
    /// Real checkout reporting status is deliberately not deletion authority:
    /// the disposal assessment verifies identity and discloses local loss.
    pub fn build(desired: &DesiredTree) -> Result<Self> {
        let plan = Plan::build(desired)?;
        let mut entries = Vec::with_capacity(plan.len());
        for step in plan.steps() {
            let outcome = match (&step.entry.intent, &step.operation) {
                (MaterializationIntent::Checkout, _) => {
                    if !step.entry.target.exists() && !step.entry.target.is_symlink() {
                        Outcome::AlreadyAbsent
                    } else {
                        Outcome::CheckoutDisposal {
                            reason: crate::materialize::disposal::assess(&step.entry)?
                                .description(),
                        }
                    }
                }
                // Same strict "only ever remove when fully clean" gate as
                // `Checkout` above — a virtual checkout's own `inspect`
                // already only ever reports `Applied` when there's nothing
                // uncommitted locally and nothing the source repository
                // has moved ahead by (#141); no separate "content vs
                // declared" split exists for it the way #140 introduced
                // for `Checkout` (a virtual checkout is never a
                // provisioning-only resource).
                (
                    MaterializationIntent::VirtualCheckout
                    | MaterializationIntent::ScopedVirtualCheckout(_),
                    _,
                ) => match status::virtual_checkout::inspect(&step.entry)? {
                    Status::Missing => Outcome::AlreadyAbsent,
                    Status::Applied => Outcome::Removed,
                    other => Outcome::CheckoutNotClean { status: other },
                },
                (_, Operation::Create) => Outcome::AlreadyAbsent,
                // A permission-only drift (#65) doesn't change ownership:
                // the content is still exactly what this overlay put
                // there, only its mode needs a `chmod` — just as safe to
                // remove as a plain `Noop`.
                (_, Operation::Noop | Operation::Repair { .. }) => Outcome::Removed,
                (_, Operation::Conflict { current }) => Outcome::NotOwned {
                    current: current.clone(),
                },
                // A pending rule-change migration (#129, blocked or not):
                // never remove something mid-migration — leave it alone
                // and report it exactly like any other not-plain-owned
                // entry, same as a `Conflict`.
                (_, Operation::Migrate { .. }) => Outcome::NotOwned {
                    current: actual::inspect(&step.entry.target)?,
                },
                // Every intent has a registered `Materializer` since #110;
                // no step should ever classify as `Deferred` — unreachable
                // in practice, but treated as "leave it alone" rather than
                // panicking on a stale/future classification.
                (_, Operation::Deferred) => Outcome::NotOwned {
                    current: ActualState::Missing,
                },
            };
            entries.push(EntryOutcome {
                entry: step.entry.clone(),
                outcome,
            });
        }
        let guards = entries
            .iter()
            .map(|entry| crate::materialize::disposal::PathGuard::capture(&entry.entry.target))
            .collect::<Result<_>>()?;
        Ok(Self { entries, guards })
    }

    pub fn entries(&self) -> &[EntryOutcome] {
        &self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn counts(&self) -> Counts {
        let mut counts = Counts::default();
        for e in &self.entries {
            match e.outcome {
                Outcome::Removed | Outcome::RemovedProjectionRetainedDirectory => {
                    counts.removed += 1
                }
                Outcome::AlreadyAbsent => counts.already_absent += 1,
                Outcome::NotOwned { .. } => counts.not_owned += 1,
                Outcome::CheckoutNotClean { .. } => counts.checkout_needs_attention += 1,
                Outcome::CheckoutDisposal { .. } => counts.checkout_needs_attention += 1,
                Outcome::RetainedDirectory => {}
            }
        }
        counts
    }

    /// Whether anything is actually queued for removal.
    pub fn has_pending_removals(&self) -> bool {
        self.entries
            .iter()
            .any(|e| matches!(e.outcome, Outcome::Removed))
    }

    /// Whether anything in this report needs attention (something was
    /// deliberately left untouched).
    pub fn needs_attention(&self) -> bool {
        self.entries.iter().any(|e| e.outcome.needs_attention())
    }

    /// Remove every entry classified [`Outcome::Removed`]. No-ops entirely
    /// under `ctx.dry_run` — never touches the filesystem/git, exactly like
    /// every `Action::execute` already does.
    ///
    /// Iterates deepest-target-first (`DesiredTree` sorts ascending by
    /// target, so a directory's entry always precedes anything nested
    /// under it — reversing that order removes children/files before the
    /// directories that contain them, giving a formerly overlay-only
    /// directory a chance to become empty before its own removal is
    /// attempted).
    pub async fn execute(&self, ctx: Ctx) -> Result<Self> {
        if ctx.dry_run {
            let mut preview = self.clone();
            if ctx.discard_checkout {
                for entry in &mut preview.entries {
                    if matches!(entry.outcome, Outcome::CheckoutDisposal { .. })
                        && matches!(
                            crate::materialize::disposal::assess(&entry.entry)?,
                            crate::materialize::disposal::Assessment::RequiresConsent { .. }
                        )
                    {
                        entry.outcome = Outcome::Removed;
                    }
                }
            }
            return Ok(preview);
        }
        let mut executed = self.clone();
        for (entry_outcome, guard) in executed.entries.iter_mut().zip(&executed.guards).rev() {
            guard.verify()?;
            if matches!(entry_outcome.entry.intent, MaterializationIntent::Checkout)
                && matches!(entry_outcome.outcome, Outcome::CheckoutDisposal { .. })
            {
                entry_outcome.outcome = match crate::materialize::disposal::remove_checkout(
                    ctx.clone(),
                    entry_outcome.entry.clone(),
                )
                .await?
                {
                    crate::materialize::disposal::Removal::Removed => Outcome::Removed,
                    crate::materialize::disposal::Removal::Retained(reason) => {
                        Outcome::CheckoutDisposal { reason }
                    }
                };
                continue;
            }
            if matches!(entry_outcome.outcome, Outcome::Removed) {
                dematerialize(&entry_outcome.entry).await?;
                let target = &entry_outcome.entry.target;
                if matches!(entry_outcome.entry.intent, MaterializationIntent::Directory)
                    && target.exists()
                {
                    entry_outcome.outcome = Outcome::RetainedDirectory;
                } else if matches!(
                    entry_outcome.entry.intent,
                    MaterializationIntent::SymlinkFile { .. }
                        | MaterializationIntent::SymlinkDirectory { .. }
                ) && (target.exists() || target.is_symlink())
                {
                    entry_outcome.outcome = Outcome::NotOwned {
                        current: actual::inspect(target)?,
                    };
                }
                if entry_outcome.entry.intent.is_virtual_checkout() {
                    entry_outcome.outcome =
                        if crate::materialize::virtual_checkout::state::record_for_blocking(target)?
                            .is_some()
                        {
                            Outcome::CheckoutNotClean {
                                status: status::virtual_checkout::inspect(&entry_outcome.entry)?,
                            }
                        } else if target.is_dir() {
                            // Dropping the verified association means teardown succeeded,
                            // even when an independent mapped destination keeps the root.
                            Outcome::RemovedProjectionRetainedDirectory
                        } else {
                            Outcome::Removed
                        };
                }
                if let MaterializationIntent::PartialFile { marker, .. } =
                    &entry_outcome.entry.intent
                    && target.exists()
                    && !matches!(
                        partial::find_block(&fs::read_to_string(target)?, marker),
                        BlockState::Absent
                    )
                {
                    entry_outcome.outcome = Outcome::NotOwned {
                        current: actual::inspect(target)?,
                    };
                }
            }
        }
        Ok(executed)
    }
}

/// Tear down a single entry classified [`Outcome::Removed`]. Dispatches on
/// intent, mirroring `materialize::symlink::build_action`'s own dispatch —
/// the removal-side counterpart, kept in this module rather than added to
/// the [`crate::materialize::Materializer`] trait since none of
/// `status`/`diff`/`sync` route their own concerns through it either (they
/// call `status::git`/`actions::git` directly for what they need).
async fn dematerialize(entry: &DesiredEntry) -> Result<()> {
    match &entry.intent {
        MaterializationIntent::Directory => remove_dir_if_empty(entry.target.clone()).await,
        MaterializationIntent::SymlinkFile {
            source,
            link_type: crate::actions::symlink::LinkType::Hard,
        } => {
            // Hard-link creation may fall back to a soft link (e.g. across
            // filesystems); accept only that exact source, never a foreign link.
            if entry.target.is_symlink() {
                return remove_symlink_if_unchanged(entry.target.clone(), source.clone()).await;
            }
            let source = source.clone();
            let target = entry.target.clone();
            spawn_blocking(move || -> Result<()> {
                if target.is_file() && crate::actions::symlink::hard_link_matches(&source, &target)?
                {
                    fs::remove_file(&target)?;
                }
                Ok(())
            })
            .await?
        }
        MaterializationIntent::SymlinkFile { source, .. }
        | MaterializationIntent::SymlinkDirectory { source, .. } => {
            remove_symlink_if_unchanged(entry.target.clone(), source.clone()).await
        }
        MaterializationIntent::Checkout => {
            anyhow::bail!("real checkout removal requires explicit disposal assessment")
        }
        MaterializationIntent::VirtualCheckout
        | MaterializationIntent::ScopedVirtualCheckout(_) => {
            remove_virtual_checkout_if_clean(entry.clone()).await
        }
        MaterializationIntent::PartialFile { content, marker } => {
            remove_partial_block_if_unchanged(entry.target.clone(), marker.clone(), content.clone())
                .await
        }
    }
}

/// Remove `path` only if it's already empty — never recursive. `NotFound`
/// (already gone) and `DirectoryNotEmpty` (still holds something else) are
/// both treated as success: there's nothing more to do, and it is never an
/// error for unapply to leave a non-empty directory in place.
async fn remove_dir_if_empty(path: PathBuf) -> Result<()> {
    spawn_blocking(move || match fs::remove_dir(&path) {
        Ok(()) => Ok(()),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::DirectoryNotEmpty
            ) =>
        {
            Ok(())
        }
        Err(e) => Err(e.into()),
    })
    .await?
}

/// Remove the symlink at `target` only if it still points exactly at
/// `source` — re-inspected here (not just trusted from classification) as
/// defense against a race between `Report::build` and `Report::execute`.
/// Anything else (already gone, or changed to point somewhere else since
/// classification) is silently left alone: never remove something that no
/// longer matches exactly what this overlay would have linked here.
async fn remove_symlink_if_unchanged(target: PathBuf, source: PathBuf) -> Result<()> {
    spawn_blocking(move || match actual::inspect(&target)? {
        ActualState::Symlink { points_to } if points_to == source => {
            if target.is_dir() {
                symlink::remove_symlink_dir(&target)?;
            } else {
                symlink::remove_symlink_file(&target)?;
            }
            Ok(())
        }
        _ => Ok(()),
    })
    .await?
}

/// Strip the managed block for `marker` from `target`, but only if it
/// *still* matches `expected` exactly right now — re-read here (not just
/// trusted from classification) for the same classify→execute
/// race-safety reason as [`remove_symlink_if_unchanged`]. A block that's
/// drifted (hand-edited) or gone malformed since classification is left
/// untouched, never forced.
///
/// If stripping the block leaves nothing behind, the
/// file itself is removed — mirroring [`remove_dir_if_empty`]'s "a
/// directory disappears exactly when it turns out to hold nothing but
/// what this overlay put there". Otherwise the file is rewritten with
/// just the block removed, every other byte preserved.
async fn remove_partial_block_if_unchanged(
    target: PathBuf,
    marker: String,
    expected: String,
) -> Result<()> {
    spawn_blocking(move || {
        let metadata = match fs::symlink_metadata(&target) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        anyhow::ensure!(
            metadata.file_type().is_file(),
            "partial target '{}' changed type",
            target.display()
        );
        let current = match fs::read_to_string(&target) {
            Ok(s) => s,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        match partial::find_block(&current, &marker) {
            BlockState::Found(existing) if existing == expected => {
                let stripped = partial::remove_block(&current, &marker);
                if stripped.is_empty() {
                    fs::remove_file(&target)
                        .with_context(|| format!("failed to remove {}", target.display()))?;
                } else {
                    crate::actions::fs::replace_file(&target, stripped, |permissions| permissions)
                        .with_context(|| format!("failed to write {}", target.display()))?;
                }
                Ok(())
            }
            _ => Ok(()), // Drifted, malformed, or already gone — leave alone.
        }
    })
    .await?
}

/// Remove only a virtual checkout's owned files and empty directories, only if
/// [`status::virtual_checkout::inspect`] still reports [`Status::Applied`]
/// right now. Also drops the XDG association record
/// (#141): once the target is gone, the record would otherwise dangle,
/// and a future virtual checkout re-materialized at the same path must not
/// be misread as already having history against a `base_oid` it never
/// actually started from.
async fn remove_virtual_checkout_if_clean(entry: DesiredEntry) -> Result<()> {
    let association_key = crate::materialize::virtual_checkout::state::key_for(&entry.target);
    let inspect_entry = entry.clone();
    let is_clean = spawn_blocking(move || {
        anyhow::Ok(matches!(
            status::virtual_checkout::inspect(&inspect_entry)?,
            Status::Applied
        ))
    })
    .await??;
    if !is_clean {
        return Ok(());
    }
    let target = entry.target.clone();
    let removal_entry = entry.clone();
    spawn_blocking(move || -> Result<()> {
        use crate::materialize::virtual_checkout::{git, state};
        let Some(record) = state::record_for_blocking(&target)? else {
            return Ok(());
        };
        state::verify(&removal_entry, &record)?;
        let crate::desired::Provenance::Overlay { source, .. } = &removal_entry.provenance else {
            unreachable!()
        };
        let (repo, managed) = git::discover_source(source)?;
        let base = git::base_tree(&repo, Some(&record.base_oid))?;
        let scope = removal_entry.intent.checkout_scope();
        let tree = git::scoped_subtree(&repo, &base, &managed, &scope)?;
        let tracked = git::tracked_blobs(&tree)?;
        for (path, expected) in git::tracked_entries(&tree)? {
            anyhow::ensure!(
                git::entries_match(git::target_entry(&target, &path)?, Some(expected)),
                "checkout mode/content changed at '{}'",
                target.join(path).display()
            );
        }
        anyhow::ensure!(
            git::diff_scoped_target(&target, &tracked, &scope)?.is_empty(),
            "checkout changed while removing '{}'",
            target.display()
        );
        for path in tracked.keys() {
            let file = target.join(path);
            fs::remove_file(&file)?;
            let mut parent = file.parent();
            while let Some(dir) = parent {
                if dir == target || !dir.starts_with(&target) {
                    break;
                }
                match fs::remove_dir(dir) {
                    Ok(()) => parent = dir.parent(),
                    Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => break,
                    Err(error) => return Err(error.into()),
                }
            }
        }
        match fs::remove_dir(&target) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => Ok(()),
            Err(error) => Err(error.into()),
        }
    })
    .await??;
    crate::materialize::virtual_checkout::state::remove_key(association_key).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::symlink::LinkType;
    use crate::desired::Provenance;
    use crate::exec::Context;
    use crate::overlays::Repository;
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    use git2::Signature;
    use rstest::rstest;
    use std::fs;
    use std::path::PathBuf;

    fn repo_and_root() -> (TempDir, Repository) {
        let td = TempDir::new().unwrap();
        let repo = Repository::new(td.path().to_path_buf());
        (td, repo)
    }

    fn ctx(root: PathBuf, repo: Repository, overlay: Option<crate::overlays::Overlay>) -> Ctx {
        let mut builder = Context::builder().root(root).repository(repo);
        if let Some(o) = overlay {
            builder = builder.overlay(o);
        }
        builder.build()
    }

    // ── classification ──────────────────────────────────────────────────

    #[rstest]
    fn missing_target_is_already_absent() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~/sub\"")
            .unwrap();
        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));

        let desired = DesiredTree::build(&c, &overlay).unwrap();
        let report = Report::build(&desired).unwrap();

        assert_eq!(report.entries().len(), 1);
        assert_eq!(report.entries()[0].outcome, Outcome::AlreadyAbsent);
        assert_eq!(report.counts().already_absent, 1);
        assert!(!report.has_pending_removals());
    }

    #[tokio::test]
    async fn applied_symlink_classifies_as_removed() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        overlay_dir.child("file.txt").write_str("content").unwrap();
        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));

        let desired = DesiredTree::build(&c, &overlay).unwrap();
        Plan::build(&desired)
            .unwrap()
            .execute(c.clone())
            .await
            .unwrap();

        let desired2 = DesiredTree::build(&c, &overlay).unwrap();
        let report = Report::build(&desired2).unwrap();
        let file_outcome = report
            .entries()
            .iter()
            .find(|e| e.entry.target == td.path().join("file.txt"))
            .unwrap();
        assert_eq!(file_outcome.outcome, Outcome::Removed);
        assert!(report.has_pending_removals());
    }

    #[test]
    fn conflicting_file_is_not_owned() {
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
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));
        let desired = DesiredTree::build(&c, &overlay).unwrap();
        let report = Report::build(&desired).unwrap();
        let file_outcome = report
            .entries()
            .iter()
            .find(|e| e.entry.target == td.path().join("file.txt"))
            .unwrap();
        assert!(matches!(file_outcome.outcome, Outcome::NotOwned { .. }));
        assert_eq!(report.counts().not_owned, 1);
    }

    #[test]
    fn dangling_soft_symlink_still_classifies_as_removed() {
        // A soft symlink whose source has since disappeared is still
        // exactly what the overlay linked there — `status::Report`
        // reports this as `Broken` for attention, but unapply should just
        // clean it up like any other still-matching link.
        //
        // `DesiredTree` itself never produces this entry once the source
        // is gone (it won't turn up in `walk_overlay_tree` anymore, see
        // `status::tests::dangling_soft_symlink_noop_is_broken`), so build
        // the tree directly instead of round-tripping through `apply`.
        let td = TempDir::new().unwrap();
        let source = td.path().join("gone.txt");
        let target = td.path().join("linked.txt");
        symlink::symlink_file(&source, &target).unwrap();

        let entry = DesiredEntry {
            target,
            provenance: Provenance::Overlay {
                overlay: "ov".to_string(),
                source: source.clone(),
            },
            intent: MaterializationIntent::SymlinkFile {
                source,
                link_type: LinkType::Soft,
            },
            permissions: None,
        };
        let desired = DesiredTree::from_entries(vec![entry]);
        let report = Report::build(&desired).unwrap();
        assert_eq!(report.entries()[0].outcome, Outcome::Removed);
    }

    #[test]
    fn empty_desired_tree_produces_empty_report() {
        let report = Report::build(&DesiredTree::default()).unwrap();
        assert!(report.is_empty());
        assert!(!report.has_pending_removals());
        assert!(!report.needs_attention());
    }

    fn partial_entry(target: PathBuf, content: &str, marker: &str) -> DesiredEntry {
        DesiredEntry {
            target,
            provenance: Provenance::PartialSidecar {
                overlay: "ov".to_string(),
                config: PathBuf::from("/repo/ov/aliases.partial.toml"),
            },
            intent: MaterializationIntent::PartialFile {
                content: content.to_string(),
                marker: marker.to_string(),
            },
            permissions: None,
        }
    }

    #[test]
    fn matching_partial_block_classifies_as_removed() {
        let td = TempDir::new().unwrap();
        let target = td.path().join("file.txt");
        fs::write(
            &target,
            "before\n# >>> over: m >>>\nalias x=y\n# <<< over: m <<<\nafter\n",
        )
        .unwrap();
        let desired = DesiredTree::from_entries(vec![partial_entry(target, "alias x=y", "m")]);
        let report = Report::build(&desired).unwrap();
        assert_eq!(report.entries()[0].outcome, Outcome::Removed);
    }

    #[test]
    fn drifted_partial_block_is_not_owned() {
        let td = TempDir::new().unwrap();
        let target = td.path().join("file.txt");
        fs::write(
            &target,
            "# >>> over: m >>>\nhand-edited\n# <<< over: m <<<\n",
        )
        .unwrap();
        let desired = DesiredTree::from_entries(vec![partial_entry(target, "alias x=y", "m")]);
        let report = Report::build(&desired).unwrap();
        assert!(matches!(
            report.entries()[0].outcome,
            Outcome::NotOwned { .. }
        ));
    }

    #[tokio::test]
    async fn execute_strips_block_and_keeps_surrounding_content() {
        let td = TempDir::new().unwrap();
        let target = td.path().join("file.txt");
        fs::write(
            &target,
            "before\n# >>> over: m >>>\nalias x=y\n# <<< over: m <<<\nafter\n",
        )
        .unwrap();
        let desired =
            DesiredTree::from_entries(vec![partial_entry(target.clone(), "alias x=y", "m")]);
        let report = Report::build(&desired).unwrap();
        report.execute(Context::builder().build()).await.unwrap();

        assert_eq!(fs::read_to_string(&target).unwrap(), "before\nafter\n");
    }

    #[tokio::test]
    async fn execute_removes_file_when_block_was_all_it_contained() {
        let td = TempDir::new().unwrap();
        let target = td.path().join("file.txt");
        fs::write(&target, "# >>> over: m >>>\nalias x=y\n# <<< over: m <<<\n").unwrap();
        let desired =
            DesiredTree::from_entries(vec![partial_entry(target.clone(), "alias x=y", "m")]);
        let report = Report::build(&desired).unwrap();
        report.execute(Context::builder().build()).await.unwrap();

        assert!(!target.exists());
    }

    #[tokio::test]
    async fn execute_leaves_drifted_block_untouched() {
        let td = TempDir::new().unwrap();
        let target = td.path().join("file.txt");
        let original = "# >>> over: m >>>\nhand-edited\n# <<< over: m <<<\n";
        fs::write(&target, original).unwrap();
        let desired =
            DesiredTree::from_entries(vec![partial_entry(target.clone(), "alias x=y", "m")]);
        let report = Report::build(&desired).unwrap();
        report.execute(Context::builder().build()).await.unwrap();

        assert_eq!(fs::read_to_string(&target).unwrap(), original);
    }

    #[tokio::test]
    async fn remove_partial_block_if_unchanged_missing_target_is_a_no_op() {
        // Simulates a classify→execute race: the target vanished between
        // `Report::build` and `Report::execute` (e.g. removed by something
        // else entirely). Never an error — nothing left to strip.
        let td = TempDir::new().unwrap();
        let target = td.path().join("does-not-exist.txt");
        remove_partial_block_if_unchanged(target.clone(), "m".to_string(), "alias x=y".to_string())
            .await
            .unwrap();
        assert!(!target.exists());
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn remove_partial_block_if_unchanged_propagates_read_errors() {
        // A non-`NotFound` I/O error (permission denied) must propagate,
        // not be silently swallowed like a genuine "already gone" race —
        // mirrors `desired::tree::tests::unreadable_subdirectory_is_skipped_with_a_warning`'s
        // own permission-based error injection.
        use std::os::unix::fs::PermissionsExt;

        let td = TempDir::new().unwrap();
        let target = td.path().join("file.txt");
        fs::write(&target, "content").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o000)).unwrap();

        let result = remove_partial_block_if_unchanged(
            target.clone(),
            "m".to_string(),
            "alias x=y".to_string(),
        )
        .await;

        // Restore permissions so the `TempDir` can clean up regardless of
        // the assertion outcome.
        fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();

        assert!(result.is_err());
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn remove_partial_block_if_unchanged_reports_context_when_removal_fails() {
        use std::os::unix::fs::PermissionsExt;

        // Stripping the block leaves nothing behind, so the file itself
        // must be removed — but its read-only parent directory rejects
        // the removal, exercising the `.with_context` wrapping around
        // `fs::remove_file`.
        let td = TempDir::new().unwrap();
        let dir = td.path().join("readonly");
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("file.txt");
        fs::write(&target, "# >>> over: m >>>\nalias x=y\n# <<< over: m <<<\n").unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();

        let result = remove_partial_block_if_unchanged(
            target.clone(),
            "m".to_string(),
            "alias x=y".to_string(),
        )
        .await;

        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();

        let err = result.unwrap_err();
        assert!(err.to_string().contains("failed to remove"));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn remove_partial_block_if_unchanged_reports_context_when_write_fails() {
        use std::os::unix::fs::PermissionsExt;

        // Stripping the block leaves surrounding content behind, so the
        // file must be rewritten — but it's read-only, exercising the
        // `.with_context` wrapping around `fs::write`.
        let td = TempDir::new().unwrap();
        let target = td.path().join("file.txt");
        fs::write(
            &target,
            "before\n# >>> over: m >>>\nalias x=y\n# <<< over: m <<<\nafter\n",
        )
        .unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o444)).unwrap();

        let result = remove_partial_block_if_unchanged(
            target.clone(),
            "m".to_string(),
            "alias x=y".to_string(),
        )
        .await;

        fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();

        let err = result.unwrap_err();
        assert!(err.to_string().contains("failed to write"));
    }

    #[tokio::test]
    async fn remove_partial_block_if_unchanged_leaves_drifted_block_untouched() {
        // Direct call (bypassing `Report::build`/`execute`, which would
        // never even reach this function for a drifted block — it
        // classifies as `Conflict`/`NotOwned` and is skipped upstream):
        // exercises the race-safety re-check on its own terms.
        let td = TempDir::new().unwrap();
        let target = td.path().join("file.txt");
        let original = "# >>> over: m >>>\nhand-edited\n# <<< over: m <<<\n";
        fs::write(&target, original).unwrap();
        remove_partial_block_if_unchanged(target.clone(), "m".to_string(), "alias x=y".to_string())
            .await
            .unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), original);
    }

    #[tokio::test]
    async fn dry_run_execute_does_not_touch_partial_block() {
        let td = TempDir::new().unwrap();
        let target = td.path().join("file.txt");
        let original = "# >>> over: m >>>\nalias x=y\n# <<< over: m <<<\n";
        fs::write(&target, original).unwrap();
        let desired =
            DesiredTree::from_entries(vec![partial_entry(target.clone(), "alias x=y", "m")]);
        let report = Report::build(&desired).unwrap();
        report
            .execute(Context::builder().dry_run(true).build())
            .await
            .unwrap();

        assert_eq!(fs::read_to_string(&target).unwrap(), original);
    }

    #[rstest]
    fn git_checkout_missing_is_already_absent() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"\n[git]\n\".config/nvim\" = \"https://example.com/nvim.git\"")
            .unwrap();
        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));

        let desired = DesiredTree::build(&c, &overlay).unwrap();
        let report = Report::build(&desired).unwrap();
        let checkout_outcome = report
            .entries()
            .iter()
            .find(|e| matches!(e.entry.intent, MaterializationIntent::Checkout))
            .unwrap();
        assert_eq!(checkout_outcome.outcome, Outcome::AlreadyAbsent);
    }

    fn init_committed_repo(path: &std::path::Path) {
        let repo = git2::Repository::init(path).unwrap();
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
    }

    fn checkout_entry(target: PathBuf) -> DesiredEntry {
        let url = git2::Repository::open(&target)
            .ok()
            .and_then(|repo| {
                repo.find_remote("origin")
                    .ok()
                    .and_then(|remote| remote.url().ok().map(String::from))
            })
            .unwrap_or_default();
        DesiredEntry {
            target,
            provenance: Provenance::Git {
                overlay: "ov".to_string(),
                repo_key: ".".to_string(),
                config: Box::new(crate::actions::git::config::GitRepoConfig {
                    url,
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
    fn a_clean_checkout_still_discloses_repository_local_loss() {
        let td = TempDir::new().unwrap();
        init_committed_repo(td.path());
        let desired = DesiredTree::from_entries(vec![checkout_entry(td.path().to_path_buf())]);
        let report = Report::build(&desired).unwrap();
        assert!(matches!(
            report.entries()[0].outcome,
            Outcome::CheckoutDisposal { .. }
        ));
    }

    #[test]
    fn dirty_checkout_needs_attention() {
        let td = TempDir::new().unwrap();
        init_committed_repo(td.path());
        fs::write(td.path().join("README.md"), "changed").unwrap();
        let desired = DesiredTree::from_entries(vec![checkout_entry(td.path().to_path_buf())]);
        let report = Report::build(&desired).unwrap();
        assert!(matches!(
            report.entries()[0].outcome,
            Outcome::CheckoutDisposal { .. }
        ));
        assert!(report.needs_attention());
    }

    #[test]
    fn pending_migration_is_never_removed() {
        // A clean git checkout sits where a directory symlink is now
        // desired (#129, a rule/`overlay.git` change) — `Plan::build`
        // classifies this as `Operation::Migrate`. Unapply must never
        // remove something mid-migration, safe or not.
        let td = TempDir::new().unwrap();
        init_committed_repo(td.path());
        let entry = DesiredEntry {
            target: td.path().to_path_buf(),
            provenance: Provenance::Overlay {
                overlay: "ov".to_string(),
                source: PathBuf::from("/repo/ov"),
            },
            intent: MaterializationIntent::SymlinkDirectory {
                source: PathBuf::from("/repo/ov"),
                link_type: LinkType::Soft,
            },
            permissions: None,
        };
        let desired = DesiredTree::from_entries(vec![entry]);
        let report = Report::build(&desired).unwrap();
        assert!(matches!(
            report.entries()[0].outcome,
            Outcome::NotOwned { .. }
        ));
        assert!(report.needs_attention());
    }

    // ── execute ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn a_hard_link_intent_removes_its_matching_soft_link_fallback() {
        let td = TempDir::new().unwrap();
        let source = td.path().join("source");
        let target = td.path().join("target");
        fs::write(&source, "overlay").unwrap();
        // Install the fallback directly so this regression does not depend on
        // whether the test machine happens to span multiple filesystems.
        symlink::symlink_file(&source, &target).unwrap();
        let desired = DesiredTree::from_entries(vec![DesiredEntry {
            target: target.clone(),
            provenance: Provenance::Overlay {
                overlay: "ov".into(),
                source: source.clone(),
            },
            intent: MaterializationIntent::SymlinkFile {
                source: source.clone(),
                link_type: LinkType::Hard,
            },
            permissions: None,
        }]);
        let report = Report::build(&desired).unwrap();
        assert_eq!(report.entries()[0].outcome, Outcome::Removed);
        let executed = report.execute(Context::builder().build()).await.unwrap();
        assert_eq!(executed.entries()[0].outcome, Outcome::Removed);
        assert_eq!(executed.counts().removed, 1);
        assert!(!executed.needs_attention());
        assert!(!target.is_symlink());
        assert_eq!(fs::read_to_string(source).unwrap(), "overlay");
    }

    #[tokio::test]
    async fn a_hard_link_intent_preserves_a_foreign_soft_link_after_classification() {
        let td = TempDir::new().unwrap();
        let source = td.path().join("source");
        let foreign = td.path().join("foreign");
        let target = td.path().join("target");
        fs::write(&source, "overlay").unwrap();
        // Even a foreign path to the same inode is not our soft-link fallback.
        fs::hard_link(&source, &foreign).unwrap();
        symlink::symlink_file(&source, &target).unwrap();
        let desired = DesiredTree::from_entries(vec![DesiredEntry {
            target: target.clone(),
            provenance: Provenance::Overlay {
                overlay: "ov".into(),
                source: source.clone(),
            },
            intent: MaterializationIntent::SymlinkFile {
                source,
                link_type: LinkType::Hard,
            },
            permissions: None,
        }]);
        let report = Report::build(&desired).unwrap();
        fs::remove_file(&target).unwrap();
        symlink::symlink_file(&foreign, &target).unwrap();
        let executed = report.execute(Context::builder().build()).await.unwrap();
        assert!(matches!(
            executed.entries()[0].outcome,
            Outcome::NotOwned { .. }
        ));
        assert_eq!(executed.counts().removed, 0);
        assert_eq!(executed.counts().not_owned, 1);
        assert!(executed.needs_attention());
        assert_eq!(fs::read_link(target).unwrap(), foreign);
    }

    #[tokio::test]
    async fn partial_drift_after_classification_is_reported_as_retained() {
        let td = TempDir::new().unwrap();
        let target = td.path().join("file");
        fs::write(&target, "# >>> over: m >>>\nalias x=y\n# <<< over: m <<<\n").unwrap();
        let desired =
            DesiredTree::from_entries(vec![partial_entry(target.clone(), "alias x=y", "m")]);
        let report = Report::build(&desired).unwrap();
        let changed = "# >>> over: m >>>\nlocal edit\n# <<< over: m <<<\n";
        fs::write(&target, changed).unwrap();
        let executed = report.execute(Context::builder().build()).await.unwrap();
        assert!(matches!(
            executed.entries()[0].outcome,
            Outcome::NotOwned { .. }
        ));
        assert_eq!(executed.counts().removed, 0);
        assert_eq!(executed.counts().not_owned, 1);
        assert!(executed.needs_attention());
        assert_eq!(fs::read_to_string(target).unwrap(), changed);
    }

    #[tokio::test]
    async fn removing_a_partial_block_preserves_a_whitespace_only_remainder() {
        let td = TempDir::new().unwrap();
        let target = td.path().join("file");
        fs::write(
            &target,
            " \t\n# >>> over: m >>>\nalias x=y\n# <<< over: m <<<\n\n",
        )
        .unwrap();
        let desired =
            DesiredTree::from_entries(vec![partial_entry(target.clone(), "alias x=y", "m")]);
        let executed = Report::build(&desired)
            .unwrap()
            .execute(Context::builder().build())
            .await
            .unwrap();
        assert_eq!(fs::read_to_string(target).unwrap(), " \t\n\n");
        assert_eq!(executed.entries()[0].outcome, Outcome::Removed);
        assert_eq!(executed.counts().removed, 1);
        assert!(!executed.needs_attention());
    }

    #[tokio::test]
    async fn removing_a_virtual_projection_reports_its_retained_container() {
        use crate::desired::CheckoutScope;
        use crate::materialize::virtual_checkout::state::{self, VirtualCheckoutRecord};

        let xdg = TempDir::new().unwrap();
        // Like the other virtual-checkout tests, nextest process isolation
        // keeps this environment mutation away from concurrent test readers.
        unsafe {
            std::env::set_var("XDG_STATE_HOME", xdg.path());
        }
        let td = TempDir::new().unwrap();
        let source = td.path().join("source");
        let target = td.path().join("target");
        fs::create_dir_all(&source).unwrap();
        init_committed_repo(&source);
        fs::create_dir_all(target.join("mapped")).unwrap();
        fs::write(target.join("README.md"), "# Test").unwrap();
        fs::write(target.join("mapped/private"), "independent data").unwrap();
        let scope = CheckoutScope {
            target_excludes: vec![PathBuf::from("mapped")],
            ..Default::default()
        };
        let base_oid = git2::Repository::open(&source)
            .unwrap()
            .head()
            .unwrap()
            .target()
            .unwrap()
            .to_string();
        state::persist(
            &target,
            VirtualCheckoutRecord {
                overlay: "ov".into(),
                base_oid,
                scope: scope.clone(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let desired = DesiredTree::from_entries(vec![DesiredEntry {
            target: target.clone(),
            provenance: Provenance::Overlay {
                overlay: "ov".into(),
                source,
            },
            intent: MaterializationIntent::ScopedVirtualCheckout(scope),
            permissions: None,
        }]);
        let report = Report::build(&desired).unwrap();
        assert_eq!(report.entries()[0].outcome, Outcome::Removed);
        let executed = report.execute(Context::builder().build()).await.unwrap();
        assert_eq!(
            executed.entries()[0].outcome,
            Outcome::RemovedProjectionRetainedDirectory
        );
        assert_eq!(executed.counts().removed, 1);
        assert_eq!(executed.counts().checkout_needs_attention, 0);
        assert!(!executed.needs_attention());
        assert!(!executed.has_pending_removals());
        let message = executed.entries()[0].to_string();
        assert!(message.contains("removed projection; retained directory:"));
        assert!(!message.contains("remove:"));
        assert!(!target.join("README.md").exists());
        assert_eq!(
            fs::read_to_string(target.join("mapped/private")).unwrap(),
            "independent data"
        );
        assert!(state::record_for_blocking(&target).unwrap().is_none());
    }

    #[tokio::test]
    async fn execute_removes_matching_symlink() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        overlay_dir.child("file.txt").write_str("content").unwrap();
        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));

        let desired = DesiredTree::build(&c, &overlay).unwrap();
        Plan::build(&desired)
            .unwrap()
            .execute(c.clone())
            .await
            .unwrap();
        let target = td.path().join("file.txt");
        assert!(target.is_symlink());

        let desired2 = DesiredTree::build(&c, &overlay).unwrap();
        let report = Report::build(&desired2).unwrap();
        report.execute(c.clone()).await.unwrap();

        assert!(!target.exists() && !target.is_symlink());
        // Overlay source is untouched — reversible.
        assert!(overlay.root.join("file.txt").exists());
    }

    #[tokio::test]
    async fn execute_leaves_foreign_file_untouched() {
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
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));
        let desired = DesiredTree::build(&c, &overlay).unwrap();
        let report = Report::build(&desired).unwrap();
        report.execute(c.clone()).await.unwrap();

        assert_eq!(
            fs::read_to_string(td.path().join("file.txt")).unwrap(),
            "existing"
        );
    }

    #[tokio::test]
    async fn execute_removes_directory_once_empty() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~/sub\"")
            .unwrap();
        overlay_dir.child("file.txt").write_str("content").unwrap();
        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));

        let desired = DesiredTree::build(&c, &overlay).unwrap();
        Plan::build(&desired)
            .unwrap()
            .execute(c.clone())
            .await
            .unwrap();
        let sub_dir = td.path().join("sub");
        assert!(sub_dir.is_dir());

        let desired2 = DesiredTree::build(&c, &overlay).unwrap();
        let report = Report::build(&desired2).unwrap();
        report.execute(c.clone()).await.unwrap();

        // Bottom-up removal: file.txt's symlink goes first, emptying
        // `sub`, which is then removed too.
        assert!(!sub_dir.exists());
    }

    #[tokio::test]
    async fn execute_keeps_non_empty_directory() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~/sub\"")
            .unwrap();
        overlay_dir.child("file.txt").write_str("content").unwrap();
        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));

        let desired = DesiredTree::build(&c, &overlay).unwrap();
        Plan::build(&desired)
            .unwrap()
            .execute(c.clone())
            .await
            .unwrap();
        let sub_dir = td.path().join("sub");
        // A foreign file lands in the overlay's target directory after
        // apply — unrelated to any DesiredEntry.
        fs::write(sub_dir.join("unrelated.txt"), "keep me").unwrap();

        let desired2 = DesiredTree::build(&c, &overlay).unwrap();
        let report = Report::build(&desired2).unwrap();
        report.execute(c.clone()).await.unwrap();

        assert!(
            sub_dir.is_dir(),
            "directory with foreign content must remain"
        );
        assert!(sub_dir.join("unrelated.txt").exists());
        assert!(!sub_dir.join("file.txt").is_symlink());
    }

    #[tokio::test]
    async fn dry_run_execute_does_not_touch_filesystem() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        overlay_dir.child("file.txt").write_str("content").unwrap();
        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));

        let desired = DesiredTree::build(&c, &overlay).unwrap();
        Plan::build(&desired)
            .unwrap()
            .execute(c.clone())
            .await
            .unwrap();
        let target = td.path().join("file.txt");
        assert!(target.is_symlink());

        let dry_ctx = Context::builder()
            .dry_run(true)
            .root(td.path().to_path_buf())
            .repository(repo.clone())
            .overlay(overlay.clone())
            .build();
        let desired2 = DesiredTree::build(&c, &overlay).unwrap();
        let report = Report::build(&desired2).unwrap();
        assert!(report.has_pending_removals());
        report.execute(dry_ctx).await.unwrap();

        assert!(target.is_symlink(), "dry-run must not remove anything");
    }

    #[tokio::test]
    async fn execute_removes_clean_checkout() {
        let source_td = TempDir::new().unwrap();
        init_committed_repo(source_td.path());
        let dest_td = TempDir::new().unwrap();
        git2::build::RepoBuilder::new()
            .clone(source_td.path().to_str().unwrap(), dest_td.path())
            .unwrap();

        let desired = DesiredTree::from_entries(vec![checkout_entry(dest_td.path().to_path_buf())]);
        let report = Report::build(&desired).unwrap();
        assert!(matches!(
            report.entries()[0].outcome,
            Outcome::CheckoutDisposal { .. }
        ));

        let ctx = Context::builder().discard_checkout(true).build();
        report.execute(ctx).await.unwrap();
        assert!(!dest_td.path().join(".git").exists());
    }

    #[tokio::test]
    async fn execute_keeps_dirty_checkout() {
        let source_td = TempDir::new().unwrap();
        init_committed_repo(source_td.path());
        let dest_td = TempDir::new().unwrap();
        git2::build::RepoBuilder::new()
            .clone(source_td.path().to_str().unwrap(), dest_td.path())
            .unwrap();
        fs::write(dest_td.path().join("README.md"), "changed").unwrap();

        let desired = DesiredTree::from_entries(vec![checkout_entry(dest_td.path().to_path_buf())]);
        let report = Report::build(&desired).unwrap();
        assert!(report.needs_attention());

        let ctx = Context::builder().no_prompt(true).build();
        report.execute(ctx).await.unwrap();
        assert!(
            dest_td.path().join(".git").exists(),
            "dirty checkout must be kept"
        );
        assert_eq!(
            fs::read_to_string(dest_td.path().join("README.md")).unwrap(),
            "changed"
        );
    }

    /// Same shape as `checkout_entry`, but a declared (non-root) entry —
    /// used by the safety-decoupling regression test below (#140).
    fn declared_checkout_entry(target: PathBuf, url: &str) -> DesiredEntry {
        DesiredEntry {
            target,
            provenance: Provenance::Git {
                overlay: "ov".to_string(),
                repo_key: ".config/nvim".to_string(),
                config: Box::new(crate::actions::git::config::GitRepoConfig {
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

    #[tokio::test]
    async fn unapply_never_removes_a_declared_repo_with_uncommitted_changes_even_though_status_reports_it_applied()
     {
        // #140/ADR-021: `over status`/`over diff` treat a declared repo as
        // `Applied` once its declared configuration matches, even with
        // unrelated uncommitted content — but `unapply`'s destructive
        // removal must stay exactly as strict as it is for the root
        // checkout. This is the regression guard for that deliberate
        // decoupling (`Report::build`/`remove_checkout_if_clean` must call
        // `status::git::inspect_content`, never the root/declared-aware
        // `status::git::inspect`).
        let source_td = TempDir::new().unwrap();
        init_committed_repo(source_td.path());
        let dest_td = TempDir::new().unwrap();
        let cloned = git2::build::RepoBuilder::new()
            .clone(source_td.path().to_str().unwrap(), dest_td.path())
            .unwrap();
        let origin_url = cloned
            .find_remote("origin")
            .unwrap()
            .url()
            .unwrap()
            .to_string();
        drop(cloned);
        fs::write(dest_td.path().join("README.md"), "written by the plugin").unwrap();

        let entry = declared_checkout_entry(dest_td.path().to_path_buf(), &origin_url);

        // Sanity check: the new provisioning-aware `status::git::inspect`
        // really does report `Applied` here — otherwise this test wouldn't
        // be exercising the decoupling it's meant to guard.
        assert_eq!(status::git::inspect(&entry).unwrap(), Status::Applied);

        let desired = DesiredTree::from_entries(vec![entry]);
        let report = Report::build(&desired).unwrap();
        assert!(matches!(
            report.entries()[0].outcome,
            Outcome::CheckoutDisposal { .. }
        ));

        let ctx = Context::builder().no_prompt(true).build();
        report.execute(ctx).await.unwrap();
        assert!(
            dest_td.path().join(".git").exists(),
            "a declared repo with uncommitted changes must never be removed"
        );
        assert_eq!(
            fs::read_to_string(dest_td.path().join("README.md")).unwrap(),
            "written by the plugin"
        );
    }

    #[rstest]
    fn counts_display_lists_every_outcome() {
        let counts = Counts {
            removed: 1,
            already_absent: 2,
            not_owned: 3,
            checkout_needs_attention: 4,
        };
        let s = format!("{counts}");
        assert!(s.contains("1 to remove"));
        assert!(s.contains("2 already absent"));
        assert!(s.contains("3 not owned"));
        assert!(s.contains("4 checkout(s) need attention"));
    }

    #[test]
    fn entry_outcome_display_covers_every_variant() {
        fn entry_outcome(outcome: Outcome) -> EntryOutcome {
            EntryOutcome {
                entry: DesiredEntry {
                    target: PathBuf::from("/tmp/target"),
                    provenance: Provenance::Overlay {
                        overlay: "ov".to_string(),
                        source: PathBuf::from("/repo/ov"),
                    },
                    intent: MaterializationIntent::SymlinkFile {
                        source: PathBuf::from("/repo/ov/target"),
                        link_type: LinkType::Soft,
                    },
                    permissions: None,
                },
                outcome,
            }
        }

        assert!(format!("{}", entry_outcome(Outcome::Removed)).contains("remove:"));
        assert!(format!("{}", entry_outcome(Outcome::AlreadyAbsent)).contains("already absent:"));
        let not_owned = entry_outcome(Outcome::NotOwned {
            current: ActualState::File,
        });
        assert!(format!("{not_owned}").contains("skip:"));
        assert!(format!("{not_owned}").contains("not created by this overlay"));
        let checkout = entry_outcome(Outcome::CheckoutNotClean {
            status: Status::Modified,
        });
        assert!(format!("{checkout}").contains("skip:"));
        assert!(format!("{checkout}").contains("modified"));
    }
}
