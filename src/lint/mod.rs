mod checks;

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};

use globset::GlobBuilder;
use walkdir::WalkDir;

use crate::overlays::{BASENAME, EXTENSIONS, GLOB_PATTERN, Overlay, Repository};

/// Severity level for a lint diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Severity::Error => write!(f, "error"),
            Severity::Warning => write!(f, "warning"),
        }
    }
}

/// A single lint finding.
#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub severity: Severity,
    /// Overlay name (or directory path for parse failures).
    pub overlay: String,
    /// Human-readable description of the issue.
    pub message: String,
    /// Optional suggestion on how to fix.
    pub hint: Option<String>,
}

impl Diagnostic {
    pub fn error(overlay: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            overlay: overlay.into(),
            message: message.into(),
            hint: None,
        }
    }

    pub fn warning(overlay: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            overlay: overlay.into(),
            message: message.into(),
            hint: None,
        }
    }

    #[must_use]
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

/// Aggregated lint results.
#[derive(Debug)]
pub struct LintResult {
    pub diagnostics: Vec<Diagnostic>,
}

impl LintResult {
    pub fn error_count(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .count()
    }

    pub fn warning_count(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Warning)
            .count()
    }

    pub fn has_errors(&self) -> bool {
        self.error_count() > 0
    }
}

/// Discover overlay directories in a repository without failing on parse errors.
///
/// Returns `(dir, overlay_name)` pairs.
fn discover_overlay_dirs(repo: &Repository) -> Vec<(PathBuf, String)> {
    let Ok(glob) = GlobBuilder::new(&GLOB_PATTERN)
        .literal_separator(true)
        .build()
    else {
        return Vec::new();
    };
    let matcher = glob.compile_matcher();

    let mut dirs: Vec<PathBuf> = WalkDir::new(&repo.root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| {
            e.path()
                .strip_prefix(&repo.root)
                .ok()
                .is_some_and(|rel| matcher.is_match(rel))
        })
        .filter_map(|e| e.path().parent().map(|p| p.to_path_buf()))
        .collect();

    dirs.sort();

    dirs.iter()
        .enumerate()
        .filter(|(idx, dir)| !matches!(dirs.get(idx + 1), Some(next) if next.starts_with(dir)))
        .filter_map(|(_, dir)| {
            dir.strip_prefix(&repo.root)
                .ok()
                .and_then(|rel| rel.to_str().map(|s| s.to_string()))
                .map(|name| (dir.clone(), name))
        })
        .collect()
}

/// Check for multiple descriptor file formats in a single overlay directory.
fn check_multiple_descriptors(dir: &Path, overlay_name: &str) -> Vec<Diagnostic> {
    let mut found: Vec<&str> = Vec::new();
    for ext in EXTENSIONS {
        let path = dir.join(format!("{BASENAME}.{ext}"));
        if path.exists() {
            found.push(ext);
        }
    }
    if found.len() > 1 {
        let files: Vec<String> = found.iter().map(|e| format!("{BASENAME}.{e}")).collect();
        vec![
            Diagnostic::warning(
                overlay_name,
                format!("multiple descriptor files found: {}", files.join(", ")),
            )
            .with_hint("use a single format to avoid unexpected config merging"),
        ]
    } else {
        Vec::new()
    }
}

/// Detect cycles in the `uses` dependency graph using iterative DFS.
fn check_cycles(overlays: &HashMap<String, &Overlay>) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let mut fully_visited: HashSet<String> = HashSet::new();

    for name in overlays.keys() {
        if fully_visited.contains(name) {
            continue;
        }

        // Iterative DFS with explicit stack tracking (node, iterator_index)
        let mut stack: Vec<String> = Vec::new();
        let mut stack_set: HashSet<String> = HashSet::new();
        let mut indices: Vec<usize> = Vec::new();

        stack.push(name.clone());
        stack_set.insert(name.clone());
        indices.push(0);

        while let Some(current) = stack.last().cloned() {
            let idx = *indices.last().unwrap();

            let deps = overlays
                .get(&current)
                .and_then(|o| o.uses.as_ref())
                .map(|u| u.as_slice())
                .unwrap_or(&[]);

            if idx < deps.len() {
                // Advance the iterator for the current node
                *indices.last_mut().unwrap() += 1;
                let dep = &deps[idx];

                if stack_set.contains(dep) {
                    // Found a cycle
                    let cycle_start = stack.iter().position(|s| s == dep).unwrap();
                    let mut cycle_path: Vec<String> = stack[cycle_start..].to_vec();
                    cycle_path.push(dep.clone());
                    diagnostics.push(Diagnostic::error(
                        dep,
                        format!(
                            "cycle detected in `uses` dependencies: {}",
                            cycle_path.join(" -> ")
                        ),
                    ));
                } else if !fully_visited.contains(dep) && overlays.contains_key(dep) {
                    stack.push(dep.clone());
                    stack_set.insert(dep.clone());
                    indices.push(0);
                }
            } else {
                // All deps explored for this node
                fully_visited.insert(current.clone());
                stack_set.remove(&current);
                stack.pop();
                indices.pop();
            }
        }
    }

    diagnostics
}

