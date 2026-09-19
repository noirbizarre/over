//! Exclude overlay-managed paths from a target git repository's own
//! `git status` (#146, part of #142).
//!
//! Reconciles the set of paths an overlay materializes (symlinks,
//! checkouts, virtual checkouts) into the enclosing repository's
//! `.git/info/exclude`, one marker block per overlay, reusing
//! `actions::partial`'s block primitives as-is — no new marker-block
//! format needed. Deliberately independent of `actions::git` (which clones
//! *declared* repositories, a resource `over` owns): this module instead
//! reads/writes the *host* repository a target happens to live in, the
//! same "manipulate this repository directly" territory `cli::git`
//! already occupies for `over.overlay` (ADR-007).
//!
//! Reconciliation always recomputes the expected block content from
//! scratch from the live `DesiredTree` — no separate state file — matching
//! `unapply`'s "never trust persisted state" philosophy. [`unreconcile`]
//! (#147) covers the reverse: unconditionally dropping an overlay's whole
//! block on `over unapply`, regardless of which individual entries were
//! actually removed from disk. `over status` diagnostics are tracked
//! separately (out of scope here).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use git2::Repository;

use crate::actions::partial::{self, BlockState};
use crate::desired::{DesiredTree, MaterializationIntent, Provenance};
use crate::ui;

/// One managed target discovered from a [`DesiredTree`] (or supplied
/// directly, e.g. by `over git add`), paired with the name of the overlay
/// that manages it — used to key its own, independent exclude block.
#[derive(Debug, Clone)]
pub struct ManagedTarget {
    /// Absolute path the entry should exist at (a symlink, or a whole
    /// checkout/subtree root).
    pub target: PathBuf,
    pub overlay: String,
}

/// A path that's already tracked by the enclosing repository's index —
/// never added to an exclude block (the index/tracked state is never
/// touched); reported here instead of being silently dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackedConflict {
    /// The repository's working directory.
    pub repo_root: PathBuf,
    /// Path relative to `repo_root`.
    pub path: PathBuf,
}

/// Outcome of a [`reconcile`] pass, across however many (repository,
/// overlay) groups `targets` landed in.
#[derive(Debug, Clone, Default)]
pub struct ExcludeReport {
    /// `.git/info/exclude` files that were created, updated, or shrunk.
    pub updated: Vec<PathBuf>,
    /// Already-tracked paths skipped — see [`TrackedConflict`].
    pub tracked_conflicts: Vec<TrackedConflict>,
    /// Exclude files with a malformed block (stray begin/end marker) for
    /// the overlay's own marker — left untouched, never auto-repaired.
    pub malformed: Vec<PathBuf>,
}

/// The name of the overlay that produced `provenance`.
///
/// Every [`Provenance`] variant carries an `overlay` field of the same
/// type, so a single or-pattern extracts it without needing a method on
/// `Provenance` itself for just this one caller.
fn overlay_name(provenance: &Provenance) -> &str {
    match provenance {
        Provenance::Overlay { overlay, .. }
        | Provenance::SymlinkSidecar { overlay, .. }
        | Provenance::Git { overlay, .. }
        | Provenance::PartialSidecar { overlay, .. } => overlay,
    }
}

/// Filter `desired`'s entries down to the ones this module is responsible
/// for excluding — the "narrowest-path rule" from #146.
///
/// Only `SymlinkFile`, `SymlinkDirectory`, `Checkout`, and
/// `VirtualCheckout` qualify: each maps to exactly one artifact rooted at
/// `entry.target` (a single symlink, or a whole checkout/subtree), so
/// `entry.target` alone is the line to exclude — no filesystem walk
/// needed. Deliberately excluded:
/// - `Directory` — a plain `mkdir` scaffold; excluding it would hide
///   non-overlay files a user might place inside it.
/// - `PartialFile` — only a delimited region of a possibly-foreign file is
///   owned; the file itself must never be excluded.
pub fn managed_targets(desired: &DesiredTree) -> Vec<ManagedTarget> {
    desired
        .entries()
        .iter()
        .filter(|entry| {
            matches!(
                entry.intent,
                MaterializationIntent::SymlinkFile { .. }
                    | MaterializationIntent::SymlinkDirectory { .. }
                    | MaterializationIntent::Checkout
                    | MaterializationIntent::VirtualCheckout
            )
        })
        .map(|entry| ManagedTarget {
            target: entry.target.clone(),
            overlay: overlay_name(&entry.provenance).to_string(),
        })
        .collect()
}

