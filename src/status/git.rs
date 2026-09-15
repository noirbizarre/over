//! Git checkout status (#12): inspects the actual git repository/worktree
//! on disk for [`MaterializationIntent::Checkout`](crate::desired::MaterializationIntent::Checkout)
//! entries, reusing git's own semantics (`repo.state()`, `repo.statuses()`,
//! `repo.graph_ahead_behind()`) rather than reimplementing clean/dirty/
//! ahead/behind/diverged detection.
//!
//! `Checkout` has a registered [`crate::materialize::Materializer`]
//! (`CheckoutMaterializer`, #110) like every other intent, but this module
//! only ever *reads* whatever `actions::git::clone_repositories` (called
//! directly by `Overlay::apply`, before the `Plan`/`Materializer` pipeline
//! runs) already put on disk — content-level sync stays `over sync`'s job.
//!
//! **Root vs. declared entries (#140, ADR-021):** [`Provenance::Git`]'s
//! `repo_key` distinguishes the overlay's own root checkout (`repo_key ==
//! `[`ROOT_PATH`](crate::actions::git::config::ROOT_PATH)`, a genuine
//! bidirectional sync surface, ADR-014) from any other declared repository
//! (an opaque, provisioned-only resource — e.g. a plugin-manager clone at a
//! subpath). [`inspect`] branches on that key: the root entry keeps the
//! full content status this module always computed
//! ([`inspect_content`]); a declared entry gets [`inspect_declared`]
//! instead, which never looks at dirty/ahead/behind/merge-in-progress state
//! — only presence and whether the declared configuration (url/tag/rev/
//! remotes/git config) is applied.
//!
//! [`inspect_content`] stays `pub(crate)` and is reused, unconditionally,
//! by `crate::unapply`'s destructive removal-safety gate for **every**
//! `Checkout` entry, root or declared — deleting a declared repo's whole
//! directory tree is exactly as irreversible as deleting the root
//! checkout's, so that gate must never be relaxed just because `status`/
//! `diff`'s *reporting* got more lenient for declared entries.

use std::path::Path;

use anyhow::Result;
use git2::{Branch, Repository, RepositoryState};

use crate::actions::git::config::{GitRepoConfig, ROOT_PATH};
use crate::actions::git::detect_default_branch;
use crate::desired::{DesiredEntry, Provenance};

use super::Status;

/// Inspect the git state behind a `Provenance::Git` entry, dispatching on
/// whether it's the overlay's own root checkout or a declared (subpath)
/// repository — see the module doc for the full rationale.
///
/// Only ever called for entries carrying
/// [`MaterializationIntent::Checkout`](crate::desired::MaterializationIntent::Checkout),
/// which `DesiredTree::build`/`build_own` only ever pair with
/// `Provenance::Git`.
pub fn inspect(entry: &DesiredEntry) -> Result<Status> {
    let Provenance::Git {
        repo_key, config, ..
    } = &entry.provenance
    else {
        unreachable!(
            "only Provenance::Git entries carry MaterializationIntent::Checkout \
             (see desired::tree::collect_own_entries)"
        );
    };

    if repo_key == ROOT_PATH {
        inspect_content(entry)
    } else {
        inspect_declared(&entry.target, config)
    }
}