/// Format an anyhow error chain into a multi-line description.
fn format_error_chain(err: &anyhow::Error) -> String {
    let mut parts: Vec<String> = Vec::new();
    for cause in err.chain() {
        parts.push(cause.to_string());
    }
    parts.join("\n  caused by: ")
}

/// Run all lint checks across all overlays in a repository.
pub fn lint_repository(repo: &Repository) -> LintResult {
    let mut diagnostics = Vec::new();
    let dirs = discover_overlay_dirs(repo);
    let mut parsed_overlays: HashMap<String, Overlay> = HashMap::new();
    let mut failed_overlays: HashSet<String> = HashSet::new();
    let all_names: HashSet<String> = dirs.iter().map(|(_, name)| name.clone()).collect();

    for (dir, name) in &dirs {
        // Check for multiple descriptor formats
        diagnostics.extend(check_multiple_descriptors(dir, name));

        // Try to parse the overlay
        match Overlay::new(repo, dir) {
            Ok(overlay) => {
                // Run per-overlay checks
                diagnostics.extend(checks::check_overlay(&overlay));
                parsed_overlays.insert(name.clone(), overlay);
            }
            Err(err) => {
                failed_overlays.insert(name.clone());
                diagnostics.push(Diagnostic::error(
                    name,
                    format!("failed to parse descriptor: {}", format_error_chain(&err)),
                ));
            }
        }
    }

    // Cross-overlay checks: uses references
    for (name, overlay) in &parsed_overlays {
        if let Some(uses) = &overlay.uses {
            for dep in uses {
                if parsed_overlays.contains_key(dep) {
                    // Exists and parsed fine — no issue
                } else if failed_overlays.contains(dep) {
                    // Exists but failed to parse — already reported on the target overlay
                    diagnostics.push(
                        Diagnostic::error(
                            name,
                            format!("uses overlay \"{dep}\" which failed to parse"),
                        )
                        .with_hint(format!("fix the configuration errors in \"{dep}\" first")),
                    );
                } else if all_names.contains(dep) {
                    // Discovered but somehow not in either set (shouldn't happen, defensive)
                    diagnostics.push(Diagnostic::error(
                        name,
                        format!("uses overlay \"{dep}\" which could not be loaded"),
                    ));
                } else {
                    diagnostics.push(Diagnostic::error(
                        name,
                        format!("uses references non-existent overlay \"{dep}\""),
                    ));
                }
            }
        }
    }

    // Cross-overlay checks: cycles
    let overlay_refs: HashMap<String, &Overlay> = parsed_overlays
        .iter()
        .map(|(k, v)| (k.clone(), v))
        .collect();
    diagnostics.extend(check_cycles(&overlay_refs));

    // Sort: errors first, then warnings; within same severity, by overlay name
    diagnostics.sort_by(|a, b| {
        a.severity
            .cmp(&b.severity)
            .then_with(|| a.overlay.cmp(&b.overlay))
    });

    LintResult { diagnostics }
}

// Implement Ord for Severity so errors sort before warnings
impl PartialOrd for Severity {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Severity {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let rank = |s: &Severity| match s {
            Severity::Error => 0,
            Severity::Warning => 1,
        };
        rank(self).cmp(&rank(other))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    use rstest::rstest;

    fn setup_repo() -> (TempDir, Repository) {
        let td = TempDir::new().unwrap();
        let repo = Repository::new(td.path().to_path_buf());
        (td, repo)
    }

    #[rstest]
    fn test_lint_empty_repository() {
        let (_, repo) = setup_repo();
        let result = lint_repository(&repo);
        assert!(result.diagnostics.is_empty());
        assert!(!result.has_errors());
    }

    #[rstest]
    fn test_lint_valid_overlay_no_diagnostics() {
        let (td, repo) = setup_repo();
        let ov = td.child("myoverlay");
        ov.create_dir_all().unwrap();
        ov.child("over.toml").write_str("target = \"~\"").unwrap();

        let result = lint_repository(&repo);
        assert!(result.diagnostics.is_empty());
    }

    #[rstest]
    fn test_lint_parse_error() {
        let (td, repo) = setup_repo();
        let ov = td.child("broken");
        ov.create_dir_all().unwrap();
        ov.child("over.toml")
            .write_str("this is not valid toml [[[")
            .unwrap();

        let result = lint_repository(&repo);
        assert!(result.has_errors());
        assert_eq!(result.error_count(), 1);
        assert!(result.diagnostics[0].message.contains("failed to parse"));
    }

