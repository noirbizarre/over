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
        .is_ok_and(|relative| entry.intent.checkout_scope().owns_target(relative))
}

/// Preflight all checkout/index changes before performing ingestion actions.
/// Returns the selections still requiring ordinary move-and-link ingestion.
pub async fn add(ctx: &Ctx, overlay: &Overlay, selections: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let ctx = ctx.clone();
    let overlay = overlay.clone();
    let selections = selections.to_vec();
    tokio::task::spawn_blocking(move || add_blocking(&ctx, &overlay, &selections)).await?
}

fn add_blocking(ctx: &Ctx, overlay: &Overlay, selections: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let tree = DesiredTree::build_own(ctx, overlay)?;
    let entries: Vec<_> = tree
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
        if path.is_dir()
            && entries
                .iter()
                .any(|entry| owns(entry, path) || entry.target.starts_with(path))
        {
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
                if !item.file_type().is_dir()
                    && !item.file_type().is_symlink()
                    && !entries.iter().any(|entry| owns(entry, item.path()))
                {
                    let logical = routing.logical_path_for(item.path())?;
                    if policy.includes(&overlay.root.join(&logical), false) {
                        remaining.insert(item.path().to_path_buf());
                    }
                }
            }
        } else if entries.iter().any(|entry| owns(entry, path)) {
            continue;
        } else {
            ensure!(
                path.symlink_metadata().is_ok(),
                "{} does not exist",
                path.display()
            );
            remaining.insert(path.clone());
        }
    }
    stage_entries(&entries, &selections, ctx.dry_run, overlay)?;
    Ok(remaining.into_iter().collect())
}

fn stage_entries(
    entries: &[DesiredEntry],
    selections: &[PathBuf],
    dry_run: bool,
    overlay: &Overlay,
) -> Result<()> {
    stage_entries_with(
        entries,
        selections,
        dry_run,
        overlay,
        &state::record_for_blocking,
        &snapshot::canonical_source_entry,
    )
}

fn stage_entries_with(
    entries: &[DesiredEntry],
    selections: &[PathBuf],
    dry_run: bool,
    overlay: &Overlay,
    record_for: &impl Fn(&Path) -> Result<Option<state::VirtualCheckoutRecord>>,
    source_entry: &impl Fn(&git2::Repository, &Path) -> Result<Option<(git2::Oid, i32)>>,
) -> Result<()> {
    // One in-memory index per repository prevents sibling units from overwriting
    // each other's changes; write only after every selected unit passes preflight.
    let mut repositories: BTreeMap<PathBuf, (git2::Repository, git2::Index)> = BTreeMap::new();
    for entry in entries {
        if !selections
            .iter()
            .any(|path| owns(entry, path) || entry.target.starts_with(path))
        {
            continue;
        }
        let record = record_for(&entry.target)?.with_context(|| {
            format!(
                "no checkout at '{}'; run over apply first",
                entry.target.display()
            )
        })?;
        let Provenance::Overlay { source, .. } = &entry.provenance else {
            unreachable!()
        };
        let (repo, managed) = git::discover_source(source)?;
        let key = repo.path().to_path_buf();
        if !repositories.contains_key(&key) {
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
    if !dry_run {
        for (_, (_, mut index)) in repositories {
            index.write()?;
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
