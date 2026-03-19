use std::path::Path;

use anyhow::{Result, anyhow};
use dirs::home_dir;
use walkdir::WalkDir;

use crate::overlays::{self, Repository};
use crate::ui::{emojis, style};
use crate::utils::short_path;

use super::{CLI, discover_repo, get_overlay_config, main_repo_root, repo_relative_path};

pub async fn execute(cli: &CLI) -> Result<()> {
    let git_repo = discover_repo()?;
    let repo_root = main_repo_root(&git_repo)?;
    let workdir = git_repo
        .workdir()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| repo_root.clone());
    let is_worktree = git_repo.is_worktree();

    let home = cli.resolve_home()?;
    let over_repo = Repository::new(home);

    // Read overlay from git config
    let overlay_name = get_overlay_config(&git_repo)?
        .ok_or_else(|| anyhow!("no overlay configured; run `git over mount` first"))?;
    let overlay = over_repo.get(&overlay_name)?;

    // Use the main repo root (not worktree) for overlay path computation
    let root = home_dir().ok_or_else(|| anyhow!("could not determine home directory"))?;
    let rel_path = repo_relative_path(&overlay, &root, &repo_root)?;

    println!(
        "{} {} {}",
        emojis::PACKAGE,
        style::white_b("Repository:"),
        style::cyan(&short_path(&repo_root.to_string_lossy())),
    );
    if is_worktree {
        println!(
            "  {} {}",
            style::white("Worktree:"),
            style::cyan(&short_path(&workdir.to_string_lossy())),
        );
    }
    println!(
        "  {} {} {}",
        style::white("Overlay:"),
        style::cyan(&overlay.name),
        style::white(&format!(
            "({})",
            short_path(&overlay.root.to_string_lossy())
        )),
    );
    println!(
        "  {} {}",
        style::white("Relative path:"),
        style::cyan(rel_path.display()),
    );
    println!();

    // ── Overlay-managed files in worktree ────────────────────────────────
    // Walk the repo working tree, find symlinks pointing into the overlay root.

    let mut managed_files: Vec<String> = Vec::new();
    let overlay_root_canonical = if overlay.root.exists() {
        overlay.root.canonicalize()?
    } else {
        overlay.root.clone()
    };

    for entry in WalkDir::new(&workdir)
        .min_depth(1)
        .into_iter()
        .filter_entry(|e| {
            // Skip .git directory
            e.file_name() != ".git"
        })
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.is_symlink()
            && let Ok(target) = std::fs::read_link(path)
        {
            let target_canonical = if target.is_absolute() && target.exists() {
                target.canonicalize().unwrap_or(target)
            } else {
                target.clone()
            };
            if target_canonical.starts_with(&overlay_root_canonical)
                && let Ok(rel) = path.strip_prefix(&workdir)
            {
                managed_files.push(rel.display().to_string());
            }
        }
    }

    // ── Overlay files not applied here ───────────────────────────────────
    // Walk the overlay directory for the relative path, find files without
    // corresponding symlinks in the worktree.

    let overlay_subdir = overlay.root.join(&rel_path);
    let mut unapplied_files: Vec<String> = Vec::new();

    if overlay_subdir.exists() {
        for entry in WalkDir::new(&overlay_subdir)
            .min_depth(1)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if path.is_file()
                && !is_overlay_descriptor(path)
                && let Ok(rel) = path.strip_prefix(&overlay_subdir)
            {
                // Check the main repo root (not worktree) since overlays
                // are symlinked into the main repo root.
                let check_path = repo_root.join(rel);
                let is_linked = is_symlink_to(&check_path, path, &overlay_root_canonical);
                if !is_linked {
                    unapplied_files.push(rel.display().to_string());
                }
            }
        }
    }

    // ── Display results ──────────────────────────────────────────────────

    if managed_files.is_empty() && unapplied_files.is_empty() {
        println!(
            "  {} {}",
            emojis::CHECKMARK,
            style::white("No overlay-managed files found"),
        );
        return Ok(());
    }

    if !managed_files.is_empty() {
        println!(
            "{} {} ({})",
            emojis::LINK,
            style::white_b("Managed files"),
            managed_files.len(),
        );
        for file in &managed_files {
            println!("  {} {}", emojis::GREEN_CIRCLE, style::cyan(file));
        }
    }

    if !unapplied_files.is_empty() {
        if !managed_files.is_empty() {
            println!();
        }
        println!(
            "{} {} ({})",
            emojis::PACKAGE,
            style::white_b("Overlay files not applied"),
            unapplied_files.len(),
        );
        for file in &unapplied_files {
            println!("  {} {}", style::yellow("?"), style::yellow(file));
        }
    }

    Ok(())
}

/// Check if a path is a symlink pointing to (or under) a file in the overlay.
fn is_symlink_to(worktree_path: &Path, overlay_file: &Path, overlay_root_canonical: &Path) -> bool {
    if !worktree_path.is_symlink() {
        return false;
    }
    let Ok(target) = std::fs::read_link(worktree_path) else {
        return false;
    };
    let target_resolved = if target.is_absolute() && target.exists() {
        target.canonicalize().unwrap_or(target)
    } else {
        target.clone()
    };

    // Check if the symlink target is the exact overlay file or under the overlay root
    if target_resolved == overlay_file {
        return true;
    }
    if let Ok(overlay_canonical) = overlay_file.canonicalize()
        && target_resolved == overlay_canonical
    {
        return true;
    }
    target_resolved.starts_with(overlay_root_canonical)
        && target_resolved.strip_prefix(overlay_root_canonical).ok()
            == overlay_file.strip_prefix(overlay_root_canonical).ok()
}

/// Check if a path is an overlay descriptor file (e.g., `over.yml`, `over.yaml`, `over.toml`).
fn is_overlay_descriptor(path: &Path) -> bool {
    let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
        return false;
    };
    let Some(ext) = path.extension().and_then(|s| s.to_str()) else {
        return false;
    };
    stem == overlays::BASENAME && overlays::EXTENSIONS.contains(&ext)
}
