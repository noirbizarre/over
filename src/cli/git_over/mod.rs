use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use clap::{Parser, Subcommand};
use dirs::home_dir;

use crate::overlays::{Overlay, Repository};
use crate::ui::style::clap_styles;

mod add;
mod mount;
mod status;

#[derive(Parser, Debug)]
#[clap(
    author,
    version,
    about = "Manage git repository overlays",
    name = "git-over",
    long_about = None,
    styles = clap_styles(),
)]
pub struct CLI {
    #[clap(
        long,
        short = 'H',
        global = true,
        required = false,
        env = "OVER_HOME",
        help = "Configuration and overlays root"
    )]
    home: Option<PathBuf>,

    #[clap(long, short, global = true, help = "Toggle debug traces")]
    debug: bool,

    #[clap(long, short, global = true, help = "Toggle verbose output")]
    verbose: bool,

    #[clap(subcommand)]
    cmd: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    #[clap(about = "Mount the current git repository to an overlay")]
    Mount(mount::Params),

    #[clap(about = "Add files from the current git repository to an overlay")]
    Add(add::Params),

    #[clap(about = "Show overlay status for the current git repository")]
    Status,
}

impl CLI {
    /// Resolve the over home directory: flag/env > default (~/.over)
    pub fn resolve_home(&self) -> Result<PathBuf> {
        if let Some(ref home) = self.home {
            Ok(home.clone())
        } else {
            let default = home_dir()
                .ok_or_else(|| anyhow!("could not determine home directory"))?
                .join(".over");
            Ok(default)
        }
    }
}

pub async fn main() -> Result<()> {
    let args = CLI::parse();
    match &args.cmd {
        Commands::Mount(opt) => mount::execute(&args, opt).await?,
        Commands::Add(opt) => add::execute(&args, opt).await?,
        Commands::Status => status::execute(&args).await?,
    }
    Ok(())
}

// ── Shared helpers ───────────────────────────────────────────────────────

/// Discover the git repository from the current working directory.
pub fn discover_repo() -> Result<git2::Repository> {
    git2::Repository::discover(".").map_err(|e| anyhow!("not a git repository: {}", e))
}

/// Get the main repository root directory.
///
/// For regular repos this is `workdir()`. For worktrees this resolves
/// back to the parent repo's root via `commondir().parent()`, since
/// the overlay config is keyed by the main repo path, not individual
/// worktree paths.
pub fn main_repo_root(repo: &git2::Repository) -> Result<PathBuf> {
    if repo.is_worktree() {
        repo.commondir()
            .parent()
            .map(|p| p.to_path_buf())
            .ok_or_else(|| anyhow!("could not determine main repository root from worktree"))
    } else {
        repo.workdir()
            .map(|p| p.to_path_buf())
            .ok_or_else(|| anyhow!("bare repositories are not supported"))
    }
}

/// Read `over.overlay` from the repository's local git config.
pub fn get_overlay_config(repo: &git2::Repository) -> Result<Option<String>> {
    let config = repo.config()?;
    match config.get_string("over.overlay") {
        Ok(name) => Ok(Some(name)),
        Err(e) if e.code() == git2::ErrorCode::NotFound => Ok(None),
        Err(e) => Err(anyhow!(
            "failed to read over.overlay from git config: {}",
            e
        )),
    }
}

/// Write `over.overlay` to the repository's local git config.
pub fn set_overlay_config(repo: &git2::Repository, name: &str) -> Result<()> {
    let mut config = repo.config()?;
    config
        .set_str("over.overlay", name)
        .map_err(|e| anyhow!("failed to write over.overlay to git config: {}", e))
}

