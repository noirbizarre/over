use std::fs;
use std::path::Path;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use tokio::task::spawn_blocking;

use crate::actions::fs::{EnsureDirLink, remove_target};
use crate::actions::symlink::LinkType;
use crate::actions::{EnsureDir, EnsureLink, EnsureSymlink};
use crate::desired::{DesiredEntry, MaterializationIntent, Provenance};
use crate::exec::{Action, Ctx};
use crate::plan::actual::{self, ActualState};
use crate::plan::{Operation, PlanStep};
use crate::status::{self, Status};

use super::Materializer;

/// The default materialization backend (ADR-006): plain directories and
/// symlinks (file- or directory-level), dispatched to the existing
/// `actions::fs`/`actions::symlink` `Action` impls. Owns every
/// [`MaterializationIntent`] except
/// [`MaterializationIntent::Checkout`](crate::desired::MaterializationIntent::Checkout)
/// (owned by `CheckoutMaterializer`, #110) and
/// [`MaterializationIntent::PartialFile`](crate::desired::MaterializationIntent::PartialFile)
/// (owned by `PartialFileMaterializer`, #66).
///
/// This is a pure move of the logic that used to live directly in
/// `plan::reconcile` (`classify`/`build_action`) — no behavior change.
pub struct SymlinkMaterializer;

#[async_trait(?Send)]
impl Materializer for SymlinkMaterializer {
    fn handles(&self, intent: &MaterializationIntent) -> bool {
        !matches!(
            intent,
            MaterializationIntent::Checkout
                | MaterializationIntent::VirtualCheckout
                | MaterializationIntent::PartialFile { .. }
        )
    }

    /// Classify a single entry against current filesystem state. Read-only.
    fn classify(&self, entry: &DesiredEntry) -> Result<Operation> {
        match &entry.intent {
            MaterializationIntent::Directory => {
                let actual = actual::inspect(&entry.target)?;
                match actual {
                    ActualState::Missing => Ok(Operation::Create),
                    // Deliberately *not* checked for a checkout-migration
                    // here (unlike the `SymlinkDirectory` arm below):
                    // `collect_own_entries` unconditionally emits a
                    // `Directory` entry for every overlay's own target root
                    // (`src/desired/tree.rs`), regardless of whether
                    // `overlay.git` *also* declares that same root path
                    // (`git = "<url>"`, the common root-checkout shorthand)
                    // — so this entry's target legitimately has a `.git`
                    // any time a root-level checkout is configured, forever,
                    // not just when a rule changed. Treating that as a
                    // pending migration would delete the checkout on every
                    // single `apply`. Coordinating the two independent
                    // `DesiredEntry` sources for the same target is a
                    // pre-existing, separate gap (not #129's to fix — see
                    // the module's `SymlinkDirectory` arm doc, which isn't
                    // exposed to this ambiguity since the root's own base
                    // entry is always plain `Directory`, never
                    // `SymlinkDirectory`).
                    ActualState::Directory => Ok(Operation::Noop),
                    // A directory-level symlink used to sit here (#126); a
                    // rule change now recurses this path instead (#129).
                    // Only migrate when it's provably the *same* overlay
                    // source this `Directory` entry would itself walk —
                    // never a foreign symlink pointing elsewhere.
                    ActualState::Symlink { ref points_to } => match &entry.provenance {
                        Provenance::Overlay { source, .. } if points_to == source => {
                            Ok(Operation::Migrate {
                                from: MaterializationIntent::SymlinkDirectory {
                                    source: points_to.clone(),
                                    link_type: LinkType::Soft,
                                },
                                to: MaterializationIntent::Directory,
                                blocked: None,
                            })
                        }
                        _ => Ok(Operation::Conflict {
                            current: actual.clone(),
                        }),
                    },
                    other => Ok(Operation::Conflict { current: other }),
                }
            }
            MaterializationIntent::SymlinkFile { source, .. } => {
                let actual = actual::inspect(&entry.target)?;
                Ok(match actual {
                    ActualState::Missing => Operation::Create,
                    ActualState::Symlink { ref points_to } if points_to == source => {
                        Operation::Noop
                    }
                    other => Operation::Conflict { current: other },
                })
            }
            MaterializationIntent::SymlinkDirectory { source, .. } => {
                let actual = actual::inspect(&entry.target)?;
                match actual {
                    ActualState::Missing => Ok(Operation::Create),
                    ActualState::Symlink { ref points_to } if points_to == source => {
                        Ok(Operation::Noop)
                    }
                    // A real directory sits where a directory-level symlink
                    // is now desired (#129): either it's actually a git
                    // checkout that should migrate (rule/`overlay.git`
                    // change), or it's a directory this same overlay
                    // previously walked file-by-file and can safely
                    // collapse (every leaf is exactly the symlink the
                    // overlay would create), or it's a genuine conflict.
                    ActualState::Directory => {
                        if let Some(op) = checkout_migration(entry)? {
                            return Ok(op);
                        }
                        if let Some(op) = virtual_checkout_migration(entry)? {
                            return Ok(op);
                        }
                        if directory_is_symlink_mirror(&entry.target, source)? {
                            return Ok(Operation::Migrate {
                                from: MaterializationIntent::Directory,
                                to: entry.intent.clone(),
                                blocked: None,
                            });
                        }
                        Ok(Operation::Conflict {
                            current: ActualState::Directory,
                        })
                    }
                    other => Ok(Operation::Conflict { current: other }),
                }
            }
            MaterializationIntent::Checkout
            | MaterializationIntent::VirtualCheckout
            | MaterializationIntent::PartialFile { .. } => {
                unreachable!("MaterializerRegistry only calls classify() after handles() passed")
            }
        }
    }

