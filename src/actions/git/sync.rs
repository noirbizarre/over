//! Low-level git2 mutating primitives for bidirectional checkout
//! synchronization (#110). Parallel to the clone/worktree/remote helpers in
//! [`super`]: those ensure a repository's *presence and configuration*;
//! this module is the only place that ever fetches, merges, or pushes
//! *content*.
//!
//! Conflict resolution is never reimplemented here: [`pull`] leaves real
//! git conflict markers in the index/working directory exactly as `git
//! merge` would, and the caller (`crate::sync`) never auto-resolves them —
//! the user resolves with ordinary git tooling (`git add`, `git commit`, or
//! `git merge --abort` — mirrored here by [`abort_merge`]).

use anyhow::{Context as _, Result};
use git2::{FetchOptions, PushOptions, Repository, ResetType, Signature};

use super::remote_callbacks;

/// Outcome of attempting to pull upstream changes into `repo`'s current
/// branch. Never invoked against a dirty working tree or a repository
/// already mid-merge — `crate::sync` checks both before calling [`pull`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PullOutcome {
    /// Detached `HEAD`, or the current branch has no upstream (or its
    /// upstream ref doesn't exist, e.g. never pushed) — nothing to compare
    /// against.
    NoUpstream,
    /// Already level with (or ahead of) the upstream; nothing to merge.
    UpToDate,
    /// Local branch had no divergent commits; its ref was simply moved
    /// forward (no merge commit created).
    FastForwarded,
    /// A real three-way merge was performed and committed cleanly.
    Merged,
    /// The merge produced conflicts: real conflict markers are now in the
    /// working directory and the index, and `repo.state()` reports
    /// `Merging`. Caller must stop (no push) until the user resolves and
    /// commits, or runs [`abort_merge`].
    Conflict,
}

/// Fetch every configured refspec from `origin` (same as plain `git
/// fetch`, no refspec override).
pub(crate) fn fetch_origin(repo: &Repository) -> Result<()> {
    let mut remote = repo
        .find_remote("origin")
        .context("no 'origin' remote configured")?;
    let mut fetch_options = FetchOptions::new();
    fetch_options.remote_callbacks(remote_callbacks()?);
    remote
        .fetch(&[] as &[&str], Some(&mut fetch_options), None)
        .context("failed to fetch 'origin'")?;
    Ok(())
}

