//! Repository-wide remote phases, deliberately separate from declared checkout sync.

use std::path::{Path, PathBuf};
use std::process::Output;

use anyhow::{Context as _, Result, bail, ensure};
use git2::{ErrorCode, ObjectType, Repository, RepositoryState, Status, StatusOptions};
use tokio::process::Command;

pub(crate) struct SourceRepository {
    worktree: PathBuf,
}

pub(crate) struct Upstream {
    branch: String,
    remote: String,
    destination: String,
}

impl SourceRepository {
    pub async fn discover(home: PathBuf) -> Result<Option<Self>> {
        tokio::task::spawn_blocking(move || {
            let repo = match Repository::discover(&home) {
                Ok(repo) => repo,
                Err(error) if error.code() == ErrorCode::NotFound => return Ok(None),
                Err(error) => return Err(error).context("could not inspect the Over source repository"),
            };
            ensure!(
                !matches!(repo.state(), RepositoryState::Rebase | RepositoryState::RebaseInteractive | RepositoryState::RebaseMerge),
                "the Over source repository has a rebase in progress; resolve it with `git rebase --continue` or `git rebase --abort`, then rerun `over sync`"
            );
            let worktree = repo.workdir().context("the Over source repository must have a worktree")?.to_path_buf();
            Ok(Some(Self { worktree }))
        }).await?
    }

    fn command(&self) -> Command {
        let mut command = Command::new("git");
        command.current_dir(&self.worktree);
        // Git hooks/aliases can export routing for an unrelated repository or index.
        for variable in [
            "GIT_DIR",
            "GIT_COMMON_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_PREFIX",
        ] {
            command.env_remove(variable);
        }
        command
    }

