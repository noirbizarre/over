//! Stage virtual-checkout content in the source index without moving targets.

pub(crate) mod patch;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, ensure};

use crate::actions::git::diff::Scratch;
use crate::desired::{DesiredEntry, DesiredTree, Provenance};
use crate::exec::Ctx;
use crate::materialize::virtual_checkout::{git, snapshot, state};
use crate::overlays::Overlay;

pub(crate) fn owns(entry: &DesiredEntry, path: &Path) -> bool {
    path.strip_prefix(&entry.target)
        .is_ok_and(|relative| match &entry.intent {
            crate::desired::MaterializationIntent::ScopedVirtualCheckout(scope) => {
                scope.owns_target_kind(relative, path.is_dir())
            }
            _ => true,
        })
}

fn projection_boundary(entry: &DesiredEntry, path: &Path) -> bool {
    let crate::desired::MaterializationIntent::ScopedVirtualCheckout(scope) = &entry.intent else {
        return false;
    };
    scope.projection.is_some()
        && path.strip_prefix(&entry.target).is_ok_and(|relative| {
            scope.owns_source_kind(relative, path.is_dir())
                && !scope
                    .target_excludes
                    .iter()
                    .any(|root| relative.starts_with(root))
        })
}

/// Preflight all checkout/index changes before performing ingestion actions.
/// Returns the selections still requiring ordinary move-and-link ingestion.
pub async fn add(ctx: &Ctx, overlay: &Overlay, selections: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let ctx = ctx.clone();
    let overlay = overlay.clone();
    let selections = selections.to_vec();
    tokio::task::spawn_blocking(move || add_blocking(&ctx, &overlay, &selections)).await?
}

pub(crate) fn add_blocking(
    ctx: &Ctx,
    overlay: &Overlay,
    selections: &[PathBuf],
) -> Result<Vec<PathBuf>> {
    let has_checkouts = DesiredTree::build_own(ctx, overlay)?
        .entries()
        .iter()
        .any(|entry| entry.intent.is_virtual_checkout());
    let _ownership = if ctx.dry_run || !has_checkouts {
        None
    } else {
        Some(state::mutation_guard()?)
    };
    add_blocking_with(ctx, overlay, selections, false)
}

pub(crate) fn preflight_crypt(
    ctx: &Ctx,
    overlay: &Overlay,
    selections: &[PathBuf],
) -> Result<Vec<PathBuf>> {
    add_blocking_with(ctx, overlay, selections, true)
}

fn add_blocking_with(
    ctx: &Ctx,
    overlay: &Overlay,
    selections: &[PathBuf],
    preserve_staged: bool,
) -> Result<Vec<PathBuf>> {
    crate::secrets::validate_ingestion(ctx, overlay, selections)?;
    crate::desired::content::validate_ingestion(ctx, overlay, selections)?;
    let tree = DesiredTree::build_own(ctx, overlay)?;
    let mut entries: Vec<_> = tree
        .entries()
        .iter()
        .filter(|entry| entry.intent.is_virtual_checkout())
        .cloned()
        .collect();
    let routing = overlay.resolve_mappings(ctx)?;
    let policy = crate::overlays::ignore::SourcePolicy::new(&ctx.repository.root, overlay)?;
    let selections = selections
        .iter()
        .map(|path| {
            routing
                .logical_path_for(path)
                .map(|logical| routing.target_for(&logical))
        })
        .collect::<Result<Vec<_>>>()?;
    acquire_paths(ctx, overlay, &selections, &mut entries)?;
    let mut remaining = BTreeSet::new();
    for path in &selections {
        let logical = routing.logical_path_for(path)?;
        ensure!(
            policy.includes(&overlay.root.join(&logical), path.is_dir()),
            "'{}' is ignored or is control metadata",
            path.display()
        );
        if path.is_dir() && overlay.is_link_dir(&logical) {
            policy.validate_directory_link(path, &overlay.root.join(&logical))?;
        }
        if !path.is_dir() && entries.iter().any(|entry| projection_boundary(entry, path)) {
            // Shared-container operands are consumed here even when they have no owned leaves.
            ensure!(
                path.is_dir() || entries.iter().any(|entry| owns(entry, path)),
                "'{}' is not owned by this overlay's checkout; select its owner explicitly",
                path.display()
            );
            continue;
        }
        if !entries.iter().any(|entry| {
            owns(entry, path) || entry.target.starts_with(path) || projection_boundary(entry, path)
        }) {
            ensure!(
                path.symlink_metadata().is_ok(),
                "{} does not exist",
                path.display()
            );
            remaining.insert(path.clone());
        } else if path.is_dir() {
            for item in walkdir::WalkDir::new(path)
                .min_depth(1)
                .into_iter()
                .filter_entry(|item| {
                    !item.path().starts_with(&overlay.root)
                        && routing.logical_path_for(item.path()).is_ok_and(|logical| {
                            policy.includes(&overlay.root.join(logical), item.file_type().is_dir())
                        })
                })
            {
                let item = item?;
                if item.file_type().is_file()
                    && !entries.iter().any(|entry| {
                        owns(entry, item.path()) || projection_boundary(entry, item.path())
                    })
                    && routing
                        .logical_path_for(item.path())
                        .is_ok_and(|logical| policy.includes(&overlay.root.join(logical), false))
                {
                    remaining.insert(item.into_path());
                }
            }
        }
    }
    stage_entries(&entries, &selections, true, overlay, preserve_staged)?;
    if !ctx.dry_run {
        // Reserve after preflight but before index publication; reset cannot transfer ownership.
        for entry in &entries {
            state::reserve(entry)?;
        }
        stage_entries(&entries, &selections, false, overlay, preserve_staged)?;
    }
    Ok(remaining.into_iter().collect())
}