/// Fetch `origin`, then fast-forward or three-way-merge `repo`'s current
/// branch with its upstream tracking ref. Reuses git's own merge
/// machinery (`merge_analysis`/`merge`) rather than reimplementing
/// three-way merge.
pub(crate) fn pull(repo: &Repository) -> Result<PullOutcome> {
    let head = repo.head().context("failed to resolve HEAD")?;
    if !head.is_branch() {
        return Ok(PullOutcome::NoUpstream);
    }
    let branch_name = head
        .shorthand()
        .context("current branch name is not valid UTF-8")?
        .to_string();

    let branch = git2::Branch::wrap(head);
    let Ok(upstream) = branch.upstream() else {
        return Ok(PullOutcome::NoUpstream);
    };
    let upstream_refname = upstream
        .get()
        .name()
        .context("upstream ref name is not valid UTF-8")?
        .to_string();
    drop(upstream);
    drop(branch);

    fetch_origin(repo)?;

    // Re-resolve the upstream ref *after* fetching: any handle obtained
    // beforehand points at its pre-fetch target.
    let upstream_ref = repo
        .find_reference(&upstream_refname)
        .with_context(|| format!("upstream ref '{upstream_refname}' not found after fetch"))?;
    let fetch_commit = repo.reference_to_annotated_commit(&upstream_ref)?;

    let (analysis, _preference) = repo.merge_analysis(&[&fetch_commit])?;

    if analysis.is_up_to_date() {
        return Ok(PullOutcome::UpToDate);
    }

    if analysis.is_fast_forward() {
        let branch_refname = format!("refs/heads/{branch_name}");
        let mut branch_ref = repo
            .find_reference(&branch_refname)
            .with_context(|| format!("local branch ref '{branch_refname}' not found"))?;
        branch_ref.set_target(fetch_commit.id(), "over sync: fast-forward")?;
        repo.set_head(&branch_refname)?;
        // Force: the working directory must move from the pre-fetch tree
        // to the fast-forwarded one; safe because the caller only ever
        // reaches this branch on a *clean* working tree (checked before
        // `pull` is invoked).
        repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))?;
        return Ok(PullOutcome::FastForwarded);
    }

    if analysis.is_normal() {
        // Writes conflict markers (if any) straight into the index/working
        // directory and puts the repo in a merging state; does not move
        // HEAD. Caller inspects the index below before deciding whether to
        // commit or leave it for the user.
        repo.merge(&[&fetch_commit], None, None)?;

        let mut index = repo.index()?;
        if index.has_conflicts() {
            return Ok(PullOutcome::Conflict);
        }

        let tree_id = index.write_tree()?;
        let tree = repo.find_tree(tree_id)?;
        let head_commit = repo.head()?.peel_to_commit()?;
        let their_commit = repo.find_commit(fetch_commit.id())?;
        let sig = signature(repo)?;
        let message = format!("Merge '{upstream_refname}' via `over sync`");
        repo.commit(
            Some("HEAD"),
            &sig,
            &sig,
            &message,
            &tree,
            &[&head_commit, &their_commit],
        )?;
        // Clears MERGE_HEAD/MERGE_MSG now that the merge commit exists —
        // mirrors what `git commit` does automatically after a conflict-free
        // `git merge`.
        repo.cleanup_state()?;
        return Ok(PullOutcome::Merged);
    }

    // ANALYSIS_UNBORN (no commits yet) or any other combination we don't
    // special-case: nothing safe to do automatically.
    Ok(PullOutcome::UpToDate)
}

/// Abort an in-progress merge left by a conflicted [`pull`]: resets the
/// index and working directory back to `HEAD` (which a plain `git2::merge`
/// never moves, conflicted or not — confirmed against git2's own docs) and
/// clears `MERGE_HEAD`/`MERGE_MSG`. Mirrors `git merge --abort` without
/// needing to persist a "pre-merge HEAD" anywhere: `HEAD` already *is* the
/// pre-merge commit.
pub(crate) fn abort_merge(repo: &Repository) -> Result<()> {
    let head_commit = repo
        .head()
        .context("failed to resolve HEAD")?
        .peel_to_commit()
        .context("HEAD does not point at a commit")?;
    repo.reset(head_commit.as_object(), ResetType::Hard, None)
        .context("failed to reset the working tree while aborting the merge")?;
    repo.cleanup_state()
        .context("failed to clear merge state (MERGE_HEAD/MERGE_MSG)")?;
    Ok(())
}

/// Push `branch` to `origin` under the same name. Callers only invoke this
/// once they've established the branch is actually ahead of its upstream
/// (`status::git`'s ahead/behind helper) — pushing an up-to-date branch is
/// harmless but wasteful.
pub(crate) fn push_branch(repo: &Repository, branch: &str) -> Result<()> {
    let mut remote = repo
        .find_remote("origin")
        .context("no 'origin' remote configured")?;
    let mut push_options = PushOptions::new();
    push_options.remote_callbacks(remote_callbacks()?);
    let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
    remote
        .push(&[refspec], Some(&mut push_options))
        .with_context(|| format!("failed to push '{branch}' to 'origin'"))?;
    Ok(())
}

