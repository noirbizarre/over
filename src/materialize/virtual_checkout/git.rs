//! Git plumbing for virtual checkout materialization (#141).
//!
//! Deliberately separate from `actions::git::sync` (fetch/merge/push
//! against a remote, ADR-014): a virtual checkout's "source" is the
//! overlay's own repository, already on disk — reconciling it with the
//! materialized target never involves network I/O, only local git objects.
//! See ADR-022 for the full design.
//!
//! No `.git` is ever created at the target: files are checked out directly
//! from the source repository's own tree objects via
//! [`git2::build::CheckoutBuilder::target_dir`], and — since there's no
//! real git index at the target to diff against — content comparison is
//! done by hashing on-disk files as git blobs and comparing `Oid`s against
//! the source tree's recorded ones.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow};
use git2::{ObjectType, Oid, Repository, Tree, TreeWalkMode, TreeWalkResult};

use crate::desired::CheckoutScope;

/// Discover the git repository backing `source` (an overlay's root, or one
/// of its subdirectories when a subtree-level `checkout` rule applies),
/// and `source`'s path relative to that repository's working directory —
/// the "managed path" a virtual checkout is rooted at.
///
/// Uses [`Repository::discover`] (walks up from `source`) rather than
/// assuming `Repository::root` (the `over` overlay collection root) is
/// itself a git repository, or that it's the same directory as the git
/// worktree root — an overlay may live in a subdirectory of a larger
/// repository.
pub(crate) fn discover_source(source: &Path) -> Result<(Repository, PathBuf)> {
    let anchor = source
        .ancestors()
        .find(|path| path.exists())
        .unwrap_or(source);
    let repo = Repository::discover(anchor).with_context(|| {
        format!(
            "'{}' is not inside a git repository — virtual checkout materialization requires \
             the overlay to live inside one",
            source.display()
        )
    })?;
    let workdir = repo.workdir().ok_or_else(|| {
        anyhow!(
            "source repository at '{}' has no working directory (bare repositories can't back \
             a virtual checkout)",
            repo.path().display()
        )
    })?;

    let canonical_source = anchor
        .canonicalize()
        .map(|root| root.join(source.strip_prefix(anchor).unwrap_or(Path::new(""))))
        .with_context(|| format!("failed to canonicalize '{}'", source.display()))?;
    let canonical_workdir = workdir
        .canonicalize()
        .with_context(|| format!("failed to canonicalize '{}'", workdir.display()))?;
    let managed_path = canonical_source
        .strip_prefix(&canonical_workdir)
        .map(Path::to_path_buf)
        .unwrap_or_default();

    Ok((repo, managed_path))
}

/// Filter both historical and current source trees through identical ownership
/// boundaries, so a sibling mapping cannot appear as drift in this checkout.
pub(crate) fn scoped_subtree<'repo>(
    repo: &'repo Repository,
    tree: &Tree<'repo>,
    managed_path: &Path,
    scope: &CheckoutScope,
) -> Result<Tree<'repo>> {
    build_content_tree(repo, &scoped_entries(repo, tree, managed_path, scope)?)
}

/// Inspect ownership without persisting synthetic tree objects in the source repo.
pub(crate) fn scoped_entries(
    repo: &Repository,
    tree: &Tree<'_>,
    managed_path: &Path,
    scope: &CheckoutScope,
) -> Result<BTreeMap<PathBuf, (Oid, i32)>> {
    for indexed in repo.index()?.iter() {
        let path = Path::new(std::str::from_utf8(&indexed.path)?);
        if let Ok(relative) = path.strip_prefix(managed_path)
            && scope.owns_source(relative)
        {
            anyhow::ensure!(
                !crate::secrets::is_encrypted(relative),
                "Age-encrypted files require ordinary file-level materialization; virtual checkouts are unsupported"
            );
        }
    }
    if !managed_path.as_os_str().is_empty() && tree.get_path(managed_path).is_err() {
        return Ok(BTreeMap::new());
    }
    let subtree = managed_subtree(repo, tree, managed_path)?;
    let mut entries = tracked_entries(&subtree)?;
    entries.retain(|path, _| scope.owns_source(path));
    for path in entries.keys() {
        anyhow::ensure!(
            !crate::secrets::is_encrypted(path),
            "Age-encrypted files require ordinary file-level materialization; virtual checkouts are unsupported"
        );
        anyhow::ensure!(
            scope.owns_target(path),
            "source path '{}' collides with an independently mapped destination",
            path.display()
        );
    }
    Ok(entries)
}

/// The tree to materialize/compare against: the commit at `base_oid` if
/// one is already recorded (an existing virtual checkout), else the
/// source repository's current `HEAD` (first-time materialization).
pub(crate) fn base_tree<'repo>(
    repo: &'repo Repository,
    base_oid: Option<&str>,
) -> Result<Tree<'repo>> {
    match base_oid {
        Some(raw) => {
            let oid = Oid::from_str(raw)
                .with_context(|| format!("invalid stored virtual checkout base oid '{raw}'"))?;
            let commit = repo.find_commit(oid).with_context(|| {
                format!("base commit '{raw}' no longer exists in the source repository")
            })?;
            commit.tree().map_err(Into::into)
        }
        None => repo
            .head()
            .context("source repository has no HEAD yet (no commits)")?
            .peel_to_tree()
            .map_err(Into::into),
    }
}

/// The subtree at `managed_path` within `tree` — `tree` itself when
/// `managed_path` is empty (a whole-overlay virtual checkout rooted at the
/// repository's own root).
pub(crate) fn managed_subtree<'repo>(
    repo: &'repo Repository,
    tree: &Tree<'repo>,
    managed_path: &Path,
) -> Result<Tree<'repo>> {
    if managed_path.as_os_str().is_empty() {
        return tree
            .as_object()
            .clone()
            .into_tree()
            .map_err(|_| anyhow!("unreachable: a Tree object always reduces to a Tree"));
    }
    let entry = tree.get_path(managed_path).with_context(|| {
        format!(
            "managed path '{}' not found in the source tree",
            managed_path.display()
        )
    })?;
    let obj = entry
        .to_object(repo)
        .with_context(|| format!("failed to resolve '{}'", managed_path.display()))?;
    obj.into_tree().map_err(|_| {
        anyhow!(
            "managed path '{}' is not a directory in the source tree",
            managed_path.display()
        )
    })
}

/// Write the already-filtered managed tree into `target` without creating
/// `.git` or updating the source index. Callers preflight target ownership;
/// unrelated paths are never removed as a side effect of initialization.
#[cfg(test)]
pub(crate) fn checkout_subtree(repo: &Repository, tree: &Tree, target: &Path) -> Result<()> {
    fs::create_dir_all(target)
        .with_context(|| format!("failed to create '{}'", target.display()))?;
    // libgit2 checkout_tree updates the source index even with target_dir.
    // Writing blobs ourselves preserves real staging and independent target holes.
    let empty = build_content_tree(repo, &BTreeMap::new())?;
    apply_tree_diff(repo, target, &empty, tree)
}

/// Raw content of the blob `oid` — used by `over diff` to build a content
/// diff between a virtual checkout's recorded base content and the
/// target's current on-disk content for a modified path.
pub(crate) fn read_blob(repo: &Repository, oid: Oid) -> Result<Vec<u8>> {
    Ok(repo
        .find_blob(oid)
        .with_context(|| format!("failed to read blob {oid}"))?
        .content()
        .to_vec())
}

/// Update `target` in place to reflect `to_tree` instead of `from_tree`,
/// writing/removing only the paths that actually differ between the two —
/// used by `over sync`'s fast-forward (never called unless the caller
/// already confirmed there's nothing local to lose for those exact
/// paths). Deliberately bypasses [`checkout_subtree`]/`git2::checkout_tree`
/// here: that API's own "does this file already look up to date"
/// comparison against an *existing* target file is unreliable for a
/// `target_dir`-redirected checkout (observed directly: a file already
/// present at `target` with different content was sometimes left
/// untouched even under `force()`/`remove_untracked()`). Reading blobs and
/// writing them with plain `std::fs` sidesteps that comparison entirely —
/// there is nothing to "look already up to date" against.
#[cfg(test)]
pub(crate) fn apply_tree_diff(
    repo: &Repository,
    target: &Path,
    from_tree: &Tree,
    to_tree: &Tree,
) -> Result<()> {
    let from_entries = tracked_entries(from_tree)?;
    let to_entries = tracked_entries(to_tree)?;
    preflight_tree_changes(
        repo,
        target,
        Path::new(""),
        &from_entries,
        &from_entries,
        &to_entries,
    )?
    .apply(target)
}

/// The complete removal/write order is computed before any mutation. In
/// particular, a late directory obstruction cannot follow an earlier file write.
pub(crate) struct TreeChanges {
    removed: Vec<PathBuf>,
    directories: Vec<PathBuf>,
    writes: Vec<(PathBuf, zeroize::Zeroizing<Vec<u8>>, i32)>,
}

