pub mod config;

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use config::{GitRepoConfig, ROOT_PATH};
use futures::future::join_all;
use git2::{Progress, Repository};
use git2_credentials::CredentialHandler;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::sync::LazyLock;
use tokio::{
    spawn,
    sync::mpsc::{self, Sender},
    task::spawn_blocking,
};

use crate::overlays::Overlay;
use crate::{
    exec::{Action, Ctx},
    ui::{self, emojis, style},
};

pub async fn clone_repositories(ctx: Ctx, overlay: &Overlay, to: &Path) -> Result<()> {
    if let Some(git_repos) = &overlay.git {
        ui::info(format!(
            "{} {}",
            emojis::THREAD,
            style::white("Cloning repositories"),
        ))?;
        let subctx = ctx.with_multiprogress(MultiProgress::new());
        let results = join_all(git_repos.iter().map(|(path, repo_config)| {
            let target = if path == ROOT_PATH {
                to.to_path_buf()
            } else {
                to.join(path)
            };
            let repo_config = repo_config.clone();
            let overlay_name = overlay.name.clone();
            let ctx = subctx.clone();
            spawn(async move {
                let action = EnsureGitRepository::new(target, repo_config, overlay_name);
                action.execute(ctx).await
            })
        }))
        .await;
        let mut errors: Vec<anyhow::Error> = Vec::new();
        for result in results {
            match result {
                Ok(Ok(())) => {}
                Ok(Err(e)) => errors.push(e),
                Err(e) => errors.push(anyhow!("clone task panicked: {}", e)),
            }
        }
        if !errors.is_empty() {
            let msgs: Vec<String> = errors.iter().map(|e| format!("{e:#}")).collect();
            return Err(anyhow!(
                "Failed to clone {} repositories:\n  {}",
                msgs.len(),
                msgs.join("\n  ")
            ));
        }
    };
    Ok(())
}

pub struct EnsureGitRepository {
    pub path: PathBuf,
    pub config: GitRepoConfig,
    pub overlay_name: String,
}

impl EnsureGitRepository {
    pub fn new(path: PathBuf, config: GitRepoConfig, overlay_name: String) -> Self {
        Self {
            path,
            config,
            overlay_name,
        }
    }

    fn short_name(&self) -> String {
        self.config
            .url
            .split("/")
            .last()
            .unwrap_or("repo")
            .trim_end_matches(".git")
            .to_string()
    }
}

impl fmt::Display for EnsureGitRepository {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} -> {}", self.path.display(), self.config.url)
    }
}

#[async_trait]
impl Action for EnsureGitRepository {
    async fn execute(&self, ctx: Ctx) -> Result<()> {
        let pb = ctx
            .try_multiprogress()
            .ok_or_else(|| anyhow!("MultiProgress not available for git clone"))?
            .add(ProgressBar::new(100));
        pb.set_style(CLONE_PROGRESS_STYLE.clone());
        pb.set_prefix(self.short_name());

        let repo_path = if self.config.worktree || self.config.worktrees.is_some() {
            self.path.join(".git")
        } else {
            self.path.clone()
        };

        // For bare repos (worktree mode), repo_path is `path/.git` which is
        // specific enough. For normal repos, check for `.git` inside the
        // directory so a pre-existing empty directory isn't mistaken for a repo
        // (e.g. the target root created by overlay apply before cloning).
        let is_bare = self.config.worktree || self.config.worktrees.is_some();
        let exists = if is_bare {
            repo_path.exists()
        } else {
            repo_path.join(".git").exists()
        };

        if !exists && !ctx.dry_run {
            let mut state = CloneState::default();
            let url = self.config.url.clone();
            let into = repo_path.clone();
            let branch = self.config.branch.clone();
            let recurse = self.config.recurse_submodules;
            let (tx, mut rx) = mpsc::channel(100);
            let tx = Arc::new(tx);
            let task = spawn_blocking(move || {
                clone(&url, &into, branch.as_deref(), is_bare, recurse, &tx)
            });

            while let Some(msg) = rx.recv().await {
                match msg {
                    CloneMessage::Progress(pr) => state.progress = pr,
                    CloneMessage::Stats(s) => state.stats = s,
                }
                state.update_bar(&pb)?;
            }

            if let Err(e) = task.await? {
                pb.println(format!("{} {}", emojis::CROSSMARK, e));
                pb.abandon_with_message(format!("{} Failed", emojis::CROSSMARK));
                return Err(anyhow!(e));
            }
        }

        if exists && !ctx.dry_run {
            if ctx.verbose {
                pb.set_style(DONE_PROGRESS_STYLE.clone());
                pb.finish_with_message("Repository exists");
            } else {
                pb.finish_and_clear();
            }
        } else {
            pb.finish_and_clear();
        }

        // Apply idempotent operations (remotes, worktrees, config) on both new and existing repos
        if !ctx.dry_run && repo_path.exists() {
            let repo_path = repo_path.clone();
            let base_path = self.path.clone();
            let config = self.config.clone();
            let overlay_name = self.overlay_name.clone();
            let verbose = ctx.verbose;
            spawn_blocking(move || {
                let repo = if config.worktree || config.worktrees.is_some() {
                    Repository::open_bare(&repo_path)
                } else {
                    Repository::open(&repo_path)
                }
                .with_context(|| format!("failed to open repository at {}", repo_path.display()))?;

                // Checkout specific tag or rev if requested (only for non-bare repos)
                if !(config.worktree || config.worktrees.is_some()) {
                    checkout_ref(&repo, &config)?;
                }

                // Ensure extra remotes
                if let Some(remotes) = &config.remotes {
                    ensure_remotes(&repo, remotes, verbose)?;
                }

                // Ensure worktrees
                if config.worktree || config.worktrees.is_some() {
                    ensure_worktrees(&repo, &base_path, &config, verbose)?;
                }

                // Apply git config
                if let Some(git_config) = &config.config {
                    apply_git_config(&repo, &git_config.entries, verbose)?;
                }

                // Apply per-worktree config (auto-managed + user entries)
                apply_per_worktree_config(&repo, &config, verbose)?;

                // Associate the overlay with this repository so that
                // subsequent `git over mount` skips the overlay prompt.
                let mut git_config = repo.config().with_context(|| {
                    format!("failed to open git config for {}", repo_path.display())
                })?;
                git_config
                    .set_str("over.overlay", &overlay_name)
                    .with_context(|| {
                        format!(
                            "failed to set over.overlay in git config for {}",
                            repo_path.display()
                        )
                    })?;

                Ok::<(), anyhow::Error>(())
            })
            .await??;
        }

        Ok(())
    }
}