/// Admission is shared by full and patch staging; selection alone is not ownership.
pub(crate) fn reserve_inputs(ctx: &Ctx, overlay: &Overlay, selections: &[PathBuf]) -> Result<()> {
    let tree = DesiredTree::build_own(ctx, overlay)?;
    let mut entries: Vec<_> = tree
        .entries()
        .iter()
        .filter(|entry| entry.intent.is_virtual_checkout())
        .cloned()
        .collect();
    acquire_paths(ctx, overlay, selections, &mut entries)?;
    for entry in &entries {
        state::reserve(entry)?;
    }
    Ok(())
}

/// Traversal permission and concrete leaf ownership differ in shared directories.
pub(crate) fn acquire_paths(
    ctx: &Ctx,
    overlay: &Overlay,
    selections: &[PathBuf],
    entries: &mut [DesiredEntry],
) -> Result<()> {
    let routing = overlay.resolve_mappings(ctx)?;
    let policy = crate::overlays::ignore::SourcePolicy::new(&ctx.repository.root, overlay)?;
    for selection in selections {
        let recursive = selection.is_dir();
        let candidates = if recursive {
            let mut paths = Vec::new();
            for item in walkdir::WalkDir::new(selection)
                .min_depth(1)
                .into_iter()
                .filter_entry(|item| {
                    !item.file_type().is_symlink()
                        && !item.path().starts_with(&overlay.root)
                        && routing.logical_path_for(item.path()).is_ok_and(|logical| {
                            policy.includes(&overlay.root.join(logical), item.file_type().is_dir())
                        })
                })
            {
                let item = item?;
                if item.file_type().is_file() {
                    paths.push(item.into_path());
                }
            }
            paths
        } else {
            vec![selection.clone()]
        };
        for path in candidates {
            if entries.iter().any(|entry| owns(entry, &path)) {
                continue;
            }
            let Some(entry) = entries
                .iter_mut()
                .filter(|entry| {
                    path.starts_with(&entry.target)
                        && entry.intent.checkout_scope().projection.is_some()
                })
                .max_by_key(|entry| entry.target.components().count())
            else {
                continue;
            };
            let relative = path.strip_prefix(&entry.target)?.to_path_buf();
            let mut scope = entry.intent.checkout_scope();
            if !scope.owns_source(&relative)
                || scope
                    .target_excludes
                    .iter()
                    .any(|root| relative.starts_with(root))
            {
                continue;
            }
            let mut claim = entry.clone();
            let mut claim_scope = scope.clone();
            claim_scope.projection = Some(Box::new(BTreeSet::from([relative.clone()])));
            claim.intent =
                crate::desired::MaterializationIntent::ScopedVirtualCheckout(claim_scope);
            if let Err(error) =
                state::validate_claims(&state::state_file()?.inspect_blocking()?, &claim)
            {
                if recursive {
                    continue;
                }
                return Err(error);
            }
            if !ctx.acquire_checkout_paths {
                ensure!(
                    recursive,
                    "'{}' has no checkout owner; specify --overlay NAME to acquire it",
                    path.display()
                );
                continue;
            }
            for ancestor in path
                .ancestors()
                .skip(1)
                .take_while(|parent| *parent != entry.target)
            {
                ensure!(
                    !ancestor.join(".git").try_exists()? && !ancestor.join(".git").is_symlink(),
                    "cannot acquire '{}' inside atomic Git repository '{}'",
                    path.display(),
                    ancestor.display()
                );
            }
            scope
                .projection
                .as_mut()
                .expect("projection checked above")
                .insert(relative);
            entry.intent = crate::desired::MaterializationIntent::ScopedVirtualCheckout(scope);
        }
    }
    Ok(())
}

