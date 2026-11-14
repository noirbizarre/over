//! Virtual checkout materialization (#141): ordinary, directly editable
//! files at the target with **no** `.git`, backed by the overlay's own
//! source repository instead of a separately declared/cloned one.
//!
//! Unrelated to `overlay.git`'s [`MaterializationIntent::Checkout`]
//! (a real `git clone`, owned by [`super::CheckoutMaterializer`], #110) —
//! see that intent's doc and ADR-022 for the full distinction.
//!
//! Like `CheckoutMaterializer`, this backend only handles *presence*:
//! `Missing -> Create`, with fresh checkouts projecting finite owned leaves into
//! shared directories. Deployed symlinks require explicit teardown first.
//! Any further nuance for an *existing* virtual
//! checkout — dirty/added/deleted files, divergence from the source
//! repository — is deliberately left to `over status`/`over diff`/
//! `over commit`/`over sync` (`crate::status::virtual_checkout`), never
//! `apply`'s reconciliation loop. First initialization into an unassociated
//! directory resolves only actual path collisions, preserving local content
//! when the user chooses Skip or Absorb.

pub(crate) mod generated;
pub(crate) mod generated_migration;
pub(crate) mod generated_snapshot;
pub(crate) mod git;
mod initial;
pub(crate) mod snapshot;
pub(crate) mod state;

use std::path::Path;

use anyhow::{Result, bail};
use async_trait::async_trait;
use tokio::task::spawn_blocking;

use crate::actions::fs::remove_target;
use crate::actions::symlink::LinkType;
use crate::desired::{DesiredEntry, MaterializationIntent, Provenance};
use crate::exec::Ctx;
use crate::plan::actual::{self, ActualState};
use crate::plan::{Operation, PlanStep};

