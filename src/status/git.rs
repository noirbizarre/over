//! Git checkout status (#12): inspects the actual git repository/worktree
//! on disk for [`MaterializationIntent::Checkout`](crate::desired::MaterializationIntent::Checkout)
//! entries, reusing git's own semantics (`repo.state()`, `repo.statuses()`,
//! `repo.graph_ahead_behind()`) rather than reimplementing clean/dirty/
//! ahead/behind/diverged detection.
//!
//! No [`crate::materialize::Materializer`] claims `Checkout` yet (#110 owns
//! that), so this only ever *reads* whatever `actions::git::clone_repositories`
//! (called directly by `Overlay::apply`, outside the `Plan`/`Materializer`
//! model) already put on disk.

use std::path::Path;

use anyhow::Result;
use git2::{Branch, Repository, RepositoryState};

use crate::desired::{DesiredEntry, Provenance};

use super::Status;

/// Inspect the git state behind a `Provenance::Git` entry.
///
/// Only ever called for entries carrying
/// [`MaterializationIntent::Checkout`](crate::desired::MaterializationIntent::Checkout),
/// which `DesiredTree::build`/`build_own` only ever pair with
/// `Provenance::Git`.
pub fn inspect(entry: &DesiredEntry) -> Result<Status> {
    let Provenance::Git { config, .. } = &entry.provenance else {
        unreachable!(
            "only Provenance::Git entries carry MaterializationIntent::Checkout \
             (see desired::tree::collect_own_entries)"
        );
    };

    let is_bare = config.worktree || config.worktrees.is_some();
    if !is_bare {
        return inspect_checkout(&entry.target);
    }

    // Bare + worktree(s) (mirrors `actions::git::ensure_worktrees`): the
    // bare repo itself has no working tree to report on, so aggregate
    // every configured worktree's status instead. `DesiredTree` only
    // carries one entry per git map path today, so we reduce to a single
    // `Status` by severity — finer per-worktree reporting is a natural
    // follow-up once #110 gives `Checkout` its own entry per worktree.
    let bare_path = entry.target.join(".git");
    if !bare_path.exists() {
        return Ok(Status::Missing);
    }
    let repo = match Repository::open_bare(&bare_path) {
        Ok(r) => r,
        Err(_) => return Ok(Status::Broken),
    };

    let mut worst: Option<Status> = None;
    // `StringArray::iter()` yields `Result<Option<&str>, Error>` (non-UTF8
    // names surface as `Ok(None)`, invalid arrays as `Err`); the double
    // `flatten()` discards both — worktree names are always plain UTF-8
    // in practice, and a corrupt entry here shouldn't fail the whole
    // aggregation.
    for name in repo.worktrees()?.iter().flatten().flatten() {
        let wt_path = entry.target.join(name);
        if !wt_path.exists() {
            continue;
        }
        let status = inspect_checkout(&wt_path)?;
        worst = Some(match worst {
            Some(current) => merge(current, status),
            None => status,
        });
    }
    // No configured worktree materialized on disk yet: the bare repo
    // exists, but there's nothing to report status *of* — treat the same
    // as a non-worktree checkout that hasn't been cloned.
    Ok(worst.unwrap_or(Status::Missing))
}

/// Inspect a single plain (or worktree) checkout directory. Worktree `.git`
/// *files* (not directories) redirecting to the shared bare repo are
/// followed transparently by `Repository::open`, so this same logic covers
/// both plain and worktree checkouts.
fn inspect_checkout(path: &Path) -> Result<Status> {
    if !path.exists() {
        return Ok(Status::Missing);
    }

    let repo = match Repository::open(path) {
        Ok(r) => r,
        // Something occupies the path but it isn't a valid git repo —
        // e.g. a leftover plain directory, or a corrupted checkout.
        Err(_) => return Ok(Status::Broken),
    };

    // A merge/rebase/cherry-pick in progress takes priority over
    // dirty/ahead/behind — git's own semantics, not reimplemented here.
    if repo.state() != RepositoryState::Clean {
        return Ok(Status::Conflict);
    }

    if is_dirty(&repo)? {
        return Ok(Status::Modified);
    }

    Ok(match ahead_behind(&repo)? {
        None | Some((0, 0)) => Status::Applied,
        Some((ahead, 0)) => Status::Ahead(ahead),
        Some((0, behind)) => Status::Behind(behind),
        Some((ahead, behind)) => Status::Diverged { ahead, behind },
    })
}