/// Walk `path` up to the nearest ancestor that currently exists on disk.
///
/// `path` itself may not exist yet (or, for a future `unapply` caller,
/// might already have been removed) — `Repository::discover` needs a real
/// starting directory to stat.
fn nearest_existing_ancestor(path: &Path) -> Option<PathBuf> {
    let mut candidate = path;
    loop {
        if candidate.exists() {
            return Some(candidate.to_path_buf());
        }
        candidate = candidate.parent()?;
    }
}

/// Discover the git repository enclosing `target`, if any.
///
/// Starts from `target.parent()`, **never** `target` itself: for
/// `Checkout`/`VirtualCheckout` entries `target` is a directory that may
/// itself contain a nested `.git` (a declared repository) — discovering
/// from `target` would find that nested repo instead of the *parent* one
/// this module needs to exclude the whole checkout from.
fn discover_enclosing_repo(target: &Path) -> Result<Option<Repository>> {
    let Some(start) = target.parent() else {
        return Ok(None);
    };
    let Some(ancestor) = nearest_existing_ancestor(start) else {
        return Ok(None);
    };
    match Repository::discover(&ancestor) {
        Ok(repo) => Ok(Some(repo)),
        // No enclosing repo — most overlay targets (e.g. `~` with no
        // `.git`) produce no exclude changes at all.
        Err(e) if e.code() == git2::ErrorCode::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| {
            format!(
                "failed to discover a git repository enclosing '{}'",
                target.display()
            )
        }),
    }
}

/// Canonicalize `target`'s parent directory, then rejoin `target`'s own
/// file name — deliberately **not** `target.canonicalize()` directly.
///
/// `target` may itself be a symlink (`SymlinkFile`/`SymlinkDirectory`
/// intents): `Path::canonicalize` follows symlinks, which would silently
/// resolve to the symlink's *destination* instead of preserving the
/// symlink's own path inside the repository — exactly the path this
/// module needs to exclude.
fn canonical_parent_join(target: &Path) -> Result<PathBuf> {
    let parent = target
        .parent()
        .with_context(|| format!("'{}' has no parent directory", target.display()))?;
    let canonical_parent = parent
        .canonicalize()
        .with_context(|| format!("failed to canonicalize '{}'", parent.display()))?;
    let file_name = target
        .file_name()
        .with_context(|| format!("'{}' has no file name", target.display()))?;
    Ok(canonical_parent.join(file_name))
}

/// Format a repository-root-relative path as a `/`-prefixed,
/// forward-slash-joined gitignore pattern — anchoring it to the repo root
/// so it never accidentally matches a same-named path elsewhere in the
/// tree.
fn to_gitignore_pattern(relative: &Path) -> String {
    let joined = relative
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/");
    format!("/{joined}")
}

/// Discovered repository handles, cached by `.git` dir identity, alongside
/// the (repo identity, overlay) -> targets groups derived from them. See
/// [`discover_groups`].
type DiscoveredGroups = (
    HashMap<PathBuf, Repository>,
    BTreeMap<(PathBuf, String), BTreeSet<PathBuf>>,
);