/// Full git working-tree content status (dirty/ahead/behind/diverged/
/// merge-in-progress), computed purely from the checkout's own upstream
/// tracking branch — exactly what this module always computed before
/// #140.
///
/// `pub(crate)`: used directly by [`inspect`] for the root sync surface,
/// and by `crate::unapply` for its removal-safety gate on *every* `Checkout`
/// entry (root or declared) — see the module doc.
pub(crate) fn inspect_content(entry: &DesiredEntry) -> Result<Status> {
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

/// Provisioning status for a declared (non-root) repository (#140,
/// ADR-021): presence + whether the declared configuration is applied —
/// never dirty/ahead/behind/diverged/merge-in-progress, which is
/// unrelated content a plugin manager (or the user) is free to mutate
/// without `over status`/`over diff` treating it as something needing
/// attention.
fn inspect_declared(target: &Path, config: &GitRepoConfig) -> Result<Status> {
    let is_bare = config.worktree || config.worktrees.is_some();
    if !is_bare {
        if !target.exists() {
            return Ok(Status::Missing);
        }
        let repo = match Repository::open(target) {
            Ok(r) => r,
            Err(_) => return Ok(Status::Broken),
        };
        return declared_config_status(&repo, config, true);
    }

    let bare_path = target.join(".git");
    if !bare_path.exists() {
        return Ok(Status::Missing);
    }
    let repo = match Repository::open_bare(&bare_path) {
        Ok(r) => r,
        Err(_) => return Ok(Status::Broken),
    };

    // Structural presence only: every explicitly named worktree directory
    // must exist, and — when `worktree: true` auto-creates an unnamed
    // default-branch worktree — that one too. Never inspects *content* of
    // any worktree, unlike `inspect_content`'s aggregation above.
    if let Some(worktrees) = &config.worktrees {
        for name in worktrees.keys() {
            if !target.join(name).exists() {
                return Ok(Status::Missing);
            }
        }
    } else if config.worktree {
        let default_branch = detect_default_branch(&repo)?;
        if !target.join(&default_branch).exists() {
            return Ok(Status::Missing);
        }
    }

    // Bare repos never checkout a tag/rev (`actions::git::checkout_ref` is
    // only ever called for non-bare repos) — `check_ref: false`.
    declared_config_status(&repo, config, false)
}

/// Whether a declared repository's configuration invariants are satisfied:
/// the declared `url` (as the `origin` remote), `tag`/`rev` (non-bare
/// only), extra `remotes`, and arbitrary `config` entries. Returns the
/// first mismatch found as [`Status::Conflict`] ("configuration drift"),
/// or [`Status::Applied`] once every declared invariant holds — regardless
/// of anything else in the repository (extra files, local commits,
/// uncommitted changes, unrelated branches).
///
/// Deliberately does **not** check `config.branch`: `actions::git`'s own
/// `EnsureGitRepository` only ever applies a branch at clone time (via
/// `RepoBuilder::branch`), never reconciling it afterwards — re-checking
/// out a different branch on an existing repository risks discarding local
/// work, so `over apply` itself never does it. Reporting a "drift" here
/// that `apply` can never fix would be misleading. Also deliberately
/// coarse about `remotes`/`config` beyond url/key-value equality (no
/// per-remote `push`/`tagopt`/`extras`, no `worktree_config`) — a natural
/// follow-up, not required to satisfy #140's acceptance criteria.
fn declared_config_status(
    repo: &Repository,
    config: &GitRepoConfig,
    check_ref: bool,
) -> Result<Status> {
    if !config.url.is_empty() {
        let matches = repo
            .find_remote("origin")
            .ok()
            .is_some_and(|r| r.url().ok() == Some(config.url.as_str()));
        if !matches {
            return Ok(Status::Conflict);
        }
    }

    if check_ref {
        let head_commit = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
        if let Some(head_commit) = head_commit {
            if let Some(tag) = &config.tag {
                let expected = repo
                    .revparse_single(&format!("refs/tags/{tag}"))
                    .ok()
                    .and_then(|o| o.peel_to_commit().ok());
                if expected.is_none_or(|c| c.id() != head_commit.id()) {
                    return Ok(Status::Conflict);
                }
            } else if let Some(rev) = &config.rev {
                let expected = repo
                    .revparse_single(rev)
                    .ok()
                    .and_then(|o| o.peel_to_commit().ok());
                if expected.is_none_or(|c| c.id() != head_commit.id()) {
                    return Ok(Status::Conflict);
                }
            }
        }
    }

    if let Some(remotes) = &config.remotes {
        for (name, remote_config) in remotes {
            let matches = repo
                .find_remote(name)
                .ok()
                .is_some_and(|r| r.url().ok() == Some(remote_config.url.as_str()));
            if !matches {
                return Ok(Status::Conflict);
            }
        }
    }

    if let Some(git_config) = &config.config {
        let cfg = repo.config()?;
        for (key, value) in &git_config.entries {
            let matches = cfg.get_string(key).ok().as_deref() == Some(value.as_str());
            if !matches {
                return Ok(Status::Conflict);
            }
        }
    }

    Ok(Status::Applied)
}

/// Inspect a single plain (or worktree) checkout directory. Worktree `.git`
/// *files* (not directories) redirecting to the shared bare repo are
/// followed transparently by `Repository::open`, so this same logic covers
/// both plain and worktree checkouts.
///
/// `pub(crate)`: reused directly by `materialize::symlink` (#129) to detect
/// a `checkout -> symlink` rule-change migration from a bare `&Path`,
/// without needing a `Provenance::Git`-carrying `DesiredEntry` — the exact
/// same safety gate `CheckoutMaterializer::classify` already relies on via
/// [`inspect`], not re-derived.
pub(crate) fn inspect_checkout(path: &Path) -> Result<Status> {
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
/// untracked, ignored files excluded). `pub(crate)`: reused by `crate::sync`
/// to refuse pulling into a dirty checkout.
pub(crate) fn is_dirty(repo: &Repository) -> Result<bool> {
    let mut opts = git2::StatusOptions::new();
    opts.include_ignored(false).include_untracked(true);
    Ok(!repo.statuses(Some(&mut opts))?.is_empty())
}

/// Ahead/behind counts of `HEAD` against its upstream tracking branch.
/// `None` when `HEAD` is detached or has no configured upstream — neither
/// is an error, just nothing to compare against. `pub(crate)`: reused by
/// `crate::sync` to decide whether a push is needed after a successful
/// pull.
pub(crate) fn ahead_behind(repo: &Repository) -> Result<Option<(usize, usize)>> {
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
            permissions: None,
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

    // ── declared (non-root) repositories: provisioning status, not content
    // status (#140, ADR-021) ─────────────────────────────────────────────

    /// Same shape as `entry()`, but with a non-`"."` `repo_key` — the exact
    /// distinction `inspect` branches on.
    fn declared_entry(target: std::path::PathBuf, config: GitRepoConfig) -> DesiredEntry {
        DesiredEntry {
            target,
            provenance: Provenance::Git {
                overlay: "ov".to_string(),
                repo_key: ".config/nvim".to_string(),
                config: Box::new(config),
            },
            intent: crate::desired::MaterializationIntent::Checkout,
            permissions: None,
        }
    }

    #[test]
    fn declared_repo_missing_is_missing() {
        let td = TempDir::new().unwrap();
        let target = td.path().join("does-not-exist");
        let e = declared_entry(target, git_config(false, None));
        assert_eq!(inspect(&e).unwrap(), Status::Missing);
    }

    #[test]
    fn declared_repo_with_correct_config_is_applied() {
        let td = TempDir::new().unwrap();
        let repo = init_committed_repo(td.path());
        repo.remote("origin", "https://example.com/plugin.git")
            .unwrap();
        let mut config = git_config(false, None);
        config.url = "https://example.com/plugin.git".to_string();
        let e = declared_entry(td.path().to_path_buf(), config);
        assert_eq!(inspect(&e).unwrap(), Status::Applied);
    }

    #[test]
    fn declared_repo_with_uncommitted_changes_is_still_applied() {
        // The whole point of #140: a plugin manager (or the user) writing
        // to a declared repo's working tree must never make `over status`
        // treat it as needing attention.
        let td = TempDir::new().unwrap();
        let repo = init_committed_repo(td.path());
        repo.remote("origin", "https://example.com/plugin.git")
            .unwrap();
        fs::write(td.path().join("README.md"), "changed by the plugin").unwrap();
        fs::write(td.path().join("new_file.txt"), "extra content").unwrap();
        let mut config = git_config(false, None);
        config.url = "https://example.com/plugin.git".to_string();
        let e = declared_entry(td.path().to_path_buf(), config);
        assert_eq!(inspect(&e).unwrap(), Status::Applied);
    }

    #[test]
    fn declared_repo_with_local_commits_ahead_of_upstream_is_still_applied() {
        let td = TempDir::new().unwrap();
        let repo = init_committed_repo(td.path());
        repo.remote("origin", "https://example.com/plugin.git")
            .unwrap();
        let sig = Signature::now("Test", "test@test.com").unwrap();
        fs::write(td.path().join("local.txt"), "local").unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "local commit", &tree, &[&head])
            .unwrap();

        let mut config = git_config(false, None);
        config.url = "https://example.com/plugin.git".to_string();
        let e = declared_entry(td.path().to_path_buf(), config);
        assert_eq!(inspect(&e).unwrap(), Status::Applied);
    }

    #[test]
    fn declared_repo_with_merge_in_progress_is_still_applied() {
        // `repo.state()` isn't even consulted for a declared repo — unlike
        // `inspect_content`'s `Status::Conflict` for the same on-disk shape.
        let td = TempDir::new().unwrap();
        let repo = init_committed_repo(td.path());
        let head_oid = repo.head().unwrap().target().unwrap();
        fs::write(repo.path().join("MERGE_HEAD"), head_oid.to_string()).unwrap();

        let e = declared_entry(td.path().to_path_buf(), git_config(false, None));
        assert_eq!(inspect(&e).unwrap(), Status::Applied);
    }

    #[test]
    fn declared_repo_with_wrong_origin_url_is_conflict() {
        let td = TempDir::new().unwrap();
        let repo = init_committed_repo(td.path());
        repo.remote("origin", "https://example.com/old-plugin.git")
            .unwrap();
        let mut config = git_config(false, None);
        config.url = "https://example.com/new-plugin.git".to_string();
        let e = declared_entry(td.path().to_path_buf(), config);
        assert_eq!(inspect(&e).unwrap(), Status::Conflict);
    }

    #[test]
    fn declared_repo_with_wrong_tag_is_conflict() {
        let td = TempDir::new().unwrap();
        let repo = init_committed_repo(td.path());
        let sig = Signature::now("Test", "test@test.com").unwrap();
        let first_commit = repo.head().unwrap().peel_to_commit().unwrap();
        repo.tag("v1.0.0", first_commit.as_object(), &sig, "release", false)
            .unwrap();
        // A second commit moves HEAD away from the tag.
        fs::write(td.path().join("file.txt"), "content").unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "second", &tree, &[&first_commit])
            .unwrap();

        let mut config = git_config(false, None);
        config.tag = Some("v1.0.0".to_string());
        let e = declared_entry(td.path().to_path_buf(), config);
        assert_eq!(inspect(&e).unwrap(), Status::Conflict);
    }

    #[test]
    fn declared_repo_missing_declared_remote_is_conflict() {
        let td = TempDir::new().unwrap();
        init_committed_repo(td.path());
        let mut remotes = HashMap::new();
        remotes.insert(
            "upstream".to_string(),
            crate::actions::git::config::RemoteConfig {
                url: "https://example.com/upstream.git".to_string(),
                fetch: None,
                push: None,
                tagopt: None,
                extras: HashMap::new(),
            },
        );
        let mut config = git_config(false, None);
        config.remotes = Some(remotes);
        let e = declared_entry(td.path().to_path_buf(), config);
        assert_eq!(inspect(&e).unwrap(), Status::Conflict);
    }

    #[test]
    fn declared_repo_missing_declared_config_entry_is_conflict() {
        let td = TempDir::new().unwrap();
        init_committed_repo(td.path());
        let mut entries = HashMap::new();
        entries.insert("core.autocrlf".to_string(), "true".to_string());
        let mut config = git_config(false, None);
        config.config = Some(crate::actions::git::config::GitConfig { entries });
        let e = declared_entry(td.path().to_path_buf(), config);
        assert_eq!(inspect(&e).unwrap(), Status::Conflict);
    }

    #[test]
    fn declared_repo_branch_drift_is_not_checked() {
        // `config.branch` is deliberately never verified for a declared
        // repo (`EnsureGitRepository` itself never reconciles it after the
        // initial clone) — documents the scope decision so it doesn't
        // regress into a false "conflict" later.
        let td = TempDir::new().unwrap();
        init_committed_repo(td.path());
        let mut config = git_config(false, None);
        config.branch = Some("some-other-branch-entirely".to_string());
        let e = declared_entry(td.path().to_path_buf(), config);
        assert_eq!(inspect(&e).unwrap(), Status::Applied);
    }

    #[test]
    fn declared_bare_worktree_repo_with_dirty_worktree_is_still_applied() {
        let source_td = TempDir::new().unwrap();
        init_committed_repo(source_td.path());
        let dest_td = TempDir::new().unwrap();
        clone_bare_with_main_worktree(source_td.path(), dest_td.path());
        fs::write(dest_td.path().join("main/README.md"), "dirtied").unwrap();

        let e = declared_entry(dest_td.path().to_path_buf(), git_config(true, None));
        assert_eq!(inspect(&e).unwrap(), Status::Applied);
    }

    #[test]
    fn declared_bare_worktree_missing_named_worktree_dir_is_missing() {
        let source_td = TempDir::new().unwrap();
        init_committed_repo(source_td.path());
        let dest_td = TempDir::new().unwrap();
        clone_bare_with_main_worktree(source_td.path(), dest_td.path());

        let mut worktrees = HashMap::new();
        worktrees.insert(
            "dev".to_string(),
            crate::actions::git::config::WorktreeEntry {
                branch: "main".to_string(),
                config: None,
            },
        );
        let e = declared_entry(
            dest_td.path().to_path_buf(),
            git_config(true, Some(worktrees)),
        );
        assert_eq!(inspect(&e).unwrap(), Status::Missing);
    }
}
