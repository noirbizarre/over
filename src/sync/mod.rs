//! Bidirectional checkout synchronization (#110): `over sync`'s domain
//! logic, driven by the CLI (`crate::cli::sync`).
//!
//! Scope, matching the roadmap (#112): only an overlay's **root** `git`
//! entry (`overlay.git["."]`, keyed by
//! [`ROOT_PATH`](crate::actions::git::config::ROOT_PATH)) is a
//! bidirectional sync surface — "the overlay itself materialized as a
//! checkout". Any subpath entry (a plugin-manager repo, etc.) is a
//! "git repository declared inside an overlay": `over` ensures it's
//! present/configured (`crate::materialize::CheckoutMaterializer`,
//! `actions::git::clone_repositories`) but never fetches/merges/pushes its
//! content here.
//!
//! Mutating primitives (fetch/merge/push/abort) live in
//! `actions::git::sync`; this module only orchestrates: finds every root
//! checkout entry in a [`DesiredTree`], expands a bare+worktrees config
//! into one independent unit per worktree directory that actually exists
//! on disk, and drives each unit through those primitives, persisting an
//! informational checkpoint (`state`) along the way.

pub mod state;

use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use git2::{Repository, RepositoryState};

use crate::actions::git::config::ROOT_PATH;
use crate::actions::git::sync as git_sync;
use crate::desired::{DesiredEntry, DesiredTree, MaterializationIntent, Provenance};
use crate::status::git as status_git;
use crate::ui::{emojis, style};
use crate::utils::short_path;
use crate::xdg::XdgDirs;
use crate::xdg::state::StateFile;

use state::{CheckoutRecord, SyncState};

/// What `over sync` should attempt for every checkout it finds.
#[derive(Debug, Clone, Copy)]
pub struct SyncOptions {
    pub pull: bool,
    pub push: bool,
    /// Abort an in-progress merge instead of syncing. Mutually exclusive
    /// with `pull`/`push` in practice (the CLI never sets both), but kept
    /// as a plain flag rather than a separate enum so `SyncOptions` stays
    /// one small, uniform struct.
    pub abort: bool,
    /// Never perform network I/O or mutate anything — mirrors
    /// `EnsureGitRepository`'s own `!ctx.dry_run` gate on cloning.
    pub dry_run: bool,
}

impl Default for SyncOptions {
    fn default() -> Self {
        Self {
            pull: true,
            push: true,
            abort: false,
            dry_run: false,
        }
    }
}

/// What happened to a single checkout/worktree during `over sync`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncOutcome {
    /// Already level with (or ahead of) its upstream; nothing to pull.
    UpToDate { path: PathBuf },
    /// Local branch had no divergent commits; its ref was moved forward.
    FastForwarded { path: PathBuf },
    /// A three-way merge was performed and committed cleanly.
    Merged { path: PathBuf },
    /// Local commits (from a prior state, or just created by a pull's
    /// merge/fast-forward) were pushed to `origin`.
    Pushed { path: PathBuf },
    /// A merge produced real conflict markers; resolve manually (`git add`
    /// + `git commit`) and re-run `over sync`, or run `over sync --abort`.
    Conflict { path: PathBuf },
    /// Refused to act — dirty working tree, no sync in progress for
    /// `--abort`, or an unresolved non-conflicted merge state.
    Blocked { path: PathBuf, reason: String },
    /// `--abort` cleared an in-progress merge, restoring the pre-merge
    /// working tree.
    Aborted { path: PathBuf },
}

impl SyncOutcome {
    /// Whether this outcome needs the user's attention — used by the CLI
    /// to decide the process exit code, mirroring
    /// `status::Status::needs_attention`.
    pub fn needs_attention(&self) -> bool {
        matches!(
            self,
            SyncOutcome::Conflict { .. } | SyncOutcome::Blocked { .. }
        )
    }

    fn path(&self) -> &Path {
        match self {
            SyncOutcome::UpToDate { path }
            | SyncOutcome::FastForwarded { path }
            | SyncOutcome::Merged { path }
            | SyncOutcome::Pushed { path }
            | SyncOutcome::Conflict { path }
            | SyncOutcome::Blocked { path, .. }
            | SyncOutcome::Aborted { path } => path,
        }
    }