fn stage_entries(
    entries: &[DesiredEntry],
    selections: &[PathBuf],
    dry_run: bool,
    overlay: &Overlay,
    preserve_staged: bool,
) -> Result<()> {
    stage_entries_with(
        entries,
        selections,
        dry_run,
        overlay,
        preserve_staged,
        &state::record_for_entry,
        &snapshot::canonical_source_entry,
    )
}

fn stage_entries_with(
    entries: &[DesiredEntry],
    selections: &[PathBuf],
    dry_run: bool,
    overlay: &Overlay,
    preserve_staged: bool,
    record_for: &impl Fn(&DesiredEntry) -> Result<Option<state::VirtualCheckoutRecord>>,
    source_entry: &impl Fn(&git2::Repository, &Path) -> Result<Option<(git2::Oid, i32)>>,
) -> Result<()> {
    // One in-memory index per repository prevents sibling units from overwriting
    // each other's changes; write only after every selected unit passes preflight.
    let mut repositories: BTreeMap<PathBuf, (git2::Repository, git2::Index)> = BTreeMap::new();
    let mut originals = BTreeMap::new();
    for entry in entries {
        if !selections
            .iter()
            .any(|path| owns(entry, path) || entry.target.starts_with(path))
        {
            continue;
        }
        let record = record_for(entry)?.with_context(|| {
            format!(
                "no checkout at '{}'; run over apply first",
                entry.target.display()
            )
        })?;
        ensure!(
            !record.initializing,
            "checkout initialization was interrupted; rerun over apply before staging"
        );
        let Provenance::Overlay { source, .. } = &entry.provenance else {
            unreachable!()
        };
        let (repo, managed) = git::discover_source(source)?;
        let key = repo.path().to_path_buf();
        if !repositories.contains_key(&key) {
            originals.insert(
                key.clone(),
                crate::actions::git::index::read_index(&repo.path().join("index"))?,
            );
            let index = repo.index()?;
            ensure!(
                !index.has_conflicts(),
                "source index has unresolved conflicts"
            );
            repositories.insert(key.clone(), (repo, index));
        }
        let (repo, index) = repositories
            .get_mut(&key)
            .expect("repository inserted above");
        let scope = entry.intent.checkout_scope();
        let head = git::base_tree(repo, None)?;
        let tracked = checkout_entries(entry, &record, repo, &managed)?;
        // Run filters in a private ODB even for previews; canonicalization must
        // not publish target blobs to the source just to validate a dry run.
        let scratch = Scratch::new(repo)?;
        let mut candidates: BTreeSet<_> = tracked.keys().cloned().collect();
        candidates.extend(git::walk_owned_files(&entry.target, &scope)?);
        for indexed in index.iter() {
            let path = PathBuf::from(std::str::from_utf8(&indexed.path)?);
            if let Ok(relative) = path.strip_prefix(&managed)
                && scope.owns_source(relative)
                && scope.owns_target(relative)
                && git::is_supported_mode(indexed.mode)
            {
                candidates.insert(relative.to_path_buf());
            }
        }
        for selection in selections.iter().filter(|selection| owns(entry, selection)) {
            ensure!(
                selection.exists()
                    || candidates
                        .iter()
                        .any(|relative| entry.target.join(relative).starts_with(selection)),
                "{} does not match any tracked checkout path",
                selection.display()
            );
        }
        for relative in candidates {
            let target = entry.target.join(&relative);
            if !selections
                .iter()
                .any(|selection| target.starts_with(selection))
            {
                continue;
            }
            // Metadata and excluded files must never leak into the source index.
            if !scope.owns_source(&relative)
                || !scope.owns_target(&relative)
                || git::is_overlay_metadata(&relative)
            {
                continue;
            }
            let logical = source.strip_prefix(&overlay.root)?.join(&relative);
            if overlay.is_excluded(&logical) {
                continue;
            }
            let source_path = managed.join(&relative);
            git::ensure_supported_path(&head, index, &source_path)?;
            let worktree = repo
                .workdir()
                .context("source has no working directory")?
                .join(&source_path);
            let head_entry = tracked.get(&relative).copied();
            let worktree_entry = source_entry(repo, &source_path)?;
            let indexed_entry = index
                .get_path(&source_path, 0)
                .map(|item| (item.id, item.mode as i32));
            ensure!(
                !preserve_staged || git::entries_match(indexed_entry, head_entry),
                "source file '{}' has staged edits; resolve them before staging the checkout",
                worktree.display()
            );
            ensure!(
                git::entries_match(worktree_entry, head_entry)
                    || git::entries_match(worktree_entry, indexed_entry),
                "source file '{}' has unstaged edits",
                worktree.display()
            );
            ensure!(
                !target.is_dir(),
                "cannot stage non-regular checkout file '{}'",
                target.display()
            );
            // Leaf metadata alone follows ancestor links and can import external
            // secrets. Share patch staging's root-and-ancestor checked read.
            match snapshot::target_content(&entry.target, &relative)? {
                Some((bytes, mode)) => {
                    let (oid, mode) = snapshot::canonical_entry(
                        repo,
                        &scratch,
                        &source_path,
                        &bytes,
                        mode,
                        &[indexed_entry, head_entry],
                    )?;
                    let blob = scratch.repo.find_blob(oid)?;
                    let index_entry = git2::IndexEntry {
                        ctime: git2::IndexTime::new(0, 0),
                        mtime: git2::IndexTime::new(0, 0),
                        dev: 0,
                        ino: 0,
                        uid: 0,
                        gid: 0,
                        mode: mode as u32,
                        file_size: blob.size().try_into().context("file too large to stage")?,
                        id: oid,
                        flags: 0,
                        flags_extended: 0,
                        path: source_path
                            .to_str()
                            .context("source path is not UTF-8")?
                            .replace('\\', "/")
                            .into_bytes(),
                    };
                    if !dry_run {
                        // The snapshot is already clean-filtered. Filtering it
                        // again can corrupt non-idempotent filters' output.
                        repo.blob(blob.content())?;
                        index.add(&index_entry)?;
                    }
                }
                None => {
                    if indexed_entry.is_some() && !dry_run {
                        index.remove_path(&source_path)?;
                    }
                }
            }
        }
    }
    for (key, (repo, _)) in &repositories {
        // Preview filters can invoke arbitrary Git commands; don't accept their
        // concurrent changes as the next publication's starting snapshot.
        ensure!(
            crate::actions::git::index::read_index(&repo.path().join("index"))? == originals[key],
            "source index changed during staging; retry"
        );
    }
    if !dry_run {
        let mut prepared = Vec::new();
        for (key, (repo, index)) in &repositories {
            let path = repo.path().join("index");
            let lock = crate::actions::git::index::IndexLock::acquire(&path)?;
            ensure!(
                crate::actions::git::index::read_index(&path)? == originals[key],
                "source index changed during staging; retry"
            );
            // Serialize privately while every source index remains locked.
            let temporary = tempfile::tempdir_in(repo.path())?;
            let temporary_path = temporary.path().join("index");
            let mut output = git2::Index::open(&temporary_path)?;
            for entry in index.iter() {
                output.add(&entry)?;
            }
            output.write()?;
            prepared.push((path, lock, std::fs::read(temporary_path)?));
        }
        for (path, mut lock, bytes) in prepared {
            lock.publish(&path, &bytes)?;
        }
    }
    Ok(())
}

fn checkout_entries(
    entry: &DesiredEntry,
    record: &state::VirtualCheckoutRecord,
    repo: &git2::Repository,
    managed: &Path,
) -> Result<BTreeMap<PathBuf, (git2::Oid, i32)>> {
    state::verify(entry, record)?;
    let scope = entry.intent.checkout_scope();
    // Compare filtered entries without creating synthetic source objects during
    // preflight, including cancelled sessions and dry runs.
    let head = git::scoped_entries(repo, &git::base_tree(repo, None)?, managed, &scope)?;
    let base = git::scoped_entries(
        repo,
        &git::base_tree(repo, Some(&record.base_oid))?,
        managed,
        &scope,
    )?;
    ensure!(
        head == base,
        "checkout '{}' is behind its source; sync before staging",
        entry.target.display()
    );
    Ok(head)
}