pub(crate) fn preflight_tree_changes(
    repo: &Repository,
    target: &Path,
    managed: &Path,
    owned: &BTreeMap<PathBuf, (Oid, i32)>,
    from: &BTreeMap<PathBuf, (Oid, i32)>,
    to: &BTreeMap<PathBuf, (Oid, i32)>,
) -> Result<TreeChanges> {
    let removed: Vec<_> = from
        .keys()
        .filter(|path| !to.contains_key(*path))
        .cloned()
        .collect();
    let mut directories = std::collections::BTreeSet::new();
    let mut writes = Vec::new();
    for path in from.keys().chain(to.keys()) {
        if from.get(path) == to.get(path) {
            continue;
        }
        let mut current = target.to_path_buf();
        for component in std::iter::once(None).chain(path.components().map(Some)) {
            if let Some(component) = component {
                current.push(component);
            }
            let metadata = match current.symlink_metadata() {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("failed to inspect '{}'", current.display()));
                }
            };
            anyhow::ensure!(
                !metadata.is_symlink(),
                "refusing changed symlink '{}'",
                current.display()
            );
            if metadata.is_file() {
                anyhow::ensure!(
                    !metadata.permissions().readonly(),
                    "file '{}' is read-only",
                    current.display()
                );
                if current != target.join(path) {
                    let ancestor = current.strip_prefix(target)?;
                    // ENOTDIR is acceptable only for an old leaf this plan owns
                    // and removes; unrelated files remain hard obstructions.
                    anyhow::ensure!(
                        removed.iter().any(|path| path == ancestor),
                        "unowned file obstruction '{}'",
                        current.display()
                    );
                    let expected = from.get(ancestor).copied();
                    let local = super::snapshot::canonical_target_entry(
                        repo,
                        target,
                        ancestor,
                        &managed.join(ancestor),
                        &[expected],
                    )?;
                    anyhow::ensure!(
                        entries_match(local, expected),
                        "modified file obstruction '{}'",
                        current.display()
                    );
                }
                break;
            }
            anyhow::ensure!(
                metadata.is_dir(),
                "unsupported obstruction '{}'",
                current.display()
            );
            if current == target.join(path) {
                anyhow::ensure!(
                    !removed.contains(path),
                    "directory replaced old leaf '{}'",
                    current.display()
                );
            }
            if current == target.join(path) && to.contains_key(path) {
                // Raw inventory deliberately includes ignored files, empty
                // foreign directories, symlinks, and independently owned holes.
                for entry in walkdir::WalkDir::new(&current) {
                    let entry = entry?;
                    let relative = entry.path().strip_prefix(target)?;
                    if entry.file_type().is_dir() {
                        anyhow::ensure!(
                            owned.keys().any(|path| path.starts_with(relative)),
                            "unowned directory obstruction '{}'",
                            entry.path().display()
                        );
                        directories.insert(relative.to_path_buf());
                    } else {
                        anyhow::ensure!(
                            entry.file_type().is_file()
                                && removed.iter().any(|path| path == relative),
                            "unowned obstruction '{}'",
                            entry.path().display()
                        );
                    }
                }
            }
        }
    }
    for path in &removed {
        let expected = from.get(path).copied();
        let local = super::snapshot::canonical_target_entry(
            repo,
            target,
            path,
            &managed.join(path),
            &[expected],
        )?;
        anyhow::ensure!(
            local.is_none() || entries_match(local, expected),
            "modified removal '{}'",
            target.join(path).display()
        );
    }
    for (path, (oid, mode)) in to {
        if from.get(path) != Some(&(*oid, *mode)) {
            // Resolve every blob before removing old leaves; corrupt/missing Git
            // objects must never leave a half-applied shape transition.
            writes.push((
                path.clone(),
                crate::crypt::policy::materialized_blob(repo, &managed.join(path), *oid)?,
                *mode,
            ));
        }
    }
    let mut directories: Vec<_> = directories.into_iter().collect();
    directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    Ok(TreeChanges {
        removed,
        directories,
        writes,
    })
}

impl TreeChanges {
    pub(crate) fn apply(self, target: &Path) -> Result<()> {
        for path in self.removed {
            let on_disk = target.join(path);
            crate::materialize::disposal::validate_relative_ancestors(target, &on_disk)?;
            match fs::remove_file(&on_disk) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("failed to remove '{}'", on_disk.display()));
                }
            }
        }
        for path in self.directories {
            // Never recursively delete: a raced-in foreign entry must stop us.
            let directory = target.join(path);
            crate::materialize::disposal::validate_relative_ancestors(target, &directory)?;
            fs::remove_dir(directory)?;
        }
        for (path, content, mode) in self.writes {
            let on_disk = target.join(path);
            crate::materialize::disposal::validate_relative_ancestors(target, &on_disk)?;
            anyhow::ensure!(
                !on_disk.is_symlink(),
                "refusing to write through symlink '{}'",
                on_disk.display()
            );
            if let Some(parent) = on_disk.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create '{}'", parent.display()))?;
            }
            crate::actions::fs::replace_file(&on_disk, content, |permissions| {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    // Git records executability, not private access bits such as 0600.
                    let existing = permissions.mode();
                    let bits = if mode & 0o111 != 0 {
                        existing | ((existing & 0o444) >> 2)
                    } else {
                        existing & !0o111
                    };
                    fs::Permissions::from_mode(bits)
                }
                #[cfg(not(unix))]
                {
                    let _ = mode;
                    permissions
                }
            })
            .with_context(|| format!("failed to write '{}'", on_disk.display()))?;
        }
        Ok(())
    }
}

/// Git-blob hash of the file currently on disk at `path` — the same `Oid`
/// it would have if committed as-is, letting content be compared against a
/// tree's recorded blob oid without needing a real git index at `target`.
pub(crate) fn hash_file(path: &Path) -> Result<Oid> {
    let bytes = fs::read(path).with_context(|| format!("failed to read '{}'", path.display()))?;
    Oid::hash_object(ObjectType::Blob, &bytes).map_err(Into::into)
}

/// An absent source is a valid addition/deletion; a symlink or directory is
/// not an absent file. Following one during commit could overwrite unrelated
/// content, so reject it during preflight along with unstaged executable edits.
pub(crate) fn source_entry(repo: &Repository, relative: &Path) -> Result<Option<(Oid, i32)>> {
    let root = repo.workdir().context("source has no working directory")?;
    file_entry(root, relative, "source")
}

pub(crate) fn entries_match(left: Option<(Oid, i32)>, right: Option<(Oid, i32)>) -> bool {
    // Non-Unix targets cannot represent Git's executable bit. Comparing it
    // there would permanently block an otherwise identical checkout file.
    if cfg!(unix) {
        left == right
    } else {
        left.map(|entry| entry.0) == right.map(|entry| entry.0)
    }
}

fn file_entry(root: &Path, relative: &Path, label: &str) -> Result<Option<(Oid, i32)>> {
    let mut path = root.to_path_buf();
    for component in relative.components() {
        path.push(component);
        match path.symlink_metadata() {
            Ok(metadata) => {
                anyhow::ensure!(
                    !metadata.is_symlink(),
                    "{label} path '{}' is a symlink",
                    path.display()
                );
                if path == root.join(relative) {
                    anyhow::ensure!(
                        metadata.is_file(),
                        "{label} path '{}' is not a regular file",
                        path.display()
                    );
                    return Ok(Some((
                        hash_file(&path)?,
                        i32::from(on_disk_filemode(&path)?),
                    )));
                }
                anyhow::ensure!(
                    metadata.is_dir(),
                    "{label} parent '{}' is not a directory",
                    path.display()
                );
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to inspect '{}'", path.display()));
            }
        }
    }
    anyhow::bail!("{label} file path is empty")
}

/// A single file-level difference between a virtual checkout's target and
/// the tracked tree it's compared against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileChange {
    /// Path relative to the checkout's target root.
    pub path: PathBuf,
    pub kind: FileChangeKind,
    /// The on-disk file's current blob oid — `None` only for
    /// [`FileChangeKind::Deleted`] (nothing on disk to hash). Lets a
    /// caller distinguish "changed to genuinely different content" from
    /// "changed to content that happens to already match some other
    /// tree" (e.g. `status::virtual_checkout::conflicting_paths`, where a
    /// local edit that converged on exactly the source's new content
    /// isn't a real conflict) without re-hashing the file a second time.
    pub local_oid: Option<Oid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileChangeKind {
    /// Present on disk, not in the tracked tree.
    Added,
    /// Present in both, content differs.
    Modified,
    /// Tracked, missing on disk.
    Deleted,
}

/// Compare `target`'s current on-disk content against `tracked` (a tree's
/// blob paths/oids, see [`tracked_blobs`]): the core primitive behind
/// `over status`/`over diff`/`over commit`'s per-file reporting for a
/// virtual checkout, computed entirely from blob hashes since there's no
/// real git index at `target` to diff against.
#[cfg(test)]
pub(crate) fn diff_target_against_tree(
    target: &Path,
    tracked: &BTreeMap<PathBuf, Oid>,
) -> Result<Vec<FileChange>> {
    diff_scoped_target(target, tracked, &CheckoutScope::default())
}

#[cfg(test)]
fn diff_scoped_target(
    target: &Path,
    tracked: &BTreeMap<PathBuf, Oid>,
    scope: &CheckoutScope,
) -> Result<Vec<FileChange>> {
    let mut changes = Vec::new();

    for (rel_path, expected_oid) in tracked {
        let on_disk = target.join(rel_path);
        match on_disk.symlink_metadata() {
            Ok(meta) if meta.is_file() => {
                let local_oid = hash_file(&on_disk)?;
                if local_oid != *expected_oid {
                    changes.push(FileChange {
                        path: rel_path.clone(),
                        kind: FileChangeKind::Modified,
                        local_oid: Some(local_oid),
                    });
                }
            }
            _ => changes.push(FileChange {
                path: rel_path.clone(),
                kind: FileChangeKind::Deleted,
                local_oid: None,
            }),
        }
    }

    for path in walk_owned_files(target, scope)? {
        // Never propose committing a stray file that happens to be named
        // like `over`'s own metadata (e.g. a user-created `over.toml`
        // inside the checkout) — at the managed path's own root, that
        // name collides with the real overlay descriptor's path.
        if !tracked.contains_key(&path) && !is_overlay_metadata(&path) {
            let local_oid = hash_file(&target.join(&path))?;
            changes.push(FileChange {
                path,
                kind: FileChangeKind::Added,
                local_oid: Some(local_oid),
            });
        }
    }

    changes.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(changes)
}

/// Shared status/commit/sync view: both content and executable changes use the
/// source repository's attributes and configuration, never the host repository's.
pub(crate) fn diff_canonical_target(
    repo: &Repository,
    managed: &Path,
    target: &Path,
    tracked: &BTreeMap<PathBuf, (Oid, i32)>,
    scope: &CheckoutScope,
) -> Result<Vec<FileChange>> {
    let mut paths: std::collections::BTreeSet<_> = tracked.keys().cloned().collect();
    paths.extend(walk_owned_files(target, scope)?);
    let scratch = crate::actions::git::diff::Scratch::new(repo)?;
    // Target publication can succeed before its checkpoint is persisted. Accept
    // raw HEAD blobs too, or retrying would clean them twice and invent conflicts.
    let head = base_tree(repo, None)?;
    let head_entries = scoped_entries(repo, &head, managed, scope)?;
    let mut changes = Vec::new();
    for path in paths {
        if is_overlay_metadata(&path) {
            continue;
        }
        let expected = tracked.get(&path).copied();
        let local = match super::snapshot::target_content(target, &path)? {
            Some((content, mode)) => Some(super::snapshot::canonical_entry(
                repo,
                &scratch,
                &managed.join(&path),
                &content,
                mode,
                &[expected, head_entries.get(&path).copied()],
            )?),
            None => None,
        };
        if !entries_match(local, expected) {
            changes.push(FileChange {
                path,
                kind: match (expected, local) {
                    (_, None) => FileChangeKind::Deleted,
                    (None, _) => FileChangeKind::Added,
                    _ => FileChangeKind::Modified,
                },
                local_oid: local.map(|entry| entry.0),
            });
        }
    }
    Ok(changes)
}

