//! Checkout materialization backend (#110).
//!
//! Fills the registry seam ADR-012 documented but deliberately left empty:
//! [`MaterializationIntent::Checkout`] entries now have a real
//! [`Materializer`]. Content-level bidirectional synchronization (fetch/
//! merge/push) is *not* implemented here — that's `over sync`
//! (`crate::sync`). This backend only owns the same "ensure the repository
//! is present in its configured form" concern
//! `actions::git::clone_repositories` already implements, wired into the
//! `DesiredTree` → `Plan` → `Materializer` pipeline like every other intent.
//!
//! `Overlay::apply_inner` still calls `actions::git::clone_repositories`
//! directly, in parallel, before building its `Plan` (unchanged, ADR-014) —
//! by the time `Plan::build` classifies a `Checkout` entry during a normal
//! `apply`, the repository already exists and classifies as `Operation::Noop`.
//! This materializer's `materialize` is therefore a correctness fallback
//! (any other caller of `Plan::execute`, e.g. tests), not the primary path.

use anyhow::Result;
use async_trait::async_trait;

use crate::actions;
use crate::desired::{DesiredEntry, MaterializationIntent, Provenance};
use crate::exec::{Action, Ctx};
use crate::plan::{Operation, PlanStep};
use crate::status::{self, Status};

use super::Materializer;

/// Owns [`MaterializationIntent::Checkout`]. See the module doc for the
/// scope split with `over sync`.
pub struct CheckoutMaterializer;

#[async_trait(?Send)]
impl Materializer for CheckoutMaterializer {
    fn handles(&self, intent: &MaterializationIntent) -> bool {
        matches!(intent, MaterializationIntent::Checkout)
    }

    /// Reuses `status::git::inspect` verbatim rather than re-deriving
    /// clean/dirty/ahead/behind/conflict detection — the same read-only
    /// inspection `over status`/`over diff` already rely on.
    fn classify(&self, entry: &DesiredEntry) -> Result<Operation> {
        Ok(match status::git::inspect(entry)? {
            Status::Missing => Operation::Create,
            // Applied/Modified/Ahead/Behind/Diverged/Broken/Conflict all
            // classify as `Noop`: `apply`/`Plan::execute` never mutates an
            // *existing* checkout's content — only `over sync` does, and
            // `over status`/`over diff` already surface these states via
            // the same `status::git::inspect` call directly. Mapping them
            // to `Operation::Conflict` would route a git checkout through
            // the filesystem conflict-resolution UI (force/no_prompt/
            // absorb-diff prompts) built for symlinks/files — the wrong
            // semantics here, and a violation of "never discard
            // uncommitted changes implicitly".
            _ => Operation::Noop,
        })
    }

    /// Only ever invoked for `Operation::Create` (`Plan::execute` skips
    /// `Noop`/`Deferred`) — i.e. only when the repository doesn't exist
    /// yet. Delegates to the same, unchanged `EnsureGitRepository` action
    /// `actions::git::clone_repositories` already runs per entry.
    async fn materialize(&self, ctx: Ctx, step: &PlanStep) -> Result<()> {
        let Provenance::Git {
            config, overlay, ..
        } = &step.entry.provenance
        else {
            unreachable!(
                "Checkout entries only ever carry Provenance::Git \
                 (see desired::tree::collect_own_entries)"
            );
        };
        actions::git::EnsureGitRepository::new(
            step.entry.target.clone(),
            (**config).clone(),
            overlay.clone(),
        )
        .execute(ctx)
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::git::config::GitRepoConfig;
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    use git2::Signature;
    use std::fs;
    use std::path::PathBuf;

    fn git_config(url: &str) -> GitRepoConfig {
        GitRepoConfig {
            url: url.to_string(),
            branch: None,
            tag: None,
            rev: None,
            recurse_submodules: false,
            worktree: false,
            per_worktree_config: false,
            worktrees: None,
            remotes: None,
            config: None,
            worktree_config: None,
        }
    }

    fn entry(target: PathBuf, config: GitRepoConfig) -> DesiredEntry {
        DesiredEntry {
            target,
            provenance: Provenance::Git {
                overlay: "ov".to_string(),
                repo_key: ".".to_string(),
                config: Box::new(config),
            },
            intent: MaterializationIntent::Checkout,
        }
    }

    fn init_committed_repo(path: &std::path::Path) {
        let repo = git2::Repository::init(path).unwrap();
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

    #[test]
    fn handles_only_checkout_intent() {
        let m = CheckoutMaterializer;
        assert!(m.handles(&MaterializationIntent::Checkout));
        assert!(!m.handles(&MaterializationIntent::Directory));
        assert!(!m.handles(&MaterializationIntent::SymlinkFile {
            source: PathBuf::from("/src"),
            link_type: crate::actions::symlink::LinkType::Soft,
        }));
    }

    #[test]
    fn classify_missing_checkout_is_create() {
        let m = CheckoutMaterializer;
        let td = TempDir::new().unwrap();
        let e = entry(td.path().join("does-not-exist"), git_config(""));
        assert!(matches!(m.classify(&e).unwrap(), Operation::Create));
    }

    #[test]
    fn classify_clean_checkout_is_noop() {
        let m = CheckoutMaterializer;
        let td = TempDir::new().unwrap();
        init_committed_repo(td.path());
        let e = entry(td.path().to_path_buf(), git_config(""));
        assert!(matches!(m.classify(&e).unwrap(), Operation::Noop));
    }

    #[test]
    fn classify_dirty_checkout_is_noop_not_conflict() {
        // A dirty (or ahead/behind/diverged/conflicted/broken) checkout
        // must never classify as `Operation::Conflict` — that would route
        // it through filesystem conflict-resolution semantics that could
        // discard the checkout's content.
        let m = CheckoutMaterializer;
        let td = TempDir::new().unwrap();
        init_committed_repo(td.path());
        fs::write(td.path().join("README.md"), "changed").unwrap();
        let e = entry(td.path().to_path_buf(), git_config(""));
        assert!(matches!(m.classify(&e).unwrap(), Operation::Noop));
    }

    #[test]
    fn classify_broken_checkout_is_noop() {
        let m = CheckoutMaterializer;
        let td = TempDir::new().unwrap();
        let dir = td.child("plain");
        dir.create_dir_all().unwrap();
        let e = entry(dir.path().to_path_buf(), git_config(""));
        assert!(matches!(m.classify(&e).unwrap(), Operation::Noop));
    }

    #[tokio::test]
    async fn materialize_clones_missing_repository() {
        use crate::exec::Context;
        use crate::overlays::Repository;

        let source_td = TempDir::new().unwrap();
        init_committed_repo(source_td.path());

        let dest_td = TempDir::new().unwrap();
        let target = dest_td.path().join("checkout");

        let m = CheckoutMaterializer;
        let e = entry(
            target.clone(),
            git_config(source_td.path().to_str().unwrap()),
        );
        let step = PlanStep {
            entry: e,
            operation: Operation::Create,
        };

        let repo = Repository::new(dest_td.path().to_path_buf());
        let ctx = Context::builder()
            .root(dest_td.path().to_path_buf())
            .repository(repo)
            .build()
            .with_multiprogress(indicatif::MultiProgress::new());

        m.materialize(ctx, &step).await.unwrap();

        assert!(target.join(".git").exists());
        assert!(target.join("README.md").exists());
    }
}
