//! Global health aggregation (#248): lint owns static diagnostics and status
//! owns runtime inspection. Doctor summarizes their findings without a second
//! interpretation of deployment, content or Git configuration health.
//!
//! Environment diagnostics deliberately write/remove a probe and may create
//! the XDG state directory. State inspection itself creates no locks or files.
//! The explicit repair path, [`repair`]: `over doctor --fix`
//! repairs a malformed `.git/info/exclude` block (#149's literal ask) via
//! [`crate::git_exclude::repair_existing`] — changing only `over`'s corrupted marker lines, never
//! user-owned git configuration or content (see ADR-024).
//!
//! Summary rows are distinct from unique findings, so a shared dependency can
//! affect several compositions without inflating the warning/error totals.

pub mod checks;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::desired::DesiredTree;
use crate::exec::Context;
use crate::git_exclude;
use crate::lint::{self, LintResult};
use crate::overlays::{Overlay, Repository};
use crate::status::configuration::Severity as RuntimeSeverity;
use crate::status::overlay::{InspectionCache, OverlayReport};

/// A finding's severity — distinct from [`crate::lint::Severity`] because
/// doctor also reports *healthy* checks (shown only with `--verbose`,
/// mirroring every other `over` subcommand's `needs_attention()`
/// convention).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Ok,
    Warning,
    Error,
}

impl From<lint::Severity> for Severity {
    fn from(severity: lint::Severity) -> Self {
        match severity {
            lint::Severity::Error => Severity::Error,
            lint::Severity::Warning => Severity::Warning,
        }
    }
}

/// One doctor finding — see the module doc for why `line` is a fully
/// rendered string rather than structured fields.
#[derive(Debug, Clone)]
pub struct Finding {
    pub severity: Severity,
    pub line: String,
}

impl Finding {
    pub fn ok(line: impl Into<String>) -> Self {
        Self {
            severity: Severity::Ok,
            line: line.into(),
        }
    }

    pub fn warning(line: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            line: line.into(),
        }
    }

    pub fn error(line: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            line: line.into(),
        }
    }

    /// Whether this finding should be shown without `--verbose` — mirrors
    /// `status::Status::needs_attention`/`ExcludeDiagnosis::needs_attention`.
    pub fn needs_attention(&self) -> bool {
        !matches!(self.severity, Severity::Ok)
    }
}

impl std::fmt::Display for Finding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", crate::utils::short_paths(&self.line))
    }
}

/// Aggregated doctor findings, grouped by section for display, plus any
/// repairs `--fix` actually performed.
#[derive(Debug, Default)]
pub struct Report {
    pub environment: Vec<Finding>,
    pub config: LintResult,
    pub overlay_health: Vec<OverlayHealth>,
    /// Summary rows reference these unique issues; they are not extra findings.
    pub runtime_issues: BTreeMap<String, Severity>,
    pub xdg_state: Vec<Finding>,
    /// Human-readable description of each repair `--fix` performed —
    /// always shown, regardless of `--verbose` (it's an action taken, not
    /// a static state).
    pub fixed: Vec<String>,
}

/// Counts remain unstyled so CLI rendering can apply semantic identity/severity roles.
#[derive(Debug, Clone)]
pub struct OverlayHealth {
    pub name: String,
    pub errors: usize,
    pub warnings: usize,
}

impl OverlayHealth {
    pub fn needs_attention(&self) -> bool {
        self.errors + self.warnings > 0
    }
}

impl Report {
    fn sections(&self) -> [&Vec<Finding>; 2] {
        [&self.environment, &self.xdg_state]
    }

    pub fn error_count(&self) -> usize {
        self.sections()
            .into_iter()
            .flatten()
            .filter(|f| matches!(f.severity, Severity::Error))
            .count()
            + self.config.error_count()
            + self
                .runtime_issues
                .values()
                .filter(|severity| **severity == Severity::Error)
                .count()
    }

    pub fn warning_count(&self) -> usize {
        self.sections()
            .into_iter()
            .flatten()
            .filter(|f| matches!(f.severity, Severity::Warning))
            .count()
            + self.config.warning_count()
            + self
                .runtime_issues
                .values()
                .filter(|severity| **severity == Severity::Warning)
                .count()
    }

    pub fn has_errors(&self) -> bool {
        self.error_count() > 0
    }
}