/// Inspect only explicitly tracked paths: status must not import unrelated target
/// content or invoke the overlay's clean filters on mounted repositories.
pub(crate) fn tracked_target_entries(
    repo: &Repository,
    managed: &Path,
    target: &Path,
    references: &[&BTreeMap<PathBuf, (Oid, i32)>],
) -> Result<BTreeMap<PathBuf, (Oid, i32)>> {
    let paths: std::collections::BTreeSet<_> = references
        .iter()
        .flat_map(|entries| entries.keys().cloned())
        .collect();
    let scratch = crate::actions::git::diff::Scratch::new(repo)?;
    let mut entries = BTreeMap::new();
    for path in paths {
        if let Some((content, mode)) = super::snapshot::target_content(target, &path)? {
            // Any known canonical blob bypasses a potentially non-idempotent filter.
            let expected = references
                .iter()
                .map(|entries| entries.get(&path).copied())
                .collect::<Vec<_>>();
            let entry = super::snapshot::canonical_entry(
                repo,
                &scratch,
                &managed.join(&path),
                &content,
                mode,
                &expected,
            )?;
            entries.insert(path, entry);
        }
    }
    Ok(entries)
}

pub(crate) fn entry_changes(
    expected: &BTreeMap<PathBuf, (Oid, i32)>,
    local: &BTreeMap<PathBuf, (Oid, i32)>,
) -> Vec<FileChange> {
    let paths: std::collections::BTreeSet<_> = expected.keys().chain(local.keys()).collect();
    paths
        .into_iter()
        .filter_map(|path| {
            let before = expected.get(path).copied();
            let after = local.get(path).copied();
            (!entries_match(after, before)).then(|| FileChange {
                path: path.clone(),
                kind: match (before, after) {
                    (_, None) => FileChangeKind::Deleted,
                    (None, _) => FileChangeKind::Added,
                    _ => FileChangeKind::Modified,
                },
                local_oid: after.map(|entry| entry.0),
            })
        })
        .collect()
}

/// Number of commits reachable from `to` but not from `from` — used to
/// report how far behind a virtual checkout's recorded `base_oid` is from
/// the source repository's current `HEAD` (mirrors `status::git::ahead_behind`'s
/// own `repo.graph_ahead_behind`, but that API compares two branch tips;
/// here `from` is an arbitrary historical commit, not a ref, so a
/// `Revwalk` is used instead).
pub(crate) fn commits_between(repo: &Repository, from: Oid, to: Oid) -> Result<usize> {
    if from == to {
        return Ok(0);
    }
    let mut walk = repo.revwalk()?;
    walk.push(to)?;
    walk.hide(from)?;
    Ok(walk.count())
}

/// Every regular file under `root`, recursively, as paths relative to
/// `root` — used to detect files added directly at a virtual checkout's
/// target that aren't tracked at all yet.
pub(crate) fn walk_owned_files(root: &Path, scope: &CheckoutScope) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    if !root.exists() {
        return Ok(files);
    }
    for entry in walkdir::WalkDir::new(root)
        .min_depth(1)
        .into_iter()
        .filter_entry(|entry| {
            entry
                .path()
                .strip_prefix(root)
                .is_ok_and(|path| scope.owns_target_kind(path, entry.file_type().is_dir()))
        })
    {
        let entry = entry.with_context(|| format!("failed to walk '{}'", root.display()))?;
        if entry.file_type().is_file()
            && let Ok(rel) = entry.path().strip_prefix(root)
        {
            files.push(rel.to_path_buf());
        }
    }
    Ok(files)
}

