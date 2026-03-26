use anyhow::{Result, anyhow};
use clap::Args;
use dirs::home_dir;

use crate::cli::common::resolve_inputs;
use crate::exec::Context;
use crate::overlays::Repository;
use crate::ui::{emojis, style};
use crate::utils::short_path;

use super::{
    CLI, discover_repo, exclude_paths, main_repo_root, repo_relative_path, resolve_overlay,
};

#[derive(Args, Debug)]
pub struct Params {
    #[clap(required = true, help = "Files, directories, or glob patterns to add")]
    files: Vec<String>,

    #[clap(short, long, help = "Name of the target overlay")]
    overlay: Option<String>,

    #[clap(long, short = 'n', help = "Run without applying changes")]
    dry_run: bool,

    #[clap(long, short, help = "Overwrite without prompting")]
    force: bool,
}

pub async fn execute(cli: &CLI, args: &Params) -> Result<()> {
    let git_repo = discover_repo()?;
    let repo_root = main_repo_root(&git_repo)?;
    let workdir = git_repo
        .workdir()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| repo_root.clone());

    if cli.debug {
        println!("Repository: {}", repo_root.display());
        if git_repo.is_worktree() {
            println!("Worktree: {}", workdir.display());
        } else if git_repo.is_bare() {
            println!("Worktree workspace (bare repo)");
        }
    }

    let home = cli.resolve_home()?;
    let over_repo = Repository::new(home);

    // Resolve overlay
    let overlay = resolve_overlay(&over_repo, &git_repo, args.overlay.as_deref())?;

    if cli.debug {
        println!("{:#?}", overlay);
    }

    // Validate that the repo root is under the overlay target.
    // Use main repo root (not worktree) for the overlay path computation.
    let root = home_dir().ok_or_else(|| anyhow!("could not determine home directory"))?;
    let _rel_path = repo_relative_path(&overlay, &root, &repo_root)?;

    println!(
        "{} {} {} {} {}",
        emojis::PACKAGE,
        style::white("Adding files from"),
        style::cyan(&short_path(&workdir.to_string_lossy())),
        style::white("to overlay"),
        style::cyan(&overlay.name),
    );

    // Build execution context with the overlay's target as root
    let ctx = Context::builder()
        .dry_run(args.dry_run)
        .debug(cli.debug)
        .verbose(cli.verbose)
        .force(args.force)
        .root(root.clone())
        .repository(over_repo)
        .overlay(overlay.clone())
        .build();

    let resolved = resolve_inputs(&args.files)?;

    overlay.add_files(&ctx, &resolved).await.map_err(|e| {
        println!(
            "{} {} {} {}",
            emojis::CROSSMARK,
            style::white_b("Failed to add to overlay"),
            style::cyan(&overlay.name),
            style::white_b(&format!(": {}", e)),
        );
        e
    })?;

    // Compute relative paths from the repo root for .git/info/exclude
    let exclude_entries: Vec<String> = resolved
        .iter()
        .filter_map(|abs_path| {
            abs_path
                .strip_prefix(&workdir)
                .ok()
                .map(|rel| format!("/{}", rel.display()))
        })
        .collect();

    if !exclude_entries.is_empty() && !args.dry_run {
        let refs: Vec<&str> = exclude_entries.iter().map(|s| s.as_str()).collect();
        exclude_paths(&git_repo, &refs)?;

        if cli.verbose {
            println!(
                "{} {} {}",
                emojis::CHECKMARK,
                style::white("Added to .git/info/exclude:"),
                style::cyan(&exclude_entries.join(", ")),
            );
        }
    }

    // Compute relative paths within the overlay for display
    let added_display: Vec<String> = resolved
        .iter()
        .filter_map(|abs_path| {
            abs_path
                .strip_prefix(&workdir)
                .ok()
                .map(|rel| rel.display().to_string())
        })
        .collect();

    println!(
        "{} {} {} {} {} ({})",
        emojis::SPARKLE,
        style::white_b("Added"),
        style::cyan(&format!("{}", added_display.len())),
        style::white_b("file(s) to overlay"),
        style::cyan(&overlay.name),
        added_display.join(", "),
    );

    Ok(())
}
