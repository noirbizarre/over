use std::collections::HashSet;

use globset::GlobBuilder;

use crate::overlays::Overlay;

use super::Diagnostic;

/// Run all per-overlay checks and return collected diagnostics.
pub fn check_overlay(overlay: &Overlay) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    diagnostics.extend(check_uses(overlay));
    diagnostics.extend(check_empty_exclude(overlay));
    diagnostics.extend(check_empty_link_dirs(overlay));
    diagnostics.extend(check_invalid_link_dirs_globs(overlay));
    diagnostics.extend(check_git(overlay));

    diagnostics
}

// ── uses checks ──────────────────────────────────────────────────────────

fn check_uses(overlay: &Overlay) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let name = &overlay.name;

    let Some(uses) = &overlay.uses else {
        return diagnostics;
    };

    // Empty uses list
    if uses.is_empty() {
        diagnostics.push(
            Diagnostic::warning(name, "empty `uses` list is redundant")
                .with_hint("remove the `uses` field or add overlay references"),
        );
        return diagnostics;
    }

    // Self-reference
    for dep in uses {
        if dep == name {
            diagnostics.push(Diagnostic::error(
                name,
                "overlay references itself in `uses`",
            ));
        }
    }

    // Duplicate entries
    let mut seen = HashSet::new();
    for dep in uses {
        if !seen.insert(dep) {
            diagnostics.push(
                Diagnostic::warning(name, format!("`uses` contains duplicate entry \"{dep}\""))
                    .with_hint("remove the duplicate reference"),
            );
        }
    }

    diagnostics
}

// ── exclude checks ───────────────────────────────────────────────────────

fn check_empty_exclude(overlay: &Overlay) -> Vec<Diagnostic> {
    if matches!(&overlay.exclude, Some(list) if list.is_empty()) {
        vec![
            Diagnostic::warning(&overlay.name, "empty `exclude` list is redundant")
                .with_hint("remove the `exclude` field or add glob patterns"),
        ]
    } else {
        Vec::new()
    }
}

// ── link_dirs checks ─────────────────────────────────────────────────────

fn check_empty_link_dirs(overlay: &Overlay) -> Vec<Diagnostic> {
    if matches!(&overlay.link_dirs, Some(list) if list.is_empty()) {
        vec![
            Diagnostic::warning(&overlay.name, "empty `link_dirs` list is redundant")
                .with_hint("remove the `link_dirs` field or add glob patterns"),
        ]
    } else {
        Vec::new()
    }
}

fn check_invalid_link_dirs_globs(overlay: &Overlay) -> Vec<Diagnostic> {
    let Some(link_dirs) = &overlay.link_dirs else {
        return Vec::new();
    };

    let mut diagnostics = Vec::new();
    for pattern in link_dirs {
        if let Err(err) = GlobBuilder::new(pattern).literal_separator(true).build() {
            diagnostics.push(
                Diagnostic::error(
                    &overlay.name,
                    format!("invalid glob pattern in `link_dirs`: \"{pattern}\""),
                )
                .with_hint(format!("{err}")),
            );
        }
    }
    diagnostics
}

// ── git checks ───────────────────────────────────────────────────────────

