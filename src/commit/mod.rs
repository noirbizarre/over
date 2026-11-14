//! `over commit`: record staged virtual-checkout snapshots in the overlay's
//! own source repository, preserving unstaged target and unrelated index changes.
//! The standalone CLI can additionally supply checked reverse patches for rendered
//! literals; sync's reusable staged-only entry points do not select those edits.
//!
//! Reused by both `cli::commit` (the standalone command) and `crate::sync`
//! (which offers to commit dirty virtual-checkout files before
//! reconciling, per the issue's "ask before committing" requirement) — the
//! decision of whether/how to prompt for a commit message is left to those
//! callers; this module never touches a terminal.
//!
//! Deliberately narrow: only ever acts on
//! [`MaterializationIntent::VirtualCheckout`] entries, never on declared
//! (`overlay.git`) repositories nested inside an overlay — those aren't
//! even representable as an argument here, since [`CommitOutcome::NotVirtualCheckout`]
//! is returned for anything else without touching the filesystem/git at
//! all.

use std::fmt;
use std::path::PathBuf;

use anyhow::{Context as _, Result};

use crate::desired::{DesiredEntry, Provenance};
use crate::materialize::virtual_checkout::git;
use crate::materialize::virtual_checkout::state::{self, VirtualCheckoutRecord};
use crate::status;
use crate::ui::{emojis, style};

pub(crate) mod reverse_patch;

/// What `over commit` should do for a single virtual checkout entry.
#[derive(Debug, Clone, Default)]
pub struct CommitOptions {
    /// Commit message. `None` falls back to an auto-generated one (see
    /// [`default_message`]) — callers wanting an interactive prompt (the
    /// CLI, `over sync`) collect it themselves before constructing this.
    pub message: Option<String>,
    /// Absolute source or target prefixes; empty selects all owned staged paths.
    pub paths: Vec<PathBuf>,
}

pub(crate) fn selects(entry: &DesiredEntry, path: &std::path::Path, paths: &[PathBuf]) -> bool {
    let Provenance::Overlay { source, .. } = &entry.provenance else {
        return false;
    };
    paths.is_empty()
        || paths.iter().any(|prefix| {
            source.join(path).starts_with(prefix) || entry.target.join(path).starts_with(prefix)
        })
}

/// Preflight uses the same path filter as execution, so no-op selections never prompt.
pub(crate) fn pending(
    entries: &[DesiredEntry],
    opts: &CommitOptions,
    reverse: &reverse_patch::Prepared,
) -> Result<bool> {
    let mut pending = false;
    for entry in entries {
        let Provenance::Overlay { source, .. } = &entry.provenance else {
            continue;
        };
        let (repo, managed) = git::discover_source(source)?;
        let scope = entry.intent.checkout_scope();
        let selected = |path: &std::path::Path| selects(entry, path, &opts.paths);
        git::ensure_supported_staged_selected(&repo, &managed, &scope, &selected)?;
        pending |= git::staged_changes_selected(&repo, &managed, &scope, &selected)?
            .iter()
            .any(|change| {
                !reverse
                    .selected_sources
                    .contains(&source.join(&change.path))
            });
    }
    Ok(pending)
}

/// Capture publication inputs before prompting; never publish a silently changed index.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Snapshot {
    repositories: std::collections::BTreeMap<PathBuf, (git2::Oid, Vec<u8>)>,
}

pub(crate) fn snapshot(entries: &[DesiredEntry]) -> Result<Snapshot> {
    let mut snapshots = std::collections::BTreeMap::new();
    for entry in entries {
        let Provenance::Overlay { source, .. } = &entry.provenance else {
            continue;
        };
        let (repo, _) = git::discover_source(source)?;
        let index_path = repo.path().join("index");
        snapshots.insert(
            index_path.clone(),
            (
                repo.head()?.peel_to_commit()?.id(),
                std::fs::read(index_path)?,
            ),
        );
    }
    Ok(Snapshot {
        repositories: snapshots,
    })
}

/// What happened when committing a single entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitOutcome {
    /// The entry isn't a virtual checkout at all — never silently treated
    /// as a no-op, per the issue's "must refuse or clearly report when the
    /// selected overlay is not checkout-materialized". Also what every
    /// declared (`overlay.git`) repository entry gets, guaranteeing this
    /// command can never touch one.
    NotVirtualCheckout,
    /// Nothing changed locally since the last commit/sync — nothing to do.
    NothingToCommit,
    /// One or more files changed both locally and in the source
    /// repository, to different content, since the last known base —
    /// refuses to commit until resolved manually (never silently prefers
    /// either side).
    Blocked { conflicting_files: Vec<PathBuf> },
    /// A new commit was created in the source repository.
    Committed { oid: String, files: usize },
    /// Successful files may be committed even when another reverse patch fails.
    ReversePatch {
        oid: Option<String>,
        files: usize,
        conflicts: Vec<String>,
    },
}