/// What to check and, optionally, repair.
pub struct Options {
    pub root: PathBuf,
    /// `None` means every overlay in the repository.
    pub overlay: Option<String>,
    pub no_uses: bool,
    /// Repair malformed `.git/info/exclude` blocks before diagnosing them
    /// (#149) — the only repair `over doctor` ever performs.
    pub fix: bool,
}

/// Run every check and return the aggregated [`Report`].
pub async fn check(repo: &Repository, opts: &Options) -> Result<Report> {
    // Later desired-tree and state loads must not log diagnostics already reported by lint.
    let repo = repo.collecting_warnings();
    let repo = &repo;
    let mut report = Report {
        environment: checks::environment_findings(),
        ..Default::default()
    };

    // Encryption findings inspect stored Git blobs, never infer exposure from targets.
    if let Ok(git) = git2::Repository::discover(&repo.root) {
        match crate::crypt::policy::inspect(&git) {
            Ok(findings) => report
                .environment
                .extend(findings.into_iter().map(|finding| match finding {
                    crate::crypt::policy::Diagnostic::Error(message) => Finding::error(message),
                    crate::crypt::policy::Diagnostic::Warning(message) => Finding::warning(message),
                    crate::crypt::policy::Diagnostic::Healthy(message) => Finding::ok(message),
                })),
            Err(error) => report.environment.push(Finding::error(format!(
                "crypt inspection failed: {error:#}"
            ))),
        }
    }

    // Overlay configuration — delegates entirely to `lint`'s own tolerant,
    // pre-deserialization pass (ADR-009); doctor never re-derives it.
    let lint_context = Context::builder()
        .repository(repo.clone())
        .root(opts.root.clone())
        .build();
    report.config = lint::lint_repository_with_context(&lint_context);

    // Repair before content inspection so status's cached snapshot is collected only once.
    let overlays = resolve_overlays(repo, opts.overlay.as_deref())?;
    if opts.fix {
        let mut trees = Vec::new();
        for overlay in &overlays {
            // Lint/status below report invalid compositions; valid ones can still repair.
            if let Ok(Some(desired)) = build_desired_tree(repo, &opts.root, overlay, opts.no_uses) {
                trees.push(desired);
            }
        }
        let repair_report = git_exclude::repair_existing_batch(&trees.iter().collect::<Vec<_>>())?;
        report
            .fixed
            .extend(repair_report.repaired.iter().map(|(path, overlay)| {
                format!(
                    "repaired malformed exclude block in '{}' (overlay '{overlay}')",
                    path.display()
                )
            }));
    }
    let mut summarized = BTreeSet::new();
    let mut cache = InspectionCache::default();
    for overlay in &overlays {
        let dependencies = crate::status::overlay::dependency_names(repo, overlay);
        let lint_explains_failure = report.config.diagnostics.iter().any(|diagnostic| {
            diagnostic.severity == lint::Severity::Error
                && dependencies.contains(&diagnostic.overlay)
        });
        let snapshot =
            OverlayReport::inspect_cached(repo, &opts.root, overlay, opts.no_uses, &mut cache);
        let mut owners = BTreeSet::from([overlay.name.clone()]);
        let runtime = match snapshot {
            Ok(snapshot) => {
                if !opts.no_uses {
                    owners.extend(snapshot.dependencies.iter().map(|item| item.name.clone()));
                }
                snapshot.health.map(|health| health.health_issues())
            }
            Err(error) => Err(error),
        };
        let static_findings: Vec<_> = report
            .config
            .diagnostics
            .iter()
            .filter(|diagnostic| owners.contains(&diagnostic.overlay))
            .collect();
        let mut errors = static_findings
            .iter()
            .filter(|item| item.severity == lint::Severity::Error)
            .count();
        let mut warnings = static_findings.len() - errors;
        match runtime {
            Ok(issues) => {
                for (key, severity) in issues {
                    let severity = match severity {
                        RuntimeSeverity::Warning => {
                            warnings += 1;
                            Severity::Warning
                        }
                        RuntimeSeverity::Error => {
                            errors += 1;
                            Severity::Error
                        }
                    };
                    report.runtime_issues.insert(key, severity);
                }
            }
            Err(_) if lint_explains_failure => {} // Preserve lint's original cause and consumer warnings.
            Err(_) => {
                errors += 1;
                report
                    .runtime_issues
                    .insert(format!("inspection:{}", overlay.name), Severity::Error);
            }
        }
        report.overlay_health.push(OverlayHealth {
            name: overlay.name.clone(),
            errors,
            warnings,
        });
        summarized.insert(overlay.name.clone());
    }
    // Tolerant discovery omits invalid descriptors; lint still supplies their identity.
    for diagnostic in &report.config.diagnostics {
        if summarized.insert(diagnostic.overlay.clone()) {
            let findings: Vec<_> = report
                .config
                .diagnostics
                .iter()
                .filter(|item| item.overlay == diagnostic.overlay)
                .collect();
            let errors = findings
                .iter()
                .filter(|item| item.severity == lint::Severity::Error)
                .count();
            report.overlay_health.push(OverlayHealth {
                name: diagnostic.overlay.clone(),
                errors,
                warnings: findings.len() - errors,
            });
        }
    }

    // Stale XDG state bookkeeping.
    report
        .xdg_state
        .extend(checks::xdg_state_findings(repo).await?);

    Ok(report)
}