/// Whether the working tree has uncommitted changes (tracked or
/// untracked, ignored files excluded).
fn is_dirty(repo: &Repository) -> Result<bool> {
    let mut opts = git2::StatusOptions::new();
    opts.include_ignored(false).include_untracked(true);
    Ok(!repo.statuses(Some(&mut opts))?.is_empty())
}

/// Ahead/behind counts of `HEAD` against its upstream tracking branch.
/// `None` when `HEAD` is detached or has no configured upstream — neither
/// is an error, just nothing to compare against.
fn ahead_behind(repo: &Repository) -> Result<Option<(usize, usize)>> {
    let head = repo.head()?;
    if !head.is_branch() {
        return Ok(None);
    }
    let Some(local_oid) = head.target() else {
        return Ok(None);
    };

    let branch = Branch::wrap(head);
    let Ok(upstream) = branch.upstream() else {
        return Ok(None);
    };
    let Some(upstream_oid) = upstream.get().target() else {
        return Ok(None);
    };

    let (ahead, behind) = repo.graph_ahead_behind(local_oid, upstream_oid)?;
    Ok(Some((ahead, behind)))
}

/// Reduce two statuses to the one that most urgently needs attention,
/// used when aggregating multiple worktrees behind a single `DesiredEntry`.
fn merge(a: Status, b: Status) -> Status {
    if severity(&b) > severity(&a) { b } else { a }
}

