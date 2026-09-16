//! Virtual checkout materialization (#141): ordinary, directly editable
//! files at the target with **no** `.git`, backed by the overlay's own
//! source repository instead of a separately declared/cloned one.
//!
//! Unrelated to `overlay.git`'s [`MaterializationIntent::Checkout`]
//! (a real `git clone`, owned by [`super::CheckoutMaterializer`], #110) —
//! see that intent's doc and ADR-022 for the full distinction.
//!
//! Like `CheckoutMaterializer`, this backend only handles *presence*:
//! `Missing -> Create`, and migration to/from a stale symlink or a legacy
//! symlink-only directory. Any further nuance for an *existing* virtual
//! checkout — dirty/added/deleted files, divergence from the source
//! repository — is deliberately left to `over status`/`over diff`/
//! `over commit`/`over sync` (`crate::status::virtual_checkout`), never
//! `apply`'s reconciliation loop; routing real, possibly-uncommitted
//! content through filesystem conflict-resolution semantics built for
//! symlinks/files could silently discard it.

pub(crate) mod git;
pub(crate) mod state;

use std::path::Path;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use tokio::task::spawn_blocking;

use crate::actions::fs::remove_target;
use crate::actions::symlink::LinkType;
use crate::desired::{DesiredEntry, MaterializationIntent, Provenance};
use crate::exec::Ctx;
use crate::plan::actual::{self, ActualState};
use crate::plan::{Operation, PlanStep};

use super::Materializer;
use state::VirtualCheckoutRecord;

/// Owns [`MaterializationIntent::VirtualCheckout`]. See the module doc for
/// the scope split with `over status`/`over diff`/`over commit`/`over sync`.
pub struct VirtualCheckoutMaterializer;

/// Best-effort reconstruction of what a stale symlink used to materialize
/// (#129) — used only for [`Operation::Migrate`]'s `from` field
/// (diagnostics), mirroring `materialize::checkout::reconstruct_symlink_intent`
/// exactly. Kept as an independent copy rather than shared: the two
/// intents' migration semantics are otherwise unrelated, and sharing four
/// lines isn't worth a cross-module dependency.
fn reconstruct_symlink_intent(points_to: &Path) -> MaterializationIntent {
    if points_to.is_dir() {
        MaterializationIntent::SymlinkDirectory {
            source: points_to.to_path_buf(),
            link_type: LinkType::Soft,
        }
    } else {
        MaterializationIntent::SymlinkFile {
            source: points_to.to_path_buf(),
            link_type: LinkType::Soft,
        }
    }
}

#[async_trait(?Send)]
impl Materializer for VirtualCheckoutMaterializer {
    fn handles(&self, intent: &MaterializationIntent) -> bool {
        matches!(intent, MaterializationIntent::VirtualCheckout)
    }

    fn classify(&self, entry: &DesiredEntry) -> Result<Operation> {
        let actual = actual::inspect(&entry.target)?;

        // A stale symlink (a rule changed from `SymlinkFile`/`SymlinkDirectory`
        // to `checkout`) — always safe to migrate away from: removing a
        // symlink never loses source content.
        if let ActualState::Symlink { points_to } = &actual {
            return Ok(Operation::Migrate {
                from: reconstruct_symlink_intent(points_to),
                to: MaterializationIntent::VirtualCheckout,
                blocked: None,
            });
        }

        if matches!(actual, ActualState::Missing) {
            return Ok(Operation::Create);
        }

        // Something already occupies the target. A recorded association
        // means this is a known virtual checkout — defer to
        // status/diff/commit/sync for anything beyond "it exists" (see
        // module doc). Uses the blocking XDG read since `classify` is
        // deliberately sync across every `Materializer` backend.
        if state::record_for_blocking(&entry.target)?.is_some() {
            return Ok(Operation::Noop);
        }

        // No recorded association: only auto-adopt a legacy symlink-only
        // directory (nothing lost by discarding it, same reasoning as
        // `CheckoutMaterializer`'s #130 handling) — anything else is a
        // genuine conflict, never silently adopted as a virtual checkout.
        if matches!(actual, ActualState::Directory) && actual::is_symlink_only(&entry.target)? {
            return Ok(Operation::Migrate {
                from: MaterializationIntent::Directory,
                to: MaterializationIntent::VirtualCheckout,
                blocked: None,
            });
        }

        Ok(Operation::Conflict { current: actual })
    }

    /// Invoked for `Operation::Create` and for an unblocked
    /// `Operation::Migrate` — the latter first removes what's there
    /// (always safe per `classify`'s own reasoning), then both fall
    /// through to the same checkout-from-source-HEAD logic.
    async fn materialize(&self, ctx: Ctx, step: &PlanStep) -> Result<()> {
        if let Operation::Conflict { current } = &step.operation {
            return Err(anyhow!(
                "refusing to materialize a virtual checkout at '{}': found {}, not a \
                 reconstructible legacy `over` installation — move it aside and rerun",
                step.entry.target.display(),
                current,
            ));
        }
        if ctx.dry_run {
            return Ok(());
        }

        if matches!(step.operation, Operation::Migrate { blocked: None, .. }) {
            let target = step.entry.target.clone();
            spawn_blocking(move || remove_target(&target)).await??;
        }

        let Provenance::Overlay { overlay, source } = &step.entry.provenance else {
            unreachable!(
                "VirtualCheckout entries only ever carry Provenance::Overlay \
                 (see desired::tree::{{collect_own_entries,walk_overlay_tree}})"
            );
        };

        let target = step.entry.target.clone();
        let overlay = overlay.clone();
        let source = source.clone();
        let record =
            spawn_blocking(move || checkout_from_head(&target, &overlay, &source)).await??;
        state::persist(&step.entry.target, record).await
    }
}