    async fn run(&self, args: &[&str], operation: &str) -> Result<Output> {
        let output = self
            .command()
            .args(args)
            .output()
            .await
            .with_context(|| format!("could not {operation} in {}", self.worktree.display()))?;
        ensure!(
            output.status.success(),
            "could not {operation} in {}: {}",
            self.worktree.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
        Ok(output)
    }

    async fn text(&self, args: &[&str], operation: &str) -> Result<String> {
        let output = self.run(args, operation).await?;
        Ok(String::from_utf8(output.stdout)
            .context("Git returned non-UTF-8 branch configuration")?
            .trim()
            .to_owned())
    }

    pub async fn upstream(&self) -> Result<Upstream> {
        let worktree = self.worktree.clone();
        tokio::task::spawn_blocking(move || {
            ensure!(Repository::open(worktree)?.state() == RepositoryState::Clean,
                "finish the in-progress Git operation in the Over source repository before requesting pull or push");
            Ok::<_, anyhow::Error>(())
        }).await??;
        let branch = self
            .text(
                &["symbolic-ref", "--quiet", "HEAD"],
                "resolve the current source branch (detached HEAD cannot be synchronized)",
            )
            .await?;
        let name = branch
            .strip_prefix("refs/heads/")
            .context("source HEAD must name a local branch")?;
        let remote = self.text(&["config", "--get", &format!("branch.{name}.remote")], "read the source branch upstream remote; configure an upstream with ordinary Git tooling").await?;
        let destination = self
            .text(
                &["config", "--get-all", &format!("branch.{name}.merge")],
                "read the source branch upstream; configure an upstream with ordinary Git tooling",
            )
            .await?;
        ensure!(
            !remote.is_empty()
                && !remote.contains('\n')
                && destination.starts_with("refs/heads/")
                && !destination.contains('\n'),
            "the source branch requires one configured upstream branch; configure it with ordinary Git tooling"
        );
        self.run(
            &["check-ref-format", &destination],
            "validate the configured source upstream branch",
        )
        .await?;
        // A local tracking branch uses '.' rather than a named remote.
        if remote != "." {
            self.run(
                &["remote", "get-url", "--", &remote],
                "resolve the configured source upstream remote",
            )
            .await?;
        }
        Ok(Upstream {
            branch,
            remote,
            destination,
        })
    }

    pub async fn pull(&self, upstream: &Upstream) -> Result<()> {
        let worktree = self.worktree.clone();
        tokio::task::spawn_blocking(move || {
            let repo = Repository::open(worktree)?;
            let mut options = StatusOptions::new();
            options.include_untracked(true).recurse_untracked_dirs(true);
            ensure!(repo.statuses(Some(&mut options))?.is_empty(),
                "the Over source worktree or index is dirty; commit or stash your changes with Git before rerunning `over sync --pull` (Over never auto-stashes)");
            Ok::<_, anyhow::Error>(())
        }).await??;
        // Explicit branch selection still uses the remote's configured fetch ref mapping.
        self.run(
            &[
                "fetch",
                "--no-recurse-submodules",
                "--",
                &upstream.remote,
                &upstream.destination,
            ],
            "fetch the source upstream",
        )
        .await?;
        let worktree = self.worktree.clone();
        tokio::task::spawn_blocking(move || {
            let repo = Repository::open(worktree)?;
            protect_ignored_files(&repo)
        })
        .await??;
        // FETCH_HEAD is the just-fetched branch even when its tracking ref did not exist yet.
        // Override ambient rebase settings that could stash changes or rewrite other branches.
        if let Err(error) = self
            .run(
                &[
                    "-c",
                    "rebase.autoStash=false",
                    "-c",
                    "rebase.updateRefs=false",
                    "rebase",
                    "--no-autostash",
                    "--no-update-refs",
                    "--no-autosquash",
                    "--merge",
                    "FETCH_HEAD",
                ],
                "rebase the Over source repository",
            )
            .await
        {
            bail!(
                "{error:#}; resolve with ordinary Git tooling (`git rebase --continue` / `git rebase --abort`), then rerun `over sync`; targets were not reconciled"
            );
        }
        Ok(())
    }

    pub async fn push(&self, upstream: &Upstream) -> Result<()> {
        ensure!(
            self.text(
                &["symbolic-ref", "--quiet", "HEAD"],
                "verify the source branch before pushing"
            )
            .await?
                == upstream.branch,
            "the source branch changed during reconciliation; push it with ordinary Git tooling"
        );
        // Explicit destination and overrides prevent configured mirror/tag/submodule pushes.
        self.run(&["-c", &format!("remote.{}.mirror=false", upstream.remote), "push", "--no-force", "--no-follow-tags", "--recurse-submodules=no", "--", &upstream.remote, &format!("HEAD:{}", upstream.destination)],
            "push the source upstream (local reconciliation has completed; resolve a rejection with Git, then retry)").await?;
        Ok(())
    }
}

fn protect_ignored_files(repo: &Repository) -> Result<()> {
    let mut options = StatusOptions::new();
    options.include_ignored(true).recurse_ignored_dirs(true);
    let statuses = repo.statuses(Some(&mut options))?;
    let ignored = statuses
        .iter()
        .filter(|entry| entry.status().contains(Status::IGNORED))
        .map(|entry| {
            entry
                .path()
                .context("ignored source path is not valid UTF-8")
                .map(|path| PathBuf::from(path.trim_end_matches('/')))
        })
        .collect::<Result<Vec<_>>>()?;
    if ignored.is_empty() {
        return Ok(());
    }
    let incoming = repo.revparse_single("FETCH_HEAD")?.peel_to_commit()?;
    protect_ignored_paths(&incoming.tree()?, &ignored)?;
    // Replaying a local commit can also recreate an ignored path that a later
    // local commit deleted; inspect every replay candidate, not just final HEAD.
    let mut commits = repo.revwalk()?;
    commits.push_head()?;
    commits.hide(incoming.id())?;
    for commit in commits {
        protect_ignored_paths(&repo.find_commit(commit?)?.tree()?, &ignored)?;
    }
    Ok(())
}

fn protect_ignored_paths(tree: &git2::Tree<'_>, ignored: &[PathBuf]) -> Result<()> {
    for path in ignored {
        // A tracked file at an ancestor also replaces an ignored directory's contents.
        for ancestor in path.ancestors().take_while(|path| *path != Path::new("")) {
            match tree.get_path(ancestor) {
                Ok(entry) => ensure!(
                    ancestor != path && entry.kind() == Some(ObjectType::Tree),
                    "source checkout is obstructed by ignored path '{}'; move it out of the way before retrying pull",
                    path.display()
                ),
                Err(error) if error.code() == ErrorCode::NotFound => continue,
                Err(error) => {
                    return Err(error).context("could not inspect source checkout obstructions");
                }
            }
        }
    }
    Ok(())
}