impl CommitOutcome {
    pub fn needs_attention(&self) -> bool {
        matches!(
            self,
            CommitOutcome::Blocked { .. } | CommitOutcome::ReversePatch { .. }
        )
    }
}

impl fmt::Display for CommitOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CommitOutcome::ReversePatch {
                oid,
                files,
                conflicts,
            } => {
                if let Some(oid) = oid {
                    write!(
                        f,
                        "committed {files} file(s) ({}); ",
                        &oid[..oid.len().min(10)]
                    )?;
                }
                write!(f, "reverse-patch conflicts:\n{}", conflicts.join("\n"))
            }
            CommitOutcome::NotVirtualCheckout => write!(
                f,
                "{} {}",
                emojis::warning(),
                style::yellow("not checkout-materialized"),
            ),
            CommitOutcome::NothingToCommit => {
                write!(
                    f,
                    "{} {}",
                    emojis::checkmark(),
                    style::white("nothing to commit")
                )
            }
            CommitOutcome::Blocked { conflicting_files } => write!(
                f,
                "{} {} ({})",
                emojis::warning(),
                style::yellow("blocked: conflicting files"),
                conflicting_files
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            CommitOutcome::Committed { oid, files } => write!(
                f,
                "{} {} {} file{} ({})",
                emojis::SPARKLE,
                style::white("committed"),
                files,
                if *files == 1 { "" } else { "s" },
                &oid[..oid.len().min(10)],
            ),
        }
    }
}

/// Commit staged, non-conflicting paths owned by this virtual checkout.
/// Advance its checkpoint to committed content, leaving further target edits
/// unstaged. Unrelated index entries are preserved.
pub async fn commit(entry: &DesiredEntry, opts: &CommitOptions) -> Result<CommitOutcome> {
    if !entry.intent.is_virtual_checkout() {
        return Ok(CommitOutcome::NotVirtualCheckout);
    }

    commit_entries(std::slice::from_ref(entry), opts).await
}

/// Commit mapped and residual units together so one source repository gets one
/// coherent staged snapshot, rather than a commit per physical destination.
pub async fn commit_entries(
    entries: &[DesiredEntry],
    opts: &CommitOptions,
) -> Result<CommitOutcome> {
    commit_entries_checked(entries, opts, None).await
}

pub(crate) async fn commit_entries_checked(
    entries: &[DesiredEntry],
    opts: &CommitOptions,
    expected: Option<Snapshot>,
) -> Result<CommitOutcome> {
    commit_entries_prepared(entries, opts, expected, None).await
}

pub(crate) async fn commit_entries_prepared(
    entries: &[DesiredEntry],
    opts: &CommitOptions,
    expected: Option<Snapshot>,
    reverse: Option<reverse_patch::Prepared>,
) -> Result<CommitOutcome> {
    let _ownership = tokio::task::spawn_blocking(state::mutation_guard).await??;
    let entries = entries.to_vec();
    let opts = opts.clone();
    let (outcome, persist) = tokio::task::spawn_blocking(move || {
        if let Some(expected) = &expected {
            anyhow::ensure!(
                &snapshot(&entries)? == expected,
                "source HEAD or index changed while preparing the commit; retry the command"
            );
        }
        if let Some(reverse) = &reverse {
            reverse.verify()?;
        }
        commit_blocking(&entries, &opts, reverse.as_ref(), expected.as_ref())
    })
    .await??;
    for (target, record) in persist {
        state::persist(&target, record).await.with_context(|| {
            format!("{outcome}; commit/source update succeeded but checkpoint persistence failed at '{}'; inspect HEAD and checkout status before retrying", target.display())
        })?;
    }
    Ok(outcome)
}