/// Blocking: discover the source repository, checkout its current `HEAD`
/// (restricted to the managed path) into `target` with no `.git`, and
/// build the association record to persist. All git2/filesystem work here
/// is synchronous, hence `spawn_blocking` at the call site.
fn checkout_from_head(
    target: &Path,
    overlay: &str,
    source: &Path,
) -> Result<VirtualCheckoutRecord> {
    let (repo, managed_path) = git::discover_source(source)?;
    let head_tree = git::base_tree(&repo, None)?;
    let subtree = git::managed_subtree(&repo, &head_tree, &managed_path)?;
    let content_tree = git::managed_content_tree(&repo, &subtree)?;
    git::checkout_subtree(&repo, &content_tree, target)?;

    let base_oid = repo.head()?.peel_to_commit()?.id().to_string();
    Ok(VirtualCheckoutRecord {
        overlay: overlay.to_string(),
        managed_path,
        base_oid,
        created_at: now(),
        last_commit_at: None,
    })
}

pub(crate) fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::Context;
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    use git2::Signature;
    use std::fs;
    use std::path::PathBuf;

    fn init_committed_repo(path: &Path) {
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

    fn entry(target: PathBuf, source: PathBuf) -> DesiredEntry {
        DesiredEntry {
            target,
            provenance: Provenance::Overlay {
                overlay: "ov".to_string(),
                source,
            },
            intent: MaterializationIntent::VirtualCheckout,
            permissions: None,
        }
    }

    #[test]
    fn handles_only_virtual_checkout_intent() {
        let m = VirtualCheckoutMaterializer;
        assert!(m.handles(&MaterializationIntent::VirtualCheckout));
        assert!(!m.handles(&MaterializationIntent::Checkout));
        assert!(!m.handles(&MaterializationIntent::Directory));
    }

    #[test]
    fn classify_missing_target_is_create() {
        let m = VirtualCheckoutMaterializer;
        let td = TempDir::new().unwrap();
        let e = entry(td.path().join("nope"), td.path().to_path_buf());
        assert!(matches!(m.classify(&e).unwrap(), Operation::Create));
    }

    #[test]
    fn classify_stale_symlink_is_migrate() {
        let m = VirtualCheckoutMaterializer;
        let td = TempDir::new().unwrap();
        let source_dir = td.child("source_dir");
        source_dir.create_dir_all().unwrap();
        let target = td.path().join("target");
        symlink::symlink_dir(source_dir.path(), &target).unwrap();

        let e = entry(target, td.path().to_path_buf());
        match m.classify(&e).unwrap() {
            Operation::Migrate { from, to, blocked } => {
                assert!(matches!(
                    from,
                    MaterializationIntent::SymlinkDirectory { .. }
                ));
                assert!(matches!(to, MaterializationIntent::VirtualCheckout));
                assert!(blocked.is_none());
            }
            other => panic!("expected Migrate, got {other:?}"),
        }
    }

    #[test]
    fn classify_directory_with_no_recorded_state_and_foreign_content_is_conflict() {
        let m = VirtualCheckoutMaterializer;
        let td = TempDir::new().unwrap();
        let target = td.child("target_dir");
        target.create_dir_all().unwrap();
        fs::write(target.path().join("foreign.txt"), "not from over").unwrap();

        let e = entry(target.path().to_path_buf(), td.path().to_path_buf());
        assert!(matches!(
            m.classify(&e).unwrap(),
            Operation::Conflict { .. }
        ));
    }

    #[tokio::test]
    async fn materialize_create_checks_out_managed_path_with_no_git() {
        let source_td = TempDir::new().unwrap();
        init_committed_repo(source_td.path());
        source_td.child("sub").create_dir_all().unwrap();
        fs::write(source_td.path().join("sub/inner.txt"), "hi").unwrap();
        let repo = git2::Repository::open(source_td.path()).unwrap();
        let sig = Signature::now("Test", "test@test.com").unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let parent = repo.head().unwrap().peel_to_commit().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "add sub", &tree, &[&parent])
            .unwrap();

        let dest_td = TempDir::new().unwrap();
        let target = dest_td.path().join("checkout");

        let m = VirtualCheckoutMaterializer;
        let e = entry(target.clone(), source_td.path().join("sub"));
        let step = PlanStep {
            entry: e,
            operation: Operation::Create,
        };
        let ctx = Context::builder().build();
        m.materialize(ctx, &step).await.unwrap();

        assert!(target.join("inner.txt").exists());
        assert!(!target.join(".git").exists());
    }

    #[tokio::test]
    async fn materialize_rejects_conflict_without_touching_content() {
        let td = TempDir::new().unwrap();
        let target = td.child("target_dir");
        target.create_dir_all().unwrap();
        fs::write(target.path().join("foreign.txt"), "not from over").unwrap();

        let m = VirtualCheckoutMaterializer;
        let e = entry(target.path().to_path_buf(), td.path().to_path_buf());
        let step = PlanStep {
            entry: e,
            operation: Operation::Conflict {
                current: ActualState::Directory,
            },
        };
        let ctx = Context::builder().build();
        let result = m.materialize(ctx, &step).await;

        assert!(result.is_err());
        assert!(target.path().join("foreign.txt").exists());
    }
}