use anyhow::Context as AnyhowContext;

/// Checkout a specific tag or rev on a non-bare repo.
fn checkout_ref(repo: &Repository, config: &GitRepoConfig) -> Result<()> {
    // branch is handled at clone time via RepoBuilder; tag/rev are post-clone operations
    if let Some(tag) = &config.tag {
        let (obj, reference) = repo.revparse_ext(tag)?;
        repo.checkout_tree(&obj, None)?;
        if let Some(reference) = reference {
            repo.set_head(
                reference
                    .name()
                    .ok_or_else(|| anyhow!("invalid reference name for tag {}", tag))?,
            )?;
        } else {
            repo.set_head_detached(obj.id())?;
        }
    } else if let Some(rev) = &config.rev {
        let obj = repo.revparse_single(rev)?;
        repo.checkout_tree(&obj, None)?;
        repo.set_head_detached(obj.id())?;
    }
    Ok(())
}

/// Ensure extra remotes exist with correct URLs and refspecs.
fn ensure_remotes(
    repo: &Repository,
    remotes: &HashMap<String, config::RemoteConfig>,
    verbose: bool,
) -> Result<()> {
    let existing_remotes = repo.remotes()?;
    let existing_names: Vec<&str> = existing_remotes.iter().flatten().collect();

    for (name, remote_config) in remotes {
        if existing_names.contains(&name.as_str()) {
            // Remote exists — update URL if different
            let existing = repo.find_remote(name.as_str())?;
            if existing.url() != Some(&remote_config.url) {
                repo.remote_set_url(name, &remote_config.url)?;
                if verbose {
                    println!("  Updated remote {name} URL to {}", remote_config.url);
                }
            }
            drop(existing);
        } else {
            // Create new remote
            if let Some(fetch) = &remote_config.fetch {
                repo.remote_with_fetch(name, &remote_config.url, fetch)?;
            } else {
                repo.remote(name, &remote_config.url)?;
            }
            if verbose {
                println!("  Added remote {name} -> {}", remote_config.url);
            }
        }

        // Apply push refspec if specified (check if already present to be idempotent)
        if let Some(push) = &remote_config.push {
            let existing = repo.find_remote(name.as_str())?;
            let already_has = existing
                .push_refspecs()?
                .into_iter()
                .flatten()
                .any(|r| r == push.as_str());
            drop(existing);
            if !already_has {
                repo.remote_add_push(name, push)?;
            }
        }

        // Apply arbitrary extra config keys to the remote section
        if !remote_config.extras.is_empty() {
            let mut git_cfg = repo.config()?;
            for (key, value) in &remote_config.extras {
                let config_key = format!("remote.{name}.{key}");
                git_cfg
                    .set_str(&config_key, value)
                    .with_context(|| format!("failed to set {config_key}"))?;
            }
        }

        // Apply tagopt
        if let Some(tagopt) = &remote_config.tagopt {
            let mut git_cfg = repo.config()?;
            let config_key = format!("remote.{name}.tagopt");
            git_cfg
                .set_str(&config_key, tagopt)
                .with_context(|| format!("failed to set {config_key}"))?;
        }
    }

    Ok(())
}

