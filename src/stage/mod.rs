//! Stage virtual-checkout content in the source index without moving targets.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, ensure};

use crate::desired::{DesiredEntry, DesiredTree, Provenance};
use crate::exec::Ctx;
use crate::materialize::virtual_checkout::{git, state};
use crate::overlays::{Overlay, mapping::absolute};

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
    let selections = selections
        .iter()
        .map(|path| absolute(path))
        .collect::<Result<Vec<_>>>()?;
    let routing = overlay.resolve_mappings(ctx)?;
    let mut remaining = BTreeSet::new();
    for path in &selections {
        routing.logical_path_for(path)?;
        if path.is_dir()
            && entries
                .iter()
                .any(|entry| owns(entry, path) || entry.target.starts_with(path))
        {
            for item in walkdir::WalkDir::new(path)
                .min_depth(1)
                .into_iter()
                .filter_entry(|item| !item.path().starts_with(&overlay.root))
            {
                let item = item?;
                if !item.file_type().is_dir()
                    && !item.file_type().is_symlink()
                    && !entries.iter().any(|entry| owns(entry, item.path()))
                {
                    let logical = routing.logical_path_for(item.path())?;
                    if !overlay.is_excluded(&logical) {
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
        let record = state::record_for_blocking(&entry.target)?.with_context(|| {
            format!(
                "no checkout at '{}'; run over apply first",
                entry.target.display()
            )
        })?;
        state::verify(entry, &record)?;
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
        let head_view = git::scoped_subtree(repo, &head, &managed, &scope)?;
        let base = git::base_tree(repo, Some(&record.base_oid))?;
        let base_view = git::scoped_subtree(repo, &base, &managed, &scope)?;
        ensure!(
            head_view.id() == base_view.id(),
            "checkout '{}' is behind its source; sync before staging",
            entry.target.display()
        );
        let tracked = git::tracked_entries(&head_view)?;
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
            let worktree_entry = git::source_entry(repo, &source_path)?;
            let indexed_entry = index
                .get_path(&source_path, 0)
                .map(|item| (item.id, item.mode as i32));
            ensure!(
                git::entries_match(worktree_entry, head_entry)
                    || git::entries_match(worktree_entry, indexed_entry),
                "source file '{}' has unstaged edits",
                worktree.display()
            );
            match target.symlink_metadata() {
                Ok(metadata) => {
                    ensure!(
                        metadata.is_file(),
                        "cannot stage non-regular checkout file '{}'",
                        target.display()
                    );
                    let bytes = std::fs::read(&target)?;
                    let index_entry = git2::IndexEntry {
                        ctime: git2::IndexTime::new(0, 0),
                        mtime: git2::IndexTime::new(0, 0),
                        dev: 0,
                        ino: 0,
                        uid: 0,
                        gid: 0,
                        mode: i32::from(git::on_disk_filemode(&target)?) as u32,
                        file_size: bytes.len().try_into().context("file too large to stage")?,
                        id: git2::Oid::ZERO_SHA1,
                        flags: 0,
                        flags_extended: 0,
                        path: source_path
                            .to_str()
                            .context("source path is not UTF-8")?
                            .replace('\\', "/")
                            .into_bytes(),
                    };
                    if !dry_run {
                        index.add_frombuffer(&index_entry, &bytes)?;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    if indexed_entry.is_some() && !dry_run {
                        index.remove_path(&source_path)?;
                    }
                }
                Err(error) => return Err(error.into()),
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
