//! Status for [`MaterializationIntent::VirtualCheckout`](crate::desired::MaterializationIntent::VirtualCheckout)
//! entries (#141): computed entirely from blob hashes against the
//! overlay's own source repository — there's no real git index at the
//! target to reuse `status::git`'s `repo.statuses()`-based approach for.
//!
//! Three states are compared per managed file: `base` (the tree recorded
//! at the checkout's last-known `base_oid`), `local` (the target's current
//! on-disk content), and `source` (the source repository's current
//! `HEAD`). This is a 3-way comparison, not a 2-way diff: a file that
//! changed in `local` *and* in `source` since `base`, to different
//! content, is a per-file [`super::Status::Conflict`] — never silently
//! resolved in either direction (AGENTS.md: never discard uncommitted
//! target changes implicitly, never overwrite a dirty/conflicting file).

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Result;
use git2::Oid;

use crate::desired::{DesiredEntry, Provenance};
use crate::materialize::virtual_checkout::git::{self, FileChange, FileChangeKind};
use crate::materialize::virtual_checkout::state;

use super::Status;

/// Aggregate [`Status`] for a virtual checkout entry — see the module doc
/// for the 3-way comparison this reduces to a single value from.
pub(crate) fn inspect(entry: &DesiredEntry) -> Result<Status> {
    Ok(inspect_with_details(entry, false, &mut Snapshots::default())?.status)
}

#[derive(Clone)]
pub(crate) struct Inspection {
    pub status: Status,
    pub staged: Vec<FileChange>,
    pub unstaged: Vec<FileChange>,
    pub unmerged: Vec<PathBuf>,
}

#[derive(Default)]
pub(crate) struct Snapshots {
    indices: BTreeMap<PathBuf, git2::Index>,
    state: Option<state::VirtualCheckoutState>,
}

pub(crate) fn inspect_tracked_cached(
    entry: &DesiredEntry,
    snapshots: &mut Snapshots,
) -> Result<Inspection> {
    // Destructive/sync callers retain broader discovery in inspect; reporting
    // must never make an untracked target addition look safe to remove.
    inspect_with_details(entry, true, snapshots)
}