    /// Short, stable label used for both display and the informational
    /// `state::CheckoutRecord::last_outcome` field.
    fn label(&self) -> &'static str {
        match self {
            SyncOutcome::UpToDate { .. } => "up to date",
            SyncOutcome::FastForwarded { .. } => "fast-forwarded",
            SyncOutcome::Merged { .. } => "merged",
            SyncOutcome::Pushed { .. } => "pushed",
            SyncOutcome::Conflict { .. } => "conflict",
            SyncOutcome::Blocked { .. } => "blocked",
            SyncOutcome::Aborted { .. } => "aborted",
        }
    }
}

impl fmt::Display for SyncOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let target = short_path(&self.path().to_string_lossy());
        match self {
            SyncOutcome::UpToDate { .. } => {
                write!(
                    f,
                    "{} {} {}",
                    emojis::CHECKMARK,
                    style::white("up to date:"),
                    target
                )
            }
            SyncOutcome::FastForwarded { .. } => write!(
                f,
                "{} {} {}",
                emojis::CHECKMARK,
                style::white("fast-forwarded:"),
                target
            ),
            SyncOutcome::Merged { .. } => {
                write!(
                    f,
                    "{} {} {}",
                    emojis::CHECKMARK,
                    style::white("merged:"),
                    target
                )
            }
            SyncOutcome::Pushed { .. } => {
                write!(
                    f,
                    "{} {} {}",
                    emojis::SPARKLE,
                    style::white("pushed:"),
                    target
                )
            }
            SyncOutcome::Conflict { .. } => write!(
                f,
                "{} {} {} ({})",
                emojis::WARNING,
                style::yellow("conflict:"),
                target,
                style::white("resolve manually, then re-run `over sync`, or `over sync --abort`"),
            ),
            SyncOutcome::Blocked { reason, .. } => write!(
                f,
                "{} {} {} ({})",
                emojis::WARNING,
                style::yellow("blocked:"),
                target,
                reason,
            ),
            SyncOutcome::Aborted { .. } => {
                write!(
                    f,
                    "{} {} {}",
                    emojis::CROSSMARK,
                    style::white("aborted:"),
                    target
                )
            }
        }
    }
}

/// One independently-syncable checkout: either the whole (non-bare)
/// checkout, or one worktree of a bare+worktrees repository.
struct SyncUnit {
    path: PathBuf,
    overlay: String,
    repo_key: String,
    worktree: Option<String>,
}

/// Find every root (`ROOT_PATH`) [`MaterializationIntent::Checkout`] entry
/// in `desired`, expand each into its sync units, and drive every unit
/// through [`SyncOptions`]. Never touches subpath `git` entries — those
/// stay opaque resources, per this module's scope (see module doc).
pub async fn sync(desired: &DesiredTree, opts: &SyncOptions) -> Result<Vec<SyncOutcome>> {
    let state_file: StateFile<SyncState> =
        StateFile::new(XdgDirs::new()?.state_dir().join("sync.toml"));

    let mut outcomes = Vec::new();
    for entry in desired.entries() {
        if !matches!(entry.intent, MaterializationIntent::Checkout) {
            continue;
        }
        let Provenance::Git { repo_key, .. } = &entry.provenance else {
            unreachable!(
                "Checkout entries only ever carry Provenance::Git \
                 (see desired::tree::collect_own_entries)"
            );
        };
        if repo_key != ROOT_PATH {
            continue;
        }

        for unit in sync_units(entry)? {
            let outcome = sync_unit(&unit, opts)?;
            if !opts.dry_run {
                persist_checkpoint(&state_file, &unit, &outcome).await?;
            }
            outcomes.push(outcome);
        }
    }

    Ok(outcomes)
}