/// Ensure worktrees exist for a bare repository.
fn ensure_worktrees(
    repo: &Repository,
    base_path: &Path,
    config: &GitRepoConfig,
    verbose: bool,
) -> Result<()> {
    let existing_worktrees = repo.worktrees()?;
    let existing_names: Vec<&str> = existing_worktrees.iter().flatten().collect();

    // Auto-create default branch worktree when worktree=true
    if config.worktree {
        let default_branch = detect_default_branch(repo)?;
        let wt_path = base_path.join(&default_branch);
        if !existing_names.contains(&default_branch.as_str()) && !wt_path.exists() {
            create_worktree(repo, &default_branch, &default_branch, &wt_path, verbose)?;
        }
    }

    // Create explicitly named worktrees
    if let Some(worktrees) = &config.worktrees {
        for (name, entry) in worktrees {
            let wt_path = base_path.join(name);
            if !existing_names.contains(&name.as_str()) && !wt_path.exists() {
                create_worktree(repo, name, &entry.branch, &wt_path, verbose)?;
            }

            // Apply per-worktree config if specified
            if let Some(ref wt_config) = entry.config {
                let wt_config_path = repo
                    .path()
                    .join("worktrees")
                    .join(name)
                    .join("config.worktree");
                // Only apply if the worktree directory exists (i.e. was created or already existed)
                if wt_config_path.parent().is_some_and(|p| p.exists()) {
                    apply_config_to_file(&wt_config_path, &wt_config.entries, verbose)?;
                }
            }
        }
    }

    Ok(())
}

/// Detect the default branch name from a repository's HEAD or remote refs.
fn detect_default_branch(repo: &Repository) -> Result<String> {
    // Try HEAD reference first
    if let Ok(head) = repo.head()
        && let Some(name) = head.shorthand()
    {
        return Ok(name.to_string());
    }

    // For bare repos, try to read the symbolic ref from origin/HEAD
    if let Ok(reference) = repo.find_reference("refs/remotes/origin/HEAD")
        && let Some(target) = reference.symbolic_target()
        && let Some(branch) = target.strip_prefix("refs/remotes/origin/")
    {
        return Ok(branch.to_string());
    }

    // Fallback to common defaults
    for candidate in &["main", "master"] {
        let refname = format!("refs/remotes/origin/{candidate}");
        if repo.find_reference(&refname).is_ok() {
            return Ok(candidate.to_string());
        }
    }

    Ok("main".to_string())
}

/// Create a single worktree from a branch.
fn create_worktree(
    repo: &Repository,
    name: &str,
    branch: &str,
    wt_path: &Path,
    verbose: bool,
) -> Result<()> {
    // Try to find the branch as a local branch first, then as a remote tracking branch
    let reference = if let Ok(branch_ref) = repo.find_branch(branch, git2::BranchType::Local) {
        Some(branch_ref.into_reference())
    } else {
        let remote_ref = format!("refs/remotes/origin/{branch}");
        if let Ok(reference) = repo.find_reference(&remote_ref) {
            // Create a local branch tracking the remote
            let commit = reference.peel_to_commit()?;
            let local_branch = repo.branch(branch, &commit, false)?;
            Some(local_branch.into_reference())
        } else {
            None
        }
    };

    if let Some(reference) = reference {
        let mut opts = git2::WorktreeAddOptions::new();
        opts.reference(Some(&reference));
        repo.worktree(name, wt_path, Some(&opts)).with_context(|| {
            format!(
                "failed to create worktree '{name}' at {}",
                wt_path.display()
            )
        })?;
        if verbose {
            println!(
                "  Created worktree '{name}' at {} (branch: {branch})",
                wt_path.display()
            );
        }
    } else {
        return Err(anyhow!("branch '{branch}' not found for worktree '{name}'"));
    }

    Ok(())
}

/// Apply arbitrary git config entries to a repository.
fn apply_git_config(
    repo: &Repository,
    entries: &HashMap<String, String>,
    verbose: bool,
) -> Result<()> {
    let mut git_cfg = repo.config()?;
    for (key, value) in entries {
        git_cfg
            .set_str(key, value)
            .with_context(|| format!("failed to set git config {key}"))?;
        if verbose {
            println!("  Set config {key} = {value}");
        }
    }
    Ok(())
}