fn inspect_with_details(
    entry: &DesiredEntry,
    tracked_only: bool,
    snapshots: &mut Snapshots,
) -> Result<Inspection> {
    if snapshots.state.is_none() {
        snapshots.state = Some(state::state_file()?.inspect_blocking()?);
    }
    if tracked_only {
        let snapshot = snapshots
            .state
            .as_ref()
            .expect("state snapshot initialized above");
        state::verify_live_target_snapshot(snapshot, entry)?;
        let actual = crate::plan::actual::inspect(&entry.target)?;
        let status = match actual {
            crate::plan::actual::ActualState::Missing => Some(Status::Missing),
            crate::plan::actual::ActualState::Directory => None,
            _ => Some(Status::Conflict),
        };
        if let Some(status) = status {
            return Ok(Inspection {
                status,
                staged: Vec::new(),
                unstaged: Vec::new(),
                unmerged: Vec::new(),
            });
        }
    }
    let Some(record) = snapshots
        .state
        .as_ref()
        .and_then(|state| state.checkouts.get(&state::key_for(&entry.target)))
    else {
        // No recorded association: `VirtualCheckoutMaterializer::classify`
        // would have produced `Create`/`Migrate` for this, never `Noop` —
        // reachable here only if the target was removed/state lost after
        // classification (a race, or manual XDG state deletion). Mirror
        // `status::git::inspect_checkout`'s own `Missing`/`Broken` split.
        return Ok(Inspection {
            status: if entry.target.exists() {
                Status::Broken
            } else {
                Status::Missing
            },
            staged: Vec::new(),
            unstaged: Vec::new(),
            unmerged: Vec::new(),
        });
    };

    let Provenance::Overlay { source, .. } = &entry.provenance else {
        unreachable!(
            "VirtualCheckout entries only ever carry Provenance::Overlay \
             (see desired::tree::{{collect_own_entries,walk_overlay_tree}})"
        );
    };

    state::verify(entry, record)?;
    let scope = entry.intent.checkout_scope();
    let (repo, managed_path) = git::discover_source(source)?;
    let key = repo.path().to_path_buf();
    if !snapshots.indices.contains_key(&key) {
        snapshots.indices.insert(key.clone(), repo.index()?);
    }
    let index_snapshot = &snapshots.indices[&key];
    let mut unmerged = std::collections::BTreeSet::new();
    for indexed in index_snapshot.iter() {
        // Conflict stages belong to one Git path, not three staged file snapshots.
        if indexed.flags & 0x3000 != 0
            && let Some(relative) = git::relative_index_path(&indexed.path, &managed_path)?
            && scope.owns_source(&relative)
            && scope.owns_target(&relative)
        {
            unmerged.insert(relative.to_path_buf());
        }
    }

    let base_tree = git::base_tree(&repo, Some(&record.base_oid))?;
    let base_entries =
        git::scoped_entries_with_index(&repo, &base_tree, &managed_path, &scope, index_snapshot)?;
    let base_blobs = blob_ids(&base_entries);

    let head_tree = git::base_tree(&repo, None)?;
    let head_entries = if head_tree.id() == base_tree.id() {
        base_entries.clone()
    } else {
        git::scoped_entries_with_index(&repo, &head_tree, &managed_path, &scope, index_snapshot)?
    };
    let head_blobs = blob_ids(&head_entries);

    let mut converged = false;
    let (local_changes, unstaged) = if tracked_only {
        let index = indexed_entries_from(index_snapshot, &managed_path, &scope, false)?;
        // Reserved paths select target files for inspection but supply no staged baseline.
        let owned = indexed_entries_from(index_snapshot, &managed_path, &scope, true)?;
        // The union preserves staged additions and historical deletions without
        // traversing unrelated working trees nested beneath the checkout.
        let local = git::tracked_target_entries(
            &repo,
            &managed_path,
            &entry.target,
            &[&base_entries, &head_entries, &owned],
        )?;
        // Local edits are Git changes; only source updates still absent from the target need reconciliation.
        converged = head_entries.iter().all(|(path, value)| {
            base_entries.get(path) == Some(value) || local.get(path) == Some(value)
        }) && base_entries
            .keys()
            .all(|path| head_entries.contains_key(path) || !local.contains_key(path));
        (
            git::entry_changes(&base_entries, &local),
            git::entry_changes(&index, &local),
        )
    } else {
        (
            git::diff_canonical_target(&repo, &managed_path, &entry.target, &base_entries, &scope)?,
            Vec::new(),
        )
    };

    // Include executable-bit drift, as the previous filtered-tree comparison did.
    let source_drifted = head_entries != base_entries;

    let staged = if tracked_only {
        let index = indexed_entries_from(index_snapshot, &managed_path, &scope, false)?;
        git::entry_changes(&head_entries, &index)
    } else {
        git::staged_changes(&repo, &managed_path, &scope)?
    };
    let status = if tracked_only && !unmerged.is_empty() {
        Status::Conflict
    } else if tracked_only && (!source_drifted || converged) {
        Status::Applied
    } else if tracked_only && conflicting_paths(&base_blobs, &head_blobs, &local_changes).is_empty()
    {
        let base_commit = repo.find_commit(Oid::from_str(&record.base_oid)?)?;
        let head_commit = repo.head()?.peel_to_commit()?;
        Status::Behind(git::commits_between(
            &repo,
            base_commit.id(),
            head_commit.id(),
        )?)
    } else if tracked_only {
        Status::Conflict
    } else {
        match (
            local_changes.is_empty() && staged.is_empty(),
            source_drifted,
        ) {
            (true, false) => Status::Applied,
            (false, false) => Status::Modified,
            (true, true) => {
                // `commits_between` needs commits, not trees, to walk history.
                let base_commit = repo.find_commit(Oid::from_str(&record.base_oid)?)?;
                let head_commit = repo.head()?.peel_to_commit()?;
                Status::Behind(git::commits_between(
                    &repo,
                    base_commit.id(),
                    head_commit.id(),
                )?)
            }
            (false, true) => {
                if conflicting_paths(&base_blobs, &head_blobs, &local_changes).is_empty() {
                    let base_commit = repo.find_commit(Oid::from_str(&record.base_oid)?)?;
                    let head_commit = repo.head()?.peel_to_commit()?;
                    Status::Diverged {
                        ahead: local_changes.len(),
                        behind: git::commits_between(&repo, base_commit.id(), head_commit.id())?,
                    }
                } else {
                    Status::Conflict
                }
            }
        }
    };
    Ok(Inspection {
        status,
        staged,
        unstaged,
        unmerged: unmerged.into_iter().collect(),
    })
}

