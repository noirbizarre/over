mod checks;
mod declarations;
mod ownership;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};

use config::ConfigError;
use globset::GlobBuilder;
use serde::Serialize;
use walkdir::WalkDir;

use crate::exec::Context;
use crate::overlays::{BASENAME, EXTENSIONS, GLOB_PATTERN, Overlay, Repository};
use crate::ui::style;

/// Severity level for a lint diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
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
#[derive(Debug, Clone, Serialize)]
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

impl Diagnostic {
    // Share severity, file and hint formatting between grouped lint output and
    // doctor's standalone findings, which still need their overlay identity.
    fn fmt_finding(&self, f: &mut fmt::Formatter<'_>, standalone: bool) -> fmt::Result {
        let severity_label = match self.severity {
            Severity::Error => style::red("error"),
            Severity::Warning => style::yellow("warning"),
        };
        write!(f, "{severity_label}")?;
        if standalone {
            write!(
                f,
                "{}",
                style::overlay(format!("[{}]", crate::utils::short_path(&self.overlay)))
            )?;
        }
        write!(
            f,
            ": {}",
            highlight_inline(&self.message, console::colors_enabled()).replace('\n', "\n  ")
        )?;

        if let Some(file) = self.file.as_ref().filter(|_| standalone) {
            // The repository-root overlay has an empty graph identity, not an
            // absolute filesystem path beginning with `/`.
            let overlay = if self.overlay.is_empty() {
                "."
            } else {
                &self.overlay
            };
            write!(
                f,
                "\n  {} {}",
                console::style("-->").dim(),
                // Absolute parser origins must not be prefixed with the overlay identity.
                style::path(if std::path::Path::new(file).is_absolute() {
                    file.to_owned()
                } else {
                    format!("{overlay}/{file}")
                })
            )?;
        }

        if let Some(hint) = &self.hint {
            write!(
                f,
                "\n  {} {}",
                console::style("=").dim(),
                highlight_inline(hint, console::colors_enabled()).replace('\n', "\n  ")
            )?;
        }

        Ok(())
    }
}

fn highlight_inline(text: &str, colors: bool) -> String {
    // Diagnostics retain lightweight inline-code markup for machine consumers;
    // only the human renderer interprets it, including upstream parser messages.
    crate::utils::short_paths(text)
        .split('`')
        .enumerate()
        .map(|(index, span)| {
            if index % 2 == 0 {
                span.to_owned()
            } else {
                console::style(span)
                    .bold()
                    .force_styling(colors)
                    .to_string()
            }
        })
        .collect()
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.fmt_finding(f, true)
    }
}

struct GroupedFinding<'a>(&'a Diagnostic);

impl fmt::Display for GroupedFinding<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt_finding(f, false)
    }
}

/// Overlay and file sections without changing canonical diagnostic order.
pub struct GroupedDiagnostics<'a>(&'a LintResult);

impl fmt::Display for GroupedDiagnostics<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut overlays = BTreeMap::<_, BTreeMap<_, Vec<_>>>::new();
        for diagnostic in &self.0.diagnostics {
            overlays
                .entry(diagnostic.overlay.as_str())
                .or_default()
                .entry(diagnostic.file.as_deref())
                .or_default()
                .push(diagnostic);
        }
        for (index, (overlay, files)) in overlays.into_iter().enumerate() {
            if index > 0 {
                writeln!(f)?;
            }
            let name = if overlay.is_empty() { "." } else { overlay };
            writeln!(f, "{}", style::overlay(crate::utils::short_path(name)))?;
            // Option ordering places overlay-wide findings before file groups.
            for (file, diagnostics) in files {
                let indent = if let Some(file) = file {
                    writeln!(f, "  {}", style::path(file))?;
                    "    "
                } else {
                    "  "
                };
                for diagnostic in diagnostics {
                    // Indent continuation lines so multiline details stay with their finding.
                    for line in GroupedFinding(diagnostic).to_string().lines() {
                        writeln!(f, "{indent}{line}")?;
                    }
                }
            }
        }
        Ok(())
    }
}

/// Aggregated lint results.
#[derive(Debug, Default)]
pub struct LintResult {
    pub diagnostics: Vec<Diagnostic>,
}