/// Apply git config entries to a `config.worktree` file at the given path.
///
/// Opens (or creates) the file via `git2::Config::open` and writes each
/// entry.  This is used for both the bare repository's own
/// `.git/config.worktree` and for per-worktree files under
/// `.git/worktrees/<name>/config.worktree`.
fn apply_config_to_file(
    path: &Path,
    entries: &HashMap<String, String>,
    verbose: bool,
) -> Result<()> {
    let mut cfg = git2::Config::open(path)
        .with_context(|| format!("failed to open config at {}", path.display()))?;
    for (key, value) in entries {
        cfg.set_str(key, value)
            .with_context(|| format!("failed to set {key} in {}", path.display()))?;
        if verbose {
            println!("  Set {} {key} = {value}", path.display());
        }
    }
    Ok(())
}

/// Apply auto-managed worktree config entries for bare repos.
///
/// When `per_worktree_config` is enabled:
/// - Writes `core.bare = false` and `extensions.worktreeConfig = true` to
///   `.git/config` (shared config).
/// - Writes `core.bare = true` to `.git/config.worktree` (bare repo's own
///   worktree config).
/// - Writes any additional user-specified entries from `worktree_config`
///   to `.git/config.worktree`.
fn apply_per_worktree_config(
    repo: &Repository,
    config: &GitRepoConfig,
    verbose: bool,
) -> Result<()> {
    if !config.per_worktree_config {
        return Ok(());
    }

    // Auto-managed shared config entries
    let mut git_cfg = repo.config()?;
    git_cfg
        .set_str("extensions.worktreeConfig", "true")
        .context("failed to set extensions.worktreeConfig")?;
    if verbose {
        println!("  Set config extensions.worktreeConfig = true");
    }

    let is_bare = config.worktree || config.worktrees.is_some();
    if is_bare {
        git_cfg
            .set_str("core.bare", "false")
            .context("failed to set core.bare = false in shared config")?;
        if verbose {
            println!("  Set config core.bare = false");
        }
    }
    drop(git_cfg);

    // Auto-managed + user entries in config.worktree
    let wt_config_path = repo.path().join("config.worktree");
    if is_bare {
        let mut auto_entries = HashMap::new();
        auto_entries.insert("core.bare".to_string(), "true".to_string());
        apply_config_to_file(&wt_config_path, &auto_entries, verbose)?;
    }

    // Additional user-specified worktree config entries
    if let Some(ref wt_config) = config.worktree_config {
        apply_config_to_file(&wt_config_path, &wt_config.entries, verbose)?;
    }

    Ok(())
}

static CLONE_PROGRESS_STYLE: LazyLock<ProgressStyle> = LazyLock::new(|| {
    ProgressStyle::with_template("{spinner:.cyan} {prefix} [{bar:.green/yellow}] {msg}")
        .unwrap()
        .tick_chars(style::TICK_CHARS_BRAILLE_4_6_DOWN.as_str())
        .progress_chars(style::THIN_PROGRESS.as_str())
});

static DONE_PROGRESS_STYLE: LazyLock<ProgressStyle> =
    LazyLock::new(|| ProgressStyle::with_template("✅ {prefix}: {msg}").unwrap());

fn clone(
    url: &str,
    dst: &Path,
    branch: Option<&str>,
    bare: bool,
    recurse_submodules: bool,
    progress: &Sender<CloneMessage>,
) -> Result<Repository> {
    let mut cb = git2::RemoteCallbacks::new();
    let git_config = git2::Config::open_default()?;

    // Credentials management
    let mut ch = CredentialHandler::new(git_config);
    cb.credentials(move |url, username, allowed| ch.try_next_credential(url, username, allowed));
    cb.transfer_progress(|stats| {
        let stats = CloneStats::from(stats);
        // Ignore send errors (receiver may have dropped)
        let _ = progress.blocking_send(CloneMessage::Stats(stats));
        true
    });

    let mut co = git2::build::CheckoutBuilder::new();
    co.progress(|path, cur, total| {
        let prog = CloneProgress {
            path: path.map(|p| p.to_path_buf()),
            current: cur,
            total,
        };
        let _ = progress.blocking_send(CloneMessage::Progress(prog));
    });

    // Clone the repository
    let mut fo = git2::FetchOptions::new();
    fo.remote_callbacks(cb)
        .download_tags(git2::AutotagOption::All)
        .update_fetchhead(true);

    let mut builder = git2::build::RepoBuilder::new();
    builder.fetch_options(fo);

    if bare {
        builder.bare(true);
    } else {
        builder.with_checkout(co);
    }

    if let Some(branch) = branch {
        builder.branch(branch);
    }

    let repo = builder.clone(url, dst)?;

    // Recurse submodules if requested (only for non-bare repos)
    if recurse_submodules && !bare {
        update_submodules(&repo)?;
    }

    Ok(repo)
}

fn update_submodules(repo: &Repository) -> Result<()> {
    for mut submodule in repo.submodules()? {
        submodule.update(true, None)?;
    }
    Ok(())
}

#[derive(Debug, Default)]
struct CloneStats {
    total_objects: usize,
    indexed_objects: usize,
    received_objects: usize,
    total_deltas: usize,
    indexed_deltas: usize,
    received_bytes: usize,
}