/// Paths that changed both locally and in the source repository since
/// `base_oid`, to different content. Detection is per-file, but `over commit`
/// blocks the whole command when any selected checkout has conflicting paths,
/// preventing a partial commit across the selection. Empty whenever [`inspect`]
/// wouldn't report [`Status::Conflict`].
#[cfg(test)]
fn conflicts(entry: &DesiredEntry) -> Result<Vec<PathBuf>> {
    let Some(record) = state::record_for_blocking(&entry.target)? else {
        return Ok(Vec::new());
    };
    let Provenance::Overlay { source, .. } = &entry.provenance else {
        unreachable!(
            "VirtualCheckout entries only ever carry Provenance::Overlay \
             (see desired::tree::{{collect_own_entries,walk_overlay_tree}})"
        );
    };
    state::verify(entry, &record)?;
    let scope = entry.intent.checkout_scope();
    let (repo, managed_path) = git::discover_source(source)?;

    let base_tree = git::base_tree(&repo, Some(&record.base_oid))?;
    let base_entries = git::scoped_entries(&repo, &base_tree, &managed_path, &scope)?;
    let base_blobs = blob_ids(&base_entries);
    let local_changes =
        git::diff_canonical_target(&repo, &managed_path, &entry.target, &base_entries, &scope)?;

    let head_tree = git::base_tree(&repo, None)?;
    let head_blobs = blob_ids(&git::scoped_entries(
        &repo,
        &head_tree,
        &managed_path,
        &scope,
    )?);

    Ok(conflicting_paths(&base_blobs, &head_blobs, &local_changes))
}

/// Commit only needs known selected paths; reconciliation's untracked walk is unnecessary.
pub(crate) fn commit_conflicts(entry: &DesiredEntry, paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let Some(record) = state::record_for_blocking(&entry.target)? else {
        return Ok(Vec::new());
    };
    let Provenance::Overlay { source, .. } = &entry.provenance else {
        unreachable!()
    };
    let (repo, managed) = git::discover_source(source)?;
    let scope = entry.intent.checkout_scope();
    let base = git::base_tree(&repo, Some(&record.base_oid))?;
    let head = git::base_tree(&repo, None)?;
    let mut base_entries = git::scoped_entries(&repo, &base, &managed, &scope)?;
    let mut head_entries = git::scoped_entries(&repo, &head, &managed, &scope)?;
    base_entries.retain(|path, _| crate::commit::selects(entry, path, paths));
    head_entries.retain(|path, _| crate::commit::selects(entry, path, paths));
    // When source has not moved, no target file can be a three-way conflict.
    if base_entries == head_entries {
        return Ok(Vec::new());
    }
    state::verify(entry, &record)?;
    let target = git::tracked_target_entries(
        &repo,
        &managed,
        &entry.target,
        &[&base_entries, &head_entries],
    )?;
    Ok(conflicting_paths(
        &blob_ids(&base_entries),
        &blob_ids(&head_entries),
        &git::entry_changes(&base_entries, &target),
    ))
}