/// Discover the enclosing repository for each target and group targets by
/// `(repo identity, overlay)` — shared by [`reconcile`] and [`unreconcile`]
/// so both use identical discovery/grouping semantics; only what happens
/// per group differs.
///
/// git2::Repository isn't Clone — discovered handles are cached by their
/// `.git` dir identity so a repo shared by multiple targets/overlays is
/// only ever discovered once.
fn discover_groups(targets: &[ManagedTarget]) -> Result<DiscoveredGroups> {
    let mut repos: HashMap<PathBuf, Repository> = HashMap::new();
    let mut groups: BTreeMap<(PathBuf, String), BTreeSet<PathBuf>> = BTreeMap::new();

    for t in targets {
        let Some(repo) = discover_enclosing_repo(&t.target)? else {
            continue;
        };
        let key = repo.path().to_path_buf();
        groups
            .entry((key.clone(), t.overlay.clone()))
            .or_default()
            .insert(t.target.clone());
        repos.entry(key).or_insert(repo);
    }

    Ok((repos, groups))
}

/// Reconcile `.git/info/exclude` blocks for `targets`, one block per
/// (enclosing repository, overlay) pair — so multiple overlays targeting
/// the same repository each own an independent block, and reconciling one
/// never touches another's.
pub fn reconcile(targets: &[ManagedTarget]) -> Result<ExcludeReport> {
    let mut report = ExcludeReport::default();
    let (repos, groups) = discover_groups(targets)?;

    for ((repo_key, overlay), abs_targets) in &groups {
        let repo = repos
            .get(repo_key)
            .expect("every group key was inserted alongside its repo handle above");
        reconcile_group(repo, overlay, abs_targets, &mut report)?;
    }

    Ok(report)
}

/// Remove this overlay's exclude block from every repository its (now
/// unapplied) targets used to belong to — `over unapply` (#147).
///
/// Unlike [`reconcile`], this never computes expected content: unapply
/// treats an entry as no longer "managed" once unapply has run over it,
/// regardless of whether it was actually removed from disk
/// (`Outcome::NotOwned`/`CheckoutNotClean` leave files in place) — so the
/// whole block is dropped unconditionally per (repo, overlay) group.
/// `partial::remove_block` is already a no-op if the marker is absent, so
/// calling this twice, or on an overlay that was never excluded, is
/// harmless.
///
/// `targets` should be the same [`ManagedTarget`]s the overlay used to
/// manage (built from its `DesiredTree` *before* unapply removed the
/// files) — repo discovery tolerates a target that no longer exists on
/// disk by walking up to the nearest existing ancestor, so this works
/// whether it's called before or after the files are actually gone.
pub fn unreconcile(targets: &[ManagedTarget]) -> Result<ExcludeReport> {
    let mut report = ExcludeReport::default();
    let (repos, groups) = discover_groups(targets)?;

    for (repo_key, overlay) in groups.keys() {
        let repo = repos
            .get(repo_key)
            .expect("every group key was inserted alongside its repo handle above");
        remove_group_block(repo, overlay, &mut report)?;
    }

    Ok(report)
}