impl CloneStats {
    fn from(stats: Progress) -> Self {
        Self {
            total_objects: stats.total_objects(),
            indexed_objects: stats.indexed_objects(),
            received_objects: stats.received_objects(),
            total_deltas: stats.total_deltas(),
            indexed_deltas: stats.indexed_deltas(),
            received_bytes: stats.received_bytes(),
        }
    }
}

#[derive(Debug, Default)]
struct CloneProgress {
    total: usize,
    current: usize,
    path: Option<PathBuf>,
}

#[derive(Debug)]
enum CloneMessage {
    Stats(CloneStats),
    Progress(CloneProgress),
}

#[derive(Debug, Default)]
struct CloneState {
    stats: CloneStats,
    progress: CloneProgress,
}

impl CloneState {
    fn update_bar(&self, bar: &ProgressBar) -> Result<()> {
        let stats = &self.stats;
        if stats.total_objects == 0 {
            return Ok(());
        }
        let network_pct = (100 * stats.received_objects) / stats.total_objects;
        let index_pct = (100 * stats.indexed_objects) / stats.total_objects;
        let co_pct = if self.progress.total > 0 {
            (100 * self.progress.current) / self.progress.total
        } else {
            0
        };
        bar.set_length(u64::try_from(stats.total_objects)?);
        bar.set_position(u64::try_from(stats.indexed_objects)?);
        let kbytes = stats.received_bytes / 1024;
        if stats.received_objects == stats.total_objects {
            bar.set_message(format!(
                "Resolving deltas {}/{}\r",
                stats.indexed_deltas, stats.total_deltas
            ));
        } else {
            bar.set_message(format!(
                "net {:3}% ({:4} kb, {:5}/{:5})  /  idx {:3}% ({:5}/{:5})  \
                    /  chk {:3}% ({:4}/{:4}) {}\r",
                network_pct,
                kbytes,
                stats.received_objects,
                stats.total_objects,
                index_pct,
                stats.indexed_objects,
                stats.total_objects,
                co_pct,
                self.progress.current,
                self.progress.total,
                self.progress
                    .path
                    .as_ref()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default()
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Create a source git repo with an initial commit and a branch for testing.
    fn create_source_repo() -> (TempDir, Repository) {
        let td = TempDir::new().unwrap();
        let repo = Repository::init(td.path()).unwrap();

        // Configure committer identity for the test repo
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "Test").unwrap();
        cfg.set_str("user.email", "test@test.com").unwrap();
        drop(cfg);

        // Create initial commit on main
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();
        fs::write(td.path().join("README.md"), "# Test").unwrap();
        let commit_id = {
            let mut index = repo.index().unwrap();
            index
                .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
                .unwrap();
            index.write().unwrap();
            let tree_id = index.write_tree().unwrap();
            let tree = repo.find_tree(tree_id).unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
                .unwrap()
        };

        // Rename default branch to "main"
        {
            let mut head = repo.find_branch("master", git2::BranchType::Local);
            if head.is_err() {
                head = repo.find_branch("main", git2::BranchType::Local);
            }
            if let Ok(mut branch) = head {
                let _ = branch.rename("main", true);
            }
        }

        // Create a "develop" branch
        {
            let commit = repo.find_commit(commit_id).unwrap();
            repo.branch("develop", &commit, false).unwrap();
        }

        (td, repo)
    }

    #[test]
    fn test_ensure_remotes_adds_new_remote() {
        let (_td, repo) = create_source_repo();
        let mut remotes = HashMap::new();
        remotes.insert(
            "upstream".to_string(),
            config::RemoteConfig {
                url: "https://github.com/upstream/repo.git".to_string(),
                fetch: Some("+refs/heads/*:refs/remotes/upstream/*".to_string()),
                push: None,
                tagopt: None,
                extras: HashMap::new(),
            },
        );

        ensure_remotes(&repo, &remotes, false).unwrap();

        let remote = repo.find_remote("upstream").unwrap();
        assert_eq!(
            remote.url().unwrap(),
            "https://github.com/upstream/repo.git"
        );
    }

    #[test]
    fn test_ensure_remotes_updates_url() {
        let (_td, repo) = create_source_repo();

        // Add a remote manually
        repo.remote("upstream", "https://old-url.com/repo.git")
            .unwrap();

        // Now ensure with a new URL
        let mut remotes = HashMap::new();
        remotes.insert(
            "upstream".to_string(),
            config::RemoteConfig {
                url: "https://new-url.com/repo.git".to_string(),
                fetch: None,
                push: None,
                tagopt: None,
                extras: HashMap::new(),
            },
        );

        ensure_remotes(&repo, &remotes, false).unwrap();

        let remote = repo.find_remote("upstream").unwrap();
        assert_eq!(remote.url().unwrap(), "https://new-url.com/repo.git");
    }

    #[test]
    fn test_ensure_remotes_with_extras() {
        let (_td, repo) = create_source_repo();
        let mut extras = HashMap::new();
        extras.insert("dmb-hierarchical".to_string(), "true".to_string());

        let mut remotes = HashMap::new();
        remotes.insert(
            "upstream".to_string(),
            config::RemoteConfig {
                url: "https://github.com/upstream/repo.git".to_string(),
                fetch: None,
                push: None,
                tagopt: Some("--tags".to_string()),
                extras,
            },
        );

        ensure_remotes(&repo, &remotes, false).unwrap();

        let cfg = repo.config().unwrap();
        assert_eq!(
            cfg.get_string("remote.upstream.dmb-hierarchical").unwrap(),
            "true"
        );
        assert_eq!(cfg.get_string("remote.upstream.tagopt").unwrap(), "--tags");
    }

    #[test]
    fn test_ensure_remotes_idempotent() {
        let (_td, repo) = create_source_repo();
        let mut remotes = HashMap::new();
        remotes.insert(
            "upstream".to_string(),
            config::RemoteConfig {
                url: "https://github.com/upstream/repo.git".to_string(),
                fetch: None,
                push: None,
                tagopt: None,
                extras: HashMap::new(),
            },
        );

        // Run twice — should not fail
        ensure_remotes(&repo, &remotes, false).unwrap();
        ensure_remotes(&repo, &remotes, false).unwrap();

        let all_remotes = repo.remotes().unwrap();
        let remote_names: Vec<&str> = all_remotes.iter().flatten().collect();
        assert_eq!(remote_names.iter().filter(|n| **n == "upstream").count(), 1);
    }

    #[test]
    fn test_apply_git_config() {
        let (_td, repo) = create_source_repo();
        let mut entries = HashMap::new();
        entries.insert("user.email".to_string(), "work@example.com".to_string());
        entries.insert("core.autocrlf".to_string(), "true".to_string());

        apply_git_config(&repo, &entries, false).unwrap();

        let cfg = repo.config().unwrap();
        assert_eq!(cfg.get_string("user.email").unwrap(), "work@example.com");
        assert_eq!(cfg.get_string("core.autocrlf").unwrap(), "true");
    }

    #[test]
    fn test_apply_git_config_idempotent() {
        let (_td, repo) = create_source_repo();
        let mut entries = HashMap::new();
        entries.insert("user.email".to_string(), "work@example.com".to_string());

        apply_git_config(&repo, &entries, false).unwrap();
        apply_git_config(&repo, &entries, false).unwrap();

        let cfg = repo.config().unwrap();
        assert_eq!(cfg.get_string("user.email").unwrap(), "work@example.com");
    }

    #[test]
    fn test_checkout_ref_tag() {
        let (td, repo) = create_source_repo();
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();

        // Create a second commit
        fs::write(td.path().join("file.txt"), "content").unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        let commit2 = repo
            .commit(Some("HEAD"), &sig, &sig, "second", &tree, &[&head])
            .unwrap();

        // Tag the first commit
        let first_commit = head;
        repo.tag(
            "v1.0.0",
            first_commit.as_object(),
            &sig,
            "release v1.0.0",
            false,
        )
        .unwrap();

        // We are on second commit now
        assert_eq!(repo.head().unwrap().peel_to_commit().unwrap().id(), commit2);

        // Checkout the tag
        let config = GitRepoConfig {
            url: String::new(),
            branch: None,
            tag: Some("v1.0.0".to_string()),
            rev: None,
            recurse_submodules: false,
            worktree: false,
            per_worktree_config: false,
            worktrees: None,
            remotes: None,
            config: None,
            worktree_config: None,
        };
        checkout_ref(&repo, &config).unwrap();

        assert_eq!(
            repo.head().unwrap().peel_to_commit().unwrap().id(),
            first_commit.id()
        );
    }

    #[test]
    fn test_checkout_ref_rev() {
        let (td, repo) = create_source_repo();
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();

        let first_commit_id = repo.head().unwrap().peel_to_commit().unwrap().id();

        // Create a second commit
        fs::write(td.path().join("file.txt"), "content").unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let head = repo.find_commit(first_commit_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "second", &tree, &[&head])
            .unwrap();

        // Checkout the first commit by rev
        let config = GitRepoConfig {
            url: String::new(),
            branch: None,
            tag: None,
            rev: Some(first_commit_id.to_string()),
            recurse_submodules: false,
            worktree: false,
            per_worktree_config: false,
            worktrees: None,
            remotes: None,
            config: None,
            worktree_config: None,
        };
        checkout_ref(&repo, &config).unwrap();

        assert_eq!(
            repo.head().unwrap().peel_to_commit().unwrap().id(),
            first_commit_id
        );
    }

    #[test]
    fn test_ensure_worktrees_creates_default_branch() {
        let (source_td, _source_repo) = create_source_repo();

        // Clone as bare
        let dest_td = TempDir::new().unwrap();
        let bare_path = dest_td.path().join("repo.git");
        let bare_repo = Repository::clone(source_td.path().to_str().unwrap(), &bare_path).unwrap();
        // Convert to bare for the test by opening as bare
        drop(bare_repo);

        // Re-init as bare clone properly
        let bare_path = dest_td.path().join("bare.git");
        let mut builder = git2::build::RepoBuilder::new();
        builder.bare(true);
        let bare_repo = builder
            .clone(source_td.path().to_str().unwrap(), &bare_path)
            .unwrap();

        let base_path = dest_td.path().join("worktree-base");
        fs::create_dir_all(&base_path).unwrap();
        let config = GitRepoConfig {
            url: String::new(),
            branch: None,
            tag: None,
            rev: None,
            recurse_submodules: false,
            worktree: true,
            per_worktree_config: true,
            worktrees: None,
            remotes: None,
            config: None,
            worktree_config: None,
        };

        let wt_base = bare_path.parent().unwrap();
        ensure_worktrees(&bare_repo, wt_base, &config, false).unwrap();

        // The default branch worktree should exist
        let main_wt = wt_base.join("main");
        assert!(
            main_wt.exists(),
            "Default branch worktree should be created"
        );
        assert!(
            main_wt.join("README.md").exists(),
            "Worktree should contain repo files"
        );
    }

    #[test]
    fn test_ensure_worktrees_creates_named_worktrees() {
        let (source_td, _source_repo) = create_source_repo();

        let dest_td = TempDir::new().unwrap();
        let bare_path = dest_td.path().join("bare.git");
        let mut builder = git2::build::RepoBuilder::new();
        builder.bare(true);
        let bare_repo = builder
            .clone(source_td.path().to_str().unwrap(), &bare_path)
            .unwrap();

        let mut worktrees = HashMap::new();
        worktrees.insert(
            "dev".to_string(),
            config::WorktreeEntry {
                branch: "develop".to_string(),
                config: None,
            },
        );

        let config = GitRepoConfig {
            url: String::new(),
            branch: None,
            tag: None,
            rev: None,
            recurse_submodules: false,
            worktree: true,
            per_worktree_config: true,
            worktrees: Some(worktrees),
            remotes: None,
            config: None,
            worktree_config: None,
        };

        let wt_base = bare_path.parent().unwrap();
        ensure_worktrees(&bare_repo, wt_base, &config, false).unwrap();

        // Both main and dev worktrees should exist
        assert!(wt_base.join("main").exists());
        assert!(wt_base.join("dev").exists());
        assert!(wt_base.join("dev").join("README.md").exists());
    }

    #[test]
    fn test_ensure_worktrees_idempotent() {
        let (source_td, _source_repo) = create_source_repo();

        let dest_td = TempDir::new().unwrap();
        let bare_path = dest_td.path().join("bare.git");
        let mut builder = git2::build::RepoBuilder::new();
        builder.bare(true);
        let bare_repo = builder
            .clone(source_td.path().to_str().unwrap(), &bare_path)
            .unwrap();

        let config = GitRepoConfig {
            url: String::new(),
            branch: None,
            tag: None,
            rev: None,
            recurse_submodules: false,
            worktree: true,
            per_worktree_config: true,
            worktrees: None,
            remotes: None,
            config: None,
            worktree_config: None,
        };

        let wt_base = bare_path.parent().unwrap();
        ensure_worktrees(&bare_repo, wt_base, &config, false).unwrap();
        // Second call should succeed without errors
        ensure_worktrees(&bare_repo, wt_base, &config, false).unwrap();

        assert!(wt_base.join("main").exists());
    }

    #[test]
    fn test_detect_default_branch() {
        let (_td, repo) = create_source_repo();
        let branch = detect_default_branch(&repo).unwrap();
        assert_eq!(branch, "main");
    }

    #[test]
    fn test_short_name() {
        let action = EnsureGitRepository::new(
            PathBuf::from("test"),
            GitRepoConfig {
                url: "https://github.com/user/my-repo.git".to_string(),
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
            },
            "test-overlay".to_string(),
        );
        assert_eq!(action.short_name(), "my-repo");
    }

    #[test]
    fn test_short_name_no_git_suffix() {
        let action = EnsureGitRepository::new(
            PathBuf::from("test"),
            GitRepoConfig {
                url: "https://github.com/user/my-repo".to_string(),
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
            },
            "test-overlay".to_string(),
        );
        assert_eq!(action.short_name(), "my-repo");
    }

    #[test]
    fn test_apply_per_worktree_config() {
        let (source_td, _source_repo) = create_source_repo();

        let dest_td = TempDir::new().unwrap();
        let bare_path = dest_td.path().join("bare.git");
        let mut builder = git2::build::RepoBuilder::new();
        builder.bare(true);
        let bare_repo = builder
            .clone(source_td.path().to_str().unwrap(), &bare_path)
            .unwrap();

        let config = GitRepoConfig {
            url: String::new(),
            branch: None,
            tag: None,
            rev: None,
            recurse_submodules: false,
            worktree: true,
            per_worktree_config: true,
            worktrees: None,
            remotes: None,
            config: None,
            worktree_config: None,
        };

        apply_per_worktree_config(&bare_repo, &config, false).unwrap();

        // Shared config should have core.bare=false and extensions.worktreeConfig=true
        let cfg = bare_repo.config().unwrap();
        assert_eq!(cfg.get_string("core.bare").unwrap(), "false");
        assert_eq!(cfg.get_string("extensions.worktreeConfig").unwrap(), "true");

        // config.worktree should have core.bare=true
        let wt_config_path = bare_path.join("config.worktree");
        assert!(wt_config_path.exists());
        let wt_cfg = git2::Config::open(&wt_config_path).unwrap();
        assert_eq!(wt_cfg.get_string("core.bare").unwrap(), "true");
    }

    #[test]
    fn test_apply_per_worktree_config_with_user_entries() {
        let (source_td, _source_repo) = create_source_repo();

        let dest_td = TempDir::new().unwrap();
        let bare_path = dest_td.path().join("bare.git");
        let mut builder = git2::build::RepoBuilder::new();
        builder.bare(true);
        let bare_repo = builder
            .clone(source_td.path().to_str().unwrap(), &bare_path)
            .unwrap();

        let mut wt_entries = HashMap::new();
        wt_entries.insert("core.sparseCheckout".to_string(), "true".to_string());

        let config = GitRepoConfig {
            url: String::new(),
            branch: None,
            tag: None,
            rev: None,
            recurse_submodules: false,
            worktree: true,
            per_worktree_config: true,
            worktrees: None,
            remotes: None,
            config: None,
            worktree_config: Some(config::GitConfig {
                entries: wt_entries,
            }),
        };

        apply_per_worktree_config(&bare_repo, &config, false).unwrap();

        // config.worktree should have both auto-managed and user entries
        let wt_config_path = bare_path.join("config.worktree");
        let wt_cfg = git2::Config::open(&wt_config_path).unwrap();
        assert_eq!(wt_cfg.get_string("core.bare").unwrap(), "true");
        assert_eq!(wt_cfg.get_string("core.sparseCheckout").unwrap(), "true");
    }

    #[test]
    fn test_apply_per_worktree_config_disabled() {
        let (_td, repo) = create_source_repo();

        let config = GitRepoConfig {
            url: String::new(),
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
        };

        apply_per_worktree_config(&repo, &config, false).unwrap();

        // Should not have set extensions.worktreeConfig
        let cfg = repo.config().unwrap();
        assert!(cfg.get_string("extensions.worktreeConfig").is_err());
    }

    #[test]
    fn test_ensure_worktrees_with_per_worktree_config() {
        let (source_td, _source_repo) = create_source_repo();

        let dest_td = TempDir::new().unwrap();
        let bare_path = dest_td.path().join("bare.git");
        let mut builder = git2::build::RepoBuilder::new();
        builder.bare(true);
        let bare_repo = builder
            .clone(source_td.path().to_str().unwrap(), &bare_path)
            .unwrap();

        let mut wt_entries = HashMap::new();
        wt_entries.insert("user.email".to_string(), "dev@example.com".to_string());

        let mut worktrees = HashMap::new();
        worktrees.insert(
            "dev".to_string(),
            config::WorktreeEntry {
                branch: "develop".to_string(),
                config: Some(config::GitConfig {
                    entries: wt_entries,
                }),
            },
        );

        let config = GitRepoConfig {
            url: String::new(),
            branch: None,
            tag: None,
            rev: None,
            recurse_submodules: false,
            worktree: true,
            per_worktree_config: true,
            worktrees: Some(worktrees),
            remotes: None,
            config: None,
            worktree_config: None,
        };

        let wt_base = bare_path.parent().unwrap();
        ensure_worktrees(&bare_repo, wt_base, &config, false).unwrap();

        // The "dev" worktree should exist
        assert!(wt_base.join("dev").exists());

        // Per-worktree config should be written
        let wt_config_path = bare_path
            .join("worktrees")
            .join("dev")
            .join("config.worktree");
        assert!(wt_config_path.exists());
        let wt_cfg = git2::Config::open(&wt_config_path).unwrap();
        assert_eq!(wt_cfg.get_string("user.email").unwrap(), "dev@example.com");
    }
}
