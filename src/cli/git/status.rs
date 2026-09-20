use std::path::Path;

use anyhow::{Result, anyhow};
use dirs::home_dir;
use tokio::task::spawn_blocking;
use walkdir::WalkDir;

use crate::overlays::{self, Repository};
use crate::ui;
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

    ui::info(format!(
        "{} {} {}",
        emojis::PACKAGE,
        style::white_b("Repository:"),
        style::cyan(&short_path(&repo_root.to_string_lossy())),
    ))
    .ok();
    if is_worktree {
        ui::info(format!(
            "  {} {}",
            style::white("Worktree:"),
            style::cyan(&short_path(&workdir.to_string_lossy())),
        ))
        .ok();
    } else if is_bare {
        ui::info(format!(
            "  {} {}",
            style::white("Mode:"),
            style::cyan("worktree workspace (bare)"),
        ))
        .ok();
    }
    ui::info(format!(
        "  {} {} {}",
        style::white("Overlay:"),
        style::cyan(&overlay.name),
        style::white(&format!(
            "({})",
            short_path(&overlay.root.to_string_lossy())
        )),
    ))
    .ok();
    ui::info(format!(
        "  {} {}",
        style::white("Relative path:"),
        style::cyan(rel_path.display()),
    ))
    .ok();
    ui::info("").ok();

    // ── Overlay-managed files in worktree ────────────────────────────────
    // Walk the repo working tree, find symlinks pointing into the overlay root.

    let overlay_root_canonical = if overlay.root.exists() {
        overlay.root.canonicalize()?
    } else {
        overlay.root.clone()
    };

    // `WalkDir` is synchronous disk I/O, hence `spawn_blocking` rather than
    // walking directly inside this `async fn`.
    let blocking_workdir = workdir.clone();
    let blocking_overlay_root = overlay_root_canonical.clone();
    let managed_files =
        spawn_blocking(move || find_managed_files(&blocking_workdir, &blocking_overlay_root))
            .await?;

    // ── Overlay files not applied here ───────────────────────────────────
    // Walk the overlay directory for the relative path, find files without
    // corresponding symlinks in the worktree.

    let overlay_subdir = overlay.root.join(&rel_path);
    let unapplied_files = if overlay_subdir.exists() {
        let blocking_repo_root = repo_root.clone();
        let blocking_overlay_root = overlay_root_canonical.clone();
        spawn_blocking(move || {
            find_unapplied_files(&overlay_subdir, &blocking_repo_root, &blocking_overlay_root)
        })
        .await?
    } else {
        Vec::new()
    };

    // ── Display results ──────────────────────────────────────────────────

    if managed_files.is_empty() && unapplied_files.is_empty() {
        ui::info(format!(
            "  {} {}",
            emojis::CHECKMARK,
            style::white("No overlay-managed files found"),
        ))
        .ok();
        return Ok(());
    }

    if !managed_files.is_empty() {
        ui::info(format!(
            "{} {} ({})",
            emojis::LINK,
            style::white_b("Managed files"),
            managed_files.len(),
        ))
        .ok();
        for file in &managed_files {
            ui::info(format!("  {} {}", emojis::GREEN_CIRCLE, style::cyan(file))).ok();
        }
    }

    if !unapplied_files.is_empty() {
        if !managed_files.is_empty() {
            ui::info("").ok();
        }
        ui::info(format!(
            "{} {} ({})",
            emojis::PACKAGE,
            style::white_b("Overlay files not applied"),
            unapplied_files.len(),
        ))
        .ok();
        for file in &unapplied_files {
            ui::info(format!("  {} {}", style::yellow("?"), style::yellow(file))).ok();
        }
    }

    Ok(())
}

/// Walk the repo working tree, find symlinks pointing into the overlay root.
fn find_managed_files(workdir: &Path, overlay_root_canonical: &Path) -> Vec<String> {
    let mut managed_files = Vec::new();
    for entry in WalkDir::new(workdir)
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
            if target_canonical.starts_with(overlay_root_canonical)
                && let Ok(rel) = path.strip_prefix(workdir)
            {
                managed_files.push(rel.display().to_string());
            }
        }
    }
    managed_files
}

/// Walk the overlay directory for the relative path, find files without
/// corresponding symlinks in the worktree.
fn find_unapplied_files(
    overlay_subdir: &Path,
    repo_root: &Path,
    overlay_root_canonical: &Path,
) -> Vec<String> {
    let mut unapplied_files = Vec::new();
    for entry in WalkDir::new(overlay_subdir)
        .min_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.is_file()
            && !is_overlay_descriptor(path)
            && let Ok(rel) = path.strip_prefix(overlay_subdir)
        {
            // Check the main repo root (not worktree) since overlays are
            // symlinked into the main repo root.
            let check_path = repo_root.join(rel);
            let is_linked = is_symlink_to(&check_path, path, overlay_root_canonical);
            if !is_linked {
                unapplied_files.push(rel.display().to_string());
            }
        }
    }
    unapplied_files
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
    fn known_overlay_descriptor_extensions_are_detected(
        #[case] name: &str,
        #[case] expected: bool,
    ) {
        assert_eq!(is_overlay_descriptor(Path::new(name)), expected);
    }

    #[rstest]
    #[case("over.json")]
    #[case("over.txt")]
    #[case("config.yml")]
    #[case("overlay.yaml")]
    #[case("over")]
    #[case(".yml")]
    fn non_descriptor_names_are_not_flagged_as_overlay_descriptors(#[case] name: &str) {
        assert!(!is_overlay_descriptor(Path::new(name)));
    }

    #[test]
    fn overlay_descriptor_detection_ignores_leading_path_components() {
        assert!(is_overlay_descriptor(Path::new("some/deep/path/over.yml")));
        assert!(!is_overlay_descriptor(Path::new(
            "some/deep/path/readme.md"
        )));
    }

    // ── is_symlink_to ────────────────────────────────────────────────────

    #[test]
    fn regular_file_is_never_considered_a_symlink_to_overlay() {
        let td = TempDir::new().unwrap();
        td.child("regular.txt").write_str("hello").unwrap();

        assert!(!is_symlink_to(
            &td.path().join("regular.txt"),
            &PathBuf::from("/some/overlay/file"),
            &PathBuf::from("/some/overlay"),
        ));
    }

    #[test]
    fn missing_path_is_not_treated_as_a_symlink_to_overlay() {
        let td = TempDir::new().unwrap();

        assert!(!is_symlink_to(
            &td.path().join("does_not_exist"),
            &PathBuf::from("/some/overlay/file"),
            &PathBuf::from("/some/overlay"),
        ));
    }

    #[test]
    fn symlink_pointing_directly_at_overlay_file_is_recognized() {
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
    fn symlink_pointing_outside_overlay_is_not_recognized() {
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
    fn symlink_matching_by_relative_path_under_overlay_root_is_recognized() {
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