/// Higher means more urgent to surface when aggregating statuses.
fn severity(status: &Status) -> u8 {
    match status {
        Status::Applied => 0,
        Status::Ahead(_) => 1,
        Status::Behind(_) => 2,
        Status::Diverged { .. } => 3,
        Status::Modified => 4,
        Status::Missing => 5,
        Status::Broken => 6,
        Status::Conflict => 7,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::git::config::GitRepoConfig;
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    use git2::Signature;
    use std::collections::HashMap;
    use std::fs;

    fn git_config(
        worktree: bool,
        worktrees: Option<HashMap<String, crate::actions::git::config::WorktreeEntry>>,
    ) -> GitRepoConfig {
        GitRepoConfig {
            url: String::new(),
            branch: None,
            tag: None,
            rev: None,
            recurse_submodules: false,
            worktree,
            per_worktree_config: false,
            worktrees,
            remotes: None,
            config: None,
            worktree_config: None,
        }
    }

    fn entry(target: std::path::PathBuf, config: GitRepoConfig) -> DesiredEntry {
        DesiredEntry {
            target,
            provenance: Provenance::Git {
                overlay: "ov".to_string(),
                repo_key: ".".to_string(),
                config: Box::new(config),
            },
            intent: crate::desired::MaterializationIntent::Checkout,
        }
    }

    /// Init a repo with a committer identity and one commit, so `head()`
    /// resolves and `graph_ahead_behind` has something to compare.
    fn init_committed_repo(path: &std::path::Path) -> Repository {
        let repo = Repository::init(path).unwrap();
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
        {
            let tree = repo.find_tree(tree_id).unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
                .unwrap();
        }
        repo
    }

    #[test]
    fn missing_path_is_missing() {
        let td = TempDir::new().unwrap();
        let target = td.path().join("does-not-exist");
        let e = entry(target, git_config(false, None));
        assert_eq!(inspect(&e).unwrap(), Status::Missing);
    }

    #[test]
    fn non_git_directory_is_broken() {
        let td = TempDir::new().unwrap();
        let target = td.child("plain");
        target.create_dir_all().unwrap();
        let e = entry(target.path().to_path_buf(), git_config(false, None));
        assert_eq!(inspect(&e).unwrap(), Status::Broken);
    }

    #[test]
    fn clean_checkout_with_no_upstream_is_applied() {
        let td = TempDir::new().unwrap();
        init_committed_repo(td.path());
        let e = entry(td.path().to_path_buf(), git_config(false, None));
        assert_eq!(inspect(&e).unwrap(), Status::Applied);
    }

    #[test]
    fn dirty_checkout_is_modified() {
        let td = TempDir::new().unwrap();
        init_committed_repo(td.path());
        fs::write(td.path().join("README.md"), "changed").unwrap();
        let e = entry(td.path().to_path_buf(), git_config(false, None));
        assert_eq!(inspect(&e).unwrap(), Status::Modified);
    }

    #[test]
    fn untracked_file_is_modified() {
        let td = TempDir::new().unwrap();
        init_committed_repo(td.path());
        fs::write(td.path().join("new.txt"), "new").unwrap();
        let e = entry(td.path().to_path_buf(), git_config(false, None));
        assert_eq!(inspect(&e).unwrap(), Status::Modified);
    }

    #[test]
    fn merge_in_progress_is_conflict() {
        let td = TempDir::new().unwrap();
        let repo = init_committed_repo(td.path());
        // `repo.state()` reports non-Clean purely from the presence of
        // `MERGE_HEAD` — no need to actually drive a conflicting merge.
        let head_oid = repo.head().unwrap().target().unwrap();
        fs::write(repo.path().join("MERGE_HEAD"), head_oid.to_string()).unwrap();
        let e = entry(td.path().to_path_buf(), git_config(false, None));
        assert_eq!(inspect(&e).unwrap(), Status::Conflict);
    }

    #[test]
    fn ahead_behind_and_diverged_are_detected() {
        let remote_td = TempDir::new().unwrap();
        init_committed_repo(remote_td.path());

        let local_td = TempDir::new().unwrap();
        let local_path = local_td.path().join("local");
        let local_repo = git2::build::RepoBuilder::new()
            .clone(remote_td.path().to_str().unwrap(), &local_path)
            .unwrap();
        // Set up `main`'s upstream to the origin tracking branch cloning
        // already created.
        {
            let mut branch = local_repo
                .find_branch("main", git2::BranchType::Local)
                .unwrap();
            branch.set_upstream(Some("origin/main")).unwrap();
        }

        let e = entry(local_path.clone(), git_config(false, None));
        assert_eq!(inspect(&e).unwrap(), Status::Applied);

        // Ahead: commit locally without pushing.
        let sig = Signature::now("Test", "test@test.com").unwrap();
        fs::write(local_path.join("local.txt"), "local").unwrap();
        {
            let mut index = local_repo.index().unwrap();
            index
                .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
                .unwrap();
            index.write().unwrap();
            let tree_id = index.write_tree().unwrap();
            let tree = local_repo.find_tree(tree_id).unwrap();
            let head = local_repo.head().unwrap().peel_to_commit().unwrap();
            local_repo
                .commit(Some("HEAD"), &sig, &sig, "local commit", &tree, &[&head])
                .unwrap();
        }
        assert_eq!(inspect(&e).unwrap(), Status::Ahead(1));

        // Behind: advance the remote, fetch it into `origin/main`, but
        // reset the local `main` back to the shared ancestor (undoing the
        // "ahead" commit above) so only "behind" is in play.
        let remote_repo = Repository::open(remote_td.path()).unwrap();
        let shared_ancestor = remote_repo.head().unwrap().peel_to_commit().unwrap();
        fs::write(remote_td.path().join("remote.txt"), "remote").unwrap();
        {
            let mut index = remote_repo.index().unwrap();
            index
                .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
                .unwrap();
            index.write().unwrap();
            let tree_id = index.write_tree().unwrap();
            let tree = remote_repo.find_tree(tree_id).unwrap();
            remote_repo
                .commit(
                    Some("HEAD"),
                    &sig,
                    &sig,
                    "remote commit",
                    &tree,
                    &[&shared_ancestor],
                )
                .unwrap();
        }
        local_repo
            .find_remote("origin")
            .unwrap()
            .fetch(&["main"], None, None)
            .unwrap();
        // `shared_ancestor` belongs to `remote_repo`'s object database —
        // look it up by id in `local_repo` (fetch above made it reachable
        // there too) before using it as a reset/commit target.
        let local_shared_ancestor = local_repo.find_commit(shared_ancestor.id()).unwrap();
        // Hard reset (not just moving the ref) so the working tree and
        // index drop `local.txt` too — otherwise it'd linger as an
        // uncommitted addition and this would report `Modified` instead.
        local_repo
            .reset(
                local_shared_ancestor.as_object(),
                git2::ResetType::Hard,
                None,
            )
            .unwrap();
        assert_eq!(inspect(&e).unwrap(), Status::Behind(1));

        // Diverged: local gets its own commit again on top of the shared
        // ancestor, while remote is already one commit ahead.
        fs::write(local_path.join("diverge.txt"), "diverge").unwrap();
        {
            let mut index = local_repo.index().unwrap();
            index
                .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
                .unwrap();
            index.write().unwrap();
            let tree_id = index.write_tree().unwrap();
            let tree = local_repo.find_tree(tree_id).unwrap();
            local_repo
                .commit(
                    Some("HEAD"),
                    &sig,
                    &sig,
                    "diverging commit",
                    &tree,
                    &[&local_shared_ancestor],
                )
                .unwrap();
        }
        assert_eq!(
            inspect(&e).unwrap(),
            Status::Diverged {
                ahead: 1,
                behind: 1
            }
        );
    }

    #[test]
    fn bare_worktree_missing_is_missing() {
        let td = TempDir::new().unwrap();
        let e = entry(td.path().to_path_buf(), git_config(true, None));
        assert_eq!(inspect(&e).unwrap(), Status::Missing);
    }

    /// Bare-clone `source` into `<dest>/.git` and add a `main` worktree at
    /// `<dest>/main`, returning the bare `Repository` handle for further
    /// setup. Shared by every bare+worktree test below.
    fn clone_bare_with_main_worktree(
        source: &std::path::Path,
        dest: &std::path::Path,
    ) -> Repository {
        let bare_path = dest.join(".git");
        let bare_repo = git2::build::RepoBuilder::new()
            .bare(true)
            .clone(source.to_str().unwrap(), &bare_path)
            .unwrap();

        let main_wt = dest.join("main");
        {
            // A bare clone already mirrors `refs/heads/*` from the source,
            // so `main` typically exists as a local branch already — only
            // create it from the remote-tracking ref as a fallback (mirrors
            // `actions::git::create_worktree`'s own local-then-remote
            // lookup). Scoped so `branch_ref`/`opts` (both borrow
            // `bare_repo`) are dropped before it's returned below.
            let branch_ref = match bare_repo.find_branch("main", git2::BranchType::Local) {
                Ok(branch) => branch.into_reference(),
                Err(_) => {
                    let reference = bare_repo
                        .find_reference("refs/remotes/origin/main")
                        .unwrap();
                    let commit = reference.peel_to_commit().unwrap();
                    bare_repo
                        .branch("main", &commit, false)
                        .unwrap()
                        .into_reference()
                }
            };
            let mut opts = git2::WorktreeAddOptions::new();
            opts.reference(Some(&branch_ref));
            bare_repo.worktree("main", &main_wt, Some(&opts)).unwrap();
        }
        bare_repo
    }

    #[test]
    fn bare_worktree_with_one_clean_worktree_is_applied() {
        let source_td = TempDir::new().unwrap();
        init_committed_repo(source_td.path());
        let dest_td = TempDir::new().unwrap();
        clone_bare_with_main_worktree(source_td.path(), dest_td.path());

        let e = entry(dest_td.path().to_path_buf(), git_config(true, None));
        assert_eq!(inspect(&e).unwrap(), Status::Applied);
    }

    #[test]
    fn bare_worktree_aggregates_across_multiple_worktrees() {
        // Two worktrees, one clean and one dirty: the aggregation loop's
        // second iteration must hit the `Some(current) => merge(...)`
        // branch (the first only ever hits `None => status`), and the
        // dirty one's higher severity should win regardless of iteration
        // order.
        let source_td = TempDir::new().unwrap();
        init_committed_repo(source_td.path());
        let dest_td = TempDir::new().unwrap();
        let bare_repo = clone_bare_with_main_worktree(source_td.path(), dest_td.path());

        let second_wt = dest_td.path().join("second");
        {
            let head_commit = bare_repo
                .find_reference("refs/heads/main")
                .unwrap()
                .peel_to_commit()
                .unwrap();
            bare_repo.branch("second", &head_commit, false).unwrap();
            let branch_ref = bare_repo
                .find_branch("second", git2::BranchType::Local)
                .unwrap()
                .into_reference();
            let mut opts = git2::WorktreeAddOptions::new();
            opts.reference(Some(&branch_ref));
            bare_repo
                .worktree("second", &second_wt, Some(&opts))
                .unwrap();
        }
        fs::write(second_wt.join("README.md"), "uncommitted change").unwrap();

        let e = entry(dest_td.path().to_path_buf(), git_config(true, None));
        assert_eq!(inspect(&e).unwrap(), Status::Modified);
    }

    #[test]
    fn bare_worktree_metadata_without_directory_is_missing() {
        // The bare repo still lists "main" as a worktree (its
        // `.git/worktrees/main` metadata is untouched), but the actual
        // checkout directory is gone — e.g. deleted by hand outside
        // `over`. Every configured worktree is then skipped via
        // `continue`, so nothing contributes to `worst` and the entry
        // falls back to `Missing`, same as a never-cloned checkout.
        let source_td = TempDir::new().unwrap();
        init_committed_repo(source_td.path());
        let dest_td = TempDir::new().unwrap();
        clone_bare_with_main_worktree(source_td.path(), dest_td.path());
        fs::remove_dir_all(dest_td.path().join("main")).unwrap();

        let e = entry(dest_td.path().to_path_buf(), git_config(true, None));
        assert_eq!(inspect(&e).unwrap(), Status::Missing);
    }

    #[test]
    fn bare_path_that_is_not_a_valid_repo_is_broken() {
        // `<target>/.git` exists but is a plain file, not a bare
        // repository — `Repository::open_bare` fails, which must not
        // bubble up as an error but report `Broken` instead.
        let td = TempDir::new().unwrap();
        fs::write(td.path().join(".git"), "not a git repo").unwrap();

        let e = entry(td.path().to_path_buf(), git_config(true, None));
        assert_eq!(inspect(&e).unwrap(), Status::Broken);
    }

    #[test]
    fn detached_head_checkout_is_applied() {
        // No branch to compare against an upstream — `ahead_behind`
        // returns `None`, which is not an error, just "nothing to
        // report", same as a clean checkout in sync with its upstream.
        let td = TempDir::new().unwrap();
        let repo = init_committed_repo(td.path());
        let head_commit = repo.head().unwrap().peel_to_commit().unwrap();
        repo.set_head_detached(head_commit.id()).unwrap();

        let e = entry(td.path().to_path_buf(), git_config(false, None));
        assert_eq!(inspect(&e).unwrap(), Status::Applied);
    }

    #[test]
    fn severity_orders_by_urgency() {
        // Matches `severity`'s own ranking: Applied < Ahead < Behind <
        // Diverged < Modified < Missing < Broken < Conflict.
        let ordered = [
            Status::Applied,
            Status::Ahead(1),
            Status::Behind(1),
            Status::Diverged {
                ahead: 1,
                behind: 1,
            },
            Status::Modified,
            Status::Missing,
            Status::Broken,
            Status::Conflict,
        ];
        for window in ordered.windows(2) {
            assert!(
                severity(&window[1]) > severity(&window[0]),
                "{:?} should be more severe than {:?}",
                window[1],
                window[0],
            );
        }
    }

    #[test]
    fn merge_keeps_the_more_severe_status() {
        assert_eq!(merge(Status::Applied, Status::Modified), Status::Modified);
        assert_eq!(merge(Status::Conflict, Status::Applied), Status::Conflict);
        assert_eq!(
            merge(Status::Ahead(1), Status::Behind(1)),
            Status::Behind(1)
        );
    }
}