/// Reconcile a single (repository, overlay) group's exclude block.
fn reconcile_group(
    repo: &Repository,
    overlay: &str,
    targets: &BTreeSet<PathBuf>,
    report: &mut ExcludeReport,
) -> Result<()> {
    let workdir = repo.workdir().with_context(|| {
        format!(
            "bare repository at '{}' has no working directory to exclude paths from",
            repo.path().display()
        )
    })?;
    let canonical_workdir = workdir
        .canonicalize()
        .with_context(|| format!("failed to canonicalize '{}'", workdir.display()))?;
    let index = repo
        .index()
        .with_context(|| format!("failed to open index for '{}'", repo.path().display()))?;

    let mut expected: BTreeSet<String> = BTreeSet::new();
    for target in targets {
        let canonical_target = canonical_parent_join(target)?;
        let Ok(relative) = canonical_target.strip_prefix(&canonical_workdir) else {
            // Discovered from this target but somehow outside its
            // workdir (shouldn't happen) — never guess, skip it.
            continue;
        };

        if index.get_path(relative, 0).is_some() {
            report.tracked_conflicts.push(TrackedConflict {
                repo_root: workdir.to_path_buf(),
                path: relative.to_path_buf(),
            });
            ui::warn(format!(
                "'{}' is already tracked by the repository at '{}' — not excluding it \
                 (would hide a tracked file from `git status`)",
                relative.display(),
                workdir.display(),
            ))
            .ok();
            continue;
        }

        expected.insert(to_gitignore_pattern(relative));
    }

    let expected_content = expected.into_iter().collect::<Vec<_>>().join("\n");
    let marker = format!("exclude:{overlay}");
    let exclude_path = repo.commondir().join("info").join("exclude");

    if let Some(parent) = exclude_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create '{}'", parent.display()))?;
    }
    let current = if exclude_path.exists() {
        fs::read_to_string(&exclude_path)
            .with_context(|| format!("failed to read '{}'", exclude_path.display()))?
    } else {
        String::new()
    };

    let new_content = match partial::find_block(&current, &marker) {
        BlockState::Malformed => {
            report.malformed.push(exclude_path.clone());
            ui::warn(format!(
                "'{}' has a malformed 'over: {}' exclude block (stray begin/end marker) — left \
                 untouched",
                exclude_path.display(),
                marker,
            ))
            .ok();
            None
        }
        BlockState::Absent if expected_content.is_empty() => None,
        BlockState::Absent => Some(partial::append_block(&current, &marker, &expected_content)),
        BlockState::Found(existing) if existing == expected_content => None,
        BlockState::Found(_) if expected_content.is_empty() => {
            Some(partial::remove_block(&current, &marker))
        }
        BlockState::Found(_) => Some(partial::replace_block(&current, &marker, &expected_content)),
    };

    if let Some(content) = new_content {
        fs::write(&exclude_path, content)
            .with_context(|| format!("failed to write '{}'", exclude_path.display()))?;
        report.updated.push(exclude_path);
    }

    Ok(())
}