    #[rstest]
    fn test_lint_multiple_descriptors() {
        let (td, repo) = setup_repo();
        let ov = td.child("multi");
        ov.create_dir_all().unwrap();
        ov.child("over.toml").write_str("target = \"~\"").unwrap();
        ov.child("over.yaml").write_str("target: \"~\"").unwrap();

        let result = lint_repository(&repo);
        assert_eq!(result.warning_count(), 1);
        assert!(
            result.diagnostics[0]
                .message
                .contains("multiple descriptor files")
        );
    }

    #[rstest]
    fn test_lint_uses_nonexistent_overlay() {
        let (td, repo) = setup_repo();
        let ov = td.child("broken_ref");
        ov.create_dir_all().unwrap();
        ov.child("over.toml")
            .write_str("target = \"~\"\nuses = [\"nonexistent\"]")
            .unwrap();

        let result = lint_repository(&repo);
        assert!(result.has_errors());
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.message.contains("uses references non-existent overlay"))
        );
    }

    #[rstest]
    fn test_lint_uses_cycle() {
        let (td, repo) = setup_repo();
        let a = td.child("a");
        a.create_dir_all().unwrap();
        a.child("over.toml")
            .write_str("target = \"~\"\nuses = [\"b\"]")
            .unwrap();

        let b = td.child("b");
        b.create_dir_all().unwrap();
        b.child("over.toml")
            .write_str("target = \"~\"\nuses = [\"a\"]")
            .unwrap();

        let result = lint_repository(&repo);
        assert!(result.has_errors());
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.message.contains("cycle detected"))
        );
    }

    #[rstest]
    fn test_lint_diamond_no_false_cycle() {
        let (td, repo) = setup_repo();

        let d = td.child("d");
        d.create_dir_all().unwrap();
        d.child("over.toml").write_str("target = \"~\"").unwrap();

        let b = td.child("b");
        b.create_dir_all().unwrap();
        b.child("over.toml")
            .write_str("target = \"~\"\nuses = [\"d\"]")
            .unwrap();

        let c = td.child("c");
        c.create_dir_all().unwrap();
        c.child("over.toml")
            .write_str("target = \"~\"\nuses = [\"d\"]")
            .unwrap();

        let a = td.child("a");
        a.create_dir_all().unwrap();
        a.child("over.toml")
            .write_str("target = \"~\"\nuses = [\"b\", \"c\"]")
            .unwrap();

        let result = lint_repository(&repo);
        assert!(
            !result.has_errors(),
            "diamond dependency should not report a cycle: {:?}",
            result.diagnostics
        );
    }

    #[rstest]
    fn test_lint_uses_failed_overlay() {
        let (td, repo) = setup_repo();

        // Overlay "good" references "broken" via uses
        let good = td.child("good");
        good.create_dir_all().unwrap();
        good.child("over.toml")
            .write_str("target = \"~\"\nuses = [\"broken\"]")
            .unwrap();

        // Overlay "broken" exists but cannot be parsed
        let broken = td.child("broken");
        broken.create_dir_all().unwrap();
        broken
            .child("over.toml")
            .write_str("this is not valid toml [[[")
            .unwrap();

        let result = lint_repository(&repo);
        assert!(result.has_errors());

        // "broken" should get a parse error
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.overlay == "broken" && d.message.contains("failed to parse")),
            "expected parse error for 'broken' overlay"
        );

        // "good" should NOT say "non-existent" — it should say "failed to parse"
        assert!(
            !result
                .diagnostics
                .iter()
                .any(|d| d.overlay == "good" && d.message.contains("non-existent")),
            "should not report non-existent for an overlay that exists but failed to parse"
        );
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.overlay == "good" && d.message.contains("which failed to parse")),
            "should report that the used overlay failed to parse"
        );

        // The hint should point to fixing the broken overlay first
        let diag = result
            .diagnostics
            .iter()
            .find(|d| d.overlay == "good" && d.message.contains("which failed to parse"))
            .unwrap();
        assert!(diag.hint.as_ref().is_some_and(|h| h.contains("broken")));
    }

    #[rstest]
    fn test_lint_sorting_errors_before_warnings() {
        let (td, repo) = setup_repo();
        let ov = td.child("zz_overlay");
        ov.create_dir_all().unwrap();
        ov.child("over.toml")
            .write_str("target = \"~\"\nuses = [\"nonexistent\"]\nexclude = []")
            .unwrap();

        let result = lint_repository(&repo);
        // Should have at least one error and one warning
        assert!(result.error_count() >= 1);
        assert!(result.warning_count() >= 1);

        // Errors come before warnings
        let first_warning_idx = result
            .diagnostics
            .iter()
            .position(|d| d.severity == Severity::Warning);
        let last_error_idx = result
            .diagnostics
            .iter()
            .rposition(|d| d.severity == Severity::Error);
        if let (Some(warn_idx), Some(err_idx)) = (first_warning_idx, last_error_idx) {
            assert!(err_idx < warn_idx);
        }
    }
}
