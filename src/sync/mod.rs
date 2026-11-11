//! Local virtual-checkout reconciliation and separate declared-checkout synchronization.
//!
//! The CLI uses [`sync_local`] exclusively: overlay-declared Git repositories
//! have independent, user-managed lifecycles (#231). The broader [`sync`] library
//! entry point retains the original #110 behavior described below.
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

use anyhow::{Context as _, Result, anyhow};
use dialoguer::{Confirm, Input};
use git2::{Repository, RepositoryState};

use crate::actions::git::config::ROOT_PATH;
use crate::actions::git::sync as git_sync;
use crate::commit::{self, CommitOptions, CommitOutcome};
use crate::desired::{DesiredEntry, DesiredTree, MaterializationIntent, Provenance};
use crate::materialize::virtual_checkout::git as vc_git;
use crate::materialize::virtual_checkout::state as vc_state;
use crate::status::git as status_git;
use crate::status::{self, Status};
use crate::ui::style::DialogTheme;
use crate::ui::{emojis, style};
use crate::utils::short_path;
use crate::xdg::state::StateFile;

use state::{CheckoutRecord, SyncState};

/// Local reconciliation directions and options for checkout synchronization.
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
    /// Never prompt interactively (#141): a virtual checkout with local
    /// changes is reported as `Blocked` instead of asking whether to
    /// commit them. Has no effect on root `Checkout` entries, which never
    /// prompt in the first place (ADR-014).
    pub no_prompt: bool,
}

impl Default for SyncOptions {
    fn default() -> Self {
        Self {
            pull: true,
            push: true,
            abort: false,
            dry_run: false,
            no_prompt: false,
        }
    }
}

/// What happened to a single checkout/worktree during `over sync`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncOutcome {
    /// Prospective action based on local state; no network or writes occurred.
    Preview { path: PathBuf, action: String },
    /// Already level with (or ahead of) its upstream; nothing to pull.
    UpToDate { path: PathBuf },
    /// Local branch had no divergent commits; its ref was moved forward.
    FastForwarded { path: PathBuf },
    /// A three-way merge was performed and committed cleanly.
    Merged { path: PathBuf },
    /// Local commits (from a prior state, or just created by a pull's
    /// merge/fast-forward) were pushed to `origin`.
    Pushed { path: PathBuf },
    /// A virtual checkout's local file changes were committed into its
    /// source repository after the user confirmed an interactive prompt
    /// (#141) — the virtual-checkout equivalent of `Pushed`, kept as its
    /// own variant since "pushed to origin" would be a misleading label
    /// for "committed locally in the overlay's own source repository".
    Committed { path: PathBuf, files: usize },
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
            | SyncOutcome::Committed { path, .. }
            | SyncOutcome::Conflict { path }
            | SyncOutcome::Blocked { path, .. }
            | SyncOutcome::Aborted { path } => path,
            SyncOutcome::Preview { path, .. } => path,
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
            SyncOutcome::Committed { .. } => "committed",
            SyncOutcome::Conflict { .. } => "conflict",
            SyncOutcome::Blocked { .. } => "blocked",
            SyncOutcome::Aborted { .. } => "aborted",
            SyncOutcome::Preview { .. } => "preview",
        }
    }
}

impl fmt::Display for SyncOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let target = short_path(&self.path().to_string_lossy());
        match self {
            SyncOutcome::Preview { action, .. } => write!(f, "would {action}: {target}"),
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
            SyncOutcome::Committed { files, .. } => write!(
                f,
                "{} {} {} ({} file{} committed)",
                emojis::SPARKLE,
                style::white("committed:"),
                target,
                files,
                if *files == 1 { "" } else { "s" },
            ),
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
    sync_entries(desired, opts, true).await
}

/// Reconcile local working copies without managing overlay-declared Git repositories.
pub async fn sync_local(desired: &DesiredTree, opts: &SyncOptions) -> Result<Vec<SyncOutcome>> {
    sync_entries(desired, opts, false).await
}