/// Expand a root `Checkout` entry into its independent sync units: one for
/// a plain checkout, or one per worktree directory that actually exists on
/// disk for a bare+worktrees repository — mirrors
/// `status::git::inspect`'s own worktree aggregation, except each worktree
/// is synced on its own rather than reduced to a single severity-ranked
/// status.
fn sync_units(entry: &DesiredEntry) -> Result<Vec<SyncUnit>> {
    let Provenance::Git {
        overlay,
        repo_key,
        config,
    } = &entry.provenance
    else {
        unreachable!(
            "Checkout entries only ever carry Provenance::Git \
             (see desired::tree::collect_own_entries)"
        );
    };

    let is_bare = config.worktree || config.worktrees.is_some();
    if !is_bare {
        return Ok(vec![SyncUnit {
            path: entry.target.clone(),
            overlay: overlay.clone(),
            repo_key: repo_key.clone(),
            worktree: None,
        }]);
    }

    let bare_path = entry.target.join(".git");
    if !bare_path.exists() {
        // Not cloned yet — `over apply`/`CheckoutMaterializer` handles
        // that; nothing to sync until it exists.
        return Ok(Vec::new());
    }
    let repo = Repository::open_bare(&bare_path)
        .with_context(|| format!("failed to open bare repository at {}", bare_path.display()))?;

    let mut units = Vec::new();
    for name in repo.worktrees()?.iter().flatten().flatten() {
        let wt_path = entry.target.join(name);
        if wt_path.exists() {
            units.push(SyncUnit {
                path: wt_path,
                overlay: overlay.clone(),
                repo_key: repo_key.clone(),
                worktree: Some(name.to_string()),
            });
        }
    }
    Ok(units)
}

/// Drive a single [`SyncUnit`] through pull/push, never touching anything
/// if the working tree is dirty or a prior merge is unresolved.
fn sync_unit(unit: &SyncUnit, opts: &SyncOptions) -> Result<SyncOutcome> {
    let path = unit.path.clone();
    let repo = Repository::open(&path)
        .with_context(|| format!("failed to open git repository at {}", path.display()))?;

    if repo.state() != RepositoryState::Clean {
        return handle_unresolved_state(&repo, &path, opts);
    }

    if opts.abort {
        return Ok(SyncOutcome::Blocked {
            path,
            reason: "no sync in progress".to_string(),
        });
    }

    if opts.pull && status_git::is_dirty(&repo)? {
        // Never touch a dirty working tree, implicitly or otherwise.
        return Ok(SyncOutcome::Blocked {
            path,
            reason: "uncommitted changes; commit or stash before syncing".to_string(),
        });
    }

    if opts.dry_run {
        // No network I/O under dry-run (mirrors `EnsureGitRepository`'s own
        // `!ctx.dry_run` gate on cloning): report using whichever
        // remote-tracking state is already on disk from the last real
        // fetch/clone, without contacting the network.
        let (ahead, behind) = status_git::ahead_behind(&repo)?.unwrap_or((0, 0));
        return Ok(if opts.pull && behind > 0 {
            SyncOutcome::FastForwarded { path }
        } else if opts.push && ahead > 0 {
            SyncOutcome::Pushed { path }
        } else {
            SyncOutcome::UpToDate { path }
        });
    }

    let mut outcome = SyncOutcome::UpToDate { path: path.clone() };

    if opts.pull {
        outcome = match git_sync::pull(&repo)? {
            git_sync::PullOutcome::NoUpstream | git_sync::PullOutcome::UpToDate => {
                SyncOutcome::UpToDate { path: path.clone() }
            }
            git_sync::PullOutcome::FastForwarded => {
                SyncOutcome::FastForwarded { path: path.clone() }
            }
            git_sync::PullOutcome::Merged => SyncOutcome::Merged { path: path.clone() },
            // Never push on top of an unresolved conflict.
            git_sync::PullOutcome::Conflict => {
                return Ok(SyncOutcome::Conflict { path: path.clone() });
            }
        };
    }

    if opts.push {
        let ahead = status_git::ahead_behind(&repo)?.map_or(0, |(ahead, _)| ahead);
        if ahead > 0 {
            let branch = current_branch_name(&repo)?;
            git_sync::push_branch(&repo, &branch)?;
            outcome = SyncOutcome::Pushed { path: path.clone() };
        }
    }

    Ok(outcome)
}

/// Handle a repository already mid-merge (or some other non-`Clean`
/// libgit2 state) when `over sync` starts.
fn handle_unresolved_state(
    repo: &Repository,
    path: &Path,
    opts: &SyncOptions,
) -> Result<SyncOutcome> {
    if opts.abort {
        git_sync::abort_merge(repo)?;
        return Ok(SyncOutcome::Aborted {
            path: path.to_path_buf(),
        });
    }
    if repo.index()?.has_conflicts() {
        return Ok(SyncOutcome::Conflict {
            path: path.to_path_buf(),
        });
    }
    Ok(SyncOutcome::Blocked {
        path: path.to_path_buf(),
        reason: "finish resolving and commit, or run `over sync --abort`".to_string(),
    })
}