/// Resolve the overlay(s) to inspect runtime health for — a
/// specific one, or every overlay in the repository.
fn resolve_overlays(repo: &Repository, name: Option<&str>) -> Result<Vec<Overlay>> {
    match name {
        Some(name) => Ok(vec![repo.get(name)?]),
        None => repo.overlays(),
    }
}

/// Build `overlay`'s [`crate::desired::DesiredTree`] against `root`,
/// exactly like `cli::status::print_overlay_status` does. `None` means
/// `overlay.target` failed to resolve (e.g. a broken template) — already
/// surfaced by `lint`'s own config diagnostics, so doctor's exclude pass
/// simply skips it rather than duplicating the error.
fn build_desired_tree(
    repo: &Repository,
    root: &Path,
    overlay: &Overlay,
    no_uses: bool,
) -> Result<Option<DesiredTree>> {
    let ctx = Context::builder()
        .root(root.to_path_buf())
        .repository(repo.clone())
        .overlay(overlay.clone())
        .no_uses(no_uses)
        .build();
    let ctx = ctx.resolve_data()?;
    let Ok(target) = overlay.resolve_target(&ctx) else {
        return Ok(None);
    };
    let ctx = ctx.with_resolved_overlay(overlay.name.clone(), target.to_string_lossy().to_string());
    Ok(Some(DesiredTree::build(&ctx, overlay)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(line: &str) -> Finding {
        Finding::ok(line)
    }
    fn warn(line: &str) -> Finding {
        Finding::warning(line)
    }
    fn err(line: &str) -> Finding {
        Finding::error(line)
    }

    #[test]
    fn finding_needs_attention_is_false_only_for_ok() {
        assert!(!ok("fine").needs_attention());
        assert!(warn("hmm").needs_attention());
        assert!(err("boom").needs_attention());
    }

    #[test]
    fn report_counts_errors_and_warnings_across_every_section() {
        let report = Report {
            environment: vec![ok("git found"), err("state dir not writable")],
            config: LintResult {
                diagnostics: vec![lint::Diagnostic::warning("demo", "redundant exclude")],
            },
            overlay_health: vec![OverlayHealth {
                name: "demo".into(),
                errors: 0,
                warnings: 2,
            }],
            runtime_issues: BTreeMap::from([
                ("one".into(), Severity::Warning),
                ("two".into(), Severity::Warning),
            ]),
            xdg_state: vec![err("corrupt state file")],
            fixed: vec![],
        };
        assert_eq!(report.error_count(), 2);
        assert_eq!(report.warning_count(), 3);
        assert!(report.has_errors());
    }

    #[test]
    fn report_without_errors_has_errors_is_false() {
        let report = Report {
            environment: vec![ok("git found")],
            config: LintResult {
                diagnostics: vec![lint::Diagnostic::warning("demo", "redundant exclude")],
            },
            ..Default::default()
        };
        assert!(!report.has_errors());
    }

    #[test]
    fn lint_severity_converts_to_doctor_severity() {
        assert_eq!(Severity::from(lint::Severity::Error), Severity::Error);
        assert_eq!(Severity::from(lint::Severity::Warning), Severity::Warning);
    }
}