/// The signature used for merge commits `over sync` creates. Prefers the
/// checkout's own configured `user.name`/`user.email` (`repo.signature()`,
/// which fails if neither is set) so authorship matches what the user
/// would get from a manual `git merge`; falls back to an `over`-authored
/// identity rather than erroring, since refusing to sync over a missing
/// git identity would be a surprising, easily-hit failure mode for a tool
/// managing the checkout on the user's behalf.
fn signature(repo: &Repository) -> Result<Signature<'static>> {
    match repo.signature() {
        Ok(sig) => Ok(sig),
        Err(_) => Signature::now("over", "over@localhost")
            .context("failed to construct a fallback git signature"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_fs::TempDir;
    use git2::{RepositoryState, Signature as GitSignature};
    use std::fs;

    /// Init a repo with a committer identity and one commit at `README.md`.
    fn init_committed_repo(path: &std::path::Path) -> Repository {
        let repo = Repository::init(path).unwrap();
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "Test").unwrap();
        cfg.set_str("user.email", "test@test.com").unwrap();
        drop(cfg);
        commit_all(&repo, "initial");
        repo
    }

    fn commit_all(repo: &Repository, message: &str) {
        let sig = GitSignature::now("Test", "test@test.com").unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let parents: Vec<_> = repo
            .head()
            .ok()
            .and_then(|h| h.peel_to_commit().ok())
            .into_iter()
            .collect();
        let parent_refs: Vec<&git2::Commit> = parents.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parent_refs)
            .unwrap();
    }

    /// Clone `source` into a fresh local checkout with `main` tracking
    /// `origin/main`, the fixture every test below builds on.
    fn clone_with_tracking(source: &std::path::Path, dest: &std::path::Path) -> Repository {
        let repo = git2::build::RepoBuilder::new()
            .clone(source.to_str().unwrap(), dest)
            .unwrap();
        {
            let mut branch = repo.find_branch("main", git2::BranchType::Local).unwrap();
            branch.set_upstream(Some("origin/main")).unwrap();
        }
        repo
    }

    #[test]
    fn pull_with_no_upstream_reports_no_upstream() {
        let td = TempDir::new().unwrap();
        let repo = init_committed_repo(td.path());
        assert_eq!(pull(&repo).unwrap(), PullOutcome::NoUpstream);
    }

    #[test]
    fn pull_up_to_date_reports_up_to_date() {
        let source_td = TempDir::new().unwrap();
        init_committed_repo(source_td.path());
        let dest_td = TempDir::new().unwrap();
        let repo = clone_with_tracking(source_td.path(), &dest_td.path().join("clone"));
        assert_eq!(pull(&repo).unwrap(), PullOutcome::UpToDate);
    }

    #[test]
    fn pull_fast_forwards_when_only_upstream_moved() {
        let source_td = TempDir::new().unwrap();
        let source_repo = init_committed_repo(source_td.path());
        let dest_td = TempDir::new().unwrap();
        let repo = clone_with_tracking(source_td.path(), &dest_td.path().join("clone"));

        fs::write(source_td.path().join("new.txt"), "new").unwrap();
        commit_all(&source_repo, "advance");

        assert_eq!(pull(&repo).unwrap(), PullOutcome::FastForwarded);
        assert!(dest_td.path().join("clone").join("new.txt").exists());
        assert_eq!(repo.state(), RepositoryState::Clean);
    }

    #[test]
    fn pull_merges_when_both_sides_advanced_without_conflict() {
        let source_td = TempDir::new().unwrap();
        let source_repo = init_committed_repo(source_td.path());
        let dest_td = TempDir::new().unwrap();
        let clone_path = dest_td.path().join("clone");
        let repo = clone_with_tracking(source_td.path(), &clone_path);

        // Diverge: one new file upstream, a different new file locally.
        fs::write(source_td.path().join("upstream.txt"), "upstream").unwrap();
        commit_all(&source_repo, "upstream change");
        fs::write(clone_path.join("local.txt"), "local").unwrap();
        commit_all(&repo, "local change");

        assert_eq!(pull(&repo).unwrap(), PullOutcome::Merged);
        assert!(clone_path.join("upstream.txt").exists());
        assert!(clone_path.join("local.txt").exists());
        assert_eq!(repo.state(), RepositoryState::Clean);
        // A real merge commit with two parents was created.
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(head.parent_count(), 2);
    }

    #[test]
    fn pull_leaves_real_conflict_markers_on_conflicting_changes() {
        let source_td = TempDir::new().unwrap();
        let source_repo = init_committed_repo(source_td.path());
        let dest_td = TempDir::new().unwrap();
        let clone_path = dest_td.path().join("clone");
        let repo = clone_with_tracking(source_td.path(), &clone_path);

        // Same file, conflicting content on both sides.
        fs::write(source_td.path().join("README.md"), "upstream version").unwrap();
        commit_all(&source_repo, "upstream edit");
        fs::write(clone_path.join("README.md"), "local version").unwrap();
        commit_all(&repo, "local edit");

        assert_eq!(pull(&repo).unwrap(), PullOutcome::Conflict);
        assert_eq!(repo.state(), RepositoryState::Merge);
        let content = fs::read_to_string(clone_path.join("README.md")).unwrap();
        assert!(content.contains("<<<<<<<"));
    }

    #[test]
    fn abort_merge_restores_clean_pre_merge_state() {
        let source_td = TempDir::new().unwrap();
        let source_repo = init_committed_repo(source_td.path());
        let dest_td = TempDir::new().unwrap();
        let clone_path = dest_td.path().join("clone");
        let repo = clone_with_tracking(source_td.path(), &clone_path);

        fs::write(source_td.path().join("README.md"), "upstream version").unwrap();
        commit_all(&source_repo, "upstream edit");
        fs::write(clone_path.join("README.md"), "local version").unwrap();
        commit_all(&repo, "local edit");
        // Capture HEAD right before the conflicted merge attempt — this is
        // what `abort_merge` must restore back to (it never moves HEAD
        // itself, by construction).
        let pre_merge_head = repo.head().unwrap().peel_to_commit().unwrap().id();
        assert_eq!(pull(&repo).unwrap(), PullOutcome::Conflict);

        abort_merge(&repo).unwrap();

        assert_eq!(repo.state(), RepositoryState::Clean);
        assert_eq!(
            repo.head().unwrap().peel_to_commit().unwrap().id(),
            pre_merge_head
        );
        assert!(!repo.index().unwrap().has_conflicts());
        let content = fs::read_to_string(clone_path.join("README.md")).unwrap();
        assert_eq!(content, "local version");
    }

    /// Bare "origin" remote seeded with one commit — libgit2's local
    /// transport refuses to push into a non-bare repository ("local push
    /// doesn't (yet) support pushing to non-bare repos"), so every push
    /// test needs a bare destination, unlike the fetch/pull tests above
    /// (fetching *from* a checked-out working directory over the local
    /// transport is fine).
    fn bare_origin_with_initial_commit(bare_path: &std::path::Path) {
        let seed_td = TempDir::new().unwrap();
        init_committed_repo(seed_td.path());
        git2::Repository::init_bare(bare_path).unwrap();
        let seed_repo = Repository::open(seed_td.path()).unwrap();
        let mut remote = seed_repo
            .remote_anonymous(bare_path.to_str().unwrap())
            .unwrap();
        remote
            .push(&["refs/heads/main:refs/heads/main"], None)
            .unwrap();
        // A freshly pushed-to bare repo has no HEAD until one is set.
        let bare_repo = Repository::open_bare(bare_path).unwrap();
        bare_repo.set_head("refs/heads/main").unwrap();
    }

    #[test]
    fn push_branch_sends_local_commits_to_origin() {
        let origin_td = TempDir::new().unwrap();
        let origin_path = origin_td.path().join("origin.git");
        bare_origin_with_initial_commit(&origin_path);

        let dest_td = TempDir::new().unwrap();
        let clone_path = dest_td.path().join("clone");
        let repo = clone_with_tracking(&origin_path, &clone_path);

        fs::write(clone_path.join("local.txt"), "local").unwrap();
        commit_all(&repo, "local change");

        push_branch(&repo, "main").unwrap();

        let origin_repo = Repository::open_bare(&origin_path).unwrap();
        let origin_head = origin_repo
            .find_reference("refs/heads/main")
            .unwrap()
            .peel_to_commit()
            .unwrap();
        let local_head = repo.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(origin_head.id(), local_head.id());
    }
}