fn check_git(overlay: &Overlay) -> Vec<Diagnostic> {
    let Some(git) = &overlay.git else {
        return Vec::new();
    };

    let mut diagnostics = Vec::new();

    for (path, config) in git {
        let ctx = if path == "." {
            "git repo (root)".to_string()
        } else {
            format!("git repo \"{path}\"")
        };

        // Empty URL
        if config.url.is_empty() {
            diagnostics.push(Diagnostic::error(
                &overlay.name,
                format!("{ctx}: `url` is empty"),
            ));
        }

        // Conflicting ref specifiers
        let ref_count = [&config.branch, &config.tag, &config.rev]
            .iter()
            .filter(|r| r.is_some())
            .count();
        if ref_count > 1 {
            let mut refs = Vec::new();
            if config.branch.is_some() {
                refs.push("`branch`");
            }
            if config.tag.is_some() {
                refs.push("`tag`");
            }
            if config.rev.is_some() {
                refs.push("`rev`");
            }
            diagnostics.push(
                Diagnostic::error(
                    &overlay.name,
                    format!("{ctx}: conflicting ref specifiers: {}", refs.join(", ")),
                )
                .with_hint("use only one of `branch`, `tag`, or `rev`"),
            );
        }

        // Worktree with tag/rev (silently ignored at runtime)
        if config.worktree || config.worktrees.is_some() {
            if config.tag.is_some() {
                diagnostics.push(
                    Diagnostic::warning(
                        &overlay.name,
                        format!("{ctx}: `tag` is ignored in worktree mode"),
                    )
                    .with_hint("remove `tag` or disable worktree mode"),
                );
            }
            if config.rev.is_some() {
                diagnostics.push(
                    Diagnostic::warning(
                        &overlay.name,
                        format!("{ctx}: `rev` is ignored in worktree mode"),
                    )
                    .with_hint("remove `rev` or disable worktree mode"),
                );
            }
        }

        // Empty worktrees map
        if matches!(&config.worktrees, Some(wts) if wts.is_empty()) {
            diagnostics.push(
                Diagnostic::warning(
                    &overlay.name,
                    format!("{ctx}: empty `worktrees` map is redundant"),
                )
                .with_hint("remove `worktrees` or add worktree entries"),
            );
        }

        // Path traversal in git map key
        if path.contains("..") {
            diagnostics.push(
                Diagnostic::error(
                    &overlay.name,
                    format!("{ctx}: path contains `..` traversal"),
                )
                .with_hint("use a path within the overlay target directory"),
            );
        }
    }

    diagnostics
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlays::Repository;
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    use rstest::rstest;

    use super::super::Severity;

    fn setup_overlay(toml_content: &str) -> Overlay {
        let td = TempDir::new().unwrap();
        let repo = Repository::new(td.path().to_path_buf());
        let ov = td.child("test_ov");
        ov.create_dir_all().unwrap();
        ov.child("over.toml").write_str(toml_content).unwrap();
        // Keep td alive for the duration via leak (tests only)
        let overlay = repo.get("test_ov").unwrap();
        // We must keep td alive; use Box::leak for test simplicity
        Box::leak(Box::new(td));
        overlay
    }

    // ── uses checks ──────────────────────────────────────────────────────

    #[rstest]
    fn test_empty_uses() {
        let overlay = setup_overlay("target = \"~\"\nuses = []");
        let diags = check_uses(&overlay);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Severity::Warning);
        assert!(diags[0].message.contains("empty `uses`"));
    }

    #[rstest]
    fn test_self_reference_uses() {
        let overlay = setup_overlay("target = \"~\"\nuses = [\"test_ov\"]");
        let diags = check_uses(&overlay);
        assert!(
            diags
                .iter()
                .any(|d| d.severity == Severity::Error && d.message.contains("references itself"))
        );
    }

    #[rstest]
    fn test_duplicate_uses() {
        let overlay = setup_overlay("target = \"~\"\nuses = [\"other\", \"other\"]");
        let diags = check_uses(&overlay);
        assert!(
            diags
                .iter()
                .any(|d| d.severity == Severity::Warning && d.message.contains("duplicate"))
        );
    }

    #[rstest]
    fn test_valid_uses_no_diagnostics() {
        let overlay = setup_overlay("target = \"~\"\nuses = [\"other\"]");
        let diags = check_uses(&overlay);
        assert!(diags.is_empty());
    }

    // ── exclude checks ───────────────────────────────────────────────────

    #[rstest]
    fn test_empty_exclude() {
        let overlay = setup_overlay("target = \"~\"\nexclude = []");
        let diags = check_empty_exclude(&overlay);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Severity::Warning);
    }

    #[rstest]
    fn test_nonempty_exclude_no_diagnostic() {
        let overlay = setup_overlay("target = \"~\"\nexclude = [\"*.bak\"]");
        let diags = check_empty_exclude(&overlay);
        assert!(diags.is_empty());
    }

    // ── link_dirs checks ─────────────────────────────────────────────────

    #[rstest]
    fn test_empty_link_dirs() {
        let overlay = setup_overlay("target = \"~\"\nlink_dirs = []");
        let diags = check_empty_link_dirs(&overlay);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Severity::Warning);
    }

    #[rstest]
    fn test_invalid_glob_in_link_dirs() {
        let overlay = setup_overlay("target = \"~\"\nlink_dirs = [\"[invalid\"]");
        let diags = check_invalid_link_dirs_globs(&overlay);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Severity::Error);
        assert!(diags[0].message.contains("invalid glob"));
    }

    #[rstest]
    fn test_valid_glob_in_link_dirs() {
        let overlay = setup_overlay("target = \"~\"\nlink_dirs = [\".config/*\"]");
        let diags = check_invalid_link_dirs_globs(&overlay);
        assert!(diags.is_empty());
    }

    // ── git checks ───────────────────────────────────────────────────────

    #[rstest]
    fn test_git_empty_url() {
        let overlay = setup_overlay(
            r#"
target = "~"

[git]
url = ""
"#,
        );
        let diags = check_git(&overlay);
        assert!(
            diags
                .iter()
                .any(|d| d.severity == Severity::Error && d.message.contains("`url` is empty"))
        );
    }

    #[rstest]
    fn test_git_conflicting_refs() {
        let overlay = setup_overlay(
            r#"
target = "~"

[git]
url = "https://example.com/repo.git"
branch = "main"
tag = "v1.0"
"#,
        );
        let diags = check_git(&overlay);
        assert!(
            diags.iter().any(|d| d.severity == Severity::Error
                && d.message.contains("conflicting ref specifiers"))
        );
    }

    #[rstest]
    fn test_git_worktree_with_tag_warns() {
        let overlay = setup_overlay(
            r#"
target = "~"

[git]
url = "https://example.com/repo.git"
tag = "v1.0"
worktree = true
"#,
        );
        let diags = check_git(&overlay);
        assert!(diags.iter().any(|d| d.severity == Severity::Warning
            && d.message.contains("`tag` is ignored in worktree mode")));
    }

    #[rstest]
    fn test_git_path_traversal() {
        let overlay = setup_overlay(
            r#"
target = "~"

[git."../../etc"]
url = "https://example.com/repo.git"
"#,
        );
        let diags = check_git(&overlay);
        assert!(
            diags
                .iter()
                .any(|d| d.severity == Severity::Error && d.message.contains("path contains `..`"))
        );
    }

    #[rstest]
    fn test_git_valid_config_no_diagnostics() {
        let overlay = setup_overlay(
            r#"
target = "~"

[git.".config/nvim"]
url = "https://example.com/repo.git"
branch = "main"
"#,
        );
        let diags = check_git(&overlay);
        assert!(diags.is_empty());
    }

    #[rstest]
    fn test_git_empty_worktrees_map() {
        let overlay = setup_overlay(
            r#"
target = "~"

[git]
url = "https://example.com/repo.git"
worktree = true

[git.worktrees]
"#,
        );
        let diags = check_git(&overlay);
        assert!(diags.iter().any(
            |d| d.severity == Severity::Warning && d.message.contains("empty `worktrees` map")
        ));
    }

    // ── check_overlay aggregation ────────────────────────────────────────

    #[rstest]
    fn test_check_overlay_clean() {
        let overlay = setup_overlay("target = \"~\"");
        let diags = check_overlay(&overlay);
        assert!(diags.is_empty());
    }

    #[rstest]
    fn test_check_overlay_multiple_issues() {
        let overlay = setup_overlay("target = \"~\"\nuses = []\nexclude = []\nlink_dirs = []");
        let diags = check_overlay(&overlay);
        // empty uses + empty exclude + empty link_dirs = 3 warnings
        assert_eq!(diags.len(), 3);
        assert!(diags.iter().all(|d| d.severity == Severity::Warning));
    }
}
