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
    let is_bare = git_repo.is_bare();

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
    } else if is_bare {
        println!(
            "  {} {}",
            style::white("Mode:"),
            style::cyan("worktree workspace (bare)"),
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
    // Compare relative paths within the overlay root; both must succeed for a valid match
    match (
        target_resolved.strip_prefix(overlay_root_canonical),
        overlay_file.strip_prefix(overlay_root_canonical),
    ) {
        (Ok(target_rel), Ok(overlay_rel)) => target_rel == overlay_rel,
        _ => false,
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    use rstest::rstest;
    use std::path::PathBuf;

    #[allow(unused_imports)]
    use symlink::symlink_file;

    // ── is_overlay_descriptor ────────────────────────────────────────────

    #[rstest]
    #[case("over.yml", true)]
    #[case("over.yaml", true)]
    #[case("over.toml", true)]
    fn test_is_overlay_descriptor_valid(#[case] name: &str, #[case] expected: bool) {
        assert_eq!(is_overlay_descriptor(Path::new(name)), expected);
    }

    #[rstest]
    #[case("over.json")]
    #[case("over.txt")]
    #[case("config.yml")]
    #[case("overlay.yaml")]
    #[case("over")]
    #[case(".yml")]
    fn test_is_overlay_descriptor_invalid(#[case] name: &str) {
        assert!(!is_overlay_descriptor(Path::new(name)));
    }

    #[test]
    fn test_is_overlay_descriptor_nested_path() {
        assert!(is_overlay_descriptor(Path::new("some/deep/path/over.yml")));
        assert!(!is_overlay_descriptor(Path::new(
            "some/deep/path/readme.md"
        )));
    }

    // ── is_symlink_to ────────────────────────────────────────────────────

    #[test]
    fn test_is_symlink_to_not_a_symlink() {
        let td = TempDir::new().unwrap();
        td.child("regular.txt").write_str("hello").unwrap();

        assert!(!is_symlink_to(
            &td.path().join("regular.txt"),
            &PathBuf::from("/some/overlay/file"),
            &PathBuf::from("/some/overlay"),
        ));
    }

    #[test]
    fn test_is_symlink_to_nonexistent_path() {
        let td = TempDir::new().unwrap();

        assert!(!is_symlink_to(
            &td.path().join("does_not_exist"),
            &PathBuf::from("/some/overlay/file"),
            &PathBuf::from("/some/overlay"),
        ));
    }

    #[test]
    fn test_is_symlink_to_exact_match() {
        let td = TempDir::new().unwrap();
        let overlay_dir = td.child("overlay");
        overlay_dir.create_dir_all().unwrap();
        let overlay_file = overlay_dir.child("config.txt");
        overlay_file.write_str("content").unwrap();

        let worktree_dir = td.child("worktree");
        worktree_dir.create_dir_all().unwrap();
        let link_path = worktree_dir.path().join("config.txt");

        symlink::symlink_file(overlay_file.path(), &link_path).unwrap();

        let overlay_root_canonical = overlay_dir.path().canonicalize().unwrap();
        assert!(is_symlink_to(
            &link_path,
            &overlay_file.path().canonicalize().unwrap(),
            &overlay_root_canonical,
        ));
    }

    #[test]
    fn test_is_symlink_to_different_target() {
        let td = TempDir::new().unwrap();
        let overlay_dir = td.child("overlay");
        overlay_dir.create_dir_all().unwrap();
        let overlay_file = overlay_dir.child("config.txt");
        overlay_file.write_str("content").unwrap();

        let other_dir = td.child("other");
        other_dir.create_dir_all().unwrap();
        let other_file = other_dir.child("config.txt");
        other_file.write_str("other content").unwrap();

        let worktree_dir = td.child("worktree");
        worktree_dir.create_dir_all().unwrap();
        let link_path = worktree_dir.path().join("config.txt");

        // Symlink to a file NOT in the overlay
        symlink::symlink_file(other_file.path(), &link_path).unwrap();

        let overlay_root_canonical = overlay_dir.path().canonicalize().unwrap();
        assert!(!is_symlink_to(
            &link_path,
            &overlay_file.path().canonicalize().unwrap(),
            &overlay_root_canonical,
        ));
    }

    #[test]
    fn test_is_symlink_to_relative_path_match() {
        let td = TempDir::new().unwrap();
        let overlay_dir = td.child("overlay");
        overlay_dir.create_dir_all().unwrap();
        let sub = overlay_dir.child("sub");
        sub.create_dir_all().unwrap();
        let overlay_file = sub.child("dotfile");
        overlay_file.write_str("data").unwrap();

        let worktree_dir = td.child("repo");
        worktree_dir.create_dir_all().unwrap();
        let link_path = worktree_dir.path().join("dotfile");

        symlink::symlink_file(overlay_file.path(), &link_path).unwrap();

        let overlay_root_canonical = overlay_dir.path().canonicalize().unwrap();
        // overlay_file is under overlay_root, so relative path matching should work
        assert!(is_symlink_to(
            &link_path,
            &overlay_file.path().canonicalize().unwrap(),
            &overlay_root_canonical,
        ));
    }
}
