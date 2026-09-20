//! `over doctor` (#149, part of #142): a health-check aggregator.
//!
//! Rather than inventing a new diagnostic taxonomy, this module aggregates
//! findings that are each already computed by an existing, focused pass —
//! [`crate::lint::lint_repository`] for overlay configuration issues,
//! [`crate::git_exclude::diagnose`] for `.git/info/exclude` drift — and
//! adds two new detection-only passes of its own: environment/plumbing
//! sanity ([`checks::environment_findings`]) and stale XDG state bookkeeping
//! ([`checks::xdg_state_findings`]).
//!
//! The one exception to "detection-only" is [`repair`]: `over doctor --fix`
//! repairs a malformed `.git/info/exclude` block (#149's literal ask) via
//! [`crate::git_exclude::repair`] — the *only* thing this command ever
//! writes, and only ever `over`'s own corrupted marker lines, never
//! user-owned git configuration or content (see ADR-024).
//!
//! Each source already has its own well-established human-facing shape
//! ([`crate::lint::Diagnostic`]'s `Display`, [`crate::git_exclude::ExcludeDiagnosis`]'s
//! `Display`) — [`Finding`] stores the fully rendered line rather than
//! reinventing a unified structured format; doctor's own job is only to
//! aggregate, gate by severity/verbosity, and count for the exit code.

pub mod checks;

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::desired::DesiredTree;
use crate::exec::Context;
use crate::git_exclude::{self, ExcludeDiagnosis, ExcludeStatus};
use crate::lint::{self, Diagnostic as LintDiagnostic};
use crate::overlays::{Overlay, Repository};

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
        write!(f, "{}", self.line)
    }
}

impl From<&LintDiagnostic> for Finding {
    fn from(diag: &LintDiagnostic) -> Self {
        Self {
            severity: diag.severity.into(),
            line: format!("{diag}"),
        }
    }
}

impl From<&ExcludeDiagnosis> for Finding {
    fn from(diagnosis: &ExcludeDiagnosis) -> Self {
        // `Missing`/`Modified`/`Orphaned` self-heal on the next `over
        // apply` (`reconcile` recomputes the block from scratch every
        // time) — a `Warning` is enough. `Malformed` never self-heals:
        // `reconcile`/`diagnose` both leave it untouched forever, so it
        // stays stuck until an explicit `over doctor --fix` — that's worth
        // an `Error`, the one case where plain `over doctor` (no `--fix`)
        // exits non-zero because of exclude drift.
        let severity = match diagnosis.status {
            ExcludeStatus::Ok => {
                if diagnosis.tracked_conflicts.is_empty() {
                    Severity::Ok
                } else {
                    Severity::Warning
                }
            }
            ExcludeStatus::Missing | ExcludeStatus::Modified | ExcludeStatus::Orphaned => {
                Severity::Warning
            }
            ExcludeStatus::Malformed => Severity::Error,
        };
        Self {
            severity,
            line: format!("{diagnosis}"),
        }
    }
}

/// Aggregated doctor findings, grouped by section for display, plus any
/// repairs `--fix` actually performed.
#[derive(Debug, Default)]
pub struct Report {
    pub environment: Vec<Finding>,
    pub config: Vec<Finding>,
    pub git_exclude: Vec<Finding>,
    pub xdg_state: Vec<Finding>,
    /// Human-readable description of each repair `--fix` performed —
    /// always shown, regardless of `--verbose` (it's an action taken, not
    /// a static state).
    pub fixed: Vec<String>,
}

impl Report {
    fn sections(&self) -> [&Vec<Finding>; 4] {
        [
            &self.environment,
            &self.config,
            &self.git_exclude,
            &self.xdg_state,
        ]
    }

    pub fn error_count(&self) -> usize {
        self.sections()
            .into_iter()
            .flatten()
            .filter(|f| matches!(f.severity, Severity::Error))
            .count()
    }

    pub fn warning_count(&self) -> usize {
        self.sections()
            .into_iter()
            .flatten()
            .filter(|f| matches!(f.severity, Severity::Warning))
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
    let mut report = Report {
        environment: checks::environment_findings(),
        ..Default::default()
    };

    // Overlay configuration — delegates entirely to `lint`'s own tolerant,
    // pre-deserialization pass (ADR-009); doctor never re-derives it.
    let lint_result = lint::lint_repository(repo);
    report.config = lint_result.diagnostics.iter().map(Finding::from).collect();

    // Per-overlay `.git/info/exclude` diagnostics (#148), optionally
    // preceded by a repair pass for malformed blocks only (#149).
    let overlays = resolve_overlays(repo, opts.overlay.as_deref())?;
    for overlay in &overlays {
        let Some(desired) = build_desired_tree(repo, &opts.root, overlay, opts.no_uses)? else {
            continue;
        };
        let targets = git_exclude::managed_targets(&desired);
        if targets.is_empty() {
            continue;
        }

        if opts.fix {
            let repair_report = git_exclude::repair(&targets)?;
            report
                .fixed
                .extend(repair_report.repaired.iter().map(|(path, overlay)| {
                    format!(
                        "repaired malformed exclude block in '{}' (overlay '{overlay}')",
                        path.display()
                    )
                }));
        }

        let diagnoses = git_exclude::diagnose(&targets)?;
        report
            .git_exclude
            .extend(diagnoses.iter().map(Finding::from));
    }

    // Stale XDG state bookkeeping.
    report.xdg_state = checks::xdg_state_findings(repo).await?;

    Ok(report)
}

/// Resolve the overlay(s) to check `.git/info/exclude` drift for — a
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
            config: vec![warn("redundant exclude")],
            git_exclude: vec![warn("exclude modified"), warn("exclude missing")],
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
            config: vec![warn("redundant exclude")],
            ..Default::default()
        };
        assert!(!report.has_errors());
    }

    #[test]
    fn lint_severity_converts_to_doctor_severity() {
        assert_eq!(Severity::from(lint::Severity::Error), Severity::Error);
        assert_eq!(Severity::from(lint::Severity::Warning), Severity::Warning);
    }

    #[test]
    fn lint_diagnostic_converts_to_finding_preserving_rendered_display() {
        let diag = LintDiagnostic::warning("myoverlay", "redundant empty exclude");
        let finding: Finding = (&diag).into();
        assert_eq!(finding.severity, Severity::Warning);
        assert_eq!(finding.line, format!("{diag}"));
    }
}