fn current_branch_name(repo: &Repository) -> Result<String> {
    repo.head()?
        .shorthand()
        .map(str::to_string)
        .context("current branch name is not valid UTF-8")
}

/// Best-effort, purely informational: never read back to decide sync
/// correctness (see module doc and `state`'s own doc comment).
async fn persist_checkpoint(
    state_file: &StateFile<SyncState>,
    unit: &SyncUnit,
    outcome: &SyncOutcome,
) -> Result<()> {
    let key = unit.path.to_string_lossy().to_string();
    let oid = Repository::open(&unit.path).ok().and_then(|repo| {
        repo.head()
            .ok()
            .and_then(|head| head.peel_to_commit().ok())
            .map(|commit| commit.id().to_string())
    });
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let record = CheckoutRecord {
        overlay: unit.overlay.clone(),
        repo_key: unit.repo_key.clone(),
        worktree: unit.worktree.clone(),
        last_synced_oid: oid,
        last_synced_at: Some(now),
        last_outcome: Some(outcome.label().to_string()),
    };
    state_file
        .update(move |s| {
            s.checkouts.insert(key, record);
            Ok(())
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::git::config::GitRepoConfig;
    use assert_fs::TempDir;
    use git2::Signature;
    use std::fs;

    fn git_config(worktree: bool) -> GitRepoConfig {
        GitRepoConfig {
            url: String::new(),
            branch: None,
            tag: None,
            rev: None,
            recurse_submodules: false,
            worktree,
            per_worktree_config: false,
            worktrees: None,
            remotes: None,
            config: None,
            worktree_config: None,
        }
    }

    fn checkout_entry(target: PathBuf, repo_key: &str, config: GitRepoConfig) -> DesiredEntry {
        DesiredEntry {
            target,
            provenance: Provenance::Git {
                overlay: "ov".to_string(),
                repo_key: repo_key.to_string(),
                config: Box::new(config),
            },
            intent: MaterializationIntent::Checkout,
            permissions: None,
        }
    }

    fn init_committed_repo(path: &std::path::Path) {
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
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
    }

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
    fn sync_units_non_bare_is_a_single_unit_at_target() {
        let td = TempDir::new().unwrap();
        let entry = checkout_entry(td.path().to_path_buf(), ".", git_config(false));
        let units = sync_units(&entry).unwrap();
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].path, td.path());
        assert_eq!(units[0].worktree, None);
    }

    #[test]
    fn sync_units_subpath_entry_is_still_expanded_but_never_synced_by_the_caller() {
        // `sync_units` itself doesn't filter by repo_key (that's `sync`'s
        // job) — verify it still just expands based on bare-ness.
        let td = TempDir::new().unwrap();
        let entry = checkout_entry(td.path().to_path_buf(), ".config/nvim", git_config(false));
        let units = sync_units(&entry).unwrap();
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].repo_key, ".config/nvim");
    }

    #[test]
    fn sync_units_bare_without_clone_yet_is_empty() {
        let td = TempDir::new().unwrap();
        let entry = checkout_entry(td.path().join("missing"), ".", git_config(true));
        let units = sync_units(&entry).unwrap();
        assert!(units.is_empty());
    }

    #[test]
    fn sync_ignores_subpath_git_entries() {
        let source_td = TempDir::new().unwrap();
        init_committed_repo(source_td.path());
        let dest_td = TempDir::new().unwrap();
        let checkout_path = dest_td.path().join("checkout");
        clone_with_tracking(source_td.path(), &checkout_path);

        let entry = checkout_entry(checkout_path, ".config/nvim", git_config(false));
        let desired = DesiredTree::from_entries(vec![entry]);

        let outcomes = tokio_test_block_on(sync(&desired, &SyncOptions::default()));
        assert!(outcomes.unwrap().is_empty());
    }

    #[tokio::test]
    async fn sync_root_entry_up_to_date_reports_up_to_date() {
        let source_td = TempDir::new().unwrap();
        init_committed_repo(source_td.path());
        let dest_td = TempDir::new().unwrap();
        let checkout_path = dest_td.path().join("checkout");
        clone_with_tracking(source_td.path(), &checkout_path);

        let entry = checkout_entry(checkout_path.clone(), ".", git_config(false));
        let desired = DesiredTree::from_entries(vec![entry]);

        let outcomes = sync(&desired, &SyncOptions::default()).await.unwrap();
        assert_eq!(outcomes.len(), 1);
        assert_eq!(
            outcomes[0],
            SyncOutcome::UpToDate {
                path: checkout_path
            }
        );
    }

    #[tokio::test]
    async fn sync_blocks_on_dirty_working_tree_without_touching_it() {
        let source_td = TempDir::new().unwrap();
        init_committed_repo(source_td.path());
        let dest_td = TempDir::new().unwrap();
        let checkout_path = dest_td.path().join("checkout");
        clone_with_tracking(source_td.path(), &checkout_path);
        fs::write(checkout_path.join("README.md"), "dirty").unwrap();

        let entry = checkout_entry(checkout_path.clone(), ".", git_config(false));
        let desired = DesiredTree::from_entries(vec![entry]);

        let outcomes = sync(&desired, &SyncOptions::default()).await.unwrap();
        assert_eq!(outcomes.len(), 1);
        assert!(matches!(outcomes[0], SyncOutcome::Blocked { .. }));
        // Untouched: still reports the dirty content, not reverted/merged.
        assert_eq!(
            fs::read_to_string(checkout_path.join("README.md")).unwrap(),
            "dirty"
        );
    }

    #[tokio::test]
    async fn dry_run_performs_no_mutation() {
        let source_td = TempDir::new().unwrap();
        let source_repo = init_committed_repo_ret(source_td.path());
        let dest_td = TempDir::new().unwrap();
        let checkout_path = dest_td.path().join("checkout");
        let checkout_repo = clone_with_tracking(source_td.path(), &checkout_path);

        fs::write(source_td.path().join("new.txt"), "new").unwrap();
        commit_all(&source_repo, "advance");
        // A real (non-dry-run) fetch beforehand, so the checkout's
        // remote-tracking ref already knows about the upstream advance —
        // dry-run itself never performs network I/O (mirrors
        // `EnsureGitRepository`'s own `!ctx.dry_run` gate), so without this
        // it would have no way to know a pull is even available.
        git_sync::fetch_origin(&checkout_repo).unwrap();

        let entry = checkout_entry(checkout_path.clone(), ".", git_config(false));
        let desired = DesiredTree::from_entries(vec![entry]);

        let opts = SyncOptions {
            dry_run: true,
            ..SyncOptions::default()
        };
        let outcomes = sync(&desired, &opts).await.unwrap();
        assert_eq!(outcomes.len(), 1);
        // Reports what *would* happen (behind its now-fetched upstream)...
        assert!(matches!(outcomes[0], SyncOutcome::FastForwarded { .. }));
        // ...without actually updating the checkout's working tree.
        assert!(!checkout_path.join("new.txt").exists());
    }

    #[tokio::test]
    async fn bare_worktrees_are_synced_independently() {
        let source_td = TempDir::new().unwrap();
        init_committed_repo(source_td.path());
        let dest_td = TempDir::new().unwrap();
        let base = dest_td.path().join("checkout");
        fs::create_dir_all(&base).unwrap();
        let bare_path = base.join(".git");
        let bare_repo = git2::build::RepoBuilder::new()
            .bare(true)
            .clone(source_td.path().to_str().unwrap(), &bare_path)
            .unwrap();

        let main_wt = base.join("main");
        {
            let branch_ref = bare_repo
                .find_branch("main", git2::BranchType::Local)
                .unwrap()
                .into_reference();
            let mut opts = git2::WorktreeAddOptions::new();
            opts.reference(Some(&branch_ref));
            bare_repo.worktree("main", &main_wt, Some(&opts)).unwrap();
        }
        {
            let wt_repo = Repository::open(&main_wt).unwrap();
            wt_repo
                .find_branch("main", git2::BranchType::Local)
                .unwrap()
                .set_upstream(Some("origin/main"))
                .unwrap();
        }

        let entry = checkout_entry(base, ".", git_config(true));
        let desired = DesiredTree::from_entries(vec![entry]);

        let outcomes = sync(&desired, &SyncOptions::default()).await.unwrap();
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0], SyncOutcome::UpToDate { path: main_wt });
    }

    // ── test-only helpers duplicated from `actions::git::sync`'s test
    // module (kept local rather than shared across `#[cfg(test)]` boundaries) ──

    fn init_committed_repo_ret(path: &std::path::Path) -> Repository {
        init_committed_repo(path);
        Repository::open(path).unwrap()
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

    fn tokio_test_block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(f)
    }
}
