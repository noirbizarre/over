use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::Args;
use serde::Serialize;

use crate::cli::CLI;
use crate::exec::Context;
use crate::lint::{
    Diagnostic, LintResult, lint_overlay_with_context, lint_repository_with_context,
};
use crate::overlays::Repository;
use crate::ui;
use crate::ui::style;

#[derive(Args, Debug)]
pub struct Params {
    /// Set the target directory (defaults to ~)
    #[arg(
        short = 'r',
        long = "target",
        value_name = "TARGET",
        env = "OVER_TARGET"
    )]
    pub root: Option<PathBuf>,

    /// Check only this overlay and its uses dependencies
    #[arg(value_name = "OVERLAY")]
    pub overlay: Option<String>,

    /// Skip linting the selected overlay's uses dependencies
    #[arg(long)]
    pub no_uses: bool,

    /// Output diagnostics as JSON
    #[arg(long)]
    pub json: bool,
}

pub async fn execute(cli: &CLI, args: &Params) -> Result<()> {
    let home = cli.resolve_home()?;
    let repo = Repository::new(home);
    let root = args
        .root
        .clone()
        .or_else(dirs::home_dir)
        .ok_or_else(|| anyhow::anyhow!("could not determine home directory"))?;
    let ctx = Context::builder().repository(repo).root(root).build();
    let result = match &args.overlay {
        Some(name) => lint_overlay_with_context(&ctx, name, !args.no_uses)?,
        None => lint_repository_with_context(&ctx),
    };

    if args.json {
        ui::json::print_json(&JsonReport {
            diagnostics: &result.diagnostics,
            error_count: result.error_count(),
            warning_count: result.warning_count(),
        })?;
    } else {
        print_human(&result);
    }

    if result.has_errors() {
        let errors = result.error_count();
        bail!(
            "lint found {} error{}",
            errors,
            if errors == 1 { "" } else { "s" },
        );
    }

    Ok(())
}

fn print_human(result: &LintResult) {
    if result.diagnostics.is_empty() {
        ui::info(format!("{}", style::green("No issues found"))).ok();
        return;
    }

    ui::info(format!("{}", result.grouped())).ok();

    // Summary — matches `cli::doctor`'s `print_summary` (both render the
    // same `lint::Diagnostic`-derived counts; kept identical so the two
    // commands can't drift into two different formats again).
    let errors = result.error_count();
    let warnings = result.warning_count();
    let mut parts = Vec::new();
    if errors > 0 {
        parts.push(format!(
            "{}",
            style::red(format!(
                "{errors} error{}",
                if errors == 1 { "" } else { "s" }
            ))
        ));
    }
    if warnings > 0 {
        parts.push(format!(
            "{}",
            style::yellow(format!(
                "{warnings} warning{}",
                if warnings == 1 { "" } else { "s" }
            ))
        ));
    }
    ui::info(format!("{} found", parts.join(", "))).ok();
}

#[derive(Serialize)]
struct JsonReport<'a> {
    diagnostics: &'a [Diagnostic],
    error_count: usize,
    warning_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_report_retains_canonical_messages_and_nullable_fields() {
        let result = LintResult {
            diagnostics: vec![Diagnostic::warning(
                "desktop",
                "unknown key `targt` ignored",
            )],
        };
        let value = serde_json::to_value(JsonReport {
            diagnostics: &result.diagnostics,
            error_count: result.error_count(),
            warning_count: result.warning_count(),
        })
        .unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "diagnostics": [{
                    "severity": "warning", "overlay": "desktop",
                    "message": "unknown key `targt` ignored", "hint": null, "file": null
                }],
                "error_count": 0, "warning_count": 1
            })
        );
    }
}