pub(crate) fn has_owned_content(root: &Path, scope: &CheckoutScope) -> Result<bool> {
    for entry in walkdir::WalkDir::new(root)
        .min_depth(1)
        .into_iter()
        .filter_entry(|entry| {
            entry
                .path()
                .strip_prefix(root)
                .is_ok_and(|path| scope.owns_target_kind(path, entry.file_type().is_dir()))
        })
    {
        if !entry?.file_type().is_dir() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whole-directory disposal needs a raw inventory: a filtered clean status
/// intentionally says nothing about ignored or independently owned content.
pub(crate) fn has_unowned_content(root: &Path, scope: &CheckoutScope) -> Result<bool> {
    for entry in walkdir::WalkDir::new(root).min_depth(1) {
        let entry = entry?;
        if !scope.owns_target_kind(entry.path().strip_prefix(root)?, entry.file_type().is_dir()) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether `rel_path` (relative to a managed subtree's own root) is
/// `over`'s own metadata rather than managed content: the overlay
/// descriptor (`over.{toml,yaml,yml}`) or a `.link.*`/`.partial.*`
/// sidecar. These must never be materialized as ordinary checked-out
/// files, mirroring symlink materialization. User selection is applied
/// separately through the operation's source policy, including to historical
/// trees and index entries that no longer exist in the source working tree.
pub(crate) fn is_overlay_metadata(rel_path: &Path) -> bool {
    crate::overlays::ignore::is_control_file(rel_path)
}

/// Every regular-file blob in `tree`, recursively, keyed by its path
/// relative to `tree`'s own root, alongside its raw git filemode. `over`'s
/// own metadata ([`is_overlay_metadata`]) is never included. Symlinks and
/// submodules (`git2::FileMode::Link`/`Commit`) are intentionally
/// excluded — comparing their on-disk content against a blob's recorded
/// content isn't meaningful the same way (a symlink's "content" on disk
/// is what it points to, not what `fs::read` returns); tracking those
/// precisely is a documented follow-up (ADR-022).
pub(crate) fn tracked_entries(tree: &Tree) -> Result<BTreeMap<PathBuf, (Oid, i32)>> {
    let mut paths = BTreeMap::new();
    tree.walk(TreeWalkMode::PreOrder, |root, entry| {
        let is_plain_blob = matches!(entry.kind(), Some(ObjectType::Blob))
            && is_supported_mode(entry.filemode() as u32);
        if is_plain_blob && let Ok(name) = entry.name() {
            let rel_path = Path::new(root).join(name);
            if !is_overlay_metadata(&rel_path) {
                paths.insert(rel_path, (entry.id(), entry.filemode()));
            }
        }
        TreeWalkResult::Ok
    })?;
    Ok(paths)
}

/// Build a tree object containing exactly `entries` (path -> (blob oid,
/// raw filemode)) and nothing else — used to materialize/refresh a
/// virtual checkout's target from [`tracked_entries`]'s already-filtered
/// view (excluding `over`'s own metadata) rather than checking out
/// `managed_subtree` directly, which would include it. Built from an
/// empty baseline via [`git2::build::TreeUpdateBuilder`] (the same
/// mechanism [`commit_changes`] uses to patch an existing tree) so nested
/// directories are created automatically from slash-containing paths.
pub(crate) fn build_content_tree<'repo>(
    repo: &'repo Repository,
    entries: &BTreeMap<PathBuf, (Oid, i32)>,
) -> Result<Tree<'repo>> {
    let empty_tree_id = repo.treebuilder(None)?.write()?;
    let empty_tree = repo.find_tree(empty_tree_id)?;

    let mut builder = git2::build::TreeUpdateBuilder::new();
    for (path, (oid, mode)) in entries {
        let mode =
            filemode_from_raw(*mode).ok_or_else(|| anyhow!("unsupported filemode {mode}"))?;
        builder.upsert(path.clone(), *oid, mode);
    }
    let tree_id = builder
        .create_updated(repo, &empty_tree)
        .context("failed to build the filtered content tree")?;
    repo.find_tree(tree_id).map_err(Into::into)
}

/// Reverse of `git2::FileMode`'s `Into<i32>`: libgit2 only ever reports one
/// of these fixed values from `TreeEntry::filemode`, so a linear scan
/// against the small, closed set is simplest.
fn filemode_from_raw(raw: i32) -> Option<git2::FileMode> {
    use git2::FileMode::*;
    [
        Unreadable,
        Tree,
        Blob,
        BlobGroupWritable,
        BlobExecutable,
        Link,
        Commit,
    ]
    .into_iter()
    .find(|mode| i32::from(*mode) == raw)
}

/// Just the blob `Oid`s from [`tracked_entries`] — what
/// [`diff_target_against_tree`]/`status::virtual_checkout` need for
/// content comparison, which never cares about filemode.
pub(crate) fn tracked_blobs(tree: &Tree) -> Result<BTreeMap<PathBuf, Oid>> {
    Ok(tracked_entries(tree)?
        .into_iter()
        .map(|(path, (oid, _mode))| (path, oid))
        .collect())
}

/// Snapshot staged content independently of target bytes: edits after `add`
/// must stay unstaged even if a commit happens much later.
pub(crate) fn staged_changes(
    repo: &Repository,
    managed: &Path,
    scope: &CheckoutScope,
) -> Result<Vec<FileChange>> {
    let head = base_tree(repo, None)?;
    let head_entries = scoped_entries(repo, &head, managed, scope)?;
    let index = repo.index()?;
    anyhow::ensure!(
        !index.has_conflicts(),
        "source index has unresolved conflicts"
    );
    let mut staged = BTreeMap::new();
    for entry in index.iter() {
        let path = Path::new(std::str::from_utf8(&entry.path)?);
        if let Ok(relative) = path.strip_prefix(managed)
            && scope.owns_source(relative)
            && scope.owns_target(relative)
            && !is_overlay_metadata(relative)
            && is_supported_mode(entry.mode)
        {
            staged.insert(relative.to_path_buf(), (entry.id, entry.mode as i32));
        }
    }
    let paths: std::collections::BTreeSet<_> = head_entries.keys().chain(staged.keys()).collect();
    Ok(paths
        .into_iter()
        .filter_map(|path| {
            let old = head_entries.get(path);
            let new = staged.get(path);
            (old != new).then(|| FileChange {
                path: path.clone(),
                kind: match (old, new) {
                    (_, None) => FileChangeKind::Deleted,
                    (None, _) => FileChangeKind::Added,
                    _ => FileChangeKind::Modified,
                },
                local_oid: new.map(|(oid, _)| *oid),
            })
        })
        .collect())
}

/// Keep index views symmetric with the regular-file-only tree view.
pub(crate) fn is_supported_mode(mode: u32) -> bool {
    matches!(mode, 0o100644 | 0o100755)
}

/// Unsupported entries may coexist with checkout content, but their staged
/// changes must not disappear silently when the user asks to commit this unit.
pub(crate) fn ensure_supported_staged(
    repo: &Repository,
    managed: &Path,
    scope: &CheckoutScope,
) -> Result<()> {
    let head = base_tree(repo, None)?;
    let index = repo.index()?;
    let mut unsupported = BTreeMap::new();
    head.walk(TreeWalkMode::PreOrder, |root, entry| {
        if entry.kind() != Some(ObjectType::Tree)
            && !is_supported_mode(entry.filemode() as u32)
            && let Ok(name) = entry.name()
        {
            unsupported.insert(
                Path::new(root).join(name),
                (entry.id(), entry.filemode() as u32),
            );
        }
        TreeWalkResult::Ok
    })?;
    for entry in index.iter().filter(|entry| !is_supported_mode(entry.mode)) {
        let path = PathBuf::from(std::str::from_utf8(&entry.path)?);
        unsupported.entry(path.clone()).or_insert_with(|| {
            head.get_path(&path)
                .map(|entry| (entry.id(), entry.filemode() as u32))
                .unwrap_or((Oid::ZERO_SHA1, 0))
        });
    }
    for (path, old) in unsupported {
        // Gitlinks are directory entries even when absent from today's worktree.
        let is_dir = old.1 == 0o160000
            || index
                .get_path(&path, 0)
                .is_some_and(|entry| entry.mode == 0o160000);
        if let Ok(relative) = path.strip_prefix(managed)
            && scope.owns_source_kind(relative, is_dir)
            && scope.owns_target_kind(relative, is_dir)
            && !is_overlay_metadata(relative)
        {
            anyhow::ensure!(
                index.get_path(&path, 0).map(|entry| (entry.id, entry.mode)) == Some(old),
                "unsupported staged checkout change at '{}'",
                path.display()
            );
        }
    }
    Ok(())
}

/// A regular target must not replace a source symlink/gitlink, including one
/// above it: that would silently convert unsupported source content on add.
pub(crate) fn ensure_supported_path(head: &Tree, index: &git2::Index, path: &Path) -> Result<()> {
    anyhow::ensure!(
        !crate::secrets::is_encrypted(path),
        "encrypted checkout staging is unsupported"
    );
    for ancestor in path.ancestors().filter(|path| !path.as_os_str().is_empty()) {
        let head_mode = head
            .get_path(ancestor)
            .ok()
            .map(|entry| entry.filemode() as u32);
        let index_mode = index.get_path(ancestor, 0).map(|entry| entry.mode);
        anyhow::ensure!(
            head_mode
                .into_iter()
                .chain(index_mode)
                .all(|mode| mode == 0o040000 || is_supported_mode(mode)),
            "cannot stage or commit unsupported checkout path '{}'",
            ancestor.display()
        );
    }
    Ok(())
}

/// Commit exactly these repository-relative staged paths, preserving unrelated
/// index entries and checking the source worktree before updating owned files.
pub(crate) fn commit_staged(repo: &Repository, paths: &[PathBuf], message: &str) -> Result<Oid> {
    let parent = repo.head()?.peel_to_commit()?;
    let head = parent.tree()?;
    let index = repo.index()?;
    anyhow::ensure!(
        !index.has_conflicts(),
        "source index has unresolved conflicts"
    );
    let workdir = repo.workdir().context("source has no working directory")?;
    let mut updates = Vec::new();
    let mut builder = git2::build::TreeUpdateBuilder::new();
    // Overlapping selections must not back up and replace the same path twice.
    for path in paths.iter().collect::<std::collections::BTreeSet<_>>() {
        ensure_supported_path(&head, &index, path)?;
        let old = head
            .get_path(path)
            .ok()
            .map(|entry| (entry.id(), entry.filemode()));
        let staged = index.get_path(path, 0);
        let new = staged.as_ref().map(|entry| (entry.id, entry.mode as i32));
        let file = workdir.join(path);
        let current = super::snapshot::canonical_source_entry(repo, path)?;
        anyhow::ensure!(
            entries_match(current, old) || entries_match(current, new),
            "source file '{}' has unstaged edits",
            file.display()
        );
        if let Some(entry) = staged {
            let mode =
                filemode_from_raw(entry.mode as i32).context("unsupported staged file mode")?;
            anyhow::ensure!(
                matches!(mode, git2::FileMode::Blob | git2::FileMode::BlobExecutable),
                "cannot commit non-regular checkout path '{}'",
                path.display()
            );
            builder.upsert(path.clone(), entry.id, mode);
            // Already-converged sources need no filesystem writes, preserving
            // their metadata and avoiding failures on read-only directories.
            if !entries_match(current, new) {
                updates.push((
                    file,
                    Some((
                        zeroize::Zeroizing::new(source_worktree_blob(repo, path, entry.id)?),
                        entry.mode,
                    )),
                ));
            }
        } else {
            builder.remove(path.clone());
            if current.is_some() {
                updates.push((file, None));
            }
        }
    }
    let tree = repo.find_tree(builder.create_updated(repo, &head)?)?;
    crate::crypt::policy::validate_tree(repo, &tree)?;
    let signature = crate::actions::git::sync::signature(repo)?;
    let mut protected_updates = false;
    for (path, _) in &updates {
        // Attribute inspection errors must never select plaintext rollback handling.
        protected_updates |= crate::crypt::policy::protected(repo, path.strip_prefix(workdir)?)?;
    }
    let mut protected_files = Vec::new();
    let mut crypt_guard = None;
    if protected_updates {
        // Create the commit object before journaling, so recovery can recognize a published HEAD.
        let committed = repo.commit(None, &signature, &signature, message, &tree, &[&parent])?;
        crypt_guard = Some(crate::crypt::access::guard(repo, true)?);
        let key = crate::crypt::access::local_key(repo)?;
        let expected = updates
            .iter()
            .map(|(path, update)| {
                (
                    path.clone(),
                    update.as_ref().map(|(bytes, _)| bytes.to_vec()),
                )
            })
            .collect();
        #[cfg(unix)]
        let mut modes: BTreeMap<PathBuf, u32> = BTreeMap::new();
        #[cfg(not(unix))]
        let modes: BTreeMap<PathBuf, u32> = BTreeMap::new();
        #[cfg(unix)]
        for (path, update) in &updates {
            if let Some((_, mode)) = update {
                modes.insert(
                    path.clone(),
                    crate::crypt::transaction::desired_mode(
                        fs::metadata(path).ok().as_ref(),
                        Some(*mode),
                    ),
                );
            }
        }
        crate::crypt::policy::begin_commit_with_modes(
            repo,
            &key,
            &updates
                .iter()
                .map(|(path, _)| path.clone())
                .collect::<Vec<_>>(),
            Some(committed.to_string()),
            expected,
            modes,
        )?;
    }
    // Prepare every write on the destination filesystem before changing any
    // source file. Backups remain available until Git accepts the HEAD update.
    let mut transaction = SourceTransaction::default();
    let result = (|| {
        for (file, update) in updates {
            if protected_updates
                && crate::crypt::policy::protected(repo, file.strip_prefix(workdir)?)?
            {
                // Protected rollback bytes live only in the encrypted journal, not adjacent backups.
                protected_files.push((file, update));
                continue;
            }
            transaction
                .prepare(
                    file.clone(),
                    update.map(|(mut bytes, mode)| (std::mem::take(&mut *bytes), mode)),
                )
                .with_context(|| {
                    format!("failed to prepare source update for '{}'", file.display())
                })?;
        }
        transaction.apply()?;
        for (file, update) in &protected_files {
            if let Some((bytes, mode)) = update {
                crate::crypt::transaction::worktree_write(file, bytes, Some(*mode))?;
            } else {
                fs::remove_file(file)?;
            }
        }
        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            message,
            &tree,
            &[&parent],
        )
        .context("failed to create commit")
    })();
    match result {
        Ok(oid) => {
            if crypt_guard.is_some()
                && let Err(error) = crate::crypt::policy::finish(repo)
            {
                // HEAD is already authoritative; do not invite a duplicate commit on retry.
                tracing::warn!(
                    "commit {oid} was published, but encrypted recovery cleanup failed: {error:#}; run over crypt recover"
                );
            }
            // Cleanup failure must not report a failed commit after HEAD moved.
            if let Err(error) = transaction.cleanup() {
                tracing::warn!(
                    "commit {oid} succeeded, but source backup cleanup failed: {error:#}"
                );
            }
            Ok(oid)
        }
        Err(error) => {
            transaction
                .rollback()
                .with_context(|| format!("{error:#}; source rollback also failed"))?;
            if crypt_guard.is_some() {
                // Release the exclusive guard before the recovery routine reacquires it.
                drop(crypt_guard.take());
                crate::crypt::policy::recover(repo)?;
            }
            Err(error)
        }
    }
}

/// Native Git applies external/process smudge filters as well as EOL conversion;
/// raw blobs belong at virtual targets, never in the source's real worktree.
pub(crate) fn source_worktree_blob(repo: &Repository, path: &Path, oid: Oid) -> Result<Vec<u8>> {
    // Smudge filters own inherited stderr until their subprocess exits.
    let _pause = crate::ui::progress::pause()?;
    let mut command = std::process::Command::new("git");
    command.current_dir(repo.workdir().context("source has no working directory")?);
    // Hooks and parent Git commands may export routing for another repository.
    for name in [
        "GIT_DIR",
        "GIT_COMMON_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_PREFIX",
    ] {
        command.env_remove(name);
    }
    let output = command
        .args(["cat-file", "--filters"])
        .arg({
            let mut argument = std::ffi::OsString::from("--path=");
            argument.push(path);
            argument
        })
        .arg(oid.to_string())
        .stderr(std::process::Stdio::inherit())
        .output()
        .context("failed to run Git smudge filters")?;
    anyhow::ensure!(
        output.status.success(),
        "Git smudge filters failed for '{}' ({})",
        path.display(),
        output.status
    );
    Ok(output.stdout)
}

/// Darwin clones preserve xattrs/resource forks, but ownership can differ and
/// ACLs are inherited from the destination directory. Correct those exceptions
/// explicitly; a best-effort `cp -p` can silently lose security metadata.
#[cfg(target_os = "macos")]
pub(crate) fn clone_macos_source(
    source: &Path,
    directory: &Path,
    expected: &fs::Metadata,
) -> Result<()> {
    use std::ffi::{c_char, c_int, c_void};
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    // These are stable libSystem APIs (clonefile(2), acl(3)); no extra library
    // or command-line utility is needed, and all operations target open files.
    unsafe extern "C" {
        fn fclonefileat(
            source: c_int,
            directory: c_int,
            name: *const c_char,
            flags: c_int,
        ) -> c_int;
        fn acl_init(count: c_int) -> *mut c_void;
        fn acl_get_fd(file: c_int) -> *mut c_void;
        fn acl_set_fd(file: c_int, acl: *mut c_void) -> c_int;
        fn acl_free(acl: *mut c_void) -> c_int;
    }

    let original = fs::File::open(source)?;
    let metadata = original.metadata()?;
    anyhow::ensure!(
        metadata.is_file() && (metadata.dev(), metadata.ino()) == (expected.dev(), expected.ino()),
        "source '{}' changed before cloning",
        source.display()
    );
    let parent = fs::File::open(directory)?;
    // A clone initially inherits destination ACLs, so keep its directory private
    // until the original ACL is installed. Mode 0700 alone cannot remove an
    // inherited ACL that grants other users directory traversal.
    parent.set_permissions(fs::Permissions::from_mode(0o700))?;
    // SAFETY: zero allocates an owned empty ACL; the live parent descriptor is
    // used only before the allocation is freed exactly once below.
    let empty_acl = unsafe { acl_init(0) };
    anyhow::ensure!(
        !empty_acl.is_null(),
        "failed to allocate private clone directory ACL"
    );
    let result = unsafe { acl_set_fd(parent.as_raw_fd(), empty_acl) };
    let error = std::io::Error::last_os_error();
    // SAFETY: empty_acl is owned and no longer used after this call.
    unsafe { acl_free(empty_acl) };
    if result != 0 {
        return Err(error).context("failed to make clone directory private");
    }
    // SAFETY: both descriptors remain open and the destination name is a valid
    // NUL-terminated string. A failed clone never replaces an existing file.
    let result =
        unsafe { fclonefileat(original.as_raw_fd(), parent.as_raw_fd(), c"new".as_ptr(), 0) };
    if result != 0 {
        return Err(std::io::Error::last_os_error()).with_context(|| {
            format!("failed to clone native metadata for '{}'", source.display())
        });
    }
    let cloned = fs::OpenOptions::new()
        .write(true)
        .open(directory.join("new"))?;
    let metadata = cloned.metadata()?;
    // Unprivileged clones may use the caller's owner/group. Never publish that
    // silent ownership change, even when cloning itself reported success.
    anyhow::ensure!(
        (metadata.uid(), metadata.gid(), metadata.mode())
            == (expected.uid(), expected.gid(), expected.mode()),
        "cannot preserve native ownership or mode for '{}'",
        source.display()
    );
    // CLONE_ACL merges inherited destination entries and is not an exact copy.
    // acl_set_fd instead installs the complete original ACL, including inherited
    // entries, and reports failure before any live file is replaced.
    // SAFETY: the source descriptor is live; a non-null result is owned until
    // acl_free, and is only passed to the matching Darwin ACL APIs.
    let mut acl = unsafe { acl_get_fd(original.as_raw_fd()) };
    if acl.is_null() {
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(error).context("failed to read source ACL");
        }
        // Darwin reports an absent ACL as ENOENT. Install an explicit empty
        // ACL so inherited permissions on the clone cannot broaden access.
        // SAFETY: zero requests an owned empty ACL from Darwin's allocator.
        acl = unsafe { acl_init(0) };
    }
    anyhow::ensure!(
        !acl.is_null(),
        "failed to allocate source ACL: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: the destination descriptor and the ACL returned above are live.
    let result = unsafe { acl_set_fd(cloned.as_raw_fd(), acl) };
    let error = std::io::Error::last_os_error();
    // SAFETY: this releases the ACL exactly once, after its last use.
    unsafe { acl_free(acl) };
    if result != 0 {
        return Err(error).context("failed to preserve source ACL");
    }
    Ok(())
}

#[derive(Default)]
// Commit and restore share replacement/rollback semantics. Callers must validate
// ownership and regular-file ancestors before preparing any filesystem changes.
pub(crate) struct SourceTransaction {
    crypt: Option<crate::crypt::transaction::FileTransaction>,
    updates: Vec<PreparedSourceUpdate>,
    created_directories: Vec<PathBuf>,
}

struct PreparedSourceUpdate {
    file: PathBuf,
    directory: PathBuf,
    has_new: bool,
    backed_up: bool,
    installed: bool,
    #[cfg(not(target_os = "linux"))]
    in_place: bool,
}

impl SourceTransaction {
    pub(crate) fn for_repository(repo: &Repository) -> Result<Self> {
        let crypt = if crate::crypt::access::configured(repo)?
            && !crate::crypt::access::local_key_missing(repo)?
        {
            Some(crate::crypt::transaction::FileTransaction::new(repo))
        } else {
            None
        };
        Ok(Self {
            crypt,
            ..Default::default()
        })
    }

    pub(crate) fn prepare(&mut self, file: PathBuf, update: Option<(Vec<u8>, u32)>) -> Result<()> {
        if let Some(crypt) = &mut self.crypt {
            return crypt.prepare(file, update);
        }
        let existing = match file.symlink_metadata() {
            Ok(metadata) => {
                anyhow::ensure!(
                    metadata.is_file(),
                    "source '{}' is not a regular file",
                    file.display()
                );
                anyhow::ensure!(
                    !metadata.permissions().readonly(),
                    "source '{}' is read-only",
                    file.display()
                );
                Some(metadata)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        #[cfg(not(target_os = "linux"))]
        if update.is_some()
            && let Some(_metadata) = &existing
        {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                // Native cloning clears set-id bits; in-place writes can too.
                anyhow::ensure!(
                    _metadata.mode() & 0o6000 == 0,
                    "cannot preserve native metadata for '{}'",
                    file.display()
                );
                // Without a native clone, keep the original inode and its ACLs.
                // Shared inodes cannot be updated without changing other links.
                #[cfg(not(target_os = "macos"))]
                anyhow::ensure!(
                    _metadata.nlink() == 1,
                    "cannot preserve native metadata for '{}'",
                    file.display()
                );
            }
            #[cfg(windows)]
            anyhow::ensure!(
                winapi_util::file::information(fs::File::open(&file)?)?.number_of_links() == 1,
                "cannot preserve hard-linked source '{}'",
                file.display()
            );
            #[cfg(not(any(unix, windows)))]
            anyhow::bail!("cannot preserve native metadata for '{}'", file.display());
        }
        let parent = file.parent().context("source file has no parent")?;
        let mut missing = Vec::new();
        let mut ancestor = parent;
        while !ancestor.exists() {
            missing.push(ancestor.to_path_buf());
            ancestor = ancestor.parent().context("source parent has no ancestor")?;
        }
        for directory in missing.into_iter().rev() {
            fs::create_dir(&directory)?;
            self.created_directories.push(directory);
        }
        // create_dir is exclusive; a counter prevents collisions between units
        // and concurrent commits without relying on random-number dependencies.
        static NEXT_TEMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let directory = loop {
            let counter = NEXT_TEMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let directory = parent.join(format!(".over-commit-{}-{counter}", std::process::id()));
            match fs::create_dir(&directory) {
                Ok(()) => break directory,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("failed to prepare '{}'", file.display()));
                }
            }
        };
        self.updates.push(PreparedSourceUpdate {
            file: file.clone(),
            directory: directory.clone(),
            has_new: update.is_some(),
            backed_up: false,
            installed: false,
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            in_place: existing.is_some() && update.is_some(),
            #[cfg(target_os = "macos")]
            in_place: update.is_some()
                && existing.as_ref().is_some_and(|metadata| {
                    use std::os::unix::fs::MetadataExt;
                    // Single-link files need no clone and work on non-APFS volumes.
                    metadata.nlink() == 1
                }),
        });
        if let Some((content, mode)) = update {
            let prepared = directory.join("new");
            // Private creation avoids exposing source secrets even temporarily.
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(target_os = "macos")]
            if let Some(metadata) = &existing
                && !self.updates.last().is_some_and(|update| update.in_place)
            {
                // Clone before truncating: writes must detach hard links while
                // retaining resource forks, xattrs, ownership and the exact ACL.
                clone_macos_source(&file, &directory, metadata)?;
                options.create_new(false).truncate(true);
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut output = options.open(&prepared)?;
            use std::io::Write as _;
            output.write_all(&content)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut access = match file.symlink_metadata() {
                    Ok(metadata) => metadata.permissions().mode(),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0o600,
                    Err(error) => return Err(error.into()),
                };
                // Git records executability, not access permissions. Only toggle
                // execute bits when necessary, and only for existing readers.
                if (access & 0o111 != 0) != (mode == 0o100755) {
                    access &= !0o111;
                    if mode == 0o100755 {
                        access |= (access & 0o444) >> 2;
                    }
                }
                let desired = fs::Permissions::from_mode(access & 0o7777);
                #[cfg(target_os = "linux")]
                if let Some(metadata) = &existing {
                    crate::actions::fs::clone_linux_metadata(&file, &prepared, metadata, desired)?;
                } else {
                    output.set_permissions(desired)?;
                }
                #[cfg(not(target_os = "linux"))]
                output.set_permissions(desired)?;
            }
            #[cfg(not(unix))]
            let _ = mode;
            output.sync_all()?;
        }
        #[cfg(not(target_os = "linux"))]
        if self.updates.last().is_some_and(|update| update.in_place) {
            // Read every backup before applying any write, so unreadable sources
            // cannot fail halfway through an otherwise valid commit.
            fs::copy(&file, directory.join("old"))?;
        }
        Ok(())
    }

    pub(crate) fn apply(&mut self) -> Result<()> {
        if let Some(crypt) = &mut self.crypt {
            return crypt.apply();
        }
        for update in &mut self.updates {
            #[cfg(not(target_os = "linux"))]
            if update.in_place {
                update.backed_up = true;
                // Mark before the write: an I/O error can leave partial bytes.
                update.installed = true;
                let prepared = update.directory.join("new");
                let permissions = fs::metadata(&prepared)?.permissions();
                crate::actions::fs::replace_file(&update.file, fs::read(&prepared)?, |_| {
                    permissions
                })?;
                continue;
            }
            match fs::rename(&update.file, update.directory.join("old")) {
                Ok(()) => update.backed_up = true,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("failed to back up '{}'", update.file.display()));
                }
            }
            if update.has_new {
                fs::rename(update.directory.join("new"), &update.file)
                    .with_context(|| format!("failed to update '{}'", update.file.display()))?;
                update.installed = true;
            }
        }
        Ok(())
    }

    pub(crate) fn rollback(&mut self) -> Result<()> {
        if let Some(crypt) = &mut self.crypt {
            return crypt.rollback();
        }
        let mut errors = Vec::new();
        for update in self.updates.iter_mut().rev() {
            let result = (|| -> Result<()> {
                #[cfg(not(target_os = "linux"))]
                if update.in_place && update.installed {
                    let backup = update.directory.join("old");
                    let permissions = fs::metadata(&backup)?.permissions();
                    crate::actions::fs::replace_file(&update.file, fs::read(&backup)?, |_| {
                        permissions
                    })?;
                    update.installed = false;
                    update.backed_up = false;
                    return Ok(());
                }
                if update.installed {
                    fs::remove_file(&update.file)?;
                    update.installed = false;
                }
                if update.backed_up {
                    fs::rename(update.directory.join("old"), &update.file)?;
                    update.backed_up = false;
                }
                Ok(())
            })();
            if let Err(error) = result {
                // Keep the backup on disk and name it in the error so failed
                // recovery never destroys the only remaining original copy.
                errors.push(format!(
                    "{} (backup '{}'): {error:#}",
                    update.file.display(),
                    update.directory.display()
                ));
            }
        }
        if !errors.is_empty() {
            anyhow::bail!("{}", errors.join("; "));
        }
        self.cleanup()?;
        for directory in self.created_directories.iter().rev() {
            fs::remove_dir(directory)?;
        }
        Ok(())
    }

    pub(crate) fn cleanup(&self) -> Result<()> {
        if let Some(crypt) = &self.crypt {
            return crypt.cleanup();
        }
        for update in &self.updates {
            fs::remove_dir_all(&update.directory)
                .with_context(|| format!("failed to remove '{}'", update.directory.display()))?;
        }
        Ok(())
    }
}

