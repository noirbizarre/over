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
use git2::build::CheckoutBuilder;
use git2::{ObjectType, Oid, Repository, Tree, TreeWalkMode, TreeWalkResult};

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
    let repo = Repository::discover(source).with_context(|| {
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

    let canonical_source = source
        .canonicalize()
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

/// Checkout every entry of `tree` directly into `target`, with no `.git`
/// created there — `tree` is expected to already be the *managed* subtree
/// (see [`managed_subtree`]), so its entries land directly under `target`
/// with no path prefix. `remove_untracked` also deletes any file at
/// `target` not present in `tree` — safe for first-time materialization
/// (target is empty) and for `over sync`'s behind-only fast-forward
/// (caller already confirmed there are no local changes to lose), but
/// callers are responsible for that safety gate: this is purely the
/// checkout mechanics.
pub(crate) fn checkout_subtree(repo: &Repository, tree: &Tree, target: &Path) -> Result<()> {
    fs::create_dir_all(target)
        .with_context(|| format!("failed to create '{}'", target.display()))?;
    // libgit2's `target_dir` checkout doesn't reliably resolve a relative
    // path the same way plain `std::fs` calls do (observed directly: a
    // relative target silently fails to create nested directories partway
    // through the checkout) — canonicalize first so it's always given an
    // absolute path, regardless of what the caller (ultimately `over`'s
    // `--root`) was given.
    let absolute_target = target
        .canonicalize()
        .with_context(|| format!("failed to resolve '{}'", target.display()))?;
    let mut co = CheckoutBuilder::new();
    co.target_dir(&absolute_target);
    co.force();
    co.remove_untracked(true);
    // Every content comparison in this module (`hash_file`,
    // `diff_target_against_tree`) hashes the target's raw on-disk bytes
    // against the tree's recorded blob oid directly — there is no
    // git index at the target to apply a matching clean filter on the
    // way back in via `over commit`. A smudge filter here (most notably
    // `core.autocrlf` converting LF to CRLF, the platform default on
    // Windows) would make a freshly materialized, untouched checkout
    // immediately hash as "modified" — disable filters so what's on disk
    // always matches the blob byte-for-byte, matching `apply_tree_diff`'s
    // own filter-free `fs::write`.
    co.disable_filters(true);
    repo.checkout_tree(tree.as_object(), Some(&mut co))
        .with_context(|| format!("failed to checkout into '{}'", target.display()))
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
pub(crate) fn apply_tree_diff(
    repo: &Repository,
    target: &Path,
    from_tree: &Tree,
    to_tree: &Tree,
) -> Result<()> {
    let from_entries = tracked_entries(from_tree)?;
    let to_entries = tracked_entries(to_tree)?;

    for (path, (oid, _mode)) in &to_entries {
        if from_entries.get(path).map(|(from_oid, _)| from_oid) != Some(oid) {
            let on_disk = target.join(path);
            if let Some(parent) = on_disk.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create '{}'", parent.display()))?;
            }
            let content = read_blob(repo, *oid)?;
            fs::write(&on_disk, content)
                .with_context(|| format!("failed to write '{}'", on_disk.display()))?;
        }
    }

    for path in from_entries.keys() {
        if !to_entries.contains_key(path) {
            let on_disk = target.join(path);
            match fs::remove_file(&on_disk) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(e)
                        .with_context(|| format!("failed to remove '{}'", on_disk.display()));
                }
            }
        }
    }

    Ok(())
}

/// Git-blob hash of the file currently on disk at `path` — the same `Oid`
/// it would have if committed as-is, letting content be compared against a
/// tree's recorded blob oid without needing a real git index at `target`.
pub(crate) fn hash_file(path: &Path) -> Result<Oid> {
    let bytes = fs::read(path).with_context(|| format!("failed to read '{}'", path.display()))?;
    Oid::hash_object(ObjectType::Blob, &bytes).map_err(Into::into)
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
pub(crate) fn diff_target_against_tree(
    target: &Path,
    tracked: &BTreeMap<PathBuf, Oid>,
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

    for path in walk_files(target)? {
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
fn walk_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    if !root.exists() {
        return Ok(files);
    }
    for entry in walkdir::WalkDir::new(root).min_depth(1) {
        let entry = entry.with_context(|| format!("failed to walk '{}'", root.display()))?;
        if entry.file_type().is_file()
            && let Ok(rel) = entry.path().strip_prefix(root)
        {
            files.push(rel.to_path_buf());
        }
    }
    Ok(files)
}

/// Whether `rel_path` (relative to a managed subtree's own root) is
/// `over`'s own metadata rather than managed content: the overlay
/// descriptor (`over.{toml,yaml,yml}`) or a `.link.*`/`.partial.*`
/// sidecar. These must never be materialized as ordinary checked-out
/// files, mirroring the exact exclusion
/// `desired::tree::walk_overlay_tree` already applies for symlink
/// materialization — without it, a virtual checkout's root would include
/// its own `over.toml` as regular content. Per-overlay `exclude` glob
/// patterns are a documented follow-up (ADR-022): only this fixed,
/// config-independent metadata is excluded so far.
fn is_overlay_metadata(rel_path: &Path) -> bool {
    [
        crate::overlays::GLOB_PATTERN.as_str(),
        "**/*.link.{toml,yaml,yml}",
        "**/*.partial.{toml,yaml,yml}",
    ]
    .iter()
    .any(|pattern| {
        globset::GlobBuilder::new(pattern)
            .literal_separator(true)
            .build()
            .ok()
            .is_some_and(|glob| glob.compile_matcher().is_match(rel_path))
    })
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
            && entry.filemode() != i32::from(git2::FileMode::Link);
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

/// The managed content of `subtree` with `over`'s own metadata removed —
/// what actually gets checked out at a virtual checkout's target. Thin
/// wrapper combining [`tracked_entries`] (already filtered) with
/// [`build_content_tree`].
pub(crate) fn managed_content_tree<'repo>(
    repo: &'repo Repository,
    subtree: &Tree<'repo>,
) -> Result<Tree<'repo>> {
    build_content_tree(repo, &tracked_entries(subtree)?)
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

/// The git filemode a file currently on disk at `path` should be recorded
/// with if newly added: executable (`0o100755`) if any executable bit is
/// set (unix only — every other platform has no equivalent concept, and
/// checked-out files there are never executable in git's model), plain
/// (`0o100644`) otherwise.
#[cfg(unix)]
fn on_disk_filemode(path: &Path) -> Result<git2::FileMode> {
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
fn on_disk_filemode(_path: &Path) -> Result<git2::FileMode> {
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
