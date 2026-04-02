mod checks;

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use config::ConfigError;
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
    /// Descriptor file where the issue was found (relative to overlay dir).
    pub file: Option<String>,
}

impl Diagnostic {
    pub fn error(overlay: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            overlay: overlay.into(),
            message: message.into(),
            hint: None,
            file: None,
        }
    }

    pub fn warning(overlay: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            overlay: overlay.into(),
            message: message.into(),
            hint: None,
            file: None,
        }
    }

    #[must_use]
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    #[must_use]
    pub fn with_file(mut self, file: impl Into<String>) -> Self {
        self.file = Some(file.into());
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
            let Some(&idx) = indices.last() else {
                break;
            };

            let deps = overlays
                .get(&current)
                .and_then(|o| o.uses.as_ref())
                .map(|u| u.as_slice())
                .unwrap_or(&[]);

            if idx < deps.len() {
                // Advance the iterator for the current node
                if let Some(last) = indices.last_mut() {
                    *last += 1;
                }
                let dep = &deps[idx];

                if stack_set.contains(dep) {
                    // Found a cycle
                    let cycle_start = stack.iter().position(|s| s == dep);
                    if let Some(start) = cycle_start {
                        let mut cycle_path: Vec<String> = stack[start..].to_vec();
                        cycle_path.push(dep.clone());
                        diagnostics.push(Diagnostic::error(
                            dep,
                            format!(
                                "cycle detected in `uses` dependencies: {}",
                                cycle_path.join(" -> ")
                            ),
                        ));
                    }
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

/// Valid user-facing top-level keys in an overlay descriptor.
/// Internal keys (`name`, `root`) set by code are excluded.
const VALID_OVERLAY_KEYS: &[&str] = &[
    "description",
    "exclude",
    "format",
    "git",
    "install",
    "link_dirs",
    "target",
    "uses",
];

/// Find the descriptor file name for an overlay directory.
/// Returns the first matching `over.{ext}` file found.
fn find_descriptor_file(dir: &Path) -> Option<String> {
    for ext in EXTENSIONS {
        let filename = format!("{BASENAME}.{ext}");
        if dir.join(&filename).exists() {
            return Some(filename);
        }
    }
    None
}

/// Compute the Levenshtein edit distance between two strings.
fn edit_distance(a: &str, b: &str) -> usize {
    let a_len = a.len();
    let b_len = b.len();
    let mut matrix = vec![vec![0usize; b_len + 1]; a_len + 1];

    for (i, row) in matrix.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in matrix[0].iter_mut().enumerate() {
        *cell = j;
    }

    for (i, ac) in a.chars().enumerate() {
        for (j, bc) in b.chars().enumerate() {
            let cost = if ac == bc { 0 } else { 1 };
            matrix[i + 1][j + 1] = (matrix[i][j + 1] + 1)
                .min(matrix[i + 1][j] + 1)
                .min(matrix[i][j] + cost);
        }
    }

    matrix[a_len][b_len]
}

/// Find the closest matching key from the valid set (if close enough).
fn suggest_key(unknown: &str) -> Option<&'static str> {
    let mut best: Option<(&str, usize)> = None;
    for &valid in VALID_OVERLAY_KEYS {
        let dist = edit_distance(unknown, valid);
        // Only suggest if edit distance is at most 2 or ≤40% of the key length
        let threshold = 2.max(unknown.len() * 2 / 5);
        if dist <= threshold && (best.is_none() || dist < best.unwrap().1) {
            best = Some((valid, dist));
        }
    }
    best.map(|(k, _)| k)
}

/// Pre-validate an overlay descriptor file by parsing it as a generic
/// TOML/YAML table and checking top-level keys against the known schema.
/// Returns diagnostics for unknown keys (before serde even tries to deserialize).
fn prevalidate_descriptor(dir: &Path, overlay_name: &str) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    for ext in EXTENSIONS {
        let filename = format!("{BASENAME}.{ext}");
        let filepath = dir.join(&filename);
        let Ok(content) = fs::read_to_string(&filepath) else {
            continue;
        };

        let keys: Vec<String> = match ext {
            &"toml" => {
                let Ok(table) = content.parse::<toml::Table>() else {
                    // Syntax error — will be caught by Overlay::new() later
                    return diagnostics;
                };
                table.keys().cloned().collect()
            }
            _ => {
                // YAML: parse as serde_yml::Value
                let Ok(value) = serde_yml::from_str::<serde_yml::Value>(&content) else {
                    return diagnostics;
                };
                match value {
                    serde_yml::Value::Mapping(map) => map
                        .keys()
                        .filter_map(|k| k.as_str().map(|s| s.to_string()))
                        .collect(),
                    _ => return diagnostics,
                }
            }
        };

        for key in &keys {
            // Skip internal keys that are set by code overrides
            if key == "name" || key == "root" {
                continue;
            }
            if !VALID_OVERLAY_KEYS.contains(&key.as_str()) {
                let mut diag = Diagnostic::error(overlay_name, format!("unknown key `{key}`"))
                    .with_file(&filename);

                if let Some(suggestion) = suggest_key(key) {
                    diag = diag.with_hint(format!("did you mean `{suggestion}`?"));
                } else {
                    diag = diag
                        .with_hint(format!("valid keys are: {}", VALID_OVERLAY_KEYS.join(", ")));
                }
                diagnostics.push(diag);
            }
        }

        // Only process the first descriptor file found
        return diagnostics;
    }

    diagnostics
}

/// Extract user-friendly diagnostics from a `config::ConfigError`.
fn diagnostics_from_config_error(
    err: &ConfigError,
    overlay_name: &str,
    descriptor_file: Option<&str>,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    match err {
        ConfigError::FileParse { cause, .. } => {
            let msg = cause.to_string();
            let mut diag = Diagnostic::error(overlay_name, format!("syntax error: {msg}"));
            if let Some(file) = descriptor_file {
                diag = diag.with_file(file);
            }
            diagnostics.push(diag);
        }
        ConfigError::Type {
            unexpected,
            expected,
            key,
            ..
        } => {
            let key_str = key.as_deref().unwrap_or("<unknown>");
            let mut diag = Diagnostic::error(
                overlay_name,
                format!("wrong type for `{key_str}`: expected {expected}, got {unexpected}"),
            );
            if let Some(file) = descriptor_file {
                diag = diag.with_file(file);
            }
            diagnostics.push(diag);
        }
        ConfigError::NotFound(key) => {
            let mut diag = Diagnostic::error(overlay_name, format!("missing required key `{key}`"));
            if let Some(file) = descriptor_file {
                diag = diag.with_file(file);
            }
            diagnostics.push(diag);
        }
        ConfigError::At { error, key, .. } => {
            // Unwrap nested errors, preserving key context
            let inner_diags = diagnostics_from_config_error(error, overlay_name, descriptor_file);
            if inner_diags.is_empty() {
                // Fallback: show the At error directly
                let key_str = key.as_deref().unwrap_or("<unknown>");
                let mut diag = Diagnostic::error(
                    overlay_name,
                    format!("invalid value for `{key_str}`: {error}"),
                );
                if let Some(file) = descriptor_file {
                    diag = diag.with_file(file);
                }
                diagnostics.push(diag);
            } else {
                // Enrich inner diagnostics with key context if they don't have one
                for mut d in inner_diags {
                    if let Some(k) = key
                        && !d.message.contains(&format!("`{k}`"))
                    {
                        d.message = format!("in `{k}`: {}", d.message);
                    }
                    diagnostics.push(d);
                }
            }
        }
        ConfigError::Message(msg) => {
            // Serde custom errors — try to extract meaningful info
            let mut diag = parse_serde_message(msg, overlay_name);
            if let Some(file) = descriptor_file {
                diag = diag.with_file(file);
            }
            diagnostics.push(diag);
        }
        ConfigError::Foreign(cause) => {
            let mut diag = Diagnostic::error(overlay_name, format!("configuration error: {cause}"));
            if let Some(file) = descriptor_file {
                diag = diag.with_file(file);
            }
            diagnostics.push(diag);
        }
        ConfigError::Frozen => {
            diagnostics.push(Diagnostic::error(
                overlay_name,
                "internal error: configuration is frozen".to_string(),
            ));
        }
        ConfigError::PathParse { cause } => {
            let mut diag = Diagnostic::error(overlay_name, format!("invalid config path: {cause}"));
            if let Some(file) = descriptor_file {
                diag = diag.with_file(file);
            }
            diagnostics.push(diag);
        }
        // ConfigError is #[non_exhaustive]
        _ => {
            let mut diag = Diagnostic::error(overlay_name, format!("configuration error: {err}"));
            if let Some(file) = descriptor_file {
                diag = diag.with_file(file);
            }
            diagnostics.push(diag);
        }
    }

    diagnostics
}

/// Parse a serde custom error message into a user-friendly diagnostic.
/// Serde produces messages like:
///   "unknown field `foo`, expected one of `bar`, `baz`"
///   "invalid type: found boolean, expected a string"
fn parse_serde_message(msg: &str, overlay_name: &str) -> Diagnostic {
    // Handle "unknown field `X`, expected one of ..."
    if let Some(rest) = msg.strip_prefix("unknown field `")
        && let Some(field_end) = rest.find('`')
    {
        let field = &rest[..field_end];
        // Filter out internal keys from the "expected" list
        let hint = if let Some(suggestion) = suggest_key(field) {
            format!("did you mean `{suggestion}`?")
        } else {
            format!("valid keys are: {}", VALID_OVERLAY_KEYS.join(", "))
        };
        return Diagnostic::error(overlay_name, format!("unknown key `{field}`")).with_hint(hint);
    }

    // Handle "unknown variant `X`, expected one of ..."
    if msg.starts_with("unknown variant") {
        return Diagnostic::error(overlay_name, msg.to_string());
    }

    // Handle "invalid type: ..."
    if msg.starts_with("invalid type") {
        return Diagnostic::error(overlay_name, msg.to_string());
    }

    // Handle "missing field `X`"
    if let Some(rest) = msg.strip_prefix("missing field `")
        && let Some(field_end) = rest.find('`')
    {
        let field = &rest[..field_end];
        return Diagnostic::error(overlay_name, format!("missing required key `{field}`"));
    }

    // Fallback: use the message as-is
    Diagnostic::error(overlay_name, msg.to_string())
}

/// Extract diagnostics from an anyhow error, attempting to downcast to ConfigError first.
fn diagnostics_from_anyhow_error(
    err: &anyhow::Error,
    overlay_name: &str,
    descriptor_file: Option<&str>,
) -> Vec<Diagnostic> {
    // Try to downcast to ConfigError for structured handling
    if let Some(config_err) = err.downcast_ref::<ConfigError>() {
        let diags = diagnostics_from_config_error(config_err, overlay_name, descriptor_file);
        if !diags.is_empty() {
            return diags;
        }
    }

    // Fallback: use the error chain but format it more cleanly
    let root_msg = err.to_string();
    let mut diag = Diagnostic::error(overlay_name, root_msg);
    if let Some(file) = descriptor_file {
        diag = diag.with_file(file);
    }
    vec![diag]
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

        // Pre-validate config keys against known schema
        let prevalidation = prevalidate_descriptor(dir, name);
        let has_unknown_keys = prevalidation.iter().any(|d| d.severity == Severity::Error);
        diagnostics.extend(prevalidation);

        // If there are unknown keys, skip deserialization (it would fail with raw serde errors)
        if has_unknown_keys {
            failed_overlays.insert(name.clone());
            continue;
        }

        // Try to parse the overlay
        let descriptor_file = find_descriptor_file(dir);
        match Overlay::new(repo, dir) {
            Ok(overlay) => {
                // Run per-overlay checks
                diagnostics.extend(checks::check_overlay(&overlay));
                parsed_overlays.insert(name.clone(), overlay);
            }
            Err(err) => {
                failed_overlays.insert(name.clone());
                diagnostics.extend(diagnostics_from_anyhow_error(
                    &err,
                    name,
                    descriptor_file.as_deref(),
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
        assert!(
            result.diagnostics[0].message.contains("syntax error"),
            "expected 'syntax error' in message, got: {}",
            result.diagnostics[0].message
        );
        // Should include file reference
        assert_eq!(
            result.diagnostics[0].file.as_deref(),
            Some("over.toml"),
            "diagnostic should reference the descriptor file"
        );
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

        // "broken" should get a syntax error
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.overlay == "broken" && d.message.contains("syntax error")),
            "expected syntax error for 'broken' overlay, got: {:?}",
            result
                .diagnostics
                .iter()
                .filter(|d| d.overlay == "broken")
                .collect::<Vec<_>>()
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

    #[rstest]
    fn test_lint_unknown_key() {
        let (td, repo) = setup_repo();
        let ov = td.child("bad_key");
        ov.create_dir_all().unwrap();
        ov.child("over.toml")
            .write_str("target = \"~\"\nfoo = \"bar\"")
            .unwrap();

        let result = lint_repository(&repo);
        assert!(result.has_errors());
        let diag = result
            .diagnostics
            .iter()
            .find(|d| d.message.contains("unknown key"))
            .expect("should report unknown key");
        assert!(diag.message.contains("`foo`"));
        assert_eq!(diag.file.as_deref(), Some("over.toml"));
    }

    #[rstest]
    fn test_lint_unknown_key_with_suggestion() {
        let (td, repo) = setup_repo();
        let ov = td.child("typo");
        ov.create_dir_all().unwrap();
        ov.child("over.toml")
            .write_str("target = \"~\"\ntargt = \"~\"")
            .unwrap();

        let result = lint_repository(&repo);
        assert!(result.has_errors());
        let diag = result
            .diagnostics
            .iter()
            .find(|d| d.message.contains("unknown key") && d.message.contains("`targt`"))
            .expect("should report unknown key 'targt'");
        assert!(
            diag.hint
                .as_ref()
                .is_some_and(|h| h.contains("did you mean `target`?")),
            "should suggest 'target', got hint: {:?}",
            diag.hint
        );
    }

    #[rstest]
    fn test_lint_multiple_unknown_keys() {
        let (td, repo) = setup_repo();
        let ov = td.child("multi_bad");
        ov.create_dir_all().unwrap();
        ov.child("over.toml")
            .write_str("target = \"~\"\nfoo = 1\nbar = 2")
            .unwrap();

        let result = lint_repository(&repo);
        let unknown_diags: Vec<_> = result
            .diagnostics
            .iter()
            .filter(|d| d.message.contains("unknown key"))
            .collect();
        assert_eq!(
            unknown_diags.len(),
            2,
            "should report both unknown keys, got: {:?}",
            unknown_diags
        );
    }

    #[rstest]
    fn test_lint_unknown_key_skips_deserialization() {
        let (td, repo) = setup_repo();
        let ov = td.child("skip_deser");
        ov.create_dir_all().unwrap();
        // Has an unknown key — should NOT produce raw serde errors
        ov.child("over.toml")
            .write_str("target = \"~\"\nunknown_field = true")
            .unwrap();

        let result = lint_repository(&repo);
        assert!(result.has_errors());
        // Should only have the pre-validation error, not a raw serde error
        for diag in &result.diagnostics {
            assert!(
                !diag.message.contains("expected one of"),
                "should not contain raw serde error text: {}",
                diag.message
            );
        }
    }

    #[rstest]
    fn test_lint_type_mismatch_produces_clean_error() {
        let (td, repo) = setup_repo();
        let ov = td.child("bad_type");
        ov.create_dir_all().unwrap();
        // `target` should be a string, not a list
        ov.child("over.toml")
            .write_str("target = [1, 2, 3]")
            .unwrap();

        let result = lint_repository(&repo);
        assert!(result.has_errors());
        // Should not contain "caused by:" chain — should be a clean error
        for diag in &result.diagnostics {
            assert!(
                !diag.message.contains("caused by:"),
                "should not contain 'caused by:' chain: {}",
                diag.message
            );
        }
    }

    #[rstest]
    fn test_lint_yaml_unknown_key() {
        let (td, repo) = setup_repo();
        let ov = td.child("yaml_bad");
        ov.create_dir_all().unwrap();
        ov.child("over.yaml")
            .write_str("target: \"~\"\nfoo: bar")
            .unwrap();

        let result = lint_repository(&repo);
        assert!(result.has_errors());
        let diag = result
            .diagnostics
            .iter()
            .find(|d| d.message.contains("unknown key"))
            .expect("should report unknown key in YAML");
        assert!(diag.message.contains("`foo`"));
        assert_eq!(diag.file.as_deref(), Some("over.yaml"));
    }

    #[rstest]
    fn test_lint_yaml_syntax_error() {
        let (td, repo) = setup_repo();
        let ov = td.child("yaml_broken");
        ov.create_dir_all().unwrap();
        ov.child("over.yaml")
            .write_str("target: [\ninvalid yaml")
            .unwrap();

        let result = lint_repository(&repo);
        assert!(result.has_errors());
        let diag = &result.diagnostics[0];
        assert!(
            diag.message.contains("syntax error"),
            "expected syntax error, got: {}",
            diag.message
        );
    }

    #[rstest]
    fn test_edit_distance() {
        assert_eq!(edit_distance("target", "targt"), 1);
        assert_eq!(edit_distance("target", "target"), 0);
        assert_eq!(edit_distance("exclude", "exclde"), 1);
        assert_eq!(edit_distance("uses", "use"), 1);
        assert_eq!(edit_distance("git", "gti"), 2);
        assert_eq!(edit_distance("abc", "xyz"), 3);
    }

    #[rstest]
    fn test_suggest_key() {
        assert_eq!(suggest_key("targt"), Some("target"));
        assert_eq!(suggest_key("exclde"), Some("exclude"));
        assert_eq!(suggest_key("descrption"), Some("description"));
        assert_eq!(suggest_key("uss"), Some("uses"));
        assert_eq!(suggest_key("gti"), Some("git"));
        // Too different — no suggestion
        assert_eq!(suggest_key("zzzzzzz"), None);
    }

    #[rstest]
    fn test_parse_serde_message_unknown_field() {
        let diag = parse_serde_message(
            "unknown field `foo`, expected one of `name`, `root`, `target`",
            "test_overlay",
        );
        assert!(diag.message.contains("unknown key `foo`"));
        // Should not leak internal keys like `name` and `root` in the hint
        assert!(diag.hint.is_some());
    }

    #[rstest]
    fn test_parse_serde_message_missing_field() {
        let diag = parse_serde_message("missing field `target`", "test_overlay");
        assert!(diag.message.contains("missing required key `target`"));
    }

    #[rstest]
    fn test_diagnostic_with_file() {
        let diag = Diagnostic::error("test", "test message").with_file("over.toml");
        assert_eq!(diag.file.as_deref(), Some("over.toml"));
    }
}