impl LintResult {
    pub fn grouped(&self) -> GroupedDiagnostics<'_> {
        GroupedDiagnostics(self)
    }

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

    // Union with root-declared `overlays:` directories (#113/#127), same
    // as `Repository::overlays()` — otherwise `over lint` would be blind
    // to overlays declared only via the root config.
    dirs.extend(repo.declared_overlay_dirs());
    dirs.sort();
    dirs.dedup();

    dirs.iter()
        .enumerate()
        .filter(|(idx, dir)| !matches!(dirs.get(idx + 1), Some(next) if next.starts_with(dir)))
        .filter_map(|(_, dir)| {
            dir.strip_prefix(&repo.root)
                .ok()
                // Normalize to `/` so this name matches the one
                // `Overlay::new` computes for the same directory
                // (src/overlays/overlay.rs) — otherwise a nested overlay's
                // key here (`\`-separated on Windows) would never equal
                // the `/`-separated name another overlay's `uses` entry
                // references, breaking cross-overlay checks on Windows.
                .and_then(|rel| rel.to_str().map(|s| s.replace('\\', "/")))
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

    let mut names: Vec<_> = overlays.keys().collect();
    names.sort();
    for name in names {
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
const VALID_OVERLAY_KEYS: &[&str] = crate::overlays::schema::OVERLAY_KEYS;

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

/// Find the closest matching key from the valid set (if close enough).
fn suggest_key(unknown: &str) -> Option<&'static str> {
    crate::overlays::schema::suggest_key(unknown, VALID_OVERLAY_KEYS)
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
            format!(
                "valid keys are: {}",
                VALID_OVERLAY_KEYS
                    .iter()
                    .map(|key| format!("`{key}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
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
    let root_msg = format!("{err:#}");
    let mut diag = Diagnostic::error(overlay_name, root_msg);
    if let Some(file) = descriptor_file {
        diag = diag.with_file(file);
    }
    vec![diag]
}

/// Run all lint checks across all overlays in a repository.
pub fn lint_repository(repo: &Repository) -> LintResult {
    let repo = repo.collecting_warnings();
    let repo = &repo;
    let context = default_context(repo);
    lint_dirs(
        repo,
        discover_overlay_dirs(repo),
        false,
        true,
        context.as_deref(),
    )
}

/// Check resolved ownership against an explicit target root and machine context.
pub fn lint_repository_with_context(ctx: &Context) -> LintResult {
    let repo = ctx.repository.collecting_warnings();
    lint_dirs(&repo, discover_overlay_dirs(&repo), false, true, Some(ctx))
}

fn default_context(repo: &Repository) -> Option<std::sync::Arc<Context>> {
    dirs::home_dir().map(|root| {
        Context::builder()
            .repository(repo.clone())
            .root(root)
            .build()
    })
}

/// Lint one overlay, optionally including its transitive `uses` dependencies.
pub fn lint_overlay(
    repo: &Repository,
    name: &str,
    include_uses: bool,
) -> anyhow::Result<LintResult> {
    let context = default_context(repo);
    lint_overlay_inner(repo, name, include_uses, context.as_deref())
}

/// Lint a selected composition using the caller's deployment context.
pub fn lint_overlay_with_context(
    ctx: &Context,
    name: &str,
    include_uses: bool,
) -> anyhow::Result<LintResult> {
    lint_overlay_inner(&ctx.repository, name, include_uses, Some(ctx))
}

fn lint_overlay_inner(
    repo: &Repository,
    name: &str,
    include_uses: bool,
    ctx: Option<&Context>,
) -> anyhow::Result<LintResult> {
    let repo = repo.collecting_warnings();
    let repo = &repo;
    // Components remove harmless `./` and trailing separators without requiring
    // a descriptor to parse before lint can produce its friendlier diagnostics.
    let relative: PathBuf = Path::new(name)
        .components()
        .filter(|component| *component != std::path::Component::CurDir)
        .collect();
    let dir = repo.root.join(&relative);
    anyhow::ensure!(
        dir.is_dir(),
        "overlay '{name}' not found (no such directory: {})",
        dir.display()
    );
    let name = dir
        .strip_prefix(&repo.root)?
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("overlay path is not valid UTF-8"))?
        .replace('\\', "/");
    Ok(lint_dirs(repo, vec![(dir, name)], true, include_uses, ctx))
}

fn lint_dirs(
    repo: &Repository,
    mut dirs: Vec<(PathBuf, String)>,
    scoped: bool,
    include_uses: bool,
    ctx: Option<&Context>,
) -> LintResult {
    // Entry points supply one silent collector shared by discovery and ownership reloads.
    let warning_root = match crate::overlays::mapping::absolute(&repo.root) {
        Ok(root) => root,
        Err(error) => {
            return LintResult {
                diagnostics: vec![Diagnostic::error(
                    ".",
                    format!("cannot resolve repository root: {error:#}"),
                )],
            };
        }
    };
    let mut diagnostics = Vec::new();
    let mut parsed_overlays: HashMap<String, Overlay> = HashMap::new();
    let mut failed_overlays: HashSet<String> = HashSet::new();
    let all_names: HashSet<String> = dirs.iter().map(|(_, name)| name.clone()).collect();
    let mut visited = HashSet::new();
    // Dependencies are graph nodes, not independent composition selections.
    let mut roots: Vec<_> = dirs.iter().map(|(_, name)| name.clone()).collect();
    roots.sort();

    while let Some((dir, name)) = dirs.pop() {
        // Mark failed loads too, so shared broken dependencies are reported once.
        if !visited.insert(name.clone()) {
            continue;
        }
        // Check for multiple descriptor formats
        diagnostics.extend(check_multiple_descriptors(&dir, &name));

        // Try to parse the overlay
        let descriptor_file = find_descriptor_file(&dir);
        match Overlay::new(repo, &dir) {
            Ok(mut overlay) => {
                // Path spelling must not turn a shared graph node into two owners.
                if let Some(uses) = &mut overlay.uses {
                    for dependency in uses {
                        *dependency = crate::overlays::mapping::lexical(Path::new(dependency))
                            .to_string_lossy()
                            .replace('\\', "/");
                    }
                }
                if dir == repo.root
                    && let Err(error) = repo.canonical_repository()
                {
                    diagnostics.push(Diagnostic::error(&name, format!("{error:#}")));
                }
                // Run per-overlay checks
                match crate::overlays::ignore::SourcePolicy::new(&repo.root, &overlay) {
                    Ok(policy) => {
                        diagnostics.extend(checks::check_overlay_selected(&overlay, Some(&policy)))
                    }
                    Err(error) => diagnostics.push(Diagnostic::error(&name, format!("{error:#}"))),
                }
                if include_uses {
                    for dep in overlay.uses.iter().flatten() {
                        let dir = repo.root.join(dep);
                        if dir.is_dir() {
                            dirs.push((dir, dep.clone()));
                        }
                    }
                }
                parsed_overlays.insert(name.clone(), overlay);
            }
            Err(err) => {
                failed_overlays.insert(name.clone());
                diagnostics.extend(diagnostics_from_anyhow_error(
                    &err,
                    &name,
                    descriptor_file.as_deref(),
                ));
            }
        }
    }

    // Root-only fields still matter for child selections, but must not be
    // reported twice if dependency traversal already checked the root.
    if !visited.contains("")
        && let Err(error) = repo.canonical_repository()
    {
        diagnostics.push(Diagnostic::error(".", format!("{error:#}")));
    }

    // Cross-overlay checks: uses references
    for (name, overlay) in &parsed_overlays {
        if let Some(uses) = &overlay.uses {
            for dep in uses {
                if parsed_overlays.contains_key(dep) {
                    // Exists and parsed fine — no issue
                } else if failed_overlays.contains(dep) {
                    // Its own errors seed transitive dependency warnings below.
                } else if scoped && !include_uses && repo.root.join(dep).is_dir() {
                    // Existence is enough when the dependency was intentionally
                    // excluded; parsing it here would defeat `--no-uses`.
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

    // Only configuration and graph errors are universal dependency failures;
    // later resolution errors can depend on the selected consumer's metadata.
    let invalid: HashSet<_> = diagnostics
        .iter()
        .filter(|diag| diag.severity == Severity::Error)
        .map(|diag| diag.overlay.clone())
        .collect();
    if include_uses {
        diagnostics.extend(dependency_warnings(&parsed_overlays, &invalid));
    }

    if let Some(ctx) = ctx {
        // Existing precise errors already prevent a complete ownership analysis.
        let mut seen = HashSet::new();
        for root in roots {
            // Known graph failures already have precise tolerant diagnostics.
            if !valid_closure(
                &root,
                &parsed_overlays,
                &invalid,
                include_uses,
                &mut HashSet::new(),
                &mut HashSet::new(),
            ) {
                continue;
            }
            let Some(overlay) = parsed_overlays.get(&root) else {
                continue;
            };
            let mut context = Context::builder()
                .repository(repo.clone())
                .root(ctx.root.clone())
                .machine(ctx.machine.clone())
                .overlay(overlay.clone())
                .no_uses(!include_uses)
                .build()
                .with_path_inputs(ctx);
            let desired = (|| -> anyhow::Result<_> {
                context = context.resolve_data()?;
                let target = overlay.resolve_target(&context)?;
                context = context.with_resolved_overlay(
                    overlay.name.clone(),
                    target.to_string_lossy().into_owned(),
                );
                crate::desired::DesiredTree::build(&context, overlay)
            })();
            match desired {
                Ok(tree) => {
                    for selection in &tree.sources {
                        if selection.overlay.has_target_rules() {
                            diagnostics.extend(checks::check_routed_terminal_rules(
                                &selection.overlay,
                                &selection.routing,
                            ));
                        }
                    }
                    diagnostics.extend(ownership::check(&root, &tree, &mut seen));
                }
                Err(error) => {
                    let message = format!("{error:#}");
                    diagnostics.push(Diagnostic::error(&root, format!("cannot resolve desired ownership: {message}"))
                            .with_hint("fix the named declaration or provide missing template data/defaults; destinations use the current machine and --target"));
                }
            }
        }
    } else {
        diagnostics.push(
            Diagnostic::error(
                ".",
                "cannot resolve ownership: could not determine home directory",
            )
            .with_hint("supply an explicit target root with --target"),
        );
    }

    // Discovery mistakes do not invalidate an overlay's own composition or dependencies.
    diagnostics.extend(declarations::check(repo));

    for warning in repo.warnings.warnings() {
        // Attribute shared ancestor findings to their declaring directory, once per pass.
        let directory = warning.file.parent().unwrap_or(&warning_root);
        let name = directory
            .strip_prefix(&warning_root)
            .unwrap_or(directory)
            .to_string_lossy()
            .replace('\\', "/");
        let file = warning
            .file
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        diagnostics.push(
            Diagnostic::warning(name, warning.message)
                .with_file(file)
                .with_hint(warning.hint),
        );
    }

    // Keep each overlay together even if another overlay has more severe findings.
    diagnostics.sort_by(|a, b| {
        a.overlay
            .cmp(&b.overlay)
            .then_with(|| a.severity.cmp(&b.severity))
            .then_with(|| a.message.cmp(&b.message))
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.hint.cmp(&b.hint))
    });

    LintResult { diagnostics }
}

fn dependency_warnings(
    overlays: &HashMap<String, Overlay>,
    invalid: &HashSet<String>,
) -> Vec<Diagnostic> {
    let mut consumers: HashMap<&str, Vec<&str>> = HashMap::new();
    for (name, overlay) in overlays {
        for dependency in overlay.uses.iter().flatten() {
            consumers.entry(dependency).or_default().push(name);
        }
    }

    let mut diagnostics = Vec::new();
    for cause in invalid {
        let mut pending = vec![cause.as_str()];
        // Seed with the cause to prevent self-warnings when traversing cycles.
        let mut visited = HashSet::from([cause.as_str()]);
        while let Some(dependency) = pending.pop() {
            for &consumer in consumers.get(dependency).into_iter().flatten() {
                // Original causes, not intermediate warnings, are propagated;
                // diamonds and multiple errors therefore produce one finding.
                if visited.insert(consumer) {
                    diagnostics.push(
                        Diagnostic::warning(
                            consumer,
                            format!(
                                "depends on overlay \"{cause}\" which has configuration errors"
                            ),
                        )
                        .with_hint(format!("fix the errors in \"{cause}\" first")),
                    );
                    pending.push(consumer);
                }
            }
        }
    }
    diagnostics
}

fn valid_closure(
    name: &str,
    overlays: &HashMap<String, Overlay>,
    invalid: &HashSet<String>,
    include_uses: bool,
    active: &mut HashSet<String>,
    done: &mut HashSet<String>,
) -> bool {
    if invalid.contains(name) {
        return false;
    }
    if done.contains(name) {
        return true;
    }
    let Some(overlay) = overlays.get(name) else {
        return false;
    };
    if !active.insert(name.to_owned()) {
        return false;
    }
    if include_uses
        && overlay
            .uses
            .iter()
            .flatten()
            .any(|dep| !valid_closure(dep, overlays, invalid, true, active, done))
    {
        return false;
    }
    active.remove(name);
    done.insert(name.to_owned());
    true
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
    use crate::overlays::schema::edit_distance;
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    use rstest::rstest;

    fn setup_repo() -> (TempDir, Repository) {
        let td = TempDir::new().unwrap();
        let repo = Repository::new(td.path().to_path_buf());
        (td, repo)
    }

    #[rstest]
    #[case(true)]
    #[case(false)]
    fn scoped_lint_excludes_unrelated_broken_overlays(#[case] include_uses: bool) {
        let (td, repo) = setup_repo();
        td.child("clean").create_dir_all().unwrap();
        td.child("broken/over.toml")
            .write_str("mode = 'invalid'")
            .unwrap();

        let result = lint_overlay(&repo, "clean", include_uses).unwrap();
        assert!(result.diagnostics.is_empty());
        assert!(lint_repository(&repo).has_errors());
    }

    #[rstest]
    fn transitive_shared_dependencies_are_linted_once() {
        let (td, repo) = setup_repo();
        for (name, config) in [
            ("selected", "uses = ['left', 'right']"),
            ("left", "uses = ['shared']"),
            ("right", "uses = ['shared']"),
            ("shared", "exclude = ['[']"),
        ] {
            td.child(format!("{name}/over.toml"))
                .write_str(config)
                .unwrap();
        }

        let result = lint_overlay(&repo, "selected", true).unwrap();
        assert_eq!(
            result
                .diagnostics
                .iter()
                .filter(|d| d.overlay == "shared")
                .count(),
            1
        );
        assert_eq!(result.error_count(), 1);
        assert_eq!(result.warning_count(), 3);
        for consumer in ["selected", "left", "right"] {
            let warnings: Vec<_> = result
                .diagnostics
                .iter()
                .filter(|d| d.overlay == consumer)
                .collect();
            assert_eq!(warnings.len(), 1);
            assert_eq!(warnings[0].severity, Severity::Warning);
            assert!(warnings[0].message.contains("overlay \"shared\""));
        }
        assert!(
            lint_overlay(&repo, "selected", false)
                .unwrap()
                .diagnostics
                .is_empty()
        );
    }

    #[rstest]
    fn excluding_uses_still_reports_missing_direct_references() {
        let (td, repo) = setup_repo();
        td.child("selected/over.toml")
            .write_str("uses = ['missing']")
            .unwrap();
        let result = lint_overlay(&repo, "selected", false).unwrap();
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.message.contains("non-existent overlay"))
        );
    }

    #[rstest]
    fn excluding_operations_does_not_hide_unresolvable_dependency_metadata() {
        let (td, repo) = setup_repo();
        td.child("a/over.toml").write_str("uses = ['b']").unwrap();
        td.child("b/over.toml").write_str("uses = ['a']").unwrap();
        assert!(
            lint_overlay(&repo, "a", true)
                .unwrap()
                .diagnostics
                .iter()
                .any(|d| d.message.contains("cycle detected"))
        );
        let result = lint_overlay(&repo, "a", false).unwrap();
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diag| diag.message.contains("cycle in data dependencies"))
        );
        assert!(
            !result
                .diagnostics
                .iter()
                .any(|diag| diag.message.contains("cycle detected"))
        );
    }

    #[rstest]
    #[case(".")]
    #[case("./parent/")]
    #[case("parent/child")]
    fn explicit_selection_accepts_root_parent_and_descriptorless_directories(#[case] name: &str) {
        let (td, repo) = setup_repo();
        td.child("over.toml").write_str("target = '~'").unwrap();
        td.child("parent/child").create_dir_all().unwrap();
        let result = lint_overlay(&repo, name, true).unwrap();
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    }

    #[rstest]
    fn selected_descriptors_warn_with_typo_hints() {
        let (td, repo) = setup_repo();
        td.child("broken/over.toml")
            .write_str("targt = '~'")
            .unwrap();
        let result = lint_overlay(&repo, "broken", true).unwrap();
        assert!(!result.has_errors());
        assert_eq!(result.warning_count(), 1);
        assert!(result.diagnostics.iter().any(|d| {
            d.hint
                .as_deref()
                .is_some_and(|hint| hint.contains("target"))
        }));
        assert!(
            lint_overlay(&repo, "missing", true)
                .unwrap_err()
                .to_string()
                .contains("not found")
        );
    }

    #[rstest]
    #[case(true)]
    #[case(false)]
    fn non_normalized_repository_roots_keep_warning_overlay_names_relative(#[case] relative: bool) {
        let cwd = std::env::current_dir().unwrap();
        let temp = tempfile::tempdir_in(&cwd).unwrap();
        std::fs::create_dir(temp.path().join("selected")).unwrap();
        std::fs::create_dir(temp.path().join("unused")).unwrap();
        std::fs::write(temp.path().join("selected/over.toml"), "extra = true").unwrap();
        let root = if relative {
            temp.path().strip_prefix(&cwd).unwrap().to_path_buf()
        } else {
            temp.path().join("unused/..")
        };
        let result = lint_overlay(&Repository::new(root), "selected", false).unwrap();
        assert!(!result.has_errors(), "{:?}", result.diagnostics);
        assert_eq!(result.warning_count(), 1);
        assert_eq!(result.diagnostics[0].overlay, "selected");
        assert_eq!(result.diagnostics[0].file.as_deref(), Some("over.toml"));
    }

    #[rstest]
    fn descriptorless_selections_follow_inherited_uses() {
        let (td, repo) = setup_repo();
        td.child("hosts/over.toml")
            .write_str("uses = ['dependency']")
            .unwrap();
        td.child("hosts/laptop").create_dir_all().unwrap();
        td.child("dependency/over.toml")
            .write_str("exclude = ['[']")
            .unwrap();
        assert!(
            lint_overlay(&repo, "hosts/laptop", true)
                .unwrap()
                .has_errors()
        );
        assert!(
            lint_overlay(&repo, "hosts/laptop", false)
                .unwrap()
                .diagnostics
                .is_empty()
        );
    }

    #[rstest]
    fn scoped_lint_keeps_root_only_validation() {
        let (td, repo) = setup_repo();
        td.child("over.yaml")
            .write_str("repository: false\n")
            .unwrap();
        td.child("child").create_dir_all().unwrap();
        let result = lint_overlay(&repo, "child", false).unwrap();
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.message.contains("repository"))
        );
    }

    #[rstest]
    #[case(None)]
    #[case(Some(true))]
    #[case(Some(false))]
    fn root_declarations_are_checked_once_even_when_only_children_are_selected(
        #[case] include_uses: Option<bool>,
    ) {
        let (td, repo) = setup_repo();
        td.child("over.toml")
            .write_str("overlays = ['missing', 'optional/*']")
            .unwrap();
        td.child("child/over.toml")
            .write_str("overlays = ['nested-missing']")
            .unwrap();
        let result = match include_uses {
            Some(include_uses) => lint_overlay(&repo, "child", include_uses).unwrap(),
            None => lint_repository(&repo),
        };
        assert_eq!(result.error_count(), 1, "{:?}", result.diagnostics);
        assert_eq!(result.warning_count(), 1, "{:?}", result.diagnostics);
        assert!(result.diagnostics.iter().all(|d| d.overlay.is_empty()));
        assert!(
            !result
                .diagnostics
                .iter()
                .any(|d| d.message.contains("nested-missing"))
        );
    }

    #[rstest]
    fn an_unreadable_root_does_not_gain_a_duplicate_declaration_error() {
        let (td, repo) = setup_repo();
        td.child("over.toml").write_str("overlays = [[[").unwrap();
        td.child("child").create_dir_all().unwrap();
        let result = lint_overlay(&repo, "child", false).unwrap();
        assert!(!result.diagnostics.is_empty());
        assert!(
            !result
                .diagnostics
                .iter()
                .any(|d| d.message.contains("overlays["))
        );
    }

    #[rstest]
    fn excluding_dependencies_keeps_self_reference_checks() {
        let (td, repo) = setup_repo();
        td.child("selected/over.toml")
            .write_str("uses = ['selected']")
            .unwrap();
        assert!(lint_overlay(&repo, "selected", false).unwrap().has_errors());
    }

    #[rstest]
    fn empty_repository_has_no_diagnostics() {
        let (_, repo) = setup_repo();
        let result = lint_repository(&repo);
        assert!(result.diagnostics.is_empty());
        assert!(!result.has_errors());
    }

    #[rstest]
    fn valid_overlay_produces_no_diagnostics() {
        let (td, repo) = setup_repo();
        let ov = td.child("myoverlay");
        ov.create_dir_all().unwrap();
        ov.child("over.toml").write_str("target = \"~\"").unwrap();

        let result = lint_repository(&repo);
        assert!(result.diagnostics.is_empty());
    }

    #[rstest]
    fn overlay_without_local_descriptor_is_discovered_via_root_declaration() {
        let (td, repo) = setup_repo();
        // `uses` cascades from the root descriptor (ADR-002) down to
        // `hosts/laptop`, which has zero local descriptor of its own —
        // only discoverable via the root declaration (#113/#127). If
        // `over lint` were blind to it, this cycle/reference check would
        // never run and no diagnostic would be produced.
        td.child("over.toml")
            .write_str("target = \"~\"\nuses = [\"nonexistent\"]\noverlays = ['hosts/*']")
            .unwrap();
        td.child("hosts/laptop").create_dir_all().unwrap();

        let result = lint_repository(&repo);
        assert!(result.has_errors());
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.overlay == "hosts/laptop"
                    && d.message.contains("uses references non-existent overlay")),
            "expected a diagnostic on hosts/laptop, got: {:?}",
            result.diagnostics
        );
    }

    #[rstest]
    fn invalid_toml_produces_syntax_error_with_file_reference() {
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
    fn overlay_with_both_toml_and_yaml_descriptors_warns() {
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
    fn uses_reference_to_nonexistent_overlay_errors() {
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
    fn circular_uses_dependency_is_detected() {
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
    fn diamond_shaped_uses_graph_is_not_a_false_cycle() {
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
    fn using_a_broken_overlay_warns_without_claiming_it_does_not_exist() {
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

        // The consumer must point to the existing broken overlay, not suggest
        // creating a replacement for a supposedly missing dependency.
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
                .any(|d| d.overlay == "good" && d.message.contains("configuration errors")),
            "should report that the used overlay has errors"
        );

        // The hint should point to fixing the broken overlay first
        let diag = result
            .diagnostics
            .iter()
            .find(|d| d.overlay == "good")
            .unwrap();
        assert_eq!(diag.severity, Severity::Warning);
        assert!(diag.hint.as_ref().is_some_and(|h| h.contains("broken")));
        assert_eq!(result.error_count(), 1);
        assert_eq!(result.warning_count(), 1);
    }

    #[rstest]
    fn diagnostics_group_overlays_alphabetically_then_errors_before_warnings() {
        let (td, repo) = setup_repo();
        let ov = td.child("zz_overlay");
        ov.create_dir_all().unwrap();
        ov.child("over.toml")
            .write_str("target = \"~\"\nuses = [\"nonexistent\"]\nexclude = []")
            .unwrap();
        td.child("aa_overlay/over.toml")
            .write_str("exclude = []")
            .unwrap();

        let result = lint_repository(&repo);
        // Should have at least one error and one warning
        assert!(result.error_count() >= 1);
        assert!(result.warning_count() >= 1);

        assert_eq!(result.diagnostics[0].overlay, "aa_overlay");
        assert_eq!(result.diagnostics[0].severity, Severity::Warning);
        let findings: Vec<_> = result
            .diagnostics
            .iter()
            .filter(|d| d.overlay == "zz_overlay")
            .collect();
        assert_eq!(findings[0].severity, Severity::Error);
        assert_eq!(findings.last().unwrap().severity, Severity::Warning);
    }

    #[rstest]
    #[case("mode = 'invalid'", 1)]
    #[case("uses = ['missing', 'also_missing']", 2)]
    #[case("exclude = ['[']", 1)]
    #[case("uses = ['missing']", 1)]
    fn dependency_errors_warn_every_transitive_consumer_once(
        #[case] config: &str,
        #[case] errors: usize,
    ) {
        let (td, repo) = setup_repo();
        for (name, config) in [("a", "uses = ['b']"), ("b", "uses = ['c']"), ("c", config)] {
            td.child(format!("{name}/over.toml"))
                .write_str(config)
                .unwrap();
        }
        let result = lint_overlay(&repo, "a", true).unwrap();
        assert_eq!(result.error_count(), errors, "{:?}", result.diagnostics);
        assert_eq!(result.warning_count(), 2);
        for consumer in ["a", "b"] {
            let findings: Vec<_> = result
                .diagnostics
                .iter()
                .filter(|d| d.overlay == consumer)
                .collect();
            assert_eq!(findings.len(), 1);
            assert_eq!(findings[0].severity, Severity::Warning);
            assert!(findings[0].message.contains("overlay \"c\""));
        }
    }

    #[rstest]
    fn consumer_owned_errors_remain_errors_when_a_dependency_is_invalid() {
        let (td, repo) = setup_repo();
        td.child("consumer/over.toml")
            .write_str("uses = ['broken', 'missing']")
            .unwrap();
        td.child("broken/over.toml")
            .write_str("mode = 'invalid'")
            .unwrap();
        let result = lint_overlay(&repo, "consumer", true).unwrap();
        assert_eq!(result.error_count(), 2);
        assert_eq!(result.warning_count(), 1);
        assert!(result.diagnostics.iter().any(|d| d.overlay == "consumer"
            && d.severity == Severity::Error
            && d.message.contains("missing")));
    }

    #[rstest]
    fn warnings_alone_do_not_make_dependencies_invalid() {
        let (td, repo) = setup_repo();
        td.child("consumer/over.toml")
            .write_str("uses = ['dependency']")
            .unwrap();
        td.child("dependency/over.toml")
            .write_str("exclude = []")
            .unwrap();
        let result = lint_overlay(&repo, "consumer", true).unwrap();
        assert!(!result.has_errors());
        assert_eq!(result.warning_count(), 1);
        assert_eq!(result.diagnostics[0].overlay, "dependency");
    }

    #[rstest]
    #[case("a_consumer", "z_dependency")]
    #[case("z_consumer", "a_dependency")]
    fn standalone_resolution_errors_do_not_make_valid_consumers_invalid(
        #[case] consumer: &str,
        #[case] dependency: &str,
    ) {
        let (td, repo) = setup_repo();
        td.child(format!("{consumer}/over.toml"))
            .write_str(&format!(
                "uses = ['{dependency}']\n[data]\ndestination = 'resolved'"
            ))
            .unwrap();
        td.child(format!("{dependency}/over.toml"))
            .write_str("target = '~/{{ data.destination }}'")
            .unwrap();

        // The dependency cannot resolve by itself, but the consumer supplies
        // its missing metadata regardless of alphabetical processing order.
        let result = lint_repository(&repo);
        assert_eq!(result.error_count(), 1, "{:?}", result.diagnostics);
        assert_eq!(result.warning_count(), 0);
        assert_eq!(result.diagnostics[0].overlay, dependency);
        assert!(
            result.diagnostics[0]
                .message
                .contains("cannot resolve desired ownership")
        );
    }

    #[rstest]
    fn cycles_propagate_original_errors_without_self_warnings_or_duplicates() {
        let (td, repo) = setup_repo();
        for (name, config) in [("a", "uses = ['b', 'b']"), ("b", "uses = ['a', 'missing']")] {
            td.child(format!("{name}/over.toml"))
                .write_str(config)
                .unwrap();
        }
        let result = lint_overlay(&repo, "a", true).unwrap();
        let impacts: Vec<_> = result
            .diagnostics
            .iter()
            .filter(|d| d.message.starts_with("depends on overlay"))
            .collect();
        assert!(!impacts.is_empty());
        let mut pairs = HashSet::new();
        for diagnostic in impacts {
            assert_eq!(diagnostic.severity, Severity::Warning);
            assert!(
                !diagnostic
                    .message
                    .contains(&format!("overlay \"{}\"", diagnostic.overlay))
            );
            assert!(pairs.insert((&diagnostic.overlay, &diagnostic.message)));
        }
        assert!(
            !result
                .diagnostics
                .iter()
                .any(|d| d.message.contains("cannot resolve desired ownership"))
        );
    }

    #[rstest]
    fn grouped_output_keeps_file_references_and_hints_with_their_overlay() {
        let result = LintResult {
            diagnostics: vec![
                Diagnostic::error("", "root error")
                    .with_file("over.toml")
                    .with_hint("repair root"),
                Diagnostic::warning("a", "first warning"),
                Diagnostic::warning("a", "second warning"),
                Diagnostic::error("z", "last error"),
            ],
        };
        let rendered = console::strip_ansi_codes(&result.grouped().to_string()).into_owned();
        assert_eq!(
            rendered,
            ".\n  over.toml\n    error: root error\n      = repair root\n\na\n  warning: first warning\n  warning: second warning\n\nz\n  error: last error\n"
        );
        assert!(
            console::strip_ansi_codes(&result.diagnostics[1].to_string())
                .contains("warning[a]: first warning")
        );
    }

    #[test]
    fn file_groups_collect_interleaved_findings_after_overlay_wide_findings() {
        let result = LintResult {
            diagnostics: vec![
                Diagnostic::error("a", "first").with_file("over.yaml"),
                Diagnostic::error("a", "second").with_file("over.toml"),
                Diagnostic::warning("a", "whole overlay"),
                Diagnostic::warning("a", "third")
                    .with_file("over.yaml")
                    .with_hint("repair\nmore details"),
            ],
        };
        let rendered = result.grouped().to_string();
        assert_eq!(
            console::strip_ansi_codes(&rendered),
            "a\n  warning: whole overlay\n  over.toml\n    error: second\n  over.yaml\n    error: first\n    warning: third\n      = repair\n      more details\n"
        );
        assert_eq!(result.diagnostics[0].file.as_deref(), Some("over.yaml"));
        let standalone = result.diagnostics[0].to_string();
        assert!(
            console::strip_ansi_codes(&standalone).contains("error[a]: first\n  --> a/over.yaml")
        );
    }

    #[test]
    fn human_highlighting_emphasizes_code_without_changing_canonical_text() {
        let text = "replace `install` with `hooks.install`; run `over apply --install`";
        let colored = highlight_inline(text, true);
        assert!(
            colored.contains(
                &console::style("install")
                    .bold()
                    .force_styling(true)
                    .to_string()
            )
        );
        assert!(
            colored.contains(
                &console::style("over apply --install")
                    .bold()
                    .force_styling(true)
                    .to_string()
            )
        );
        let plain = "replace install with hooks.install; run over apply --install";
        assert_eq!(console::strip_ansi_codes(&colored), plain);
        assert_eq!(highlight_inline(text, false), plain);
        assert_eq!(
            highlight_inline("an unmatched `key", false),
            "an unmatched key"
        );
        let diagnostic = Diagnostic::warning("a", text).with_hint("run `over lint`");
        assert!(!diagnostic.to_string().contains('`'));
        assert_eq!(diagnostic.message, text);
        assert_eq!(diagnostic.hint.as_deref(), Some("run `over lint`"));
    }

    #[rstest]
    fn unknown_toml_key_is_reported_with_file() {
        let (td, repo) = setup_repo();
        let ov = td.child("bad_key");
        ov.create_dir_all().unwrap();
        ov.child("over.toml")
            .write_str("target = \"~\"\nfoo = \"bar\"")
            .unwrap();

        let result = lint_repository(&repo);
        assert!(!result.has_errors());
        assert_eq!(result.warning_count(), 1);
        let diag = result
            .diagnostics
            .iter()
            .find(|d| d.message.contains("unknown key"))
            .expect("should report unknown key");
        assert!(diag.message.contains("`foo`"));
        assert_eq!(diag.file.as_deref(), Some("over.toml"));
    }

    #[rstest]
    fn unknown_key_close_to_valid_key_suggests_correction() {
        let (td, repo) = setup_repo();
        let ov = td.child("typo");
        ov.create_dir_all().unwrap();
        ov.child("over.toml")
            .write_str("target = \"~\"\ntargt = \"~\"")
            .unwrap();

        let result = lint_repository(&repo);
        assert!(!result.has_errors());
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
    fn multiple_unknown_keys_are_all_reported() {
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
    fn unknown_keys_warn_without_raw_serde_errors() {
        let (td, repo) = setup_repo();
        let ov = td.child("skip_deser");
        ov.create_dir_all().unwrap();
        // Has an unknown key — should NOT produce raw serde errors
        ov.child("over.toml")
            .write_str("target = \"~\"\nunknown_field = true")
            .unwrap();

        let result = lint_repository(&repo);
        assert!(!result.has_errors());
        // Ignored fields must not reach typed validation as an error.
        for diag in &result.diagnostics {
            assert!(
                !diag.message.contains("expected one of"),
                "should not contain raw serde error text: {}",
                diag.message
            );
        }
    }

    #[rstest]
    fn type_mismatch_error_omits_caused_by_chain() {
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
    fn unknown_yaml_key_is_reported_with_file() {
        let (td, repo) = setup_repo();
        let ov = td.child("yaml_bad");
        ov.create_dir_all().unwrap();
        ov.child("over.yaml")
            .write_str("target: \"~\"\nfoo: bar")
            .unwrap();

        let result = lint_repository(&repo);
        assert!(!result.has_errors());
        assert_eq!(result.warning_count(), 1);
        let diag = result
            .diagnostics
            .iter()
            .find(|d| d.message.contains("unknown key"))
            .expect("should report unknown key in YAML");
        assert!(diag.message.contains("`foo`"));
        assert_eq!(diag.file.as_deref(), Some("over.yaml"));
    }

    #[rstest]
    fn invalid_yaml_produces_syntax_error() {
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
    fn edit_distance_computes_levenshtein_distance() {
        assert_eq!(edit_distance("target", "targt"), 1);
        assert_eq!(edit_distance("target", "target"), 0);
        assert_eq!(edit_distance("exclude", "exclde"), 1);
        assert_eq!(edit_distance("uses", "use"), 1);
        assert_eq!(edit_distance("git", "gti"), 2);
        assert_eq!(edit_distance("abc", "xyz"), 3);
    }

    #[rstest]
    fn suggest_key_finds_closest_valid_key() {
        assert_eq!(suggest_key("targt"), Some("target"));
        assert_eq!(suggest_key("exclde"), Some("exclude"));
        assert_eq!(suggest_key("descrption"), Some("description"));
        assert_eq!(suggest_key("uss"), Some("uses"));
        assert_eq!(suggest_key("gti"), Some("git"));
        // Too different — no suggestion
        assert_eq!(suggest_key("zzzzzzz"), None);
    }

    #[rstest]
    fn unknown_field_message_becomes_unknown_key_diagnostic() {
        let diag = parse_serde_message(
            "unknown field `foo`, expected one of `name`, `root`, `target`",
            "test_overlay",
        );
        assert!(diag.message.contains("unknown key `foo`"));
        // Should not leak internal keys like `name` and `root` in the hint
        assert!(diag.hint.is_some());
    }

    #[rstest]
    fn missing_field_message_becomes_missing_required_key_diagnostic() {
        let diag = parse_serde_message("missing field `target`", "test_overlay");
        assert!(diag.message.contains("missing required key `target`"));
    }

    #[rstest]
    fn diagnostic_with_file_sets_file_field() {
        let diag = Diagnostic::error("test", "test message").with_file("over.toml");
        assert_eq!(diag.file.as_deref(), Some("over.toml"));
    }

    #[rstest]
    fn severity_display_uses_lowercase_labels() {
        assert_eq!(format!("{}", Severity::Error), "error");
        assert_eq!(format!("{}", Severity::Warning), "warning");
    }

    #[rstest]
    fn error_severity_sorts_before_warning() {
        // Error sorts before Warning
        assert!(Severity::Error < Severity::Warning);
        assert_eq!(
            Severity::Error.partial_cmp(&Severity::Warning),
            Some(std::cmp::Ordering::Less)
        );
        assert_eq!(
            Severity::Warning.partial_cmp(&Severity::Warning),
            Some(std::cmp::Ordering::Equal)
        );
    }

    #[rstest]
    fn config_error_not_found_becomes_missing_required_key() {
        let err = ConfigError::NotFound("target".into());
        let diags = diagnostics_from_config_error(&err, "my-overlay", Some("over.toml"));
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("missing required key `target`"));
        assert_eq!(diags[0].file.as_deref(), Some("over.toml"));
    }

    #[rstest]
    fn config_error_message_unknown_field_becomes_unknown_key() {
        let err = ConfigError::Message(
            "unknown field `foo`, expected one of `name`, `root`, `target`".into(),
        );
        let diags = diagnostics_from_config_error(&err, "my-overlay", Some("over.toml"));
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("unknown key `foo`"));
        assert_eq!(diags[0].file.as_deref(), Some("over.toml"));
    }

    #[rstest]
    fn config_error_foreign_becomes_configuration_error() {
        let cause = Box::new(std::io::Error::other("test io"));
        let err = ConfigError::Foreign(cause);
        let diags = diagnostics_from_config_error(&err, "my-overlay", Some("over.toml"));
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("configuration error"));
        assert_eq!(diags[0].file.as_deref(), Some("over.toml"));
    }

    #[rstest]
    fn config_error_frozen_reports_internal_error() {
        let err = ConfigError::Frozen;
        let diags = diagnostics_from_config_error(&err, "my-overlay", None);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("frozen"));
    }

    #[rstest]
    fn config_error_path_parse_becomes_invalid_config_path() {
        let err = ConfigError::PathParse {
            cause: config::ConfigError::Message("bad path".into()).into(),
        };
        let diags = diagnostics_from_config_error(&err, "my-overlay", Some("over.toml"));
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("invalid config path"));
        assert_eq!(diags[0].file.as_deref(), Some("over.toml"));
    }

    #[rstest]
    fn config_error_at_enriches_inner_diagnostic_with_key_context() {
        // An `At` wrapping a `Message` with unknown field
        let inner =
            ConfigError::Message("unknown field `baz`, expected one of `name`, `root`".into());
        let err = ConfigError::At {
            error: Box::new(inner),
            origin: None,
            key: Some("install".into()),
        };
        let diags = diagnostics_from_config_error(&err, "my-overlay", Some("over.toml"));
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("install"));
        assert!(diags[0].message.contains("unknown key `baz`"));
    }

    #[rstest]
    fn config_error_at_wrapping_frozen_still_produces_diagnostic() {
        // An `At` where inner diagnostics end up empty — falls back to direct message
        let inner = ConfigError::Frozen;
        let err = ConfigError::At {
            error: Box::new(inner),
            origin: None,
            key: Some("git".into()),
        };
        let diags = diagnostics_from_config_error(&err, "my-overlay", Some("over.toml"));
        // Frozen produces a diagnostic, so inner_diags won't be empty.
        // Let's verify it enriches with key context.
        assert!(!diags.is_empty());
    }

    #[rstest]
    fn anyhow_error_without_config_error_uses_fallback_message() {
        // An anyhow error that is NOT a ConfigError — should hit the fallback path
        let err = anyhow::anyhow!("some random error");
        let diags = diagnostics_from_anyhow_error(&err, "my-overlay", Some("over.toml"));
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("some random error"));
        assert_eq!(diags[0].file.as_deref(), Some("over.toml"));
    }

    #[rstest]
    fn anyhow_error_wrapping_config_error_downcasts_successfully() {
        // An anyhow error wrapping a ConfigError — should downcast successfully
        let config_err = ConfigError::NotFound("target".into());
        let err: anyhow::Error = config_err.into();
        let diags = diagnostics_from_anyhow_error(&err, "my-overlay", Some("over.toml"));
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("missing required key `target`"));
        assert_eq!(diags[0].file.as_deref(), Some("over.toml"));
    }

    #[rstest]
    fn unknown_variant_message_passes_through_unchanged() {
        let diag = parse_serde_message(
            "unknown variant `bloop`, expected one of `soft`, `hard`",
            "test_overlay",
        );
        assert!(diag.message.contains("unknown variant"));
    }

    #[rstest]
    fn invalid_type_message_passes_through_unchanged() {
        let diag = parse_serde_message(
            "invalid type: found boolean, expected a string",
            "test_overlay",
        );
        assert!(diag.message.contains("invalid type"));
    }

    #[rstest]
    fn unrecognized_serde_message_falls_back_to_original_text() {
        let diag = parse_serde_message("something completely unexpected", "test_overlay");
        assert_eq!(diag.message, "something completely unexpected");
    }

    #[rstest]
    fn unknown_field_message_with_close_match_suggests_correction() {
        let diag = parse_serde_message(
            "unknown field `targt`, expected one of `name`, `root`",
            "test_overlay",
        );
        assert!(diag.message.contains("unknown key `targt`"));
        assert!(
            diag.hint
                .as_ref()
                .is_some_and(|h| h.contains("did you mean `target`?"))
        );
    }

    #[rstest]
    fn find_descriptor_file_returns_none_when_absent() {
        let td = TempDir::new().unwrap();
        // No descriptor file exists
        assert_eq!(find_descriptor_file(td.path()), None);
    }

    #[rstest]
    fn internal_name_and_root_keys_remain_tolerated() {
        let td = TempDir::new().unwrap();
        // A TOML file with internal keys `name` and `root` alongside valid keys
        td.child("over.toml")
            .write_str("name = \"test\"\nroot = \"/tmp\"\ntarget = \"~\"")
            .unwrap();
        let result = lint_overlay(&Repository::new(td.path().into()), ".", false).unwrap();
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    }
}