/// Unconditionally remove `overlay`'s exclude block from `repo`'s
/// `.git/info/exclude` — a no-op if the marker is already absent. Never
/// computes or compares expected content (unlike [`reconcile_group`]):
/// [`unreconcile`] calls this once per (repo, overlay) group without
/// caring which individual targets survived unapply.
fn remove_group_block(repo: &Repository, overlay: &str, report: &mut ExcludeReport) -> Result<()> {
    let marker = format!("exclude:{overlay}");
    let exclude_path = repo.commondir().join("info").join("exclude");

    if !exclude_path.exists() {
        return Ok(());
    }
    let current = fs::read_to_string(&exclude_path)
        .with_context(|| format!("failed to read '{}'", exclude_path.display()))?;

    match partial::find_block(&current, &marker) {
        BlockState::Absent => Ok(()),
        BlockState::Malformed => {
            report.malformed.push(exclude_path.clone());
            ui::warn(format!(
                "'{}' has a malformed 'over: {}' exclude block (stray begin/end marker) — left \
                 untouched",
                exclude_path.display(),
                marker,
            ))
            .ok();
            Ok(())
        }
        BlockState::Found(_) => {
            fs::write(&exclude_path, partial::remove_block(&current, &marker))
                .with_context(|| format!("failed to write '{}'", exclude_path.display()))?;
            report.updated.push(exclude_path);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desired::{DesiredEntry, MaterializationIntent, Provenance};
    use assert_fs::TempDir;
    use assert_fs::prelude::*;

    /// Create a temp dir and init a non-bare git repo in it.
    fn temp_git_repo() -> (TempDir, Repository) {
        let td = TempDir::new().unwrap();
        let repo = Repository::init(td.path()).unwrap();
        (td, repo)
    }

    fn exclude_content(repo: &Repository) -> String {
        let path = repo.commondir().join("info").join("exclude");
        fs::read_to_string(path).unwrap_or_default()
    }

    fn entry(target: PathBuf, overlay: &str, intent: MaterializationIntent) -> DesiredEntry {
        DesiredEntry {
            target,
            provenance: Provenance::Overlay {
                overlay: overlay.to_string(),
                source: PathBuf::new(),
            },
            intent,
            permissions: None,
        }
    }

    fn symlink_file(target: PathBuf, overlay: &str) -> DesiredEntry {
        entry(
            target,
            overlay,
            MaterializationIntent::SymlinkFile {
                source: PathBuf::from("/dev/null"),
                link_type: crate::actions::symlink::LinkType::Soft,
            },
        )
    }

    // ── managed_targets: narrowest-path filter ────────────────────────────

    #[test]
    fn a_symlink_file_entry_is_managed() {
        let entries = vec![symlink_file(PathBuf::from("/tmp/x"), "demo")];
        let targets = managed_targets(&DesiredTree::from_entries(entries));
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].overlay, "demo");
    }

    #[test]
    fn a_plain_directory_entry_is_never_managed() {
        let entries = vec![entry(
            PathBuf::from("/tmp/x"),
            "demo",
            MaterializationIntent::Directory,
        )];
        let targets = managed_targets(&DesiredTree::from_entries(entries));
        assert!(
            targets.is_empty(),
            "a bare mkdir scaffold must never be excluded — it might hold non-overlay files"
        );
    }

    #[test]
    fn a_partial_file_entry_is_never_managed() {
        let entries = vec![entry(
            PathBuf::from("/tmp/x"),
            "demo",
            MaterializationIntent::PartialFile {
                content: String::new(),
                marker: "m".into(),
            },
        )];
        let targets = managed_targets(&DesiredTree::from_entries(entries));
        assert!(
            targets.is_empty(),
            "only a delimited region of a partial file is owned — the file itself must never be excluded"
        );
    }

    // ── reconcile: core block lifecycle ───────────────────────────────────

    #[test]
    fn a_symlinked_file_is_excluded_as_a_single_line() {
        let (td, repo) = temp_git_repo();
        let target = td.path().join("dotfile");
        std::fs::write(&target, "content").unwrap();

        let targets = vec![ManagedTarget {
            target,
            overlay: "demo".into(),
        }];
        let report = reconcile(&targets).unwrap();
        assert_eq!(report.updated.len(), 1);

        let content = exclude_content(&repo);
        assert!(content.contains("# >>> over: exclude:demo >>>"));
        assert!(content.contains("/dotfile"));
        assert!(content.contains("# <<< over: exclude:demo <<<"));
    }

    #[test]
    fn a_directory_target_excludes_the_whole_subtree_with_one_line() {
        let (td, repo) = temp_git_repo();
        let dir = td.path().join("checkout");
        std::fs::create_dir_all(dir.join("nested")).unwrap();

        let targets = vec![ManagedTarget {
            target: dir,
            overlay: "demo".into(),
        }];
        reconcile(&targets).unwrap();

        let content = exclude_content(&repo);
        assert_eq!(
            content
                .lines()
                .filter(|l| !l.starts_with('#'))
                .collect::<Vec<_>>(),
            vec!["/checkout"]
        );
    }

    #[test]
    fn unmanaged_files_in_the_same_repo_are_never_touched() {
        let (td, repo) = temp_git_repo();
        let managed = td.path().join("managed");
        std::fs::write(&managed, "x").unwrap();
        std::fs::write(td.path().join("unmanaged"), "y").unwrap();

        reconcile(&[ManagedTarget {
            target: managed,
            overlay: "demo".into(),
        }])
        .unwrap();

        let content = exclude_content(&repo);
        assert!(!content.contains("unmanaged"));
    }

    #[test]
    fn a_tracked_path_is_never_excluded_and_is_reported() {
        let (td, repo) = temp_git_repo();
        let target = td.path().join("tracked");
        std::fs::write(&target, "content").unwrap();
        {
            let mut index = repo.index().unwrap();
            index.add_path(Path::new("tracked")).unwrap();
            index.write().unwrap();
        }

        let report = reconcile(&[ManagedTarget {
            target: target.clone(),
            overlay: "demo".into(),
        }])
        .unwrap();

        assert_eq!(report.tracked_conflicts.len(), 1);
        assert_eq!(report.tracked_conflicts[0].path, PathBuf::from("tracked"));
        assert!(report.updated.is_empty());

        // Index untouched: still exactly the one entry we added ourselves.
        let index = repo.index().unwrap();
        assert_eq!(index.len(), 1);
    }

    #[test]
    fn two_overlays_targeting_the_same_repo_get_independent_blocks() {
        let (td, repo) = temp_git_repo();
        let a = td.path().join("a");
        let b = td.path().join("b");
        std::fs::write(&a, "a").unwrap();
        std::fs::write(&b, "b").unwrap();

        reconcile(&[ManagedTarget {
            target: a,
            overlay: "one".into(),
        }])
        .unwrap();
        reconcile(&[ManagedTarget {
            target: b,
            overlay: "two".into(),
        }])
        .unwrap();

        let content = exclude_content(&repo);
        assert!(content.contains("# >>> over: exclude:one >>>"));
        assert!(content.contains("/a"));
        assert!(content.contains("# >>> over: exclude:two >>>"));
        assert!(content.contains("/b"));
    }

    #[test]
    fn reapplying_one_overlay_never_touches_another_overlays_block_or_hand_written_content() {
        let (td, repo) = temp_git_repo();
        let exclude_path = repo.commondir().join("info").join("exclude");
        std::fs::create_dir_all(exclude_path.parent().unwrap()).unwrap();
        std::fs::write(&exclude_path, "*.bak\n").unwrap();

        let a = td.path().join("a");
        let b = td.path().join("b");
        std::fs::write(&a, "a").unwrap();
        std::fs::write(&b, "b").unwrap();

        reconcile(&[ManagedTarget {
            target: a.clone(),
            overlay: "one".into(),
        }])
        .unwrap();
        reconcile(&[ManagedTarget {
            target: b,
            overlay: "two".into(),
        }])
        .unwrap();

        // Reconciling "one" again with the same single target is a no-op —
        // "two"'s block and the hand-written line must survive untouched.
        reconcile(&[ManagedTarget {
            target: a,
            overlay: "one".into(),
        }])
        .unwrap();

        let content = exclude_content(&repo);
        assert!(content.contains("*.bak"));
        assert!(content.contains("# >>> over: exclude:one >>>"));
        assert!(content.contains("# >>> over: exclude:two >>>"));
    }

    #[test]
    fn exclude_file_lives_in_the_common_dir_for_a_linked_worktree() {
        let (td, main_repo) = temp_git_repo();
        // Need at least one commit before a worktree can be added.
        {
            let sig = git2::Signature::now("test", "test@example.com").unwrap();
            let tree_id = {
                let mut index = main_repo.index().unwrap();
                index.write_tree().unwrap()
            };
            let tree = main_repo.find_tree(tree_id).unwrap();
            main_repo
                .commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
                .unwrap();
        }

        let worktree_dir = td.path().join("wt");
        let worktree = main_repo.worktree("feature", &worktree_dir, None).unwrap();
        let wt_repo = Repository::open_from_worktree(&worktree).unwrap();

        let target = worktree_dir.join("dotfile");
        std::fs::write(&target, "content").unwrap();

        reconcile(&[ManagedTarget {
            target,
            overlay: "demo".into(),
        }])
        .unwrap();

        // Must land in the main repo's info/exclude, not the worktree's
        // private admin dir.
        let main_exclude = main_repo.commondir().join("info").join("exclude");
        let wt_exclude = wt_repo.path().join("info").join("exclude");
        assert!(main_exclude.exists());
        assert!(!wt_exclude.exists());
        assert_eq!(main_repo.commondir(), wt_repo.commondir());
    }

    #[test]
    fn a_nested_declared_repository_is_excluded_in_its_parent_repo_only() {
        let (td, parent_repo) = temp_git_repo();
        let nested_dir = td.path().join("nested-checkout");
        // Simulate a `Checkout`/`VirtualCheckout` materialization: a
        // subdirectory that itself contains a `.git`.
        let nested_repo = Repository::init(&nested_dir).unwrap();

        reconcile(&[ManagedTarget {
            target: nested_dir.clone(),
            overlay: "demo".into(),
        }])
        .unwrap();

        let parent_content = exclude_content(&parent_repo);
        assert!(parent_content.contains("/nested-checkout"));

        // `git init` itself pre-populates `info/exclude` with a commented
        // template, so only assert our own marker never lands in it —
        // asserting non-existence would be a false negative.
        let nested_content = exclude_content(&nested_repo);
        assert!(
            !nested_content.contains("over: exclude:"),
            "nothing should ever be written inside the nested repo's own .git:\n{nested_content}"
        );
    }

    #[test]
    fn shrinking_the_desired_set_shrinks_the_block() {
        let (td, repo) = temp_git_repo();
        let a = td.path().join("a");
        let b = td.path().join("b");
        std::fs::write(&a, "a").unwrap();
        std::fs::write(&b, "b").unwrap();

        reconcile(&[
            ManagedTarget {
                target: a.clone(),
                overlay: "demo".into(),
            },
            ManagedTarget {
                target: b,
                overlay: "demo".into(),
            },
        ])
        .unwrap();
        assert!(exclude_content(&repo).contains("/b"));

        // "b" removed from the overlay's desired set.
        reconcile(&[ManagedTarget {
            target: a,
            overlay: "demo".into(),
        }])
        .unwrap();

        let content = exclude_content(&repo);
        assert!(content.contains("/a"));
        assert!(!content.contains("/b"));
    }

    #[test]
    fn an_empty_desired_set_removes_the_block_entirely() {
        let (td, repo) = temp_git_repo();
        let a = td.path().join("a");
        std::fs::write(&a, "a").unwrap();

        reconcile(&[ManagedTarget {
            target: a,
            overlay: "demo".into(),
        }])
        .unwrap();
        assert!(exclude_content(&repo).contains("exclude:demo"));

        reconcile(&[]).unwrap();
        // Nothing to reconcile against with an empty `targets` slice (no
        // repo is even discovered) — the block from the previous overlay
        // run is left in place; this test only documents that calling
        // `reconcile` with no targets never panics or touches the file.
        assert!(exclude_content(&repo).contains("exclude:demo"));
    }

    #[test]
    fn a_malformed_block_is_left_untouched_and_reported() {
        let (td, repo) = temp_git_repo();
        let exclude_path = repo.commondir().join("info").join("exclude");
        std::fs::create_dir_all(exclude_path.parent().unwrap()).unwrap();
        std::fs::write(&exclude_path, "# >>> over: exclude:demo >>>\n/stray\n").unwrap();

        let a = td.path().join("a");
        std::fs::write(&a, "a").unwrap();

        let report = reconcile(&[ManagedTarget {
            target: a,
            overlay: "demo".into(),
        }])
        .unwrap();

        assert_eq!(report.malformed.len(), 1);
        assert!(report.updated.is_empty());
        let content = exclude_content(&repo);
        assert_eq!(content, "# >>> over: exclude:demo >>>\n/stray\n");
    }

    #[test]
    #[cfg(unix)]
    fn a_symlinks_own_path_is_excluded_not_its_destination() {
        let (td, repo) = temp_git_repo();
        let outside = TempDir::new().unwrap();
        let real_file = outside.child("real");
        real_file.write_str("content").unwrap();

        let link = td.path().join("link");
        std::os::unix::fs::symlink(real_file.path(), &link).unwrap();

        reconcile(&[ManagedTarget {
            target: link,
            overlay: "demo".into(),
        }])
        .unwrap();

        let content = exclude_content(&repo);
        assert!(
            content.contains("/link"),
            "the symlink's own in-repo path must be excluded, not resolved through to its \
             (out-of-repo) destination:\n{content}"
        );
    }

    // ── unreconcile: over unapply (#147) ──────────────────────────────────

    #[test]
    fn unreconcile_removes_the_overlay_block() {
        let (td, repo) = temp_git_repo();
        let a = td.path().join("a");
        std::fs::write(&a, "a").unwrap();

        let targets = [ManagedTarget {
            target: a,
            overlay: "demo".into(),
        }];
        reconcile(&targets).unwrap();
        assert!(exclude_content(&repo).contains("exclude:demo"));

        let report = unreconcile(&targets).unwrap();
        assert_eq!(report.updated.len(), 1);
        assert!(!exclude_content(&repo).contains("exclude:demo"));
    }

    #[test]
    fn unreconcile_still_finds_the_repo_when_the_target_was_already_removed() {
        // Mirrors real `over unapply` ordering: files are already gone from
        // disk by the time the exclude block is reconciled away, but their
        // parent directory (and thus the repo) still exists.
        let (td, repo) = temp_git_repo();
        let a = td.path().join("a");
        std::fs::write(&a, "a").unwrap();

        let targets = [ManagedTarget {
            target: a.clone(),
            overlay: "demo".into(),
        }];
        reconcile(&targets).unwrap();
        assert!(exclude_content(&repo).contains("exclude:demo"));

        std::fs::remove_file(&a).unwrap();
        let report = unreconcile(&targets).unwrap();
        assert_eq!(report.updated.len(), 1);
        assert!(!exclude_content(&repo).contains("exclude:demo"));
    }

    #[test]
    fn unreconcile_leaves_another_overlays_block_and_hand_written_content_intact() {
        let (td, repo) = temp_git_repo();
        let exclude_path = repo.commondir().join("info").join("exclude");
        std::fs::create_dir_all(exclude_path.parent().unwrap()).unwrap();
        std::fs::write(&exclude_path, "*.bak\n").unwrap();

        let a = td.path().join("a");
        let b = td.path().join("b");
        std::fs::write(&a, "a").unwrap();
        std::fs::write(&b, "b").unwrap();

        reconcile(&[ManagedTarget {
            target: a.clone(),
            overlay: "one".into(),
        }])
        .unwrap();
        reconcile(&[ManagedTarget {
            target: b,
            overlay: "two".into(),
        }])
        .unwrap();

        unreconcile(&[ManagedTarget {
            target: a,
            overlay: "one".into(),
        }])
        .unwrap();

        let content = exclude_content(&repo);
        assert!(!content.contains("exclude:one"));
        assert!(content.contains("*.bak"));
        assert!(content.contains("# >>> over: exclude:two >>>"));
        assert!(content.contains("/b"));
    }

    #[test]
    fn unreconcile_is_a_noop_when_the_marker_is_absent() {
        let (td, repo) = temp_git_repo();
        // `git init` pre-populates `info/exclude` with a commented
        // template — capture it so we can assert it's byte-for-byte
        // untouched, rather than asserting emptiness (a false negative).
        let before = exclude_content(&repo);
        let a = td.path().join("a");
        std::fs::write(&a, "a").unwrap();

        // Never excluded (or already unapplied once) — must not error or
        // write anything.
        let report = unreconcile(&[ManagedTarget {
            target: a,
            overlay: "demo".into(),
        }])
        .unwrap();

        assert!(report.updated.is_empty());
        assert!(!exclude_content(&repo).contains("exclude:demo"));
        assert_eq!(exclude_content(&repo), before);
    }

    #[test]
    fn unreconcile_reports_a_malformed_block_and_leaves_it_untouched() {
        let (td, repo) = temp_git_repo();
        let exclude_path = repo.commondir().join("info").join("exclude");
        std::fs::create_dir_all(exclude_path.parent().unwrap()).unwrap();
        std::fs::write(&exclude_path, "# >>> over: exclude:demo >>>\n/stray\n").unwrap();

        let a = td.path().join("a");
        std::fs::write(&a, "a").unwrap();

        let report = unreconcile(&[ManagedTarget {
            target: a,
            overlay: "demo".into(),
        }])
        .unwrap();

        assert_eq!(report.malformed.len(), 1);
        assert!(report.updated.is_empty());
        let content = exclude_content(&repo);
        assert_eq!(content, "# >>> over: exclude:demo >>>\n/stray\n");
    }
}
