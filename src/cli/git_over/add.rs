use std::env::current_dir;
use std::path::PathBuf;

use anyhow::{Result, anyhow};
use clap::Args;
use dirs::home_dir;
use glob;

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

/// Check whether a string contains glob metacharacters.
fn is_glob_pattern(s: &str) -> bool {
    s.contains('*') || s.contains('?') || s.contains('[') || s.contains('{')
}

/// Expand a tilde prefix in a path string to the home directory.
fn expand_tilde(s: &str) -> PathBuf {
    if let Some(rest) = s.strip_prefix("~/")
        && let Some(home) = home_dir()
    {
        return home.join(rest);
    } else if s == "~"
        && let Some(home) = home_dir()
    {
        return home;
    }
    PathBuf::from(s)
}

/// Resolve a list of input strings (which may be globs, tildes, relative paths,
/// directories, or plain files) into a flat list of absolute paths.
fn resolve_inputs(inputs: &[String]) -> Result<Vec<PathBuf>> {
    let cwd = current_dir()?;
    let mut resolved = Vec::new();

    for input in inputs {
        let expanded = expand_tilde(input);
        let pattern_str = expanded.to_string_lossy();

        if is_glob_pattern(&pattern_str) {
            let matches: Vec<_> = glob::glob(&pattern_str)
                .map_err(|e| anyhow!("Invalid glob pattern '{}': {}", input, e))?
                .filter_map(|entry| entry.ok())
                .collect();

            if matches.is_empty() {
                return Err(anyhow!("No files matched pattern '{}'", input));
            }

            for path in matches {
                let abs = if path.is_relative() {
                    cwd.join(&path)
                } else {
                    path
                };
                resolved.push(abs);
            }
        } else {
            let abs = if expanded.is_relative() {
                cwd.join(&expanded)
            } else {
                expanded
            };
            if !abs.exists() {
                return Err(anyhow!("{} does not exist", abs.display()));
            }
            resolved.push(abs);
        }
    }

    Ok(resolved)
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
    let ctx = Context::new(
        args.dry_run,
        cli.debug,
        cli.verbose,
        args.force,
        false,
        root.clone(),
        over_repo,
        Some(overlay.clone()),
    );

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