use super::Materializer;

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
        intent.is_virtual_checkout()
    }

    fn classify(&self, entry: &DesiredEntry) -> Result<Operation> {
        if entry.intent.checkout_scope().rendered.is_some() {
            return generated::classify(entry);
        }
        // A missing new destination does not mean this source unit is undeployed.
        state::verify_live_target(entry)?;
        let actual = actual::inspect(&entry.target)?;
        let scope = entry.intent.checkout_scope();
        if !scope.target_excludes.is_empty() || scope.source_policy.is_some() {
            let Provenance::Overlay { source, .. } = &entry.provenance else {
                unreachable!()
            };
            let (repo, managed) = git::discover_source(source)?;
            let head = git::base_tree(&repo, None)?;
            git::scoped_entries(&repo, &head, &managed, &scope)?;
        }

        // A stale symlink (a rule changed from `SymlinkFile`/`SymlinkDirectory`
        // to `checkout`) — always safe to migrate away from: removing a
        // symlink never loses source content.
        if let ActualState::Symlink { points_to } = &actual {
            anyhow::ensure!(
                scope.projection.is_none(),
                "cannot convert deployed symlinks to a virtual checkout; unapply the old layout first"
            );
            let Provenance::Overlay { source, .. } = &entry.provenance else {
                unreachable!()
            };
            if points_to != source {
                return Ok(Operation::Conflict { current: actual });
            }
            return Ok(Operation::Migrate {
                from: reconstruct_symlink_intent(points_to),
                to: entry.intent.clone(),
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
        if let Some(record) = state::record_for_entry(entry)? {
            state::verify(entry, &record)?;
            if !record.initializing {
                return Ok(Operation::Noop);
            }
        }

        // Preserve the legacy migration checks before assessing ordinary content.
        if matches!(actual, ActualState::Directory)
            && (!scope.target_excludes.is_empty() || scope.source_policy.is_some())
            && !git::has_owned_content(&entry.target, &scope)?
        {
            return Ok(Operation::Create);
        }
        let Provenance::Overlay { source, .. } = &entry.provenance else {
            unreachable!()
        };
        if matches!(actual, ActualState::Directory)
            && super::symlink::directory_is_symlink_mirror(&entry.target, source)?
        {
            anyhow::ensure!(
                scope.projection.is_none(),
                "cannot convert a deployed symlink mirror to a virtual checkout; unapply the old layout first"
            );
            anyhow::ensure!(
                scope.target_excludes.is_empty(),
                "cannot migrate a legacy directory with mapping holes; unapply its old layout first"
            );
            return Ok(Operation::Migrate {
                from: MaterializationIntent::Directory,
                to: entry.intent.clone(),
                blocked: git::has_unowned_content(&entry.target, &scope)?.then(|| "legacy directory contains ignored or unmanaged content; move it aside before changing materialization".into()),
            });
        }

        if matches!(actual, ActualState::Directory) {
            return if initial::has_conflicts(entry)? {
                Ok(Operation::Conflict { current: actual })
            } else {
                Ok(Operation::Create)
            };
        }

        Ok(Operation::Conflict { current: actual })
    }

    /// Initialize missing or unassociated directory targets, resolving leaf
    /// conflicts. Unblocked migrations first remove their verified legacy layout.
    async fn materialize(&self, ctx: Ctx, step: &PlanStep) -> Result<()> {
        if step.entry.intent.checkout_scope().rendered.is_some() {
            if !ctx.dry_run {
                let entry = step.entry.clone();
                tokio::task::spawn_blocking(move || generated::reconcile(&entry, false))
                    .await??
                    .ensure_clean()?;
            }
            return Ok(());
        }
        if let Operation::Conflict { current } = &step.operation
            && !matches!(current, ActualState::Directory)
        {
            bail!(
                "refusing to materialize a virtual checkout at '{}': found {}, not a \
                 reconstructible legacy `over` installation — move it aside and rerun",
                step.entry.target.display(),
                current,
            );
        }
        if ctx.dry_run {
            return Ok(());
        }

        if matches!(step.operation, Operation::Conflict { .. }) {
            anyhow::ensure!(
                matches!(
                    self.classify(&step.entry)?,
                    Operation::Create
                        | Operation::Conflict {
                            current: ActualState::Directory
                        }
                ),
                "virtual checkout target changed since classification"
            );
        }

        if matches!(step.operation, Operation::Create) {
            anyhow::ensure!(
                matches!(self.classify(&step.entry)?, Operation::Create),
                "virtual checkout target changed since classification"
            );
        }

        if matches!(step.operation, Operation::Migrate { blocked: None, .. }) {
            anyhow::ensure!(
                matches!(
                    self.classify(&step.entry)?,
                    Operation::Migrate { blocked: None, .. }
                ),
                "migration target '{}' changed since classification",
                step.entry.target.display()
            );
            if matches!(
                step.operation,
                Operation::Migrate {
                    from: MaterializationIntent::Directory,
                    ..
                }
            ) {
                anyhow::ensure!(
                    !git::has_unowned_content(
                        &step.entry.target,
                        &step.entry.intent.checkout_scope()
                    )?,
                    "cannot migrate a directory containing ignored or unmanaged content"
                );
            }
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
        let scope = step.entry.intent.checkout_scope();
        let ctx = ctx.clone();
        let record =
            spawn_blocking(move || initial::initialize(&ctx, &target, &overlay, &source, scope))
                .await??;
        state::persist(&step.entry.target, record).await
    }
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

    /// Points `$XDG_STATE_HOME` at a fresh, writable temp dir for the
    /// duration of the returned guard's lifetime — `classify`/`materialize`
    /// read/write `VirtualCheckoutState` via the real, non-injectable
    /// `XdgDirs::new()` (the `Materializer` trait's fixed signature leaves
    /// no room to inject a `StateFile` here), so any test reaching that
    /// code path must isolate this or it silently pollutes the real
    /// `$XDG_STATE_HOME/over/virtual_checkout.toml`.
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
        let _xdg = isolate_xdg_state();
        let m = VirtualCheckoutMaterializer;
        let td = TempDir::new().unwrap();
        let e = entry(td.path().join("nope"), td.path().to_path_buf());
        assert!(matches!(m.classify(&e).unwrap(), Operation::Create));
    }

    #[test]
    fn classify_stale_symlink_is_migrate() {
        let _xdg = isolate_xdg_state();
        let m = VirtualCheckoutMaterializer;
        let td = TempDir::new().unwrap();
        let source_dir = td.child("source_dir");
        source_dir.create_dir_all().unwrap();
        let target = td.path().join("target");
        symlink::symlink_dir(source_dir.path(), &target).unwrap();

        let e = entry(target, source_dir.path().to_path_buf());
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
    fn an_existing_directory_with_unrelated_content_is_a_valid_container() {
        let _xdg = isolate_xdg_state();
        let m = VirtualCheckoutMaterializer;
        let td = TempDir::new().unwrap();
        let target = td.child("target_dir");
        target.create_dir_all().unwrap();
        fs::write(target.path().join("foreign.txt"), "not from over").unwrap();
        let source = TempDir::new().unwrap();
        init_committed_repo(source.path());
        let e = entry(target.path().to_path_buf(), source.path().to_path_buf());
        assert!(matches!(m.classify(&e).unwrap(), Operation::Create));
    }

    #[tokio::test]
    async fn materialize_create_checks_out_managed_path_with_no_git() {
        let _xdg = isolate_xdg_state();
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