/// Per-file changes between a virtual checkout's `base_oid` and the
/// target's current on-disk content, retained as a test helper for base-relative
/// comparisons. Command reports distinguish staged and unstaged changes instead;
/// `over add` stages selected snapshots and `over commit` records only staged
/// changes. Empty for a checkout with no recorded association (mirrors
/// [`inspect`]'s own `Missing`/`Broken` fallback).
#[cfg(test)]
pub(crate) fn file_changes(entry: &DesiredEntry) -> Result<Vec<FileChange>> {
    let Some(record) = state::record_for_blocking(&entry.target)? else {
        return Ok(Vec::new());
    };
    let Provenance::Overlay { source, .. } = &entry.provenance else {
        unreachable!(
            "VirtualCheckout entries only ever carry Provenance::Overlay \
             (see desired::tree::{{collect_own_entries,walk_overlay_tree}})"
        );
    };
    state::verify(entry, &record)?;
    let scope = entry.intent.checkout_scope();
    let (repo, managed_path) = git::discover_source(source)?;
    let base_tree = git::base_tree(&repo, Some(&record.base_oid))?;
    let base_entries = git::scoped_entries(&repo, &base_tree, &managed_path, &scope)?;
    git::diff_canonical_target(&repo, &managed_path, &entry.target, &base_entries, &scope)
}

pub(crate) fn staged_changes(entry: &DesiredEntry) -> Result<Vec<FileChange>> {
    let Provenance::Overlay { source, .. } = &entry.provenance else {
        unreachable!()
    };
    let (repo, managed) = git::discover_source(source)?;
    git::staged_changes(&repo, &managed, &entry.intent.checkout_scope())
}

fn blob_ids(entries: &BTreeMap<PathBuf, (Oid, i32)>) -> BTreeMap<PathBuf, Oid> {
    entries
        .iter()
        .map(|(path, (oid, _))| (path.clone(), *oid))
        .collect()
}

pub(crate) fn unstaged_changes(entry: &DesiredEntry) -> Result<Vec<FileChange>> {
    let Provenance::Overlay { source, .. } = &entry.provenance else {
        unreachable!()
    };
    let (repo, managed) = git::discover_source(source)?;
    let scope = entry.intent.checkout_scope();
    let blobs = indexed_entries(&repo, &managed, &scope, false)?;
    git::diff_canonical_target(&repo, &managed, &entry.target, &blobs, &scope)
}

fn indexed_entries(
    repo: &git2::Repository,
    managed: &std::path::Path,
    scope: &crate::desired::CheckoutScope,
    include_intent_to_add: bool,
) -> Result<BTreeMap<PathBuf, (Oid, i32)>> {
    indexed_entries_from(&repo.index()?, managed, scope, include_intent_to_add)
}

fn indexed_entries_from(
    index: &git2::Index,
    managed: &std::path::Path,
    scope: &crate::desired::CheckoutScope,
    include_intent_to_add: bool,
) -> Result<BTreeMap<PathBuf, (Oid, i32)>> {
    let mut blobs = BTreeMap::new();
    for indexed in index.iter() {
        if !include_intent_to_add && indexed.flags & 0x3000 != 0 {
            continue;
        }
        // Match diff's unstaged-new-file view instead of treating the placeholder as staged.
        if !include_intent_to_add
            && git2::IndexEntryExtendedFlag::from_bits_truncate(indexed.flags_extended)
                .is_intent_to_add()
        {
            continue;
        }
        if let Some(relative) = git::relative_index_path(&indexed.path, managed)?
            && scope.owns_source(&relative)
            && scope.owns_target(&relative)
            && !git::is_overlay_metadata(&relative)
            && git::is_supported_mode(indexed.mode)
        {
            blobs.insert(relative.to_path_buf(), (indexed.id, indexed.mode as i32));
        }
    }
    Ok(blobs)
}