/// All-synchronous core: conflict/no-op checks, blob/tree/commit creation
/// (git2), and building the updated state record — none of it needs to be
/// `async`, so it all runs inside one `spawn_blocking` call.
#[allow(clippy::type_complexity)]
fn commit_blocking(
    entries: &[DesiredEntry],
    opts: &CommitOptions,
    reverse: Option<&reverse_patch::Prepared>,
    expected: Option<&Snapshot>,
) -> Result<(CommitOutcome, Vec<(PathBuf, VirtualCheckoutRecord)>)> {
    let mut repository = None;
    let mut paths = Vec::new();
    let mut changes = Vec::new();
    let mut records = Vec::new();
    for entry in entries {
        if reverse.is_none() && !opts.paths.is_empty() {
            crate::desired::content::validate_selection(entry, &opts.paths)?;
        }
        let conflicting_files = if reverse.is_some() {
            reverse_patch::ordinary_conflicts(entry, &opts.paths)?
        } else {
            status::virtual_checkout::commit_conflicts(entry, &opts.paths)?
        };
        if !conflicting_files.is_empty() {
            return Ok((CommitOutcome::Blocked { conflicting_files }, Vec::new()));
        }

        let Provenance::Overlay { source, .. } = &entry.provenance else {
            unreachable!(
                "VirtualCheckout entries only ever carry Provenance::Overlay \
             (see desired::tree::{{collect_own_entries,walk_overlay_tree}})"
            );
        };

        let (repo, managed_path) = git::discover_source(source)?;
        let scope = entry.intent.checkout_scope();
        let selected = |path: &std::path::Path| selects(entry, path, &opts.paths);
        git::ensure_supported_staged_selected(&repo, &managed_path, &scope, &selected)?;
        let mut file_changes =
            git::staged_changes_selected(&repo, &managed_path, &scope, &selected)?;
        let record = state::record_for_entry(entry)?;
        if let Some(reverse) = reverse {
            // Generated source paths participate only through a successfully translated candidate.
            file_changes.retain(|change| {
                !reverse
                    .selected_sources
                    .contains(&source.join(&change.path))
            });
        }
        let destinations: Vec<_> = file_changes
            .iter()
            .filter(|change| {
                reverse.is_none()
                    || record
                        .as_ref()
                        .and_then(|record| record.generated.as_ref())
                        .is_none_or(|generated| {
                            !generated
                                .sources
                                .values()
                                .any(|template| template == &source.join(&change.path))
                        })
            })
            .map(|change| entry.target.join(&change.path))
            .collect();
        crate::desired::content::validate_selection(entry, &destinations)?;
        if let Some(reverse) = reverse {
            for file in reverse
                .files
                .iter()
                .filter(|file| file.target == entry.target)
            {
                file_changes.push(git::FileChange {
                    path: file.source.strip_prefix(source)?.to_path_buf(),
                    kind: git::FileChangeKind::Modified,
                    local_oid: None,
                });
            }
        }
        if file_changes.is_empty() {
            continue;
        }
        let Some(record) = record else {
            anyhow::bail!(
                "no virtual checkout association recorded for '{}' — was it materialized by `over apply`?",
                entry.target.display(),
            );
        };
        state::verify(entry, &record)?;
        let base = git::base_tree(&repo, Some(&record.base_oid))?;
        let head = git::base_tree(&repo, None)?;
        let mut base_entries = git::scoped_entries(&repo, &base, &managed_path, &scope)?;
        let mut head_entries = git::scoped_entries(&repo, &head, &managed_path, &scope)?;
        if reverse.is_some()
            && let Some(generated) = &record.generated
        {
            // Only generated sources have per-file history and a checked disjoint merge.
            let ordinary = |path: &PathBuf, _: &mut (git2::Oid, i32)| {
                !generated
                    .sources
                    .values()
                    .any(|template| template == &source.join(path))
            };
            base_entries.retain(ordinary);
            head_entries.retain(ordinary);
        }
        anyhow::ensure!(
            // A whole-unit checkpoint must not absorb unselected, unsynchronized source changes.
            base_entries == head_entries,
            "checkout '{}' is behind its source; sync before committing",
            entry.target.display()
        );
        drop(base);
        drop(head);
        if let Some(existing) = &repository {
            let existing: &git2::Repository = existing;
            anyhow::ensure!(
                existing.path() == repo.path(),
                "checkout units belong to different source repositories"
            );
        } else {
            repository = Some(repo);
        }
        paths.extend(
            file_changes
                .iter()
                .map(|change| managed_path.join(&change.path)),
        );
        changes.extend(file_changes);
        records.push((entry.target.clone(), record));
    }
    let Some(repo) = repository else {
        return Ok((
            if let Some(reverse) = reverse.filter(|reverse| !reverse.conflicts.is_empty()) {
                CommitOutcome::ReversePatch {
                    oid: None,
                    files: 0,
                    conflicts: reverse.conflicts.clone(),
                }
            } else {
                CommitOutcome::NothingToCommit
            },
            Vec::new(),
        ));
    };
    let message = opts
        .message
        .clone()
        .unwrap_or_else(|| default_message(&changes));
    if crate::crypt::access::configured(&repo)? {
        let index = repo.index()?;
        let head = repo.head()?.peel_to_tree()?;
        for path in [
            ".gitattributes",
            ".over/.gitattributes",
            ".over/crypt/access.json",
        ] {
            let relative = PathBuf::from(path);
            if let Some(entry) = index.get_path(&relative, 0)
                && head.get_path(&relative).ok().map(|old| old.id()) != Some(entry.id)
            {
                if path == ".gitattributes" {
                    let old = head
                        .get_path(&relative)
                        .ok()
                        .map(|entry| repo.find_blob(entry.id()))
                        .transpose()?;
                    let before = old.as_ref().map(|blob| blob.content()).unwrap_or_default();
                    let after = repo.find_blob(entry.id)?;
                    anyhow::ensure!(
                        crate::crypt::policy::is_policy_extension(before, after.content()),
                        "staged .gitattributes includes unrelated edits; commit the complete intended policy/content transition explicitly with Git or separate unrelated policy edits before over commit"
                    );
                }
                // Policy is inseparable from encrypted content; refuse unstaged policy edits.
                anyhow::ensure!(
                    crate::crypt::policy::bootstrap_equal(
                        &relative,
                        repo.find_blob(entry.id)?.content(),
                        &std::fs::read(crate::crypt::access::root(&repo)?.join(path))?
                    ),
                    "resolve unstaged crypt metadata before committing"
                );
                paths.push(relative);
            }
        }
    }
    let oid = if let Some(reverse) = reverse {
        let updates = reverse
            .files
            .iter()
            .map(|file| {
                (
                    file.path.clone(),
                    git::SourceUpdate {
                        indexed: file.indexed.clone(),
                        worktree: file.worktree.clone(),
                        mode: file.mode,
                        expected: file.expected,
                    },
                )
            })
            .collect();
        git::commit_staged_prepared(&repo, &paths, &message, &updates, &mut |oid| {
            if let Some(expected) = expected {
                anyhow::ensure!(
                    &snapshot(entries)? == expected,
                    "source HEAD or index changed while preparing the commit; retry the command"
                );
            }
            reverse.verify()?;
            reverse.journal(&repo, oid)?;
            // Journaling performs filesystem I/O; editors and native Git do not honor our guard.
            reverse.verify_inputs()?;
            if let Some(expected) = expected {
                anyhow::ensure!(
                    &snapshot(entries)? == expected,
                    "source HEAD or index changed while journaling the commit; retry the command"
                );
            }
            Ok(())
        })?
    } else {
        git::commit_staged(&repo, &paths, &message)?
    };

    if reverse.is_some() {
        reverse_patch::recover(entries).with_context(|| format!("commit {oid} succeeded but generated checkpoint publication failed; rerun over commit to recover"))?;
        // Recovery publishes the generated checkpoint without touching physical files.
        for (target, record) in &mut records {
            if let Some(entry) = entries.iter().find(|entry| &entry.target == target) {
                *record =
                    state::record_for_entry(entry)?.context("checkout association disappeared")?;
            }
        }
    }

    for (_, record) in &mut records {
        record.base_oid = oid.to_string();
        record.last_commit_at = Some(crate::materialize::virtual_checkout::now());
    }

    Ok((
        if let Some(reverse) = reverse.filter(|reverse| !reverse.conflicts.is_empty()) {
            CommitOutcome::ReversePatch {
                oid: Some(oid.to_string()),
                files: changes.len(),
                conflicts: reverse.conflicts.clone(),
            }
        } else {
            CommitOutcome::Committed {
                oid: oid.to_string(),
                files: changes.len(),
            }
        },
        records,
    ))
}