    async fn materialize(&self, ctx: Ctx, step: &PlanStep) -> Result<()> {
        match &step.operation {
            Operation::Migrate {
                from,
                blocked: None,
                ..
            } => materialize_migration(ctx, &step.entry, from).await,
            // Defensive: `Plan::execute`'s `is_actionable` never calls
            // `materialize` for a blocked migration — but never silently
            // fall through to the plain `build_action` path below either,
            // which could force-discard whatever's blocking it (e.g. a
            // dirty checkout) if this were ever invoked directly. Treat it
            // exactly like `Noop`.
            Operation::Migrate {
                blocked: Some(reason),
                ..
            } => {
                tracing::warn!(
                    target = %step.entry.target.display(),
                    reason = %reason,
                    "materialize called for a blocked migration; skipping",
                );
                Ok(())
            }
            _ => {
                let action = build_action(ctx.clone(), &step.entry);
                action.execute(ctx).await
            }
        }
    }
}

/// `entry.target` already exists as a real directory. If it's actually a
/// git checkout, return the [`Operation::Migrate`] reconciling it to
/// `entry.intent` (#129) — safe (`blocked: None`) when fully in sync with
/// its upstream, blocked otherwise (never silently discarded, never routed
/// through force/no_prompt/absorb-diff — the exact safety gate
/// `CheckoutMaterializer::classify` already relies on for the reverse
/// direction, reused here via `status::git::inspect_checkout` rather than
/// re-derived). `None` when `entry.target` isn't a checkout at all — the
/// fast `.git`-presence check skips the `git2` call entirely for the
/// overwhelmingly common case, leaving the caller to decide its own
/// default.
fn checkout_migration(entry: &DesiredEntry) -> Result<Option<Operation>> {
    if !entry.target.join(".git").exists() {
        return Ok(None);
    }
    Ok(match status::git::inspect_checkout(&entry.target)? {
        // `.git` exists but isn't a valid repo — not actually a checkout.
        Status::Broken => None,
        Status::Applied => Some(Operation::Migrate {
            from: MaterializationIntent::Checkout,
            to: entry.intent.clone(),
            blocked: None,
        }),
        unsafe_status => Some(Operation::Migrate {
            from: MaterializationIntent::Checkout,
            to: entry.intent.clone(),
            blocked: Some(status::describe(&unsafe_status)),
        }),
    })
}

/// `entry.target` already exists as a real directory. If it's actually a
/// virtual checkout (#141) — has a recorded XDG association — return the
/// [`Operation::Migrate`] reconciling it to `entry.intent`: safe
/// (`blocked: None`) only when fully clean (`Status::Applied`, no local
/// edits and not behind the source repository), blocked otherwise. Mirrors
/// [`checkout_migration`] exactly, for the same reason: never silently
/// discard uncommitted virtual-checkout content, never route it through
/// force/no_prompt/absorb-diff. `None` when `entry.target` isn't a
/// recognized virtual checkout at all.
fn virtual_checkout_migration(entry: &DesiredEntry) -> Result<Option<Operation>> {
    use crate::materialize::virtual_checkout::state;

    if state::record_for_blocking(&entry.target)?.is_none() {
        return Ok(None);
    }
    Ok(match status::virtual_checkout::inspect(entry)? {
        Status::Applied => Some(Operation::Migrate {
            from: MaterializationIntent::VirtualCheckout,
            to: entry.intent.clone(),
            blocked: None,
        }),
        unsafe_status => Some(Operation::Migrate {
            from: MaterializationIntent::VirtualCheckout,
            to: entry.intent.clone(),
            blocked: Some(status::describe(&unsafe_status)),
        }),
    })
}