/// Paths that changed both locally (since `base`) and in the source
/// repository (`base` -> `head`), to genuinely different content — the
/// only case that's unsafe to reconcile automatically in either
/// direction. A path where the local edit happens to already match
/// exactly what the source moved to (both sides independently converged)
/// is *not* a conflict — there's nothing to lose by treating it as
/// already reconciled, and refusing to commit it would leave the
/// checkout permanently stuck with no way to clear the flag short of
/// re-adopting from scratch.
fn conflicting_paths(
    base_blobs: &BTreeMap<PathBuf, Oid>,
    head_blobs: &BTreeMap<PathBuf, Oid>,
    local_changes: &[FileChange],
) -> Vec<PathBuf> {
    local_changes
        .iter()
        .filter(|change| match change.kind {
            // Source-side either doesn't have this path at all (never
            // conflicting: nothing to reconcile against) or moved it away
            // from `base` — any divergence from `base` on the source side
            // for a path `local` also touched is unsafe to auto-resolve,
            // unless `local` already matches exactly what `head` moved to.
            FileChangeKind::Modified | FileChangeKind::Added | FileChangeKind::Deleted => {
                // A source deletion is a change too; treating a missing HEAD
                // blob as "no conflict" would lose a simultaneous local edit.
                let head = head_blobs.get(&change.path).copied();
                base_blobs.get(&change.path).copied() != head && change.local_oid != head
            }
        })
        .map(|change| change.path.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desired::MaterializationIntent;
    use crate::materialize::virtual_checkout::state::VirtualCheckoutRecord;
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    use git2::Signature;
    use std::fs;

    /// Points `$XDG_STATE_HOME` at a fresh, writable temp dir for the
    /// duration of the returned guard's lifetime — `inspect` reads (and
    /// `materialize_and_record` below writes) `VirtualCheckoutState` via
    /// the real, non-injectable `XdgDirs::new()`, so every test here must
    /// isolate this or it silently pollutes the real
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

    fn init_committed_repo(path: &std::path::Path) -> git2::Repository {
        let repo = git2::Repository::init(path).unwrap();
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "Test").unwrap();
        cfg.set_str("user.email", "test@test.com").unwrap();
        drop(cfg);
        let sig = Signature::now("Test", "test@test.com").unwrap();
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

    fn commit_all(repo: &git2::Repository, message: &str) {
        let sig = Signature::now("Test", "test@test.com").unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let parent = repo.head().unwrap().peel_to_commit().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &[&parent])
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

    async fn materialize_and_record(source_root: &std::path::Path, target: &std::path::Path) {
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
        state::persist(
            target,
            VirtualCheckoutRecord {
                overlay: "ov".to_string(),
                managed_path: PathBuf::new(),
                base_oid,
                created_at: 0,
                last_commit_at: None,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }

    #[test]
    fn missing_target_with_no_record_is_missing() {
        let _xdg = isolate_xdg_state();
        let td = TempDir::new().unwrap();
        let e = entry(td.path().join("nope"), td.path().to_path_buf());
        assert_eq!(inspect(&e).unwrap(), Status::Missing);
    }

    #[tokio::test]
    async fn clean_checkout_is_applied() {
        let _xdg = isolate_xdg_state();
        let td = TempDir::new().unwrap();
        td.child("a.txt").write_str("a").unwrap();
        init_committed_repo(td.path());
        let target = td.child("target");
        materialize_and_record(td.path(), target.path()).await;

        let e = entry(target.path().to_path_buf(), td.path().to_path_buf());
        assert_eq!(inspect(&e).unwrap(), Status::Applied);
        assert!(file_changes(&e).unwrap().is_empty());
    }

    #[tokio::test]
    async fn locally_modified_file_is_modified() {
        let _xdg = isolate_xdg_state();
        let td = TempDir::new().unwrap();
        td.child("a.txt").write_str("a").unwrap();
        init_committed_repo(td.path());
        let target = td.child("target");
        materialize_and_record(td.path(), target.path()).await;

        fs::write(target.path().join("a.txt"), "changed").unwrap();

        let e = entry(target.path().to_path_buf(), td.path().to_path_buf());
        assert_eq!(inspect(&e).unwrap(), Status::Modified);
        let changes = file_changes(&e).unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].kind, FileChangeKind::Modified);
    }

    #[tokio::test]
    async fn source_only_drift_is_behind() {
        let _xdg = isolate_xdg_state();
        let td = TempDir::new().unwrap();
        td.child("a.txt").write_str("a").unwrap();
        let repo = init_committed_repo(td.path());
        let target = td.child("target");
        materialize_and_record(td.path(), target.path()).await;

        fs::write(td.path().join("a.txt"), "advanced").unwrap();
        commit_all(&repo, "advance");

        let e = entry(target.path().to_path_buf(), td.path().to_path_buf());
        assert_eq!(inspect(&e).unwrap(), Status::Behind(1));
    }

    #[tokio::test]
    async fn non_overlapping_local_and_source_changes_are_diverged() {
        let _xdg = isolate_xdg_state();
        let td = TempDir::new().unwrap();
        td.child("a.txt").write_str("a").unwrap();
        td.child("b.txt").write_str("b").unwrap();
        let repo = init_committed_repo(td.path());
        let target = td.child("target");
        materialize_and_record(td.path(), target.path()).await;

        // Local change to a.txt; source-side change to a *different* file.
        fs::write(target.path().join("a.txt"), "local-change").unwrap();
        fs::write(td.path().join("b.txt"), "source-change").unwrap();
        commit_all(&repo, "advance b only");

        let e = entry(target.path().to_path_buf(), td.path().to_path_buf());
        assert!(matches!(inspect(&e).unwrap(), Status::Diverged { .. }));
    }

    #[tokio::test]
    async fn same_file_changed_differently_on_both_sides_is_conflict() {
        let _xdg = isolate_xdg_state();
        let td = TempDir::new().unwrap();
        td.child("a.txt").write_str("a").unwrap();
        let repo = init_committed_repo(td.path());
        let target = td.child("target");
        materialize_and_record(td.path(), target.path()).await;

        fs::write(target.path().join("a.txt"), "local-change").unwrap();
        fs::write(td.path().join("a.txt"), "source-change").unwrap();
        commit_all(&repo, "advance a differently");

        let e = entry(target.path().to_path_buf(), td.path().to_path_buf());
        assert_eq!(inspect(&e).unwrap(), Status::Conflict);
    }

    #[tokio::test]
    async fn local_edit_converging_on_the_same_content_as_source_is_not_a_conflict() {
        let _xdg = isolate_xdg_state();
        // Both sides independently changed a.txt to the exact same new
        // content — nothing would actually be lost by treating this as
        // reconciled, unlike a genuine conflict.
        let td = TempDir::new().unwrap();
        td.child("a.txt").write_str("a").unwrap();
        let repo = init_committed_repo(td.path());
        let target = td.child("target");
        materialize_and_record(td.path(), target.path()).await;

        fs::write(target.path().join("a.txt"), "converged\n").unwrap();
        fs::write(td.path().join("a.txt"), "converged\n").unwrap();
        commit_all(&repo, "advance to the same content");

        let e = entry(target.path().to_path_buf(), td.path().to_path_buf());
        assert!(conflicts(&e).unwrap().is_empty());
        assert!(matches!(inspect(&e).unwrap(), Status::Diverged { .. }));
    }

    #[tokio::test]
    async fn added_local_file_is_modified_not_conflict() {
        let _xdg = isolate_xdg_state();
        let td = TempDir::new().unwrap();
        td.child("a.txt").write_str("a").unwrap();
        init_committed_repo(td.path());
        let target = td.child("target");
        materialize_and_record(td.path(), target.path()).await;

        fs::write(target.path().join("new.txt"), "new").unwrap();

        let e = entry(target.path().to_path_buf(), td.path().to_path_buf());
        assert_eq!(inspect(&e).unwrap(), Status::Modified);
    }
}
