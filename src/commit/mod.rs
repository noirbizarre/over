//! `over commit` (#141): record changes made directly in a virtual
//! checkout back into the overlay's own source repository.
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

use anyhow::Result;

use crate::desired::{DesiredEntry, MaterializationIntent, Provenance};
use crate::materialize::virtual_checkout::git;
use crate::materialize::virtual_checkout::state::{self, VirtualCheckoutRecord};
use crate::status;
use crate::ui::{emojis, style};

/// What `over commit` should do for a single virtual checkout entry.
#[derive(Debug, Clone, Default)]
pub struct CommitOptions {
    /// Commit message. `None` falls back to an auto-generated one (see
    /// [`default_message`]) — callers wanting an interactive prompt (the
    /// CLI, `over sync`) collect it themselves before constructing this.
    pub message: Option<String>,
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
}

impl CommitOutcome {
    pub fn needs_attention(&self) -> bool {
        matches!(self, CommitOutcome::Blocked { .. })
    }
}

impl fmt::Display for CommitOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CommitOutcome::NotVirtualCheckout => write!(
                f,
                "{} {}",
                emojis::WARNING,
                style::yellow("not checkout-materialized"),
            ),
            CommitOutcome::NothingToCommit => {
                write!(
                    f,
                    "{} {}",
                    emojis::CHECKMARK,
                    style::white("nothing to commit")
                )
            }
            CommitOutcome::Blocked { conflicting_files } => write!(
                f,
                "{} {} ({})",
                emojis::WARNING,
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

/// Commit every locally changed, non-conflicting file under `entry`'s
/// virtual checkout into its source repository, updating the checkout's
/// recorded `base_oid` to the new commit. No-ops (informationally) when
/// there's nothing to commit or a conflict blocks it — never partially
/// commits some files while silently dropping conflicting ones.
pub async fn commit(entry: &DesiredEntry, opts: &CommitOptions) -> Result<CommitOutcome> {
    if !matches!(entry.intent, MaterializationIntent::VirtualCheckout) {
        return Ok(CommitOutcome::NotVirtualCheckout);
    }

    let entry = entry.clone();
    let opts = opts.clone();
    let (outcome, persist) =
        tokio::task::spawn_blocking(move || commit_blocking(&entry, &opts)).await??;
    if let Some((target, record)) = persist {
        state::persist(&target, record).await?;
    }
    Ok(outcome)
}

/// All-synchronous core: conflict/no-op checks, blob/tree/commit creation
/// (git2), and building the updated state record — none of it needs to be
/// `async`, so it all runs inside one `spawn_blocking` call.
#[allow(clippy::type_complexity)]
fn commit_blocking(
    entry: &DesiredEntry,
    opts: &CommitOptions,
) -> Result<(CommitOutcome, Option<(PathBuf, VirtualCheckoutRecord)>)> {
    let conflicting_files = status::virtual_checkout::conflicts(entry)?;
    if !conflicting_files.is_empty() {
        return Ok((CommitOutcome::Blocked { conflicting_files }, None));
    }

    let file_changes = status::virtual_checkout::file_changes(entry)?;
    if file_changes.is_empty() {
        return Ok((CommitOutcome::NothingToCommit, None));
    }

    let Some(mut record) = state::record_for_blocking(&entry.target)? else {
        anyhow::bail!(
            "no virtual checkout association recorded for '{}' — was it materialized by \
             `over apply`?",
            entry.target.display(),
        );
    };
    let Provenance::Overlay { source, .. } = &entry.provenance else {
        unreachable!(
            "VirtualCheckout entries only ever carry Provenance::Overlay \
             (see desired::tree::{{collect_own_entries,walk_overlay_tree}})"
        );
    };

    let (repo, managed_path) = git::discover_source(source)?;
    let message = opts
        .message
        .clone()
        .unwrap_or_else(|| default_message(&file_changes));
    let oid = git::commit_changes(&repo, &managed_path, &entry.target, &file_changes, &message)?;

    record.base_oid = oid.to_string();
    record.last_commit_at = Some(crate::materialize::virtual_checkout::now());

    Ok((
        CommitOutcome::Committed {
            oid: oid.to_string(),
            files: file_changes.len(),
        },
        Some((entry.target.clone(), record)),
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
    use crate::materialize::virtual_checkout::state::VirtualCheckoutRecord;
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    use git2::Signature;
    use std::fs;

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
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn non_virtual_checkout_entry_is_refused() {
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
        let td = TempDir::new().unwrap();
        td.child("a.txt").write_str("original\n").unwrap();
        let repo = init_committed_repo(td.path());
        let target = td.child("target");
        materialize_and_record(td.path(), target.path()).await;

        fs::write(target.path().join("a.txt"), "edited\n").unwrap();

        let e = entry(target.path().to_path_buf(), td.path().to_path_buf());
        let outcome = commit(
            &e,
            &CommitOptions {
                message: Some("update a.txt".to_string()),
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

    #[tokio::test]
    async fn commit_preserves_sibling_files_outside_the_managed_path() {
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
            },
        )
        .await
        .unwrap();

        fs::write(sub_target.join("a.txt"), "edited\n").unwrap();

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