async fn sync_entries(
    desired: &DesiredTree,
    opts: &SyncOptions,
    declared_checkouts: bool,
) -> Result<Vec<SyncOutcome>> {
    let state_file: StateFile<SyncState> = state::state_file()?;

    let mut outcomes = Vec::new();
    for entry in desired.entries() {
        match &entry.intent {
            MaterializationIntent::Checkout if declared_checkouts => {
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
            // #141: a virtual checkout is always its own single sync
            // unit — no worktree expansion (that concept is specific to
            // `overlay.git`'s bare+worktrees form), and reconciliation is
            // entirely local (no network I/O), unlike the root `Checkout`
            // case above.
            MaterializationIntent::VirtualCheckout
            | MaterializationIntent::ScopedVirtualCheckout(_) => {
                outcomes.push(sync_virtual_checkout(entry, opts).await?);
            }
            _ => {}
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
        let action = if opts.pull && behind > 0 && ahead > 0 {
            "merge (cached upstream; fetch may change affected paths)"
        } else if opts.pull && behind > 0 {
            "fast-forward (cached upstream; fetch may change affected paths)"
        } else if opts.push && ahead > 0 {
            "push (cached upstream)"
        } else {
            "leave unchanged (cached upstream)"
        };
        if opts.pull && behind > 0 {
            for message in pull_preview(&repo, &path, ahead > 0)? {
                crate::ui::info(message)?;
            }
        }
        return Ok(SyncOutcome::Preview {
            path,
            action: action.into(),
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

fn pull_preview(repo: &Repository, path: &Path, divergent: bool) -> Result<Vec<String>> {
    let head = repo.head()?.peel_to_commit()?;
    let branch = git2::Branch::wrap(repo.head()?);
    let upstream = branch.upstream()?.get().peel_to_commit()?;
    // HEAD-to-upstream would mislabel local-only additions as incoming deletions.
    // Reading the merge base avoids simulating a merge (and writing objects).
    let base = if divergent {
        repo.find_commit(repo.merge_base(head.id(), upstream.id())?)?
    } else {
        head
    };
    let diff = repo.diff_tree_to_tree(Some(&base.tree()?), Some(&upstream.tree()?), None)?;
    let mut messages = Vec::new();
    for delta in diff.deltas() {
        if let Some(relative) = delta.new_file().path().or(delta.old_file().path()) {
            messages.push(format!(
                "{} {:?}: {}",
                if divergent {
                    "incoming candidate (merge-base to cached upstream; merge may conflict)"
                } else {
                    "would reconcile"
                },
                delta.status(),
                path.join(relative).display()
            ));
        }
    }
    Ok(messages)
}

fn abort_preview(repo: &Repository, path: &Path) -> Result<Vec<String>> {
    let head = repo.head()?.peel_to_tree()?;
    let mut restored_paths = Vec::new();
    head.walk(git2::TreeWalkMode::PreOrder, |root, entry| {
        if entry.kind() != Some(git2::ObjectType::Tree) {
            let Ok(name) = entry.name() else {
                return git2::TreeWalkResult::Abort;
            };
            restored_paths.push(Path::new(root).join(name));
        }
        git2::TreeWalkResult::Ok
    })?;
    let mut options = git2::StatusOptions::new();
    options
        .include_untracked(true)
        .include_ignored(true)
        .recurse_untracked_dirs(true)
        .recurse_ignored_dirs(true)
        .update_index(false);
    let mut messages = Vec::new();
    for entry in repo.statuses(Some(&mut options))?.iter() {
        let relative = Path::new(entry.path()?);
        let status = entry.status();
        // Untracked/ignored entries alone are not tracked changes being discarded.
        let tracked = !(status - (git2::Status::WT_NEW | git2::Status::IGNORED)).is_empty();
        if tracked {
            messages.push(format!(
                "would discard tracked/index merge changes: {}",
                path.join(relative).display()
            ));
        }
        // A path removed from the index can still obstruct restoring HEAD, as can
        // a file/directory replacement at an ancestor or descendant of that path.
        if status.intersects(git2::Status::WT_NEW | git2::Status::IGNORED)
            && restored_paths
                .iter()
                .any(|restored| restored.starts_with(relative) || relative.starts_with(restored))
        {
            messages.push(format!(
                "untracked/ignored obstruction may be overwritten when restoring HEAD: {}",
                path.join(relative).display()
            ));
        }
    }
    Ok(messages)
}

/// Handle a repository already mid-merge (or some other non-`Clean`
/// libgit2 state) when `over sync` starts.
fn handle_unresolved_state(
    repo: &Repository,
    path: &Path,
    opts: &SyncOptions,
) -> Result<SyncOutcome> {
    if opts.abort {
        if repo.state() != RepositoryState::Merge {
            return Ok(SyncOutcome::Blocked { path: path.to_path_buf(), reason: "only an in-progress merge can be aborted; resolve other Git operations manually".into() });
        }
        if opts.dry_run {
            for message in abort_preview(repo, path)? {
                crate::ui::info(message)?;
            }
            return Ok(SyncOutcome::Preview {
                path: path.to_path_buf(),
                action: "abort merge and discard uncommitted tracked/index changes".into(),
            });
        }
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

/// Reconcile a single virtual checkout entry (#141). Entirely local — the
/// "source" is the overlay's own repository, already on disk, so there's
/// never any fetch/push against a remote here, unlike [`sync_unit`]'s root
/// `Checkout` handling above. Deterministic per [`Status`]:
///
/// - `Applied` -> [`SyncOutcome::UpToDate`].
/// - `Behind` (source-only changes, nothing local to lose) -> re-checkout
///   the drifted content and advance the recorded `base_oid` ->
///   [`SyncOutcome::FastForwarded`]. Gated on `opts.pull` (mirrors "pull"
///   for a root checkout: bringing new upstream content in).
/// - `Modified` (local-only changes) -> gated on `opts.push` (mirrors
///   "push": sending local content back to the source); if allowed and
///   not `opts.no_prompt`, asks for confirmation (and a message) before
///   committing — declining, or `no_prompt`, leaves everything untouched
///   and reports [`SyncOutcome::Blocked`].
/// - `Diverged`: reconcile source-only changes and converged files while
///   preserving independent target/index edits; overlapping edits block.
/// - `Conflict`: never auto-resolve incompatible same-file edits.
/// - `Missing`/`Broken` -> [`SyncOutcome::Blocked`] (nothing to sync;
///   `over apply` materializes it).
async fn sync_virtual_checkout(entry: &DesiredEntry, opts: &SyncOptions) -> Result<SyncOutcome> {
    let path = entry.target.clone();
    // Virtual reconciliation has no pending merge to undo. Dispatch before
    // inspection so abort cannot refresh files or reach the commit prompt.
    if opts.abort {
        return Ok(SyncOutcome::Blocked {
            path,
            reason: "virtual checkout has no abortable operation".into(),
        });
    }
    let vc_status = status::virtual_checkout::inspect(entry)?;

    match vc_status {
        Status::Locked => anyhow::bail!("encrypted virtual checkouts are unsupported"),
        Status::Applied => Ok(SyncOutcome::UpToDate { path }),
        Status::Behind(_) | Status::Diverged { .. } => {
            if !opts.pull {
                return Ok(SyncOutcome::UpToDate { path });
            }
            if opts.dry_run {
                reconcile_virtual_checkout(entry, true).await?;
                return Ok(SyncOutcome::Preview {
                    path,
                    action: "refresh virtual checkout from source".into(),
                });
            }
            reconcile_virtual_checkout(entry, false).await?;
            Ok(SyncOutcome::FastForwarded { path })
        }
        Status::Modified => {
            if !opts.push {
                return Ok(SyncOutcome::Blocked {
                    path,
                    reason: "uncommitted changes (run `over commit`, or re-run without \
                             --pull-only)"
                        .to_string(),
                });
            }
            if opts.dry_run {
                // Never prompts, never mutates under dry-run (mirrors
                // `EnsureGitRepository`'s own `!ctx.dry_run` gate).
                return Ok(SyncOutcome::Blocked {
                    path,
                    reason: "uncommitted changes; would prompt to commit".to_string(),
                });
            }
            commit_virtual_checkout_interactively(entry, opts).await
        }
        Status::Conflict => Ok(SyncOutcome::Conflict { path }),
        Status::Missing => Ok(SyncOutcome::Blocked {
            path,
            reason: "not materialized yet; run `over apply`".to_string(),
        }),
        Status::Broken => Ok(SyncOutcome::Blocked {
            path,
            reason: "virtual checkout association is broken; run `over apply` to re-adopt it"
                .to_string(),
        }),
        Status::Ahead(_) => unreachable!(
            "status::virtual_checkout::inspect never reports Ahead \
             (there's no notion of the target being ahead of the source)"
        ),
    }
}

/// Ask whether to commit a virtual checkout's local changes, then a commit
/// message, and commit if confirmed — never touches anything if the user
/// declines. `opts.no_prompt` skips straight to `Blocked` instead of
/// asking, per the issue's "the user must be able to decline committing"
/// and "never auto-commit without explicit user consent".
async fn commit_virtual_checkout_interactively(
    entry: &DesiredEntry,
    opts: &SyncOptions,
) -> Result<SyncOutcome> {
    let path = entry.target.clone();
    let pending = status::virtual_checkout::staged_changes(entry)?;
    if pending.is_empty() || !status::virtual_checkout::unstaged_changes(entry)?.is_empty() {
        return Ok(SyncOutcome::Blocked {
            path,
            reason: "uncommitted changes; stage selected paths with `over add`, then run `over commit`; unstaged edits are preserved".to_string(),
        });
    }
    if opts.no_prompt {
        return Ok(SyncOutcome::Blocked {
            path,
            reason: "uncommitted changes; run `over commit` (--no-prompt disables the sync-time \
                     confirmation)"
                .to_string(),
        });
    }

    let confirmed = Confirm::with_theme(&DialogTheme::default())
        .with_prompt(format!(
            "{} staged file{} changed at {} — commit {} now?",
            pending.len(),
            if pending.len() == 1 { "" } else { "s" },
            short_path(&path.to_string_lossy()),
            if pending.len() == 1 { "it" } else { "them" },
        ))
        .default(true)
        .interact()
        .map_err(|e| anyhow!("commit confirmation prompt failed: {}", e))?;

    if !confirmed {
        return Ok(SyncOutcome::Blocked {
            path,
            reason: "local changes not committed (declined)".to_string(),
        });
    }

    let message = Input::<String>::with_theme(&DialogTheme::default())
        .with_prompt("Commit message")
        .allow_empty(true)
        .interact_text()
        .map_err(|e| anyhow!("commit message prompt failed: {}", e))?;
    let message = if message.trim().is_empty() {
        None
    } else {
        Some(message)
    };

    match commit::commit(entry, &CommitOptions { message }).await? {
        CommitOutcome::Committed { files, .. } => Ok(SyncOutcome::Committed { path, files }),
        // Raced with something else changing the checkout between
        // `inspect` above and now — report accurately rather than
        // pretending the earlier snapshot still holds.
        CommitOutcome::NothingToCommit => Ok(SyncOutcome::UpToDate { path }),
        CommitOutcome::Blocked { conflicting_files } => {
            let _ = conflicting_files;
            Ok(SyncOutcome::Conflict { path })
        }
        CommitOutcome::NotVirtualCheckout => unreachable!(
            "sync_virtual_checkout only ever calls commit::commit for a VirtualCheckout entry"
        ),
    }
}

/// Update `entry`'s managed subtree in place to reflect the source
/// repository's current `HEAD` (writing/removing only the paths that
/// actually drifted since the recorded `base_oid`) and advance that
/// `base_oid`. Recheck every source-changed path against both base and HEAD:
/// independent local/index edits survive, and already-converged files only
/// need their checkpoint advanced. All git2/filesystem work is synchronous.
///
/// Deliberately does *not* use [`vc_git::checkout_subtree`] here (unlike
/// the materializer's own first-time `Create` path): re-checking out the
/// *whole* subtree via `git2::checkout_tree` proved unreliable at
/// overwriting a file that already exists at `target` with different
/// content (observed directly in CI, not reproducible locally — see
/// `vc_git::apply_tree_diff`'s own doc). Reading/writing only the
/// changed blobs with plain `std::fs` avoids that entirely.
async fn reconcile_virtual_checkout(entry: &DesiredEntry, dry_run: bool) -> Result<()> {
    let Provenance::Overlay { source, .. } = &entry.provenance else {
        unreachable!(
            "VirtualCheckout entries only ever carry Provenance::Overlay \
             (see desired::tree::{{collect_own_entries,walk_overlay_tree}})"
        );
    };
    let Some(mut record) = vc_state::record_for(&entry.target).await? else {
        anyhow::bail!(
            "no virtual checkout association recorded for '{}' — run `over apply` first",
            entry.target.display(),
        );
    };

    let target = entry.target.clone();
    let source = source.clone();
    let base_oid = record.base_oid.clone();
    let inspect_entry = entry.clone();
    let scope = entry.intent.checkout_scope();
    let verification_record = record.clone();
    let new_base_oid = tokio::task::spawn_blocking(move || {
        vc_state::verify(&inspect_entry, &verification_record)?;
        let (repo, managed_path) = vc_git::discover_source(&source)?;
        let from_tree = vc_git::base_tree(&repo, Some(&base_oid))?;
        let to_tree = vc_git::base_tree(&repo, None)?;
        // Entry maps are read-only; filtered subtree construction writes objects.
        let base_entries = vc_git::scoped_entries(&repo, &from_tree, &managed_path, &scope)?;
        let head_entries = vc_git::scoped_entries(&repo, &to_tree, &managed_path, &scope)?;
        let index = repo.index()?;
        anyhow::ensure!(
            !index.has_conflicts(),
            "source index has unresolved conflicts"
        );
        let paths: std::collections::BTreeSet<_> =
            base_entries.keys().chain(head_entries.keys()).collect();
        let mut updates_from = std::collections::BTreeMap::new();
        let mut updates_to = std::collections::BTreeMap::new();
        for relative in paths {
            let base = base_entries.get(relative).copied();
            let head = head_entries.get(relative).copied();
            if base == head {
                continue;
            }
            let local = crate::materialize::virtual_checkout::snapshot::canonical_target_entry(
                &repo,
                &target,
                relative,
                &managed_path.join(relative),
                &[base, head],
            )?;
            let indexed = index
                .get_path(&managed_path.join(relative), 0)
                .map(|entry| (entry.id, entry.mode as i32));
            anyhow::ensure!(
                vc_git::entries_match(local, base) || vc_git::entries_match(local, head),
                "target '{}' changed incompatibly with its source",
                target.join(relative).display()
            );
            anyhow::ensure!(
                indexed == base || indexed == head,
                "staged path '{}' changed incompatibly with its source",
                relative.display()
            );
            // Do not rewrite converged files: they may have private permissions
            // or be read-only. Unchanged local/index paths remain untouched too.
            if !vc_git::entries_match(local, head) {
                if dry_run {
                    crate::ui::info(format!(
                        "would {}: {}",
                        if head.is_none() {
                            "remove"
                        } else if local.is_none() {
                            "create"
                        } else {
                            "replace"
                        },
                        target.join(relative).display()
                    ))?;
                }
                if let Some(base) = base {
                    updates_from.insert(relative.clone(), base);
                }
                if let Some(head) = head {
                    updates_to.insert(relative.clone(), head);
                }
            }
        }
        let changes = vc_git::preflight_tree_changes(
            &repo,
            &target,
            &managed_path,
            &base_entries,
            &updates_from,
            &updates_to,
        )?;
        if !dry_run {
            changes.apply(&target)?;
        }
        anyhow::Ok(repo.head()?.peel_to_commit()?.id().to_string())
    })
    .await??;

    if !dry_run {
        record.base_oid = new_base_oid;
        vc_state::persist(&entry.target, record).await?;
    }
    Ok(())
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
    use assert_fs::prelude::*;
    use git2::Signature;
    use std::fs;

    /// Points `$XDG_STATE_HOME` at a fresh, writable temp dir for the
    /// duration of the returned guard's lifetime. `sync()` persists a
    /// checkpoint (`SyncState`) and, for `VirtualCheckout` entries, reads/
    /// writes `VirtualCheckoutState` via the real, non-injectable
    /// `XdgDirs::new()` — every test here that calls `sync`/
    /// `fast_forward_virtual_checkout` must isolate this or it silently
    /// pollutes the real `$XDG_STATE_HOME/over/{sync,virtual_checkout}.toml`
    /// with throwaway records that outlive the test (observed directly,
    /// not just theoretical — see #149's `over doctor` follow-up).
    ///
    /// # Safety
    /// `env::set_var` is only unsound when other threads read/write the
    /// process environment concurrently; `cargo nextest` runs each test in
    /// its own process, matching `xdg::tests`' own `set_env` justification.
    fn isolate_xdg_state() -> TempDir {
        let tmp = TempDir::new().unwrap();
        unsafe {
            std::env::set_var("XDG_STATE_HOME", tmp.path());
        }
        tmp
    }

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

    fn checkout_snapshot(root: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
        fn collect(
            root: &Path,
            directory: &Path,
            files: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>,
        ) {
            for entry in fs::read_dir(directory).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    collect(root, &path, files);
                } else {
                    files.insert(
                        path.strip_prefix(root).unwrap().to_path_buf(),
                        fs::read(path).unwrap(),
                    );
                }
            }
        }
        // Include .git so object creation, index writes and cleared merge state
        // cannot hide behind an otherwise unchanged working tree.
        let mut files = std::collections::BTreeMap::new();
        collect(root, root, &mut files);
        files
    }

    #[rstest::rstest]
    #[case(false)]
    #[case(true)]
    fn pull_preview_only_lists_incoming_additions_without_mutating_the_checkout(
        #[case] divergent: bool,
    ) {
        let source_td = TempDir::new().unwrap();
        let source = init_committed_repo_ret(source_td.path());
        let dest_td = TempDir::new().unwrap();
        let path = dest_td.path().join("checkout");
        let repo = clone_with_tracking(source_td.path(), &path);
        if divergent {
            fs::write(path.join("local.txt"), "local").unwrap();
            commit_all(&repo, "local addition");
        }
        fs::write(source_td.path().join("upstream.txt"), "upstream").unwrap();
        commit_all(&source, "upstream addition");
        git_sync::fetch_origin(&repo).unwrap();
        let before = checkout_snapshot(&path);

        let messages = pull_preview(&repo, &path, divergent).unwrap();
        let prefix = if divergent {
            "incoming candidate (merge-base to cached upstream; merge may conflict)"
        } else {
            "would reconcile"
        };
        assert_eq!(
            messages,
            vec![format!(
                "{prefix} Added: {}",
                path.join("upstream.txt").display()
            )]
        );
        let unit = SyncUnit {
            path: path.clone(),
            overlay: "ov".into(),
            repo_key: ROOT_PATH.into(),
            worktree: None,
        };
        let outcome = sync_unit(
            &unit,
            &SyncOptions {
                dry_run: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(matches!(outcome, SyncOutcome::Preview { .. }));
        assert_eq!(checkout_snapshot(&path), before);
    }

    #[rstest::rstest]
    #[case(false)]
    #[case(true)]
    fn abort_preview_separates_restoration_obstructions_from_unrelated_untracked_files(
        #[case] ignored: bool,
    ) {
        let source_td = TempDir::new().unwrap();
        let source = init_committed_repo_ret(source_td.path());
        fs::write(source_td.path().join("restore.txt"), "committed").unwrap();
        fs::create_dir(source_td.path().join("blocked")).unwrap();
        fs::write(source_td.path().join("blocked/child.txt"), "committed").unwrap();
        commit_all(&source, "restoration paths");
        let dest_td = TempDir::new().unwrap();
        let path = dest_td.path().join("checkout");
        let repo = clone_with_tracking(source_td.path(), &path);
        fs::write(path.join("README.md"), "local").unwrap();
        commit_all(&repo, "local edit");
        fs::write(source_td.path().join("README.md"), "upstream").unwrap();
        commit_all(&source, "upstream edit");
        assert_eq!(
            git_sync::pull(&repo).unwrap(),
            git_sync::PullOutcome::Conflict
        );
        fs::write(path.join("staged.txt"), "staged addition").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("staged.txt")).unwrap();
        index.remove_path(Path::new("restore.txt")).unwrap();
        index.remove_path(Path::new("blocked/child.txt")).unwrap();
        index.write().unwrap();
        fs::write(path.join("restore.txt"), "untracked replacement").unwrap();
        fs::remove_dir_all(path.join("blocked")).unwrap();
        fs::write(path.join("blocked"), "ancestor obstruction").unwrap();
        fs::write(path.join("unrelated.txt"), "keep me").unwrap();
        if ignored {
            fs::write(
                repo.path().join("info/exclude"),
                "restore.txt\nblocked\nunrelated.txt\n",
            )
            .unwrap();
        }
        let before = checkout_snapshot(&path);

        let messages = abort_preview(&repo, &path).unwrap();
        for relative in ["README.md", "staged.txt"] {
            assert!(messages.contains(&format!(
                "would discard tracked/index merge changes: {}",
                path.join(relative).display()
            )));
        }
        for relative in ["restore.txt", "blocked"] {
            assert!(messages.contains(&format!(
                "untracked/ignored obstruction may be overwritten when restoring HEAD: {}",
                path.join(relative).display()
            )));
        }
        assert!(
            !messages
                .iter()
                .any(|message| message.contains("unrelated.txt"))
        );
        assert!(matches!(
            handle_unresolved_state(
                &repo,
                &path,
                &SyncOptions {
                    abort: true,
                    dry_run: true,
                    ..Default::default()
                }
            )
            .unwrap(),
            SyncOutcome::Preview { .. }
        ));
        assert_eq!(repo.state(), RepositoryState::Merge);
        assert_eq!(checkout_snapshot(&path), before);
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
        let _xdg = isolate_xdg_state();
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
        let _xdg = isolate_xdg_state();
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
        let _xdg = isolate_xdg_state();
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
        let _xdg = isolate_xdg_state();
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
        assert!(matches!(outcomes[0], SyncOutcome::Preview { .. }));
        // ...without actually updating the checkout's working tree.
        assert!(!checkout_path.join("new.txt").exists());
    }

    #[tokio::test]
    async fn bare_worktrees_are_synced_independently() {
        let _xdg = isolate_xdg_state();
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

    // ── #141: virtual checkout sync tests ──────────────────────────────

    fn virtual_checkout_entry(target: PathBuf, source: PathBuf) -> DesiredEntry {
        DesiredEntry {
            target,
            provenance: Provenance::Overlay {
                overlay: "ov".to_string(),
                source,
            },
            intent: MaterializationIntent::VirtualCheckout,
            permissions: None,
        }
    }

    #[tokio::test]
    async fn abort_does_not_inspect_or_prompt_for_a_virtual_checkout() {
        let xdg = isolate_xdg_state();
        let td = TempDir::new().unwrap();
        let entry =
            virtual_checkout_entry(td.path().join("target"), td.path().join("missing-source"));
        let outcome = sync_virtual_checkout(
            &entry,
            &SyncOptions {
                abort: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(
            matches!(outcome, SyncOutcome::Blocked { reason, .. } if reason.contains("no abortable operation"))
        );
        assert_eq!(fs::read_dir(xdg.path()).unwrap().count(), 0);
        assert_eq!(fs::read_dir(td.path()).unwrap().count(), 0);
    }

    async fn materialize_virtual_checkout(source_root: &std::path::Path, target: &std::path::Path) {
        let repo = Repository::open(source_root).unwrap();
        fs::create_dir_all(target).unwrap();
        let head_tree = repo.head().unwrap().peel_to_tree().unwrap();
        vc_git::checkout_subtree(&repo, &head_tree, target).unwrap();
        let base_oid = repo
            .head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .id()
            .to_string();
        vc_state::persist(
            target,
            crate::materialize::virtual_checkout::state::VirtualCheckoutRecord {
                overlay: "ov".to_string(),
                managed_path: PathBuf::new(),
                base_oid,
                created_at: 0,
                last_commit_at: None,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn virtual_checkout_up_to_date_reports_up_to_date() {
        let _xdg = isolate_xdg_state();
        let source_td = TempDir::new().unwrap();
        source_td.child("a.txt").write_str("a").unwrap();
        init_committed_repo(source_td.path());
        let target_td = TempDir::new().unwrap();
        materialize_virtual_checkout(source_td.path(), target_td.path()).await;

        let entry = virtual_checkout_entry(
            target_td.path().to_path_buf(),
            source_td.path().to_path_buf(),
        );
        let desired = DesiredTree::from_entries(vec![entry]);
        let outcomes = sync(&desired, &SyncOptions::default()).await.unwrap();
        assert_eq!(outcomes.len(), 1);
        assert_eq!(
            outcomes[0],
            SyncOutcome::UpToDate {
                path: target_td.path().to_path_buf()
            }
        );
    }

    #[rstest::rstest]
    #[case(false)]
    #[case(true)]
    #[tokio::test]
    async fn abort_skips_virtual_refresh_in_a_composition_with_a_real_merge(#[case] dry_run: bool) {
        let xdg = isolate_xdg_state();
        let source_td = TempDir::new().unwrap();
        let source = init_committed_repo_ret(source_td.path());
        let target_td = TempDir::new().unwrap();
        materialize_virtual_checkout(source_td.path(), target_td.path()).await;
        let checkout_td = TempDir::new().unwrap();
        let checkout = clone_with_tracking(source_td.path(), checkout_td.path());
        fs::write(checkout_td.path().join("README.md"), "local").unwrap();
        commit_all(&checkout, "local edit");
        fs::write(source_td.path().join("README.md"), "upstream").unwrap();
        commit_all(&source, "upstream edit");
        assert_eq!(
            git_sync::pull(&checkout).unwrap(),
            git_sync::PullOutcome::Conflict
        );
        let desired = DesiredTree::from_entries(vec![
            virtual_checkout_entry(target_td.path().into(), source_td.path().into()),
            checkout_entry(checkout_td.path().into(), ROOT_PATH, git_config(false)),
        ]);
        let target_before = checkout_snapshot(target_td.path());
        let source_before = checkout_snapshot(source_td.path());
        let state_before = fs::read(xdg.path().join("over/virtual_checkout.toml")).unwrap();
        let outcomes = sync(
            &desired,
            &SyncOptions {
                abort: true,
                dry_run,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(outcomes.iter().any(|outcome| matches!(outcome, SyncOutcome::Blocked { path, reason } if path == target_td.path() && reason.contains("no abortable operation"))));
        assert!(outcomes.iter().any(|outcome| if dry_run {
            matches!(outcome, SyncOutcome::Preview { .. })
        } else {
            matches!(outcome, SyncOutcome::Aborted { .. })
        }));
        assert_eq!(checkout_snapshot(target_td.path()), target_before);
        assert_eq!(checkout_snapshot(source_td.path()), source_before);
        assert_eq!(
            fs::read(xdg.path().join("over/virtual_checkout.toml")).unwrap(),
            state_before
        );
        assert_eq!(
            checkout.state(),
            if dry_run {
                RepositoryState::Merge
            } else {
                RepositoryState::Clean
            }
        );
    }

    #[tokio::test]
    #[rstest::rstest]
    async fn virtual_sync_preflights_shape_transitions_before_any_target_mutation(
        #[values(false, true)] directory_to_file: bool,
        #[values(
            "clean",
            "edited",
            "untracked",
            "empty-untracked",
            "readonly",
            "hole",
            "missing-old"
        )]
        obstruction: &str,
        #[values(false, true)] dry_run: bool,
    ) {
        let xdg = isolate_xdg_state();
        let source = TempDir::new().unwrap();
        let repo = init_committed_repo_ret(source.path());
        let old = if directory_to_file {
            "shape/nested/leaf"
        } else {
            "shape"
        };
        let new = if directory_to_file {
            "shape"
        } else {
            "shape/nested/leaf"
        };
        fs::create_dir_all(source.path().join(old).parent().unwrap()).unwrap();
        fs::write(source.path().join(old), "old").unwrap();
        fs::write(source.path().join("aaa"), "old unrelated").unwrap();
        if directory_to_file && obstruction == "hole" {
            fs::write(source.path().join("shape/foreign"), "independently owned").unwrap();
        }
        commit_all(&repo, "base shape");
        let target = TempDir::new().unwrap();
        materialize_virtual_checkout(source.path(), target.path()).await;
        if directory_to_file {
            fs::remove_dir_all(source.path().join("shape")).unwrap();
        } else {
            fs::remove_file(source.path().join(old)).unwrap();
            fs::create_dir_all(source.path().join(new).parent().unwrap()).unwrap();
        }
        fs::write(source.path().join(new), "new").unwrap();
        fs::write(source.path().join("aaa"), "new unrelated").unwrap();
        commit_all(&repo, "new shape");
        match obstruction {
            "missing-old" => fs::remove_file(target.path().join(old)).unwrap(),
            "edited" => fs::write(target.path().join(old), "local edit").unwrap(),
            "untracked" if directory_to_file => {
                fs::write(target.path().join("shape/foreign"), "private").unwrap()
            }
            "empty-untracked" if directory_to_file => {
                fs::create_dir(target.path().join("shape/foreign")).unwrap()
            }
            "readonly" => {
                let file = target.path().join(old);
                let mut permissions = fs::metadata(&file).unwrap().permissions();
                permissions.set_readonly(true);
                fs::set_permissions(file, permissions).unwrap();
            }
            _ => {}
        }
        let mut entry = virtual_checkout_entry(target.path().into(), source.path().into());
        if directory_to_file && obstruction == "hole" {
            let scope = crate::desired::CheckoutScope {
                source_excludes: vec![PathBuf::from("shape/foreign")],
                target_excludes: vec![PathBuf::from("shape/foreign")],
                ..Default::default()
            };
            entry.intent = MaterializationIntent::ScopedVirtualCheckout(scope.clone());
            let mut record = vc_state::record_for(target.path()).await.unwrap().unwrap();
            record.scope = scope;
            vc_state::persist(target.path(), record).await.unwrap();
        }
        let before = checkout_snapshot(target.path());
        let state_before = fs::read(xdg.path().join("over/virtual_checkout.toml")).unwrap();
        let result = if obstruction == "clean" {
            // Exercise public dispatch too: shape-only upstream changes must be
            // classified as pullable rather than reaching a commit prompt.
            sync(
                &DesiredTree::from_entries(vec![entry.clone()]),
                &SyncOptions {
                    dry_run,
                    no_prompt: true,
                    ..Default::default()
                },
            )
            .await
            .map(|outcomes| {
                assert!(matches!(
                    outcomes[0],
                    SyncOutcome::FastForwarded { .. } | SyncOutcome::Preview { .. }
                ));
            })
        } else {
            reconcile_virtual_checkout(&entry, dry_run).await
        };
        let blocked = obstruction == "edited"
            || obstruction == "readonly"
            || (directory_to_file
                && matches!(obstruction, "untracked" | "empty-untracked" | "hole"));
        assert_eq!(result.is_err(), blocked, "{result:?}");
        if blocked || dry_run {
            assert_eq!(checkout_snapshot(target.path()), before);
            assert_eq!(
                fs::read(xdg.path().join("over/virtual_checkout.toml")).unwrap(),
                state_before
            );
        } else {
            assert_eq!(fs::read(target.path().join(new)).unwrap(), b"new");
            assert_eq!(
                fs::read(target.path().join("aaa")).unwrap(),
                b"new unrelated"
            );
            assert_eq!(
                status::virtual_checkout::inspect(&entry).unwrap(),
                Status::Applied
            );
        }
        // Windows cannot clean up read-only fixture files.
        if obstruction == "readonly" {
            let file = target.path().join(old);
            fs::set_permissions(
                &file,
                fs::metadata(source.path().join(new)).unwrap().permissions(),
            )
            .unwrap();
        }
    }

    #[tokio::test]
    async fn virtual_checkout_behind_fast_forwards_without_prompting() {
        let _xdg = isolate_xdg_state();
        let source_td = TempDir::new().unwrap();
        source_td.child("a.txt").write_str("a").unwrap();
        let source_repo = init_committed_repo_ret(source_td.path());
        let target_td = TempDir::new().unwrap();
        materialize_virtual_checkout(source_td.path(), target_td.path()).await;

        fs::write(source_td.path().join("a.txt"), "advanced").unwrap();
        commit_all(&source_repo, "advance");

        let entry = virtual_checkout_entry(
            target_td.path().to_path_buf(),
            source_td.path().to_path_buf(),
        );
        let desired = DesiredTree::from_entries(vec![entry]);
        let outcomes = sync(&desired, &SyncOptions::default()).await.unwrap();
        assert_eq!(outcomes.len(), 1);
        assert_eq!(
            outcomes[0],
            SyncOutcome::FastForwarded {
                path: target_td.path().to_path_buf()
            }
        );
        assert_eq!(
            fs::read_to_string(target_td.path().join("a.txt")).unwrap(),
            "advanced"
        );
    }

    #[tokio::test]
    async fn virtual_checkout_dry_run_behind_does_not_mutate() {
        let _xdg = isolate_xdg_state();
        let source_td = TempDir::new().unwrap();
        source_td.child("a.txt").write_str("a").unwrap();
        let source_repo = init_committed_repo_ret(source_td.path());
        let target_td = TempDir::new().unwrap();
        materialize_virtual_checkout(source_td.path(), target_td.path()).await;

        fs::write(source_td.path().join("a.txt"), "advanced").unwrap();
        commit_all(&source_repo, "advance");

        let entry = virtual_checkout_entry(
            target_td.path().to_path_buf(),
            source_td.path().to_path_buf(),
        );
        let desired = DesiredTree::from_entries(vec![entry]);
        let opts = SyncOptions {
            dry_run: true,
            ..SyncOptions::default()
        };
        let outcomes = sync(&desired, &opts).await.unwrap();
        assert_eq!(
            outcomes[0],
            SyncOutcome::Preview {
                path: target_td.path().to_path_buf(),
                action: "refresh virtual checkout from source".into(),
            }
        );
        // Reports what *would* happen without actually touching the target.
        assert_eq!(
            fs::read_to_string(target_td.path().join("a.txt")).unwrap(),
            "a"
        );
    }

    #[tokio::test]
    async fn virtual_checkout_modified_with_no_prompt_is_blocked_and_untouched() {
        let _xdg = isolate_xdg_state();
        let source_td = TempDir::new().unwrap();
        source_td.child("a.txt").write_str("a").unwrap();
        init_committed_repo(source_td.path());
        let target_td = TempDir::new().unwrap();
        materialize_virtual_checkout(source_td.path(), target_td.path()).await;

        fs::write(target_td.path().join("a.txt"), "local edit").unwrap();

        let entry = virtual_checkout_entry(
            target_td.path().to_path_buf(),
            source_td.path().to_path_buf(),
        );
        let desired = DesiredTree::from_entries(vec![entry]);
        let opts = SyncOptions {
            no_prompt: true,
            ..SyncOptions::default()
        };
        let outcomes = sync(&desired, &opts).await.unwrap();
        assert_eq!(outcomes.len(), 1);
        assert!(matches!(outcomes[0], SyncOutcome::Blocked { .. }));
        assert!(outcomes[0].needs_attention());
        // Untouched: neither the target nor the source repository changed.
        assert_eq!(
            fs::read_to_string(target_td.path().join("a.txt")).unwrap(),
            "local edit"
        );
        let source_repo = Repository::open(source_td.path()).unwrap();
        assert_eq!(
            source_repo
                .head()
                .unwrap()
                .peel_to_commit()
                .unwrap()
                .parent_count(),
            0
        );
    }

    #[tokio::test]
    async fn virtual_checkout_push_only_disabled_reports_blocked_for_local_changes() {
        let _xdg = isolate_xdg_state();
        let source_td = TempDir::new().unwrap();
        source_td.child("a.txt").write_str("a").unwrap();
        init_committed_repo(source_td.path());
        let target_td = TempDir::new().unwrap();
        materialize_virtual_checkout(source_td.path(), target_td.path()).await;

        fs::write(target_td.path().join("a.txt"), "local edit").unwrap();

        let entry = virtual_checkout_entry(
            target_td.path().to_path_buf(),
            source_td.path().to_path_buf(),
        );
        let desired = DesiredTree::from_entries(vec![entry]);
        let opts = SyncOptions {
            push: false,
            ..SyncOptions::default()
        };
        let outcomes = sync(&desired, &opts).await.unwrap();
        assert!(matches!(outcomes[0], SyncOutcome::Blocked { .. }));
    }

    #[tokio::test]
    async fn virtual_checkout_conflicting_file_reports_conflict_without_mutating() {
        let _xdg = isolate_xdg_state();
        let source_td = TempDir::new().unwrap();
        source_td.child("a.txt").write_str("a").unwrap();
        let source_repo = init_committed_repo_ret(source_td.path());
        let target_td = TempDir::new().unwrap();
        materialize_virtual_checkout(source_td.path(), target_td.path()).await;

        fs::write(target_td.path().join("a.txt"), "local edit").unwrap();
        fs::write(source_td.path().join("a.txt"), "source edit").unwrap();
        commit_all(&source_repo, "advance differently");

        let entry = virtual_checkout_entry(
            target_td.path().to_path_buf(),
            source_td.path().to_path_buf(),
        );
        let desired = DesiredTree::from_entries(vec![entry]);
        let outcomes = sync(&desired, &SyncOptions::default()).await.unwrap();
        assert_eq!(outcomes.len(), 1);
        assert!(matches!(outcomes[0], SyncOutcome::Conflict { .. }));
        assert!(outcomes[0].needs_attention());
        // Neither side touched: still exactly the diverging content.
        assert_eq!(
            fs::read_to_string(target_td.path().join("a.txt")).unwrap(),
            "local edit"
        );
    }

    #[tokio::test]
    async fn virtual_checkout_missing_is_blocked() {
        let _xdg = isolate_xdg_state();
        let source_td = TempDir::new().unwrap();
        source_td.child("a.txt").write_str("a").unwrap();
        init_committed_repo(source_td.path());
        let target_td = TempDir::new().unwrap();
        let missing_target = target_td.path().join("does-not-exist");

        let entry = virtual_checkout_entry(missing_target, source_td.path().to_path_buf());
        let desired = DesiredTree::from_entries(vec![entry]);
        let outcomes = sync(&desired, &SyncOptions::default()).await.unwrap();
        assert_eq!(outcomes.len(), 1);
        assert!(matches!(outcomes[0], SyncOutcome::Blocked { .. }));
    }

    #[tokio::test]
    async fn virtual_checkout_malformed_base_oid_errors_rather_than_misreporting() {
        let _xdg = isolate_xdg_state();
        let source_td = TempDir::new().unwrap();
        source_td.child("a.txt").write_str("a").unwrap();
        init_committed_repo(source_td.path());
        let target_td = TempDir::new().unwrap();
        materialize_virtual_checkout(source_td.path(), target_td.path()).await;
        // Corrupt the recorded association so `inspect` can't resolve a
        // base tree from it.
        vc_state::persist(
            target_td.path(),
            crate::materialize::virtual_checkout::state::VirtualCheckoutRecord {
                overlay: "ov".to_string(),
                managed_path: PathBuf::new(),
                base_oid: "not-a-real-oid".to_string(),
                created_at: 0,
                last_commit_at: None,
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let entry = virtual_checkout_entry(
            target_td.path().to_path_buf(),
            source_td.path().to_path_buf(),
        );
        let desired = DesiredTree::from_entries(vec![entry]);
        let result = sync(&desired, &SyncOptions::default()).await;
        // `inspect` propagates the malformed-oid error rather than
        // silently misreporting status.
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn virtual_checkout_broken_with_no_recorded_association_is_blocked() {
        let _xdg = isolate_xdg_state();
        let source_td = TempDir::new().unwrap();
        source_td.child("a.txt").write_str("a").unwrap();
        init_committed_repo(source_td.path());
        // A real directory exists at the target, but no XDG association
        // was ever recorded for it (e.g. the state file was lost) —
        // `Status::Broken`, mapped to a `Blocked` sync outcome rather than
        // guessed at.
        let target_td = TempDir::new().unwrap();
        fs::create_dir_all(target_td.path().join("checkout")).unwrap();
        let target = target_td.path().join("checkout");

        let entry = virtual_checkout_entry(target, source_td.path().to_path_buf());
        let desired = DesiredTree::from_entries(vec![entry]);
        let outcomes = sync(&desired, &SyncOptions::default()).await.unwrap();
        assert_eq!(outcomes.len(), 1);
        assert!(matches!(outcomes[0], SyncOutcome::Blocked { .. }));
    }

    #[tokio::test]
    async fn virtual_checkout_pull_only_disabled_leaves_behind_status_untouched() {
        let _xdg = isolate_xdg_state();
        let source_td = TempDir::new().unwrap();
        source_td.child("a.txt").write_str("a").unwrap();
        let source_repo = init_committed_repo_ret(source_td.path());
        let target_td = TempDir::new().unwrap();
        materialize_virtual_checkout(source_td.path(), target_td.path()).await;

        fs::write(source_td.path().join("a.txt"), "advanced").unwrap();
        commit_all(&source_repo, "advance");

        let entry = virtual_checkout_entry(
            target_td.path().to_path_buf(),
            source_td.path().to_path_buf(),
        );
        let desired = DesiredTree::from_entries(vec![entry]);
        let opts = SyncOptions {
            pull: false,
            ..SyncOptions::default()
        };
        let outcomes = sync(&desired, &opts).await.unwrap();
        assert_eq!(outcomes.len(), 1);
        assert_eq!(
            outcomes[0],
            SyncOutcome::UpToDate {
                path: target_td.path().to_path_buf()
            }
        );
        // Never fast-forwarded: `--push-only` means the pull direction is
        // disabled entirely, not just gated on dry-run.
        assert_eq!(
            fs::read_to_string(target_td.path().join("a.txt")).unwrap(),
            "a"
        );
    }

    #[tokio::test]
    async fn virtual_checkout_dry_run_modified_reports_blocked_without_prompting() {
        let _xdg = isolate_xdg_state();
        let source_td = TempDir::new().unwrap();
        source_td.child("a.txt").write_str("a").unwrap();
        init_committed_repo(source_td.path());
        let target_td = TempDir::new().unwrap();
        materialize_virtual_checkout(source_td.path(), target_td.path()).await;

        fs::write(target_td.path().join("a.txt"), "local edit").unwrap();

        let entry = virtual_checkout_entry(
            target_td.path().to_path_buf(),
            source_td.path().to_path_buf(),
        );
        let desired = DesiredTree::from_entries(vec![entry]);
        let opts = SyncOptions {
            dry_run: true,
            ..SyncOptions::default()
        };
        let outcomes = sync(&desired, &opts).await.unwrap();
        assert_eq!(outcomes.len(), 1);
        assert!(matches!(outcomes[0], SyncOutcome::Blocked { .. }));
        // Never mutated, and no interactive prompt was ever reached.
        assert_eq!(
            fs::read_to_string(target_td.path().join("a.txt")).unwrap(),
            "local edit"
        );
    }

    #[tokio::test]
    async fn virtual_checkout_sync_preserves_local_edits_while_advancing_unrelated_source_paths() {
        let _xdg = isolate_xdg_state();
        // Reconcile source-only paths without staging or rewriting local edits.
        let source_td = TempDir::new().unwrap();
        source_td.child("a.txt").write_str("a").unwrap();
        source_td.child("b.txt").write_str("b").unwrap();
        let source_repo = init_committed_repo_ret(source_td.path());
        let target_td = TempDir::new().unwrap();
        materialize_virtual_checkout(source_td.path(), target_td.path()).await;

        fs::write(target_td.path().join("a.txt"), "local edit").unwrap();
        fs::write(source_td.path().join("b.txt"), "source edit").unwrap();
        commit_all(&source_repo, "advance b only");

        let entry = virtual_checkout_entry(
            target_td.path().to_path_buf(),
            source_td.path().to_path_buf(),
        );
        let desired = DesiredTree::from_entries(vec![entry]);
        let outcomes = sync(&desired, &SyncOptions::default()).await.unwrap();
        assert_eq!(outcomes.len(), 1);
        assert!(matches!(outcomes[0], SyncOutcome::FastForwarded { .. }));
        assert_eq!(
            fs::read_to_string(target_td.path().join("a.txt")).unwrap(),
            "local edit"
        );
        assert_eq!(
            fs::read_to_string(target_td.path().join("b.txt")).unwrap(),
            "source edit"
        );
    }

    #[tokio::test]
    async fn fast_forward_virtual_checkout_errors_without_a_recorded_association() {
        let _xdg = isolate_xdg_state();
        let source_td = TempDir::new().unwrap();
        source_td.child("a.txt").write_str("a").unwrap();
        let source_repo = init_committed_repo_ret(source_td.path());
        let target_td = TempDir::new().unwrap();
        // Materialized on disk, but never recorded in the XDG state file —
        // a corrupted/lost-state scenario `fast_forward_virtual_checkout`
        // must refuse outright rather than guessing a base.
        fs::create_dir_all(target_td.path()).unwrap();
        let head_tree = source_repo.head().unwrap().peel_to_tree().unwrap();
        vc_git::checkout_subtree(&source_repo, &head_tree, target_td.path()).unwrap();
        fs::write(source_td.path().join("a.txt"), "advanced").unwrap();
        commit_all(&source_repo, "advance");

        let entry = virtual_checkout_entry(
            target_td.path().to_path_buf(),
            source_td.path().to_path_buf(),
        );
        let result = reconcile_virtual_checkout(&entry, false).await;
        assert!(result.is_err());
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