/// Append paths to `.git/info/exclude` idempotently.
pub fn exclude_paths(repo: &git2::Repository, paths: &[&str]) -> Result<()> {
    let git_dir = repo.path(); // .git/ directory
    let exclude_dir = git_dir.join("info");
    std::fs::create_dir_all(&exclude_dir)?;
    let exclude_path = exclude_dir.join("exclude");

    // Read existing content
    let existing = if exclude_path.exists() {
        std::fs::read_to_string(&exclude_path)?
    } else {
        String::new()
    };

    let existing_lines: std::collections::HashSet<&str> = existing.lines().collect();

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&exclude_path)?;

    for path in paths {
        if !existing_lines.contains(path) {
            writeln!(file, "{}", path)?;
        }
    }

    Ok(())
}

/// Compute the relative path from the overlay's resolved target to the repo working directory.
///
/// For example, if the overlay target is `~` and the repo is at `~/projects/myapp`,
/// this returns `projects/myapp`.
pub fn repo_relative_path(overlay: &Overlay, root: &Path, repo_workdir: &Path) -> Result<PathBuf> {
    // Resolve the overlay target (using a minimal context for template rendering)
    let ctx = crate::exec::Context::builder()
        .root(root.to_path_buf())
        .repository(Repository::new(PathBuf::new()))
        .overlay(overlay.clone())
        .build();
    let target = overlay.resolve_target(&ctx)?;

    repo_workdir
        .strip_prefix(&target)
        .map(|p| p.to_path_buf())
        .map_err(|_| {
            anyhow!(
                "repository {} is not under overlay target {}",
                repo_workdir.display(),
                target.display(),
            )
        })
}