/// Default commit message when none was supplied — always mentions how
/// many files changed so an unattended/`--no-prompt` commit is still
/// distinguishable from another in `git log`.
fn default_message(changes: &[git::FileChange]) -> String {
    format!(
        "over commit: {} file{} changed",
        changes.len(),
        if changes.len() == 1 { "" } else { "s" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desired::MaterializationIntent;
    use crate::materialize::virtual_checkout::state::VirtualCheckoutRecord;
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    use git2::Signature;
    use std::fs;

    #[tokio::test]
    async fn changing_the_index_after_preflight_requires_preparing_a_new_commit() {
        let _xdg = isolate_xdg_state();
        let source = TempDir::new().unwrap();
        source.child("a.txt").write_str("original").unwrap();
        let repo = init_committed_repo(source.path());
        let target = TempDir::new().unwrap();
        materialize_and_record(source.path(), target.path()).await;
        let entries = [entry(target.path().into(), source.path().into())];
        let mut index = repo.index().unwrap();
        let selected = index.get_path(std::path::Path::new("a.txt"), 0).unwrap();
        index
            .add_frombuffer(&selected, b"first staged snapshot")
            .unwrap();
        index.write().unwrap();
        let expected = snapshot(&entries).unwrap();
        index
            .add_frombuffer(&selected, b"changed while prompting")
            .unwrap();
        index.write().unwrap();
        let head = repo.head().unwrap().target();
        let error = commit_entries_checked(&entries, &CommitOptions::default(), Some(expected))
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("HEAD or index changed"),
            "{error:#}"
        );
        assert_eq!(repo.head().unwrap().target(), head);
        assert_eq!(fs::read(source.path().join("a.txt")).unwrap(), b"original");
    }

    /// Points `$XDG_STATE_HOME` at a fresh, writable temp dir for the
    /// duration of the returned guard's lifetime — `commit()` reads/writes
    /// `VirtualCheckoutState` via the real, non-injectable `XdgDirs::new()`,
    /// so every test here that calls `commit`/`state::persist`/
    /// `state::record_for_blocking` must isolate this or it silently
    /// pollutes the real `$XDG_STATE_HOME/over/virtual_checkout.toml` with
    /// throwaway records that outlive the test.
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

    fn init_committed_repo(path: &std::path::Path) -> git2::Repository {
        let repo = git2::Repository::init(path).unwrap();
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

    fn entry(target: PathBuf, source: PathBuf) -> DesiredEntry {
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

    async fn materialize_and_record(source_root: &std::path::Path, target: &std::path::Path) {
        let repo = git2::Repository::open(source_root).unwrap();
        fs::create_dir_all(target).unwrap();
        let head_tree = repo.head().unwrap().peel_to_tree().unwrap();
        git::checkout_subtree(&repo, &head_tree, target).unwrap();
        let base_oid = repo
            .head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .id()
            .to_string();
        state::persist(
            target,
            VirtualCheckoutRecord {
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
    async fn non_virtual_checkout_entry_is_refused() {
        let _xdg = isolate_xdg_state();
        let e = DesiredEntry {
            target: PathBuf::from("/tmp/whatever"),
            provenance: Provenance::Overlay {
                overlay: "ov".to_string(),
                source: PathBuf::from("/repo/ov"),
            },
            intent: MaterializationIntent::Directory,
            permissions: None,
        };
        let outcome = commit(&e, &CommitOptions::default()).await.unwrap();
        assert_eq!(outcome, CommitOutcome::NotVirtualCheckout);
    }

    #[tokio::test]
    async fn no_local_changes_is_nothing_to_commit() {
        let _xdg = isolate_xdg_state();
        let td = TempDir::new().unwrap();
        td.child("a.txt").write_str("a").unwrap();
        init_committed_repo(td.path());
        let target = td.child("target");
        materialize_and_record(td.path(), target.path()).await;

        let e = entry(target.path().to_path_buf(), td.path().to_path_buf());
        let outcome = commit(&e, &CommitOptions::default()).await.unwrap();
        assert_eq!(outcome, CommitOutcome::NothingToCommit);
    }

    #[tokio::test]
    async fn modified_file_is_committed_into_the_source_repository() {
        let _xdg = isolate_xdg_state();
        let td = TempDir::new().unwrap();
        td.child("a.txt").write_str("original\n").unwrap();
        let repo = init_committed_repo(td.path());
        let target = td.child("target");
        materialize_and_record(td.path(), target.path()).await;

        fs::write(target.path().join("a.txt"), "edited\n").unwrap();
        let mut index = repo.index().unwrap();
        let indexed = index.get_path(std::path::Path::new("a.txt"), 0).unwrap();
        index.add_frombuffer(&indexed, b"edited\n").unwrap();
        index.write().unwrap();

        let e = entry(target.path().to_path_buf(), td.path().to_path_buf());
        let outcome = commit(
            &e,
            &CommitOptions {
                message: Some("update a.txt".to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let (oid, files) = match outcome {
            CommitOutcome::Committed { oid, files } => (oid, files),
            other => panic!("expected Committed, got {other:?}"),
        };
        assert_eq!(files, 1);

        let commit_oid = git2::Oid::from_str(&oid).unwrap();
        let created_commit = repo.find_commit(commit_oid).unwrap();
        assert_eq!(created_commit.message(), Ok("update a.txt"));
        assert_eq!(created_commit.parent_count(), 1);
        let tree = created_commit.tree().unwrap();
        let blob = tree
            .get_path(std::path::Path::new("a.txt"))
            .unwrap()
            .to_object(&repo)
            .unwrap();
        assert_eq!(blob.as_blob().unwrap().content(), b"edited\n");

        // The recorded base_oid advances so the checkout is clean again.
        let record = state::record_for_blocking(target.path()).unwrap().unwrap();
        assert_eq!(record.base_oid, oid);
        assert!(record.last_commit_at.is_some());

        // Nothing left to commit now.
        let outcome2 = commit(&e, &CommitOptions::default()).await.unwrap();
        assert_eq!(outcome2, CommitOutcome::NothingToCommit);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn checkpoint_failure_reports_the_successful_commit_instead_of_implying_rollback() {
        use std::os::unix::fs::PermissionsExt;
        let xdg = isolate_xdg_state();
        let td = TempDir::new().unwrap();
        td.child("a.txt").write_str("original").unwrap();
        let repo = init_committed_repo(td.path());
        let target = td.child("target");
        materialize_and_record(td.path(), target.path()).await;
        fs::write(target.path().join("a.txt"), "edited").unwrap();
        let mut index = repo.index().unwrap();
        let selected = index.get_path(std::path::Path::new("a.txt"), 0).unwrap();
        index.add_frombuffer(&selected, b"edited").unwrap();
        index.write().unwrap();
        let before = repo.head().unwrap().target();
        let state_dir = xdg.path().join("over");
        // Fail checkpoint publication, not acquisition of the mutation sentinel.
        drop(state::mutation_guard().unwrap());
        let permissions = fs::metadata(&state_dir).unwrap().permissions();
        fs::set_permissions(&state_dir, fs::Permissions::from_mode(0o500)).unwrap();
        let result = commit(
            &entry(target.path().into(), td.path().into()),
            &CommitOptions {
                message: Some("selected update".into()),
                ..Default::default()
            },
        )
        .await;
        // Restore before asserting so failure cannot strand read-only fixtures.
        fs::set_permissions(&state_dir, permissions).unwrap();
        let error = result.unwrap_err().to_string();
        assert!(error.contains("commit/source update succeeded"), "{error}");
        assert!(error.contains(&repo.head().unwrap().target().unwrap().to_string()[..10]));
        assert_ne!(repo.head().unwrap().target(), before);
        assert_eq!(
            fs::read_to_string(td.path().join("a.txt")).unwrap(),
            "edited"
        );
    }

    #[tokio::test]
    async fn commit_preserves_sibling_files_outside_the_managed_path() {
        let _xdg = isolate_xdg_state();
        let td = TempDir::new().unwrap();
        td.child("sub/a.txt").write_str("a").unwrap();
        td.child("other.txt").write_str("sibling").unwrap();
        let repo = init_committed_repo(td.path());

        // A subtree-scoped virtual checkout (managed_path = "sub"), unlike
        // every other test here which roots the checkout at the whole
        // repository.
        let dest_td = TempDir::new().unwrap();
        let sub_target = dest_td.path().join("sub_checkout");
        fs::create_dir_all(&sub_target).unwrap();
        let head_tree = repo.head().unwrap().peel_to_tree().unwrap();
        let subtree = git::managed_subtree(&repo, &head_tree, std::path::Path::new("sub")).unwrap();
        git::checkout_subtree(&repo, &subtree, &sub_target).unwrap();
        let base_oid = repo
            .head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .id()
            .to_string();
        state::persist(
            &sub_target,
            VirtualCheckoutRecord {
                overlay: "ov".to_string(),
                managed_path: PathBuf::from("sub"),
                base_oid,
                created_at: 0,
                last_commit_at: None,
                ..Default::default()
            },
        )
        .await
        .unwrap();

        fs::write(sub_target.join("a.txt"), "edited\n").unwrap();
        let mut index = repo.index().unwrap();
        let indexed = index
            .get_path(std::path::Path::new("sub/a.txt"), 0)
            .unwrap();
        index.add_frombuffer(&indexed, b"edited\n").unwrap();
        index.write().unwrap();

        let e = entry(sub_target.clone(), td.path().join("sub"));
        let outcome = commit(&e, &CommitOptions::default()).await.unwrap();
        let oid = match outcome {
            CommitOutcome::Committed { oid, .. } => oid,
            other => panic!("expected Committed, got {other:?}"),
        };

        let created_commit = repo
            .find_commit(git2::Oid::from_str(&oid).unwrap())
            .unwrap();
        let tree = created_commit.tree().unwrap();
        // The sibling file outside "sub" must still be present, untouched.
        let sibling = tree
            .get_path(std::path::Path::new("other.txt"))
            .unwrap()
            .to_object(&repo)
            .unwrap();
        assert_eq!(sibling.as_blob().unwrap().content(), b"sibling");
    }

    #[tokio::test]
    async fn commit_blocked_on_a_conflicting_file() {
        let _xdg = isolate_xdg_state();
        let td = TempDir::new().unwrap();
        td.child("a.txt").write_str("a").unwrap();
        let repo = init_committed_repo(td.path());
        let target = td.child("target");
        materialize_and_record(td.path(), target.path()).await;

        fs::write(target.path().join("a.txt"), "local edit").unwrap();
        fs::write(td.path().join("a.txt"), "source edit").unwrap();
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
            repo.commit(
                Some("HEAD"),
                &sig,
                &sig,
                "advance differently",
                &tree,
                &[&parent],
            )
            .unwrap();
        }

        let e = entry(target.path().to_path_buf(), td.path().to_path_buf());
        let outcome = commit(&e, &CommitOptions::default()).await.unwrap();
        assert!(outcome.needs_attention());
        match outcome {
            CommitOutcome::Blocked { conflicting_files } => {
                assert_eq!(conflicting_files, vec![PathBuf::from("a.txt")]);
            }
            other => panic!("expected Blocked, got {other:?}"),
        }
        // Never touched: the target still has the local edit, and the
        // source repository never received a new commit for it.
        assert_eq!(
            fs::read_to_string(target.path().join("a.txt")).unwrap(),
            "local edit"
        );
    }

    fn stage_bytes(repo: &git2::Repository, path: &str, bytes: &[u8]) {
        let mut index = repo.index().unwrap();
        let indexed = index.get_path(std::path::Path::new(path), 0).unwrap();
        index.add_frombuffer(&indexed, bytes).unwrap();
        index.write().unwrap();
    }

    #[cfg(unix)]
    #[rstest::rstest]
    #[case(0o600)]
    #[case(0o700)]
    #[tokio::test]
    async fn committing_private_source_files_preserves_their_access_bits(#[case] mode: u32) {
        use std::os::unix::fs::PermissionsExt;

        let _xdg = isolate_xdg_state();
        let source = TempDir::new().unwrap();
        source.child("secret").write_str("original").unwrap();
        fs::set_permissions(
            source.path().join("secret"),
            fs::Permissions::from_mode(mode),
        )
        .unwrap();
        let repo = init_committed_repo(source.path());
        let target = TempDir::new().unwrap();
        materialize_and_record(source.path(), target.path()).await;
        fs::write(target.path().join("secret"), "staged").unwrap();
        stage_bytes(&repo, "secret", b"staged");
        let e = entry(target.path().to_path_buf(), source.path().to_path_buf());

        let outcome = commit(&e, &CommitOptions::default()).await.unwrap();

        assert!(matches!(outcome, CommitOutcome::Committed { .. }));
        assert_eq!(fs::read(source.path().join("secret")).unwrap(), b"staged");
        assert_eq!(
            fs::metadata(source.path().join("secret"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            mode
        );
        let record = state::record_for_blocking(target.path()).unwrap().unwrap();
        assert_eq!(
            record.base_oid,
            repo.head().unwrap().target().unwrap().to_string()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn source_chmod_after_staging_is_rejected_without_mutation() {
        use std::os::unix::fs::PermissionsExt;

        let _xdg = isolate_xdg_state();
        let source = TempDir::new().unwrap();
        source.child("secret").write_str("original").unwrap();
        fs::set_permissions(
            source.path().join("secret"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let repo = init_committed_repo(source.path());
        let target = TempDir::new().unwrap();
        materialize_and_record(source.path(), target.path()).await;
        stage_bytes(&repo, "secret", b"staged");
        fs::write(target.path().join("secret"), "later target edit").unwrap();
        fs::set_permissions(
            source.path().join("secret"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let head = repo.head().unwrap().target().unwrap();
        let index = fs::read(repo.path().join("index")).unwrap();
        let record = state::record_for_blocking(target.path()).unwrap().unwrap();
        let e = entry(target.path().to_path_buf(), source.path().to_path_buf());

        let error = commit(&e, &CommitOptions::default()).await.unwrap_err();

        assert!(error.to_string().contains("unstaged edits"));
        assert_eq!(repo.head().unwrap().target().unwrap(), head);
        assert_eq!(fs::read(repo.path().join("index")).unwrap(), index);
        assert_eq!(fs::read(source.path().join("secret")).unwrap(), b"original");
        assert_eq!(
            fs::metadata(source.path().join("secret"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::read(target.path().join("secret")).unwrap(),
            b"later target edit"
        );
        let after = state::record_for_blocking(target.path()).unwrap().unwrap();
        assert_eq!(after.base_oid, record.base_oid);
        assert_eq!(after.last_commit_at, record.last_commit_at);
    }

    #[rstest::rstest]
    #[case(false)]
    #[case(true)]
    #[tokio::test]
    async fn source_obstructions_and_commit_failures_preserve_all_checkout_state(
        #[case] lock_head: bool,
    ) {
        let _xdg = isolate_xdg_state();
        let source = TempDir::new().unwrap();
        source.child("a").write_str("original a").unwrap();
        source.child("b").write_str("original b").unwrap();
        source
            .child("unrelated")
            .write_str("unrelated original")
            .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(source.path().join("a"), fs::Permissions::from_mode(0o600))
                .unwrap();
            fs::set_permissions(source.path().join("b"), fs::Permissions::from_mode(0o700))
                .unwrap();
        }
        let repo = init_committed_repo(source.path());
        let target = TempDir::new().unwrap();
        materialize_and_record(source.path(), target.path()).await;
        for path in ["a", "b"] {
            stage_bytes(&repo, path, b"staged");
            fs::write(target.path().join(path), "later target edit").unwrap();
        }
        // Metadata is outside the checkout's commit selection, so this staged
        // entry proves a failed transaction doesn't reset the whole index.
        source
            .child("over.toml")
            .write_str("unrelated staged")
            .unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("over.toml")).unwrap();
        index.write().unwrap();
        fs::write(source.path().join("unrelated"), "unrelated worktree edit").unwrap();
        if lock_head {
            // A ref lock makes libgit2 fail after source writes were applied,
            // exercising rollback rather than just the preflight guard.
            let reference = repo.head().unwrap();
            let lock = repo
                .path()
                .join(format!("{}.lock", reference.name().unwrap()));
            fs::write(lock, "occupied").unwrap();
        } else {
            fs::remove_file(source.path().join("b")).unwrap();
            fs::create_dir(source.path().join("b")).unwrap();
            fs::write(source.path().join("b/keep"), "obstruction").unwrap();
        }
        let head = repo.head().unwrap().target().unwrap();
        let index = fs::read(repo.path().join("index")).unwrap();
        let record = state::record_for_blocking(target.path()).unwrap().unwrap();
        let e = entry(target.path().to_path_buf(), source.path().to_path_buf());

        assert!(commit(&e, &CommitOptions::default()).await.is_err());

        assert_eq!(repo.head().unwrap().target().unwrap(), head);
        assert_eq!(fs::read(repo.path().join("index")).unwrap(), index);
        assert_eq!(fs::read(source.path().join("a")).unwrap(), b"original a");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(source.path().join("a"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            if lock_head {
                assert_eq!(
                    fs::metadata(source.path().join("b"))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o700
                );
            }
        }
        if lock_head {
            assert_eq!(fs::read(source.path().join("b")).unwrap(), b"original b");
        } else {
            assert_eq!(
                fs::read(source.path().join("b/keep")).unwrap(),
                b"obstruction"
            );
        }
        for path in ["a", "b"] {
            assert_eq!(
                fs::read(target.path().join(path)).unwrap(),
                b"later target edit"
            );
        }
        assert_eq!(
            fs::read(source.path().join("unrelated")).unwrap(),
            b"unrelated worktree edit"
        );
        let after = state::record_for_blocking(target.path()).unwrap().unwrap();
        assert_eq!(after.base_oid, record.base_oid);
        assert_eq!(after.last_commit_at, record.last_commit_at);
    }

    #[test]
    fn commit_outcome_display_covers_every_variant() {
        assert!(format!("{}", CommitOutcome::NotVirtualCheckout).contains("not checkout"));
        assert!(format!("{}", CommitOutcome::NothingToCommit).contains("nothing to commit"));
        assert!(!CommitOutcome::NothingToCommit.needs_attention());
        let blocked = CommitOutcome::Blocked {
            conflicting_files: vec![PathBuf::from("a.txt"), PathBuf::from("b.txt")],
        };
        let s = format!("{blocked}");
        assert!(s.contains("a.txt"));
        assert!(s.contains("b.txt"));
        assert!(blocked.needs_attention());
        let committed = CommitOutcome::Committed {
            oid: "deadbeefdeadbeefdeadbeef".to_string(),
            files: 3,
        };
        let s = format!("{committed}");
        assert!(s.contains("3 files"));
        assert!(s.contains("deadbeefde"));
        assert!(!committed.needs_attention());
    }
}