/// Recursively confirm every entry under `target` is exactly the symlink
/// the overlay would itself create for it under `source` — a directory
/// "fully owned" by the overlay, nothing foreign, nothing real. Lets a
/// `recurse -> directory-level symlink` rule change (#129) collapse the
/// directory safely instead of falling back to `Operation::Conflict`;
/// removing it loses nothing the overlay didn't already provide.
fn directory_is_symlink_mirror(target: &Path, source: &Path) -> Result<bool> {
    for dir_entry in fs::read_dir(target)? {
        let dir_entry = dir_entry?;
        let path = dir_entry.path();
        let file_type = dir_entry.file_type()?;
        let expected = source.join(dir_entry.file_name());
        if file_type.is_symlink() {
            if fs::read_link(&path)? != expected {
                return Ok(false);
            }
        } else if file_type.is_dir() {
            if !directory_is_symlink_mirror(&path, &expected)? {
                return Ok(false);
            }
        } else {
            // A real file (or anything else): not reconstructible from the
            // overlay alone.
            return Ok(false);
        }
    }
    Ok(true)
}

/// Perform an unblocked `Operation::Migrate` (#129): remove whatever
/// currently occupies `entry.target` (re-verifying a checkout's safety
/// right before deleting — classify → execute isn't atomic, and a checkout
/// must never be discarded even if it turned dirty in between), then
/// materialize `entry.intent` fresh via the same tested `Action`s
/// `build_action` already dispatches to. The target is empty/gone by the
/// time that action runs, so it always takes the plain `Create` path —
/// never the force/no_prompt/absorb-diff conflict machinery, which is
/// exactly what a rule-driven migration must avoid triggering.
async fn materialize_migration(
    ctx: Ctx,
    entry: &DesiredEntry,
    from: &MaterializationIntent,
) -> Result<()> {
    if ctx.dry_run {
        return Ok(());
    }

    if matches!(from, MaterializationIntent::Checkout) {
        match status::git::inspect_checkout(&entry.target)? {
            Status::Applied => {}
            other => {
                return Err(anyhow!(
                    "refusing to migrate '{}': checkout {} since it was classified — rerun to re-evaluate",
                    entry.target.display(),
                    status::describe(&other),
                ));
            }
        }
    }
    if matches!(from, MaterializationIntent::VirtualCheckout) {
        match status::virtual_checkout::inspect(entry)? {
            Status::Applied => {}
            other => {
                return Err(anyhow!(
                    "refusing to migrate '{}': virtual checkout {} since it was classified — rerun to re-evaluate",
                    entry.target.display(),
                    status::describe(&other),
                ));
            }
        }
        // The target is about to be removed and replaced by a symlink —
        // the XDG association would otherwise dangle (and could
        // misattribute a future virtual checkout re-materialized at the
        // same path to this stale `base_oid`).
        crate::materialize::virtual_checkout::state::remove(&entry.target).await?;
    }

    let target = entry.target.clone();
    spawn_blocking(move || remove_target(&target)).await??;

    build_action(ctx.clone(), entry).execute(ctx).await
}

