//! Root-level `overlays:` declarations (#113/#127): directories that
//! belong to the repository's overlay set without their own local
//! descriptor file, resolved once from the repository root's own
//! `over.{toml,yaml,yml}` and unioned with descriptor-glob discovery
//! (`GLOB_PATTERN`) in `Repository::overlays()` — see ADR-018.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A single root-level overlay/workspace declaration. `path` is a glob
/// pattern (e.g. `hosts/*`) or a literal path, relative to the repository
/// root. Only the repository root's own descriptor's `overlays:` key is
/// read — a directory matched this way cannot itself declare further
/// `overlays:` entries (ADR-018: no recursive/nested expansion).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverlayDeclaration {
    pub path: String,
}

/// Expand root-declared paths/globs into concrete directories that exist
/// on disk. Uses the filesystem-expanding `glob` crate rather than
/// `globset` (which only tests an already-known path against a pattern,
/// as used by `GLOB_PATTERN`/`exclude`/`rules`) because a declaration
/// describes what to enumerate on disk, not what to test — mirrors
/// `cli::common::resolve_inputs`. A malformed glob, or one matching
/// nothing, is silently skipped (ADR-018): no error, no lint check in
/// this issue.
pub(super) fn resolve_declared_dirs(
    repo_root: &Path,
    declarations: &[OverlayDeclaration],
) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for decl in declarations {
        let pattern = repo_root.join(&decl.path);
        let Some(pattern_str) = pattern.to_str() else {
            continue;
        };
        let Ok(paths) = glob::glob(pattern_str) else {
            tracing::warn!("invalid overlays declaration pattern '{}'", decl.path);
            continue;
        };
        // Only directories are overlays; a glob like `hosts/*` may also
        // match plain files sitting alongside overlay directories.
        dirs.extend(paths.filter_map(Result::ok).filter(|p| p.is_dir()));
    }
    dirs
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn a_literal_path_matching_an_existing_directory_resolves_to_it() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("shared")).unwrap();

        let dirs = resolve_declared_dirs(
            tmp.path(),
            &[OverlayDeclaration {
                path: "shared".to_string(),
            }],
        );

        assert_eq!(dirs, vec![tmp.path().join("shared")]);
    }

    #[test]
    fn a_glob_path_resolves_every_matching_directory() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("hosts/laptop")).unwrap();
        fs::create_dir_all(tmp.path().join("hosts/desktop")).unwrap();

        let mut dirs = resolve_declared_dirs(
            tmp.path(),
            &[OverlayDeclaration {
                path: "hosts/*".to_string(),
            }],
        );
        dirs.sort();

        assert_eq!(
            dirs,
            vec![
                tmp.path().join("hosts/desktop"),
                tmp.path().join("hosts/laptop"),
            ]
        );
    }

    #[test]
    fn a_path_matching_only_files_resolves_empty() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("hosts")).unwrap();
        fs::write(tmp.path().join("hosts/README.md"), "not a dir").unwrap();

        let dirs = resolve_declared_dirs(
            tmp.path(),
            &[OverlayDeclaration {
                path: "hosts/*".to_string(),
            }],
        );

        assert!(dirs.is_empty());
    }

    #[test]
    fn a_path_matching_nothing_on_disk_resolves_empty() {
        let tmp = TempDir::new().unwrap();

        let dirs = resolve_declared_dirs(
            tmp.path(),
            &[OverlayDeclaration {
                path: "nonexistent/*".to_string(),
            }],
        );

        assert!(dirs.is_empty());
    }

    #[test]
    fn a_malformed_glob_pattern_resolves_empty_without_erroring() {
        let tmp = TempDir::new().unwrap();

        // `glob::Pattern` rejects unmatched brackets.
        let dirs = resolve_declared_dirs(
            tmp.path(),
            &[OverlayDeclaration {
                path: "hosts/[".to_string(),
            }],
        );

        assert!(dirs.is_empty());
    }
}