/// Resolve or prompt for an overlay, returning both the overlay and its name.
///
/// If `overlay_name` is provided, use it directly. Otherwise check git config,
/// then fall back to interactive selection.
pub fn resolve_overlay(
    over_repo: &Repository,
    git_repo: &git2::Repository,
    overlay_name: Option<&str>,
) -> Result<Overlay> {
    if let Some(name) = overlay_name {
        return over_repo.get(name);
    }

    // Try git config
    if let Some(name) = get_overlay_config(git_repo)? {
        return over_repo.get(&name);
    }

    // Interactive selection
    let overlays = over_repo.overlays()?;
    if overlays.is_empty() {
        return Err(anyhow!("no overlays found in repository"));
    }

    let selection = dialoguer::FuzzySelect::with_theme(&dialoguer::theme::ColorfulTheme::default())
        .with_prompt("Choose the target overlay")
        .default(0)
        .items(&overlays[..])
        .interact()
        .map_err(|e| anyhow!("overlay selection cancelled: {}", e))?;

    let overlay = overlays[selection].clone();

    // Persist selection to git config
    set_overlay_config(git_repo, &overlay.name)?;

    Ok(overlay)
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    use std::fs;

    /// Helper: create a temp dir and init a bare-style git repo (non-bare, with workdir).
    fn temp_git_repo() -> (TempDir, git2::Repository) {
        let td = TempDir::new().unwrap();
        let repo = git2::Repository::init(td.path()).unwrap();
        (td, repo)
    }

    #[test]
    fn test_main_repo_root_regular() {
        let (td, repo) = temp_git_repo();
        let root = main_repo_root(&repo).unwrap();
        assert_eq!(root, td.path().canonicalize().unwrap());
    }

    #[test]
    fn test_get_overlay_config_none() {
        let (_td, repo) = temp_git_repo();
        let cfg = get_overlay_config(&repo).unwrap();
        assert_eq!(cfg, None);
    }

    #[test]
    fn test_set_and_get_overlay_config() {
        let (_td, repo) = temp_git_repo();
        set_overlay_config(&repo, "myoverlay").unwrap();
        let cfg = get_overlay_config(&repo).unwrap();
        assert_eq!(cfg, Some("myoverlay".into()));
    }

    #[test]
    fn test_set_overlay_config_overwrite() {
        let (_td, repo) = temp_git_repo();
        set_overlay_config(&repo, "first").unwrap();
        set_overlay_config(&repo, "second").unwrap();
        let cfg = get_overlay_config(&repo).unwrap();
        assert_eq!(cfg, Some("second".into()));
    }

    #[test]
    fn test_exclude_paths_creates_file() {
        let (_td, repo) = temp_git_repo();
        exclude_paths(&repo, &["/overlay", "*.bak"]).unwrap();

        let exclude_path = repo.path().join("info").join("exclude");
        let content = fs::read_to_string(&exclude_path).unwrap();
        assert!(content.contains("/overlay"));
        assert!(content.contains("*.bak"));
    }

    #[test]
    fn test_exclude_paths_idempotent() {
        let (_td, repo) = temp_git_repo();
        exclude_paths(&repo, &["/overlay"]).unwrap();
        exclude_paths(&repo, &["/overlay"]).unwrap();

        let exclude_path = repo.path().join("info").join("exclude");
        let content = fs::read_to_string(&exclude_path).unwrap();
        let count = content.matches("/overlay").count();
        assert_eq!(count, 1, "path should appear only once");
    }

    #[test]
    fn test_exclude_paths_appends_new() {
        let (_td, repo) = temp_git_repo();
        exclude_paths(&repo, &["/first"]).unwrap();
        exclude_paths(&repo, &["/second"]).unwrap();

        let exclude_path = repo.path().join("info").join("exclude");
        let content = fs::read_to_string(&exclude_path).unwrap();
        assert!(content.contains("/first"));
        assert!(content.contains("/second"));
    }

    #[test]
    fn test_repo_relative_path_under_target() {
        let td = TempDir::new().unwrap();
        // Create a minimal overlay with target = td path
        let overlay_dir = td.child("overlay");
        overlay_dir.create_dir_all().unwrap();
        let target_str = td.path().to_string_lossy().to_string();
        overlay_dir
            .child("over.toml")
            .write_str(&format!("target = \"{}\"", target_str))
            .unwrap();

        let over_repo = Repository::new(td.path().to_path_buf());
        let overlay = over_repo.get("overlay").unwrap();

        let repo_dir = td.path().join("projects").join("myapp");
        fs::create_dir_all(&repo_dir).unwrap();

        let rel = repo_relative_path(&overlay, td.path(), &repo_dir).unwrap();
        assert_eq!(rel, PathBuf::from("projects/myapp"));
    }

    #[test]
    fn test_repo_relative_path_not_under_target() {
        let td = TempDir::new().unwrap();
        let overlay_dir = td.child("overlay");
        overlay_dir.create_dir_all().unwrap();
        let target_str = td.path().join("subdir").to_string_lossy().to_string();
        overlay_dir
            .child("over.toml")
            .write_str(&format!("target = \"{}\"", target_str))
            .unwrap();

        let over_repo = Repository::new(td.path().to_path_buf());
        let overlay = over_repo.get("overlay").unwrap();

        let repo_dir = PathBuf::from("/tmp/elsewhere");
        let result = repo_relative_path(&overlay, td.path(), &repo_dir);
        assert!(result.is_err());
    }

    #[test]
    fn test_resolve_overlay_by_name() {
        let td = TempDir::new().unwrap();
        let overlay_dir = td.child("myoverlay");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();

        let over_repo = Repository::new(td.path().to_path_buf());
        let (_gtd, git_repo) = temp_git_repo();

        let overlay = resolve_overlay(&over_repo, &git_repo, Some("myoverlay")).unwrap();
        assert_eq!(overlay.name, "myoverlay");
    }

    #[test]
    fn test_resolve_overlay_from_git_config() {
        let td = TempDir::new().unwrap();
        let overlay_dir = td.child("dev");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();

        let over_repo = Repository::new(td.path().to_path_buf());
        let (_gtd, git_repo) = temp_git_repo();
        set_overlay_config(&git_repo, "dev").unwrap();

        let overlay = resolve_overlay(&over_repo, &git_repo, None).unwrap();
        assert_eq!(overlay.name, "dev");
    }
}