/// Translate an entry into the existing, tested `Action` that materializes
/// it — the single place that knows about concrete `actions::fs`/
/// `actions::symlink` types.
///
/// Dispatches on `provenance`, not just `intent`, because two different
/// existing actions handle symlinks with different conflict semantics:
/// regular per-file overlay entries go through `EnsureLink`/`EnsureDirLink`
/// (interactive skip/overwrite/absorb/diff resolution, ADR-006), while
/// `.link.*` sidecar entries go through `EnsureSymlink` (unconditional
/// overwrite, and the only one of the two that supports hard links).
fn build_action(ctx: Ctx, entry: &DesiredEntry) -> Box<dyn Action> {
    let is_sidecar = matches!(entry.provenance, Provenance::SymlinkSidecar { .. });

    match &entry.intent {
        MaterializationIntent::Directory => Box::new(EnsureDir::new(entry.target.clone())),
        MaterializationIntent::SymlinkFile { source, link_type } => {
            if is_sidecar {
                Box::new(EnsureSymlink::new(
                    source.clone(),
                    entry.target.clone(),
                    link_type.clone(),
                    false,
                ))
            } else {
                Box::new(EnsureLink::new(ctx, source.clone(), entry.target.clone()))
            }
        }
        MaterializationIntent::SymlinkDirectory { source, link_type } => {
            if is_sidecar {
                Box::new(EnsureSymlink::new(
                    source.clone(),
                    entry.target.clone(),
                    link_type.clone(),
                    true,
                ))
            } else {
                Box::new(EnsureDirLink::new(
                    ctx,
                    source.clone(),
                    entry.target.clone(),
                ))
            }
        }
        MaterializationIntent::Checkout
        | MaterializationIntent::VirtualCheckout
        | MaterializationIntent::PartialFile { .. } => {
            unreachable!(
                "SymlinkMaterializer::handles filters out Checkout/VirtualCheckout/PartialFile entries"
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::symlink::LinkType;
    use crate::exec::Context;
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    use git2::Signature;
    use std::path::PathBuf;

    fn entry(intent: MaterializationIntent) -> DesiredEntry {
        DesiredEntry {
            target: PathBuf::from("/tmp/does-not-matter-for-this-test"),
            provenance: Provenance::Overlay {
                overlay: "ov".to_string(),
                source: PathBuf::from("/repo/ov"),
            },
            intent,
            permissions: None,
        }
    }

    /// Points `$XDG_STATE_HOME` at a fresh, writable temp dir for the
    /// duration of the returned guard's lifetime — the virtual-checkout
    /// migration tests below read/write `VirtualCheckoutState` via the
    /// real, non-injectable `XdgDirs::new()` (`classify`/`materialize` are
    /// fixed `Materializer` trait methods, leaving no room to inject a
    /// `StateFile`), so every test reaching that code path must isolate
    /// this or it silently pollutes the real
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

    /// `git2::Repository::init` + one commit, mirroring
    /// `materialize::checkout::tests::init_committed_repo` — a clean
    /// checkout by default (no upstream configured, so `ahead_behind`
    /// reports `None` -> `Status::Applied`).
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

    #[test]
    fn handles_returns_false_for_checkout() {
        let m = SymlinkMaterializer;
        assert!(!m.handles(&MaterializationIntent::Checkout));
    }

    #[test]
    fn handles_returns_false_for_partial_file() {
        let m = SymlinkMaterializer;
        assert!(!m.handles(&MaterializationIntent::PartialFile {
            content: "x".to_string(),
            marker: "m".to_string(),
        }));
    }

    #[test]
    fn handles_returns_true_for_directory_and_symlinks() {
        let m = SymlinkMaterializer;
        assert!(m.handles(&MaterializationIntent::Directory));
        assert!(m.handles(&MaterializationIntent::SymlinkFile {
            source: PathBuf::from("/src"),
            link_type: LinkType::Soft,
        }));
        assert!(m.handles(&MaterializationIntent::SymlinkDirectory {
            source: PathBuf::from("/src"),
            link_type: LinkType::Soft,
        }));
    }

    #[test]
    fn missing_directory_classifies_as_create() {
        let m = SymlinkMaterializer;
        let e = entry(MaterializationIntent::Directory);
        assert!(matches!(m.classify(&e).unwrap(), Operation::Create));
    }

    #[test]
    fn existing_directory_classifies_as_noop() {
        use assert_fs::TempDir;
        let td = TempDir::new().unwrap();
        let m = SymlinkMaterializer;
        let e = DesiredEntry {
            target: td.path().to_path_buf(),
            ..entry(MaterializationIntent::Directory)
        };
        assert!(matches!(m.classify(&e).unwrap(), Operation::Noop));
    }

    #[test]
    fn file_where_directory_expected_is_a_conflict() {
        use assert_fs::TempDir;
        let td = TempDir::new().unwrap();
        let blocked = td.path().join("blocked");
        std::fs::write(&blocked, "in the way").unwrap();
        let m = SymlinkMaterializer;
        let e = DesiredEntry {
            target: blocked,
            ..entry(MaterializationIntent::Directory)
        };
        assert!(matches!(
            m.classify(&e).unwrap(),
            Operation::Conflict { .. }
        ));
    }

    #[test]
    fn build_action_directory() {
        let entry = DesiredEntry {
            target: PathBuf::from("/tmp/does-not-matter"),
            provenance: Provenance::Overlay {
                overlay: "ov".to_string(),
                source: PathBuf::from("/repo/ov"),
            },
            intent: MaterializationIntent::Directory,
            permissions: None,
        };
        let ctx = Context::builder().build();
        let action = build_action(ctx, &entry);
        assert!(format!("{action}").contains("create directory:"));
    }

    #[test]
    fn directory_intent_coexisting_with_a_checkout_is_noop_not_migrate() {
        // `collect_own_entries` unconditionally emits a `Directory` entry
        // for every overlay's own target root, regardless of whether
        // `overlay.git` *also* declares that same root as a checkout
        // (`git = "<url>"`) — a legitimate, permanent coexistence, not a
        // rule change. Misclassifying this as `Migrate` would delete the
        // checkout on every single `apply` (a real regression #129 must
        // not reintroduce).
        let td = TempDir::new().unwrap();
        init_committed_repo(td.path());
        let m = SymlinkMaterializer;
        let e = DesiredEntry {
            target: td.path().to_path_buf(),
            ..entry(MaterializationIntent::Directory)
        };
        assert!(matches!(m.classify(&e).unwrap(), Operation::Noop));
    }

    #[test]
    fn classify_clean_checkout_is_migrate_to_symlink_directory() {
        let td = TempDir::new().unwrap();
        init_committed_repo(td.path());
        let m = SymlinkMaterializer;
        let e = DesiredEntry {
            target: td.path().to_path_buf(),
            ..entry(MaterializationIntent::SymlinkDirectory {
                source: PathBuf::from("/repo/ov"),
                link_type: LinkType::Soft,
            })
        };
        match m.classify(&e).unwrap() {
            Operation::Migrate { from, to, blocked } => {
                assert!(matches!(from, MaterializationIntent::Checkout));
                assert!(matches!(to, MaterializationIntent::SymlinkDirectory { .. }));
                assert!(blocked.is_none());
            }
            other => panic!("expected Migrate, got {other:?}"),
        }
    }

    #[test]
    fn classify_dirty_checkout_is_migrate_blocked_not_conflict() {
        // A dirty checkout must never classify as `Operation::Conflict`
        // (force/no_prompt/absorb-diff would discard it) nor as a bare
        // `Noop`/`Migrate{blocked: None}` (silently invisible) — it must be
        // reported as a blocked migration.
        let td = TempDir::new().unwrap();
        init_committed_repo(td.path());
        fs::write(td.path().join("README.md"), "changed").unwrap();
        let m = SymlinkMaterializer;
        let e = DesiredEntry {
            target: td.path().to_path_buf(),
            ..entry(MaterializationIntent::SymlinkDirectory {
                source: PathBuf::from("/repo/ov"),
                link_type: LinkType::Soft,
            })
        };
        match m.classify(&e).unwrap() {
            Operation::Migrate {
                from,
                blocked: Some(reason),
                ..
            } => {
                assert!(matches!(from, MaterializationIntent::Checkout));
                assert!(reason.contains("uncommitted"));
            }
            other => panic!("expected blocked Migrate, got {other:?}"),
        }
    }

    /// Materializes a whole-repository virtual checkout at `target` and
    /// persists its XDG association — the shared setup for the
    /// `virtual_checkout_migration` tests below.
    async fn setup_virtual_checkout(source_root: &std::path::Path, target: &std::path::Path) {
        let repo = git2::Repository::open(source_root).unwrap();
        std::fs::create_dir_all(target).unwrap();
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
                managed_path: PathBuf::new(),
                base_oid,
                created_at: 0,
                last_commit_at: None,
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn classify_clean_virtual_checkout_is_migrate_to_symlink_directory() {
        let _xdg = isolate_xdg_state();
        let td = TempDir::new().unwrap();
        init_committed_repo(td.path());
        let target = td.child("checkout");
        setup_virtual_checkout(td.path(), target.path()).await;

        let m = SymlinkMaterializer;
        let e = DesiredEntry {
            target: target.path().to_path_buf(),
            provenance: Provenance::Overlay {
                overlay: "ov".to_string(),
                source: td.path().to_path_buf(),
            },
            ..entry(MaterializationIntent::SymlinkDirectory {
                source: PathBuf::from("/repo/ov"),
                link_type: LinkType::Soft,
            })
        };
        match m.classify(&e).unwrap() {
            Operation::Migrate { from, to, blocked } => {
                assert!(matches!(from, MaterializationIntent::VirtualCheckout));
                assert!(matches!(to, MaterializationIntent::SymlinkDirectory { .. }));
                assert!(blocked.is_none());
            }
            other => panic!("expected Migrate, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn classify_dirty_virtual_checkout_is_migrate_blocked_not_conflict() {
        let _xdg = isolate_xdg_state();
        let td = TempDir::new().unwrap();
        init_committed_repo(td.path());
        let target = td.child("checkout");
        setup_virtual_checkout(td.path(), target.path()).await;
        fs::write(target.path().join("README.md"), "changed").unwrap();

        let m = SymlinkMaterializer;
        let e = DesiredEntry {
            target: target.path().to_path_buf(),
            provenance: Provenance::Overlay {
                overlay: "ov".to_string(),
                source: td.path().to_path_buf(),
            },
            ..entry(MaterializationIntent::SymlinkDirectory {
                source: PathBuf::from("/repo/ov"),
                link_type: LinkType::Soft,
            })
        };
        match m.classify(&e).unwrap() {
            Operation::Migrate {
                from,
                blocked: Some(reason),
                ..
            } => {
                assert!(matches!(from, MaterializationIntent::VirtualCheckout));
                assert!(reason.contains("uncommitted"));
            }
            other => panic!("expected blocked Migrate, got {other:?}"),
        }
    }

    #[test]
    fn classify_directory_symlink_matching_source_is_migrate_to_directory() {
        let td = TempDir::new().unwrap();
        let source = td.child("source_dir");
        source.create_dir_all().unwrap();
        let target = td.path().join("target_dir");
        symlink::symlink_dir(source.path(), &target).unwrap();

        let m = SymlinkMaterializer;
        let e = DesiredEntry {
            target: target.clone(),
            provenance: Provenance::Overlay {
                overlay: "ov".to_string(),
                source: source.path().to_path_buf(),
            },
            intent: MaterializationIntent::Directory,
            permissions: None,
        };
        match m.classify(&e).unwrap() {
            Operation::Migrate { from, to, blocked } => {
                assert!(matches!(
                    from,
                    MaterializationIntent::SymlinkDirectory { .. }
                ));
                assert!(matches!(to, MaterializationIntent::Directory));
                assert!(blocked.is_none());
            }
            other => panic!("expected Migrate, got {other:?}"),
        }
    }

    #[test]
    fn classify_foreign_symlink_is_still_conflict() {
        // A symlink at the target that does *not* point at this same
        // entry's overlay source must never be treated as a migration —
        // only reconstructible, overlay-owned state is safe to collapse.
        let td = TempDir::new().unwrap();
        let foreign = td.child("foreign_dir");
        foreign.create_dir_all().unwrap();
        let target = td.path().join("target_dir");
        symlink::symlink_dir(foreign.path(), &target).unwrap();

        let m = SymlinkMaterializer;
        let e = DesiredEntry {
            target,
            provenance: Provenance::Overlay {
                overlay: "ov".to_string(),
                source: PathBuf::from("/repo/ov/somewhere-else"),
            },
            intent: MaterializationIntent::Directory,
            permissions: None,
        };
        assert!(matches!(
            m.classify(&e).unwrap(),
            Operation::Conflict { .. }
        ));
    }

    #[test]
    fn classify_symlink_mirror_directory_is_migrate_to_symlink_directory() {
        let td = TempDir::new().unwrap();
        let source = td.child("source_dir");
        source.create_dir_all().unwrap();
        source.child("a.txt").write_str("a").unwrap();
        source.child("sub").create_dir_all().unwrap();
        source.child("sub/b.txt").write_str("b").unwrap();

        // A directory previously walked file-by-file: every leaf is a
        // symlink pointing under `source`, nothing foreign.
        let target = td.path().join("target_dir");
        fs::create_dir_all(target.join("sub")).unwrap();
        symlink::symlink_file(source.path().join("a.txt"), target.join("a.txt")).unwrap();
        symlink::symlink_file(source.path().join("sub/b.txt"), target.join("sub/b.txt")).unwrap();

        let m = SymlinkMaterializer;
        let e = DesiredEntry {
            target: target.clone(),
            provenance: Provenance::Overlay {
                overlay: "ov".to_string(),
                source: source.path().to_path_buf(),
            },
            intent: MaterializationIntent::SymlinkDirectory {
                source: source.path().to_path_buf(),
                link_type: LinkType::Soft,
            },
            permissions: None,
        };
        match m.classify(&e).unwrap() {
            Operation::Migrate { from, to, blocked } => {
                assert!(matches!(from, MaterializationIntent::Directory));
                assert!(matches!(to, MaterializationIntent::SymlinkDirectory { .. }));
                assert!(blocked.is_none());
            }
            other => panic!("expected Migrate, got {other:?}"),
        }
    }

    #[test]
    fn classify_directory_with_foreign_file_is_still_conflict() {
        // A real (non-symlink) file inside the directory means it isn't a
        // pure mirror of the overlay source — never safe to auto-collapse.
        let td = TempDir::new().unwrap();
        let source = td.child("source_dir");
        source.create_dir_all().unwrap();

        let target = td.path().join("target_dir");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("foreign.txt"), "not from the overlay").unwrap();

        let m = SymlinkMaterializer;
        let e = DesiredEntry {
            target: target.clone(),
            provenance: Provenance::Overlay {
                overlay: "ov".to_string(),
                source: source.path().to_path_buf(),
            },
            intent: MaterializationIntent::SymlinkDirectory {
                source: source.path().to_path_buf(),
                link_type: LinkType::Soft,
            },
            permissions: None,
        };
        assert!(matches!(
            m.classify(&e).unwrap(),
            Operation::Conflict { .. }
        ));
    }

    #[tokio::test]
    async fn materialize_migrate_checkout_to_symlink_replaces_directory() {
        let td = TempDir::new().unwrap();
        let checkout_dir = td.child("checkout");
        checkout_dir.create_dir_all().unwrap();
        init_committed_repo(checkout_dir.path());

        let source = td.child("elsewhere");
        source.create_dir_all().unwrap();

        let m = SymlinkMaterializer;
        let target = checkout_dir.path().to_path_buf();
        let e = DesiredEntry {
            target: target.clone(),
            provenance: Provenance::Overlay {
                overlay: "ov".to_string(),
                source: source.path().to_path_buf(),
            },
            intent: MaterializationIntent::SymlinkDirectory {
                source: source.path().to_path_buf(),
                link_type: LinkType::Soft,
            },
            permissions: None,
        };
        let step = PlanStep {
            entry: e,
            operation: Operation::Migrate {
                from: MaterializationIntent::Checkout,
                to: MaterializationIntent::SymlinkDirectory {
                    source: source.path().to_path_buf(),
                    link_type: LinkType::Soft,
                },
                blocked: None,
            },
        };
        let ctx = Context::builder().build();
        m.materialize(ctx, &step).await.unwrap();

        assert!(
            target.is_symlink(),
            "checkout should be gone, replaced by a symlink"
        );
        assert_eq!(fs::read_link(&target).unwrap(), source.path());
    }

    #[tokio::test]
    async fn materialize_migrate_virtual_checkout_to_symlink_replaces_directory_and_drops_state() {
        let _xdg = isolate_xdg_state();
        let td = TempDir::new().unwrap();
        init_committed_repo(td.path());
        let checkout_dir = td.child("checkout");
        setup_virtual_checkout(td.path(), checkout_dir.path()).await;

        let source = td.child("elsewhere");
        source.create_dir_all().unwrap();

        let m = SymlinkMaterializer;
        let target = checkout_dir.path().to_path_buf();
        let e = DesiredEntry {
            target: target.clone(),
            // The real, discoverable source repository — needed for
            // `status::virtual_checkout::inspect`'s safety re-check right
            // before deleting; independent of where the *new* symlink
            // below points.
            provenance: Provenance::Overlay {
                overlay: "ov".to_string(),
                source: td.path().to_path_buf(),
            },
            intent: MaterializationIntent::SymlinkDirectory {
                source: source.path().to_path_buf(),
                link_type: LinkType::Soft,
            },
            permissions: None,
        };
        let step = PlanStep {
            entry: e,
            operation: Operation::Migrate {
                from: MaterializationIntent::VirtualCheckout,
                to: MaterializationIntent::SymlinkDirectory {
                    source: source.path().to_path_buf(),
                    link_type: LinkType::Soft,
                },
                blocked: None,
            },
        };
        let ctx = Context::builder().build();
        m.materialize(ctx, &step).await.unwrap();

        assert!(
            target.is_symlink(),
            "virtual checkout should be gone, replaced by a symlink"
        );
        assert_eq!(fs::read_link(&target).unwrap(), source.path());
        // The XDG association must be dropped too, so a virtual checkout
        // later re-materialized at the same path never gets misattributed
        // to this stale `base_oid`.
        assert!(
            crate::materialize::virtual_checkout::state::record_for_blocking(&target)
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn materialize_migrate_dirty_virtual_checkout_refuses_even_if_invoked_directly() {
        let _xdg = isolate_xdg_state();
        // `Plan::execute`'s `is_actionable` never calls `materialize` for a
        // blocked migration, but `materialize_migration` re-verifies
        // safety right before deleting regardless — classify -> execute
        // isn't atomic, and dirty virtual-checkout content must never be
        // discarded even if this were ever invoked directly.
        let td = TempDir::new().unwrap();
        init_committed_repo(td.path());
        let checkout_dir = td.child("checkout");
        setup_virtual_checkout(td.path(), checkout_dir.path()).await;
        fs::write(checkout_dir.path().join("README.md"), "changed").unwrap();

        let symlink_target = PathBuf::from("/repo/ov/somewhere");
        let m = SymlinkMaterializer;
        let target = checkout_dir.path().to_path_buf();
        let e = DesiredEntry {
            target: target.clone(),
            provenance: Provenance::Overlay {
                overlay: "ov".to_string(),
                source: td.path().to_path_buf(),
            },
            intent: MaterializationIntent::SymlinkDirectory {
                source: symlink_target.clone(),
                link_type: LinkType::Soft,
            },
            permissions: None,
        };
        let step = PlanStep {
            entry: e,
            operation: Operation::Migrate {
                from: MaterializationIntent::VirtualCheckout,
                to: MaterializationIntent::SymlinkDirectory {
                    source: symlink_target,
                    link_type: LinkType::Soft,
                },
                blocked: None,
            },
        };
        let ctx = Context::builder().build();
        let result = m.materialize(ctx, &step).await;

        assert!(result.is_err());
        assert_eq!(
            fs::read_to_string(target.join("README.md")).unwrap(),
            "changed",
            "uncommitted change must remain"
        );
    }

    #[tokio::test]
    async fn materialize_migrate_blocked_never_invoked_is_defensive_noop() {
        // Even if something ever called `materialize` directly for a
        // blocked migration (bypassing `Plan::execute`'s `is_actionable`
        // filter), and even with `--force`, the dirty checkout must remain
        // untouched — never silently discarded.
        let td = TempDir::new().unwrap();
        init_committed_repo(td.path());
        fs::write(td.path().join("README.md"), "changed").unwrap();

        let m = SymlinkMaterializer;
        let target = td.path().to_path_buf();
        let source = PathBuf::from("/repo/ov/somewhere");
        let e = DesiredEntry {
            target: target.clone(),
            provenance: Provenance::Overlay {
                overlay: "ov".to_string(),
                source: source.clone(),
            },
            intent: MaterializationIntent::SymlinkDirectory {
                source: source.clone(),
                link_type: LinkType::Soft,
            },
            permissions: None,
        };
        let step = PlanStep {
            entry: e,
            operation: Operation::Migrate {
                from: MaterializationIntent::Checkout,
                to: MaterializationIntent::SymlinkDirectory {
                    source,
                    link_type: LinkType::Soft,
                },
                blocked: Some("has uncommitted changes".to_string()),
            },
        };
        let ctx = Context::builder().force(true).build();
        m.materialize(ctx, &step).await.unwrap();

        assert!(
            target.join(".git").exists(),
            "checkout must remain a git repo"
        );
        assert_eq!(
            fs::read_to_string(target.join("README.md")).unwrap(),
            "changed",
            "uncommitted change must remain"
        );
    }

    #[test]
    fn build_action_symlink_sidecar_uses_ensure_symlink() {
        let entry = DesiredEntry {
            target: PathBuf::from("/tmp/link"),
            provenance: Provenance::SymlinkSidecar {
                overlay: "ov".to_string(),
                config: PathBuf::from("/repo/ov/x.link.toml"),
                template: "/opt/x".to_string(),
                resolved: PathBuf::from("/opt/x"),
            },
            intent: MaterializationIntent::SymlinkFile {
                source: PathBuf::from("/opt/x"),
                link_type: LinkType::Soft,
            },
            permissions: None,
        };
        let ctx = Context::builder().build();
        let action = build_action(ctx, &entry);
        assert!(format!("{action}").contains("symlink"));
    }
}