/// The git filemode a file currently on disk at `path` should be recorded
/// with if newly added: executable (`0o100755`) if any executable bit is
/// set (unix only — every other platform has no equivalent concept, and
/// checked-out files there are never executable in git's model), plain
/// (`0o100644`) otherwise.
#[cfg(unix)]
pub(crate) fn on_disk_filemode(path: &Path) -> Result<git2::FileMode> {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(path)
        .with_context(|| format!("failed to read metadata for '{}'", path.display()))?
        .permissions()
        .mode();
    Ok(if mode & 0o111 != 0 {
        git2::FileMode::BlobExecutable
    } else {
        git2::FileMode::Blob
    })
}

#[cfg(not(unix))]
pub(crate) fn on_disk_filemode(_path: &Path) -> Result<git2::FileMode> {
    Ok(git2::FileMode::Blob)
}

/// Create a new commit in `repo` recording `changes` (from
/// [`diff_target_against_tree`]) at `managed_path`, on top of `repo`'s
/// current `HEAD` — everything outside `managed_path` (sibling overlays,
/// other files in the same repository) is carried over unchanged via
/// [`git2::build::TreeUpdateBuilder`], which shares every untouched tree
/// object rather than rebuilding the whole tree by hand. This is an
/// ordinary, single-parent commit: fully inspectable and revertable with
/// plain `git log`/`git show`/`git revert` in the source repository.
#[cfg(test)]
pub(crate) fn commit_changes(
    repo: &Repository,
    managed_path: &Path,
    target: &Path,
    changes: &[FileChange],
    message: &str,
) -> Result<Oid> {
    let head_commit = repo
        .head()
        .context("source repository has no HEAD yet (no commits)")?
        .peel_to_commit()?;
    let head_tree = head_commit.tree()?;

    let mut builder = git2::build::TreeUpdateBuilder::new();
    for change in changes {
        let full_path = if managed_path.as_os_str().is_empty() {
            change.path.clone()
        } else {
            managed_path.join(&change.path)
        };
        match change.kind {
            FileChangeKind::Deleted => {
                builder.remove(full_path);
            }
            FileChangeKind::Added | FileChangeKind::Modified => {
                let on_disk = target.join(&change.path);
                let bytes = fs::read(&on_disk)
                    .with_context(|| format!("failed to read '{}'", on_disk.display()))?;
                let oid = repo.blob(&bytes)?;
                let mode = on_disk_filemode(&on_disk)?;
                builder.upsert(full_path, oid, mode);
            }
        }
    }

    let tree_id = builder
        .create_updated(repo, &head_tree)
        .with_context(|| format!("failed to build updated tree for '{}'", target.display()))?;
    let tree = repo.find_tree(tree_id)?;
    let sig = crate::actions::git::sync::signature(repo)?;
    repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &[&head_commit])
        .with_context(|| "failed to create commit".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    use git2::Signature;

    fn init_committed_repo(path: &Path) -> Repository {
        let repo = Repository::init(path).unwrap();
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

    #[cfg(target_os = "linux")]
    #[rstest::rstest]
    fn virtual_refresh_preserves_metadata_without_touching_an_unowned_hardlink(
        #[values(false, true)] hardlinked: bool,
    ) {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let source = TempDir::new().unwrap();
        source.child("file").write_str("old").unwrap();
        let repo = init_committed_repo(source.path());
        let from = repo.head().unwrap().peel_to_tree().unwrap();
        let new_blob = repo.blob(b"new").unwrap();
        let to = build_content_tree(
            &repo,
            &BTreeMap::from([(PathBuf::from("file"), (new_blob, 0o100755))]),
        )
        .unwrap();
        let destination = TempDir::new().unwrap();
        let target = destination.path().join("file");
        let alias = destination.path().join("unowned");
        fs::write(&alias, "old").unwrap();
        fs::set_permissions(&alias, fs::Permissions::from_mode(0o600)).unwrap();
        xattr::set(&alias, "user.over-regression", b"keep metadata").unwrap();
        if hardlinked {
            fs::hard_link(&alias, &target).unwrap();
        } else {
            fs::copy(&alias, &target).unwrap();
            xattr::set(&target, "user.over-regression", b"keep metadata").unwrap();
        }
        let original = fs::metadata(&alias).unwrap();

        apply_tree_diff(&repo, destination.path(), &from, &to).unwrap();

        let refreshed = fs::metadata(&target).unwrap();
        let untouched = fs::metadata(&alias).unwrap();
        assert_eq!(
            (refreshed.uid(), refreshed.gid()),
            (original.uid(), original.gid())
        );
        assert_eq!(refreshed.permissions().mode() & 0o7777, 0o700);
        assert_eq!(untouched.mode(), original.mode());
        assert_eq!(untouched.ino(), original.ino());
        assert_eq!(fs::read(&target).unwrap(), b"new");
        assert_eq!(fs::read(&alias).unwrap(), b"old");
        for path in [&target, &alias] {
            assert_eq!(
                xattr::get(path, "user.over-regression").unwrap(),
                Some(b"keep metadata".to_vec())
            );
        }
    }

    #[cfg(unix)]
    #[rstest::rstest]
    #[case(0o100644, 0o600)]
    #[case(0o100755, 0o700)]
    fn new_source_files_use_private_access_permissions(#[case] git_mode: u32, #[case] access: u32) {
        use std::os::unix::fs::PermissionsExt;

        let source = TempDir::new().unwrap();
        source.child("existing").write_str("existing").unwrap();
        let repo = init_committed_repo(source.path());
        let mut index = repo.index().unwrap();
        let mut entry = index.get_path(Path::new("existing"), 0).unwrap();
        entry.path = b"nested/new".to_vec();
        entry.mode = git_mode;
        index
            .add_frombuffer(&entry, b"new private content")
            .unwrap();
        index.write().unwrap();

        let oid = commit_staged(&repo, &[PathBuf::from("nested/new")], "add private file").unwrap();

        assert_eq!(repo.head().unwrap().target(), Some(oid));
        assert_eq!(
            fs::read(source.path().join("nested/new")).unwrap(),
            b"new private content"
        );
        assert_eq!(
            fs::metadata(source.path().join("nested/new"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            access
        );
        assert_eq!(
            repo.find_commit(oid)
                .unwrap()
                .tree()
                .unwrap()
                .get_path(Path::new("nested/new"))
                .unwrap()
                .filemode() as u32,
            git_mode
        );
    }

    #[cfg(unix)]
    #[test]
    fn staged_executability_changes_do_not_broaden_private_read_or_write_access() {
        use std::os::unix::fs::PermissionsExt;

        let source = TempDir::new().unwrap();
        source.child("script").write_str("original").unwrap();
        fs::set_permissions(
            source.path().join("script"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let repo = init_committed_repo(source.path());
        let mut index = repo.index().unwrap();
        let mut entry = index.get_path(Path::new("script"), 0).unwrap();
        entry.mode = 0o100755;
        index.add_frombuffer(&entry, b"staged script").unwrap();
        index.write().unwrap();

        commit_staged(&repo, &[PathBuf::from("script")], "make executable").unwrap();

        assert_eq!(
            fs::metadata(source.path().join("script"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    #[cfg(unix)]
    #[test]
    fn source_preparation_rejects_readonly_updates_and_deletions_before_mutation() {
        use std::os::unix::fs::PermissionsExt;

        let source = TempDir::new().unwrap();
        source.child("file").write_str("original").unwrap();
        let file = source.path().join("file");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o400)).unwrap();
        for update in [None, Some((b"new".to_vec(), 0o100644))] {
            let mut transaction = SourceTransaction::default();
            assert!(transaction.prepare(file.clone(), update).is_err());
            transaction.rollback().unwrap();
            assert_eq!(fs::read(&file).unwrap(), b"original");
            assert_eq!(fs::read_dir(source.path()).unwrap().count(), 1);
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[rstest::rstest]
    fn source_transactions_preserve_native_metadata_and_isolate_hardlinks(
        #[values(false, true)] rollback: bool,
    ) {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let source = TempDir::new().unwrap();
        let file = source.path().join("file");
        let alias = source.path().join("alias");
        fs::write(&file, b"original").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o640)).unwrap();
        #[cfg(target_os = "linux")]
        let acl = {
            xattr::set(&file, "user.over-regression", b"retain me").unwrap();
            let mut acl = 2_u32.to_le_bytes().to_vec();
            for (tag, access, id) in [
                (1_u16, 6_u16, u32::MAX),
                (2, 4, 12345),
                (4, 4, u32::MAX),
                (16, 4, u32::MAX),
                (32, 0, u32::MAX),
            ] {
                acl.extend(tag.to_le_bytes());
                acl.extend(access.to_le_bytes());
                acl.extend(id.to_le_bytes());
            }
            xattr::set(&file, "system.posix_acl_access", &acl).unwrap();
            acl
        };
        fs::hard_link(&file, &alias).unwrap();
        let original = fs::metadata(&file).unwrap();
        let mut transaction = SourceTransaction::default();
        transaction
            .prepare(file.clone(), Some((b"new".to_vec(), 0o100644)))
            .unwrap();
        transaction.apply().unwrap();
        if rollback {
            transaction.rollback().unwrap();
        } else {
            transaction.cleanup().unwrap();
        }
        let current = fs::metadata(&file).unwrap();
        assert_eq!(
            (current.uid(), current.gid(), current.mode()),
            (original.uid(), original.gid(), original.mode())
        );
        #[cfg(target_os = "linux")]
        {
            assert_eq!(
                xattr::get(&file, "system.posix_acl_access").unwrap(),
                Some(acl)
            );
            assert_eq!(
                xattr::get(&file, "user.over-regression").unwrap(),
                Some(b"retain me".to_vec())
            );
        }
        assert_eq!(
            fs::read(&file).unwrap(),
            if rollback {
                b"original".as_slice()
            } else {
                b"new".as_slice()
            }
        );
        assert_eq!(fs::read(&alias).unwrap(), b"original");
        assert_eq!(fs::metadata(&alias).unwrap().ino(), original.ino());
        assert_eq!(current.ino() == original.ino(), rollback);
    }

    #[cfg(unix)]
    #[test]
    fn a_mid_apply_write_failure_rolls_back_prior_writes_and_deletions() {
        use std::os::unix::fs::PermissionsExt;

        let source = TempDir::new().unwrap();
        for (path, mode) in [("a", 0o600), ("b", 0o700), ("c", 0o600)] {
            source.child(path).write_str(path).unwrap();
            fs::set_permissions(source.path().join(path), fs::Permissions::from_mode(mode))
                .unwrap();
        }
        let mut transaction = SourceTransaction::default();
        transaction
            .prepare(source.path().join("a"), Some((b"new a".to_vec(), 0o100644)))
            .unwrap();
        transaction.prepare(source.path().join("b"), None).unwrap();
        transaction
            .prepare(
                source.path().join("nested/added"),
                Some((b"added".to_vec(), 0o100644)),
            )
            .unwrap();
        transaction
            .prepare(source.path().join("c"), Some((b"new c".to_vec(), 0o100644)))
            .unwrap();
        // Removing the last prepared write deterministically fails installation
        // after an earlier replacement and deletion, even when run as root.
        fs::remove_file(transaction.updates[3].directory.join("new")).unwrap();

        assert!(transaction.apply().is_err());
        transaction.rollback().unwrap();

        for (path, mode) in [("a", 0o600), ("b", 0o700), ("c", 0o600)] {
            assert_eq!(fs::read(source.path().join(path)).unwrap(), path.as_bytes());
            assert_eq!(
                fs::metadata(source.path().join(path))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                mode
            );
        }
        assert_eq!(fs::read_dir(source.path()).unwrap().count(), 3);
    }

    #[test]
    fn discover_source_finds_the_repository_and_relative_managed_path() {
        let td = TempDir::new().unwrap();
        let overlay = td.child("dotfiles/ov");
        overlay.create_dir_all().unwrap();
        init_committed_repo(td.path());

        let (repo, managed_path) = discover_source(overlay.path()).unwrap();
        assert_eq!(
            repo.workdir().unwrap().canonicalize().unwrap(),
            td.path().canonicalize().unwrap()
        );
        assert_eq!(managed_path, PathBuf::from("dotfiles/ov"));
    }

    #[test]
    fn discover_source_errors_outside_any_repository() {
        let td = TempDir::new().unwrap();
        let result = discover_source(td.path());
        assert!(result.is_err());
    }

    #[test]
    fn discover_source_errors_for_a_bare_repository() {
        let td = TempDir::new().unwrap();
        Repository::init_bare(td.path()).unwrap();
        let err = discover_source(td.path()).map(|_| ()).unwrap_err();
        assert!(err.to_string().contains("no working directory"));
    }

    #[test]
    fn base_tree_errors_for_an_unknown_oid() {
        let td = TempDir::new().unwrap();
        td.child("a.txt").write_str("a").unwrap();
        let repo = init_committed_repo(td.path());
        let bogus = "0".repeat(40);
        let err = base_tree(&repo, Some(&bogus)).unwrap_err();
        assert!(err.to_string().contains("no longer exists"));
    }

    #[test]
    fn base_tree_errors_for_a_malformed_oid() {
        let td = TempDir::new().unwrap();
        td.child("a.txt").write_str("a").unwrap();
        let repo = init_committed_repo(td.path());
        let err = base_tree(&repo, Some("not-an-oid")).unwrap_err();
        assert!(
            err.to_string()
                .contains("invalid stored virtual checkout base oid")
        );
    }

    #[test]
    fn managed_subtree_errors_for_a_path_not_in_the_tree() {
        let td = TempDir::new().unwrap();
        td.child("a.txt").write_str("a").unwrap();
        let repo = init_committed_repo(td.path());
        let head_tree = repo.head().unwrap().peel_to_tree().unwrap();
        let err = managed_subtree(&repo, &head_tree, Path::new("does-not-exist")).unwrap_err();
        assert!(err.to_string().contains("not found in the source tree"));
    }

    #[test]
    fn managed_subtree_errors_when_the_path_is_a_file_not_a_directory() {
        let td = TempDir::new().unwrap();
        td.child("a.txt").write_str("a").unwrap();
        let repo = init_committed_repo(td.path());
        let head_tree = repo.head().unwrap().peel_to_tree().unwrap();
        let err = managed_subtree(&repo, &head_tree, Path::new("a.txt")).unwrap_err();
        assert!(err.to_string().contains("is not a directory"));
    }

    #[test]
    fn checkout_subtree_produces_no_git_directory() {
        let src_td = TempDir::new().unwrap();
        src_td.child("sub/file.txt").write_str("hello").unwrap();
        src_td.child("other.txt").write_str("other").unwrap();
        let repo = init_committed_repo(src_td.path());

        let dst_td = TempDir::new().unwrap();
        let head_tree = repo.head().unwrap().peel_to_tree().unwrap();
        let subtree = managed_subtree(&repo, &head_tree, Path::new("sub")).unwrap();
        checkout_subtree(&repo, &subtree, dst_td.path()).unwrap();

        assert!(dst_td.path().join("file.txt").exists());
        assert!(!dst_td.path().join("other.txt").exists());
        assert!(!dst_td.path().join(".git").exists());
    }

    #[test]
    fn checkout_subtree_at_root_checks_out_the_whole_tree() {
        let src_td = TempDir::new().unwrap();
        src_td.child("sub/file.txt").write_str("hello").unwrap();
        src_td.child("other.txt").write_str("other").unwrap();
        let repo = init_committed_repo(src_td.path());

        let dst_td = TempDir::new().unwrap();
        let head_tree = repo.head().unwrap().peel_to_tree().unwrap();
        let subtree = managed_subtree(&repo, &head_tree, Path::new("")).unwrap();
        checkout_subtree(&repo, &subtree, dst_td.path()).unwrap();

        assert!(dst_td.path().join("sub/file.txt").exists());
        assert!(dst_td.path().join("other.txt").exists());
        assert!(!dst_td.path().join(".git").exists());
    }

    #[test]
    fn hash_file_matches_the_committed_blob_oid() {
        let src_td = TempDir::new().unwrap();
        src_td.child("file.txt").write_str("hello").unwrap();
        let repo = init_committed_repo(src_td.path());

        let head_tree = repo.head().unwrap().peel_to_tree().unwrap();
        let entry = head_tree.get_path(Path::new("file.txt")).unwrap();

        let disk_oid = hash_file(&src_td.path().join("file.txt")).unwrap();
        assert_eq!(disk_oid, entry.id());
    }

    #[test]
    fn tracked_blobs_lists_every_nested_file_relative_to_the_subtree() {
        let src_td = TempDir::new().unwrap();
        src_td.child("sub/a.txt").write_str("a").unwrap();
        src_td.child("sub/nested/b.txt").write_str("b").unwrap();
        src_td.child("other.txt").write_str("other").unwrap();
        let repo = init_committed_repo(src_td.path());

        let head_tree = repo.head().unwrap().peel_to_tree().unwrap();
        let subtree = managed_subtree(&repo, &head_tree, Path::new("sub")).unwrap();
        let blobs = tracked_blobs(&subtree).unwrap();

        assert_eq!(blobs.len(), 2);
        assert!(blobs.contains_key(&PathBuf::from("a.txt")));
        assert!(blobs.contains_key(&PathBuf::from("nested/b.txt")));
    }

    #[test]
    fn apply_tree_diff_overwrites_a_file_already_present_with_different_content() {
        // The scenario `over sync`'s fast-forward hits: `target` already
        // has the file from an earlier materialization, and the source
        // moved it to different content — this must always win, unlike
        // `checkout_subtree`'s own `git2::checkout_tree`-based approach,
        // which proved unreliable at overwriting an already-present file
        // (see this function's own doc comment).
        let src_td = TempDir::new().unwrap();
        src_td.child("a.txt").write_str("original").unwrap();
        let repo = init_committed_repo(src_td.path());
        let from_tree = repo.head().unwrap().peel_to_tree().unwrap();

        std::fs::write(src_td.path().join("a.txt"), "advanced").unwrap();
        std::fs::write(src_td.path().join("b.txt"), "new file").unwrap();
        {
            let sig = Signature::now("Test", "test@test.com").unwrap();
            let mut index = repo.index().unwrap();
            index
                .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
                .unwrap();
            index.write().unwrap();
            let tree_id = index.write_tree().unwrap();
            let tree = repo.find_tree(tree_id).unwrap();
            let parent = repo.head().unwrap().peel_to_commit().unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "advance", &tree, &[&parent])
                .unwrap();
        }
        let to_tree = repo.head().unwrap().peel_to_tree().unwrap();

        // `target` already has the pre-advance content materialized.
        let target_td = TempDir::new().unwrap();
        checkout_subtree(&repo, &from_tree, target_td.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(target_td.path().join("a.txt")).unwrap(),
            "original"
        );

        apply_tree_diff(&repo, target_td.path(), &from_tree, &to_tree).unwrap();

        assert_eq!(
            std::fs::read_to_string(target_td.path().join("a.txt")).unwrap(),
            "advanced"
        );
        assert_eq!(
            std::fs::read_to_string(target_td.path().join("b.txt")).unwrap(),
            "new file"
        );
    }

    #[test]
    fn apply_tree_diff_removes_a_file_deleted_upstream() {
        let src_td = TempDir::new().unwrap();
        src_td.child("a.txt").write_str("a").unwrap();
        src_td.child("b.txt").write_str("b").unwrap();
        let repo = init_committed_repo(src_td.path());
        let from_tree = repo.head().unwrap().peel_to_tree().unwrap();

        std::fs::remove_file(src_td.path().join("b.txt")).unwrap();
        {
            let sig = Signature::now("Test", "test@test.com").unwrap();
            let mut index = repo.index().unwrap();
            index.remove_all(["b.txt"].iter(), None).unwrap();
            index.write().unwrap();
            let tree_id = index.write_tree().unwrap();
            let tree = repo.find_tree(tree_id).unwrap();
            let parent = repo.head().unwrap().peel_to_commit().unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "remove b", &tree, &[&parent])
                .unwrap();
        }
        let to_tree = repo.head().unwrap().peel_to_tree().unwrap();

        let target_td = TempDir::new().unwrap();
        checkout_subtree(&repo, &from_tree, target_td.path()).unwrap();
        assert!(target_td.path().join("b.txt").exists());

        apply_tree_diff(&repo, target_td.path(), &from_tree, &to_tree).unwrap();

        assert!(!target_td.path().join("b.txt").exists());
        assert!(target_td.path().join("a.txt").exists());
    }

    #[test]
    fn apply_tree_diff_removing_an_already_missing_file_is_a_no_op() {
        // The `NotFound` arm: a file the tree says should disappear but
        // that's already gone from `target` (e.g. the user deleted it
        // themselves) must never be treated as an error.
        let src_td = TempDir::new().unwrap();
        src_td.child("a.txt").write_str("a").unwrap();
        src_td.child("b.txt").write_str("b").unwrap();
        let repo = init_committed_repo(src_td.path());
        let from_tree = repo.head().unwrap().peel_to_tree().unwrap();

        std::fs::remove_file(src_td.path().join("b.txt")).unwrap();
        {
            let sig = Signature::now("Test", "test@test.com").unwrap();
            let mut index = repo.index().unwrap();
            index.remove_all(["b.txt"].iter(), None).unwrap();
            index.write().unwrap();
            let tree_id = index.write_tree().unwrap();
            let tree = repo.find_tree(tree_id).unwrap();
            let parent = repo.head().unwrap().peel_to_commit().unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "remove b", &tree, &[&parent])
                .unwrap();
        }
        let to_tree = repo.head().unwrap().peel_to_tree().unwrap();

        let target_td = TempDir::new().unwrap();
        checkout_subtree(&repo, &from_tree, target_td.path()).unwrap();
        // Already gone at `target` before `apply_tree_diff` even runs.
        std::fs::remove_file(target_td.path().join("b.txt")).unwrap();

        apply_tree_diff(&repo, target_td.path(), &from_tree, &to_tree).unwrap();

        assert!(target_td.path().join("a.txt").exists());
    }

    #[test]
    fn diff_target_against_tree_reports_everything_deleted_when_target_is_entirely_missing() {
        let src_td = TempDir::new().unwrap();
        src_td.child("a.txt").write_str("a").unwrap();
        let repo = init_committed_repo(src_td.path());
        let head_tree = repo.head().unwrap().peel_to_tree().unwrap();
        let tracked = tracked_blobs(&head_tree).unwrap();

        let dst_td = TempDir::new().unwrap();
        let missing_target = dst_td.path().join("does-not-exist");

        let changes = diff_target_against_tree(&missing_target, &tracked).unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].kind, FileChangeKind::Deleted);
    }

    #[cfg(unix)]
    #[test]
    fn commit_changes_preserves_an_executable_bit_for_a_newly_added_file() {
        use std::os::unix::fs::PermissionsExt;

        let src_td = TempDir::new().unwrap();
        src_td.child("a.txt").write_str("a").unwrap();
        let repo = init_committed_repo(src_td.path());

        let target_td = TempDir::new().unwrap();
        let head_tree = repo.head().unwrap().peel_to_tree().unwrap();
        checkout_subtree(&repo, &head_tree, target_td.path()).unwrap();
        let script = target_td.path().join("run.sh");
        std::fs::write(&script, "#!/bin/sh\necho hi\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let changes = vec![FileChange {
            path: PathBuf::from("run.sh"),
            kind: FileChangeKind::Added,
            local_oid: Some(hash_file(&script).unwrap()),
        }];
        let commit_oid = commit_changes(
            &repo,
            Path::new(""),
            target_td.path(),
            &changes,
            "add script",
        )
        .unwrap();

        let commit = repo.find_commit(commit_oid).unwrap();
        let tree = commit.tree().unwrap();
        let entry = tree.get_path(Path::new("run.sh")).unwrap();
        assert_eq!(entry.filemode(), i32::from(git2::FileMode::BlobExecutable));
    }

    #[test]
    fn commit_changes_removes_a_locally_deleted_file() {
        let src_td = TempDir::new().unwrap();
        src_td.child("a.txt").write_str("a").unwrap();
        src_td.child("b.txt").write_str("b").unwrap();
        let repo = init_committed_repo(src_td.path());

        let target_td = TempDir::new().unwrap();
        let head_tree = repo.head().unwrap().peel_to_tree().unwrap();
        checkout_subtree(&repo, &head_tree, target_td.path()).unwrap();
        std::fs::remove_file(target_td.path().join("b.txt")).unwrap();

        let changes = vec![FileChange {
            path: PathBuf::from("b.txt"),
            kind: FileChangeKind::Deleted,
            local_oid: None,
        }];
        let commit_oid =
            commit_changes(&repo, Path::new(""), target_td.path(), &changes, "remove b").unwrap();

        let commit = repo.find_commit(commit_oid).unwrap();
        let tree = commit.tree().unwrap();
        assert!(tree.get_path(Path::new("b.txt")).is_err());
        assert!(tree.get_path(Path::new("a.txt")).is_ok());
    }

    #[test]
    fn checkout_subtree_resolves_a_relative_target_correctly() {
        // Regression test: libgit2's `target_dir` checkout silently failed
        // to create nested directories when given a relative path (only
        // reproducible with a *subtree* checkout — the root-only tests
        // above happened not to hit it). `checkout_subtree` canonicalizes
        // first specifically to guard against this.
        let src_td = TempDir::new().unwrap();
        let ov = src_td.child("myov");
        ov.create_dir_all().unwrap();
        ov.child("profile.sh").write_str("export FOO=bar").unwrap();
        ov.child("sub/nested.txt")
            .write_str("nested content")
            .unwrap();
        let repo = init_committed_repo(src_td.path());

        let head_tree = repo.head().unwrap().peel_to_tree().unwrap();
        let subtree = managed_subtree(&repo, &head_tree, Path::new("myov")).unwrap();

        let dst_td = TempDir::new().unwrap();
        // Relative to the current directory, not absolute — each nextest
        // test runs in its own process, so changing it here doesn't affect
        // other tests.
        std::env::set_current_dir(dst_td.path().parent().unwrap()).unwrap();
        let relative_target = PathBuf::from(dst_td.path().file_name().unwrap());

        checkout_subtree(&repo, &subtree, &relative_target).unwrap();

        assert!(dst_td.path().join("profile.sh").exists());
        assert!(dst_td.path().join("sub/nested.txt").exists());
    }

    #[test]
    fn base_tree_falls_back_to_head_when_no_base_oid_recorded() {
        let src_td = TempDir::new().unwrap();
        src_td.child("file.txt").write_str("hello").unwrap();
        let repo = init_committed_repo(src_td.path());

        let head_tree = repo.head().unwrap().peel_to_tree().unwrap();
        let tree = base_tree(&repo, None).unwrap();
        assert_eq!(tree.id(), head_tree.id());
    }

    #[test]
    fn diff_target_against_tree_detects_modified_added_and_deleted() {
        let src_td = TempDir::new().unwrap();
        src_td.child("a.txt").write_str("a").unwrap();
        src_td.child("b.txt").write_str("b").unwrap();
        src_td.child("c.txt").write_str("c").unwrap();
        let repo = init_committed_repo(src_td.path());
        let head_tree = repo.head().unwrap().peel_to_tree().unwrap();
        let tracked = tracked_blobs(&head_tree).unwrap();

        let dst_td = TempDir::new().unwrap();
        dst_td.child("a.txt").write_str("a").unwrap(); // unchanged
        dst_td.child("b.txt").write_str("changed").unwrap(); // modified
        // c.txt deliberately absent -> deleted
        dst_td.child("new.txt").write_str("new").unwrap(); // added

        let changes = diff_target_against_tree(dst_td.path(), &tracked).unwrap();
        let summary: Vec<_> = changes.iter().map(|c| (c.path.clone(), c.kind)).collect();
        assert_eq!(
            summary,
            vec![
                (PathBuf::from("b.txt"), FileChangeKind::Modified),
                (PathBuf::from("c.txt"), FileChangeKind::Deleted),
                (PathBuf::from("new.txt"), FileChangeKind::Added),
            ]
        );
        // Every non-`Deleted` change carries the on-disk file's current
        // blob oid, computed once, reusable without re-hashing.
        for change in &changes {
            match change.kind {
                FileChangeKind::Deleted => assert!(change.local_oid.is_none()),
                _ => assert!(change.local_oid.is_some()),
            }
        }
    }

    #[test]
    fn diff_target_against_tree_is_empty_when_nothing_changed() {
        let src_td = TempDir::new().unwrap();
        src_td.child("a.txt").write_str("a").unwrap();
        let repo = init_committed_repo(src_td.path());
        let head_tree = repo.head().unwrap().peel_to_tree().unwrap();
        let tracked = tracked_blobs(&head_tree).unwrap();

        let dst_td = TempDir::new().unwrap();
        dst_td.child("a.txt").write_str("a").unwrap();

        assert!(
            diff_target_against_tree(dst_td.path(), &tracked)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn base_tree_resolves_an_explicit_recorded_oid() {
        let src_td = TempDir::new().unwrap();
        src_td.child("file.txt").write_str("hello").unwrap();
        let repo = init_committed_repo(src_td.path());
        let head_oid = repo.head().unwrap().peel_to_commit().unwrap().id();

        let tree = base_tree(&repo, Some(&head_oid.to_string())).unwrap();
        assert_eq!(tree.id(), repo.head().unwrap().peel_to_tree().unwrap().id());
    }

    fn commit_all(repo: &Repository, message: &str) {
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

    #[test]
    fn commits_between_counts_commits_reachable_only_from_to() {
        let td = TempDir::new().unwrap();
        td.child("a.txt").write_str("a").unwrap();
        let repo = init_committed_repo(td.path());
        let base = repo.head().unwrap().peel_to_commit().unwrap().id();

        std::fs::write(td.path().join("a.txt"), "aa").unwrap();
        commit_all(&repo, "second");
        std::fs::write(td.path().join("a.txt"), "aaa").unwrap();
        commit_all(&repo, "third");
        let head = repo.head().unwrap().peel_to_commit().unwrap().id();

        assert_eq!(commits_between(&repo, base, head).unwrap(), 2);
        assert_eq!(commits_between(&repo, head, head).unwrap(), 0);
    }
}
