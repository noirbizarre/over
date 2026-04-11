use clap::Args;
use console::style;

use crate::cli::CLI;
use crate::lint::{Severity, lint_repository};
use crate::overlays::Repository;
use anyhow::Result;

#[derive(Args, Debug)]
pub struct Params {}

pub async fn execute(cli: &CLI, _args: &Params) -> Result<()> {
    let home = cli.resolve_home()?;
    let repo = Repository::new(home);
    let result = lint_repository(&repo);

    if result.diagnostics.is_empty() {
        println!("{}", style("No issues found").green().bold());
        return Ok(());
    }

    for diag in &result.diagnostics {
        let severity_label = match diag.severity {
            Severity::Error => style("error").red().bold(),
            Severity::Warning => style("warning").yellow().bold(),
        };
        let overlay_label = style(format!("[{}]", diag.overlay)).cyan();

        println!("{severity_label}{overlay_label}: {}", diag.message);

        if let Some(file) = &diag.file {
            let location = format!("{}/{file}", diag.overlay);
            println!("  {} {}", style("-->").dim(), style(location).dim());
        }

        if let Some(hint) = &diag.hint {
            println!("  {} {hint}", style("=").dim());
        }

        println!();
    }

    // Summary
    let errors = result.error_count();
    let warnings = result.warning_count();
    let mut parts = Vec::new();
    if errors > 0 {
        parts.push(format!(
            "{}",
            style(format!(
                "{} error{}",
                errors,
                if errors == 1 { "" } else { "s" }
            ))
            .red()
            .bold()
        ));
    }
    if warnings > 0 {
        parts.push(format!(
            "{}",
            style(format!(
                "{} warning{}",
                warnings,
                if warnings == 1 { "" } else { "s" }
            ))
            .yellow()
            .bold()
        ));
    }
    println!("{} found", parts.join(", "));

    if result.has_errors() {
        anyhow::bail!(
            "lint found {} error{}",
            errors,
            if errors == 1 { "" } else { "s" },
        );
    }

    Ok(())
}
