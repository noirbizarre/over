use anyhow::Result;
use clap::Args;
use console::style;

use crate::cli::CLI;
use crate::lint::lint_repository;
use crate::overlays::Repository;
use crate::ui;

#[derive(Args, Debug)]
pub struct Params {}

pub async fn execute(cli: &CLI, _args: &Params) -> Result<()> {
    let home = cli.resolve_home()?;
    let repo = Repository::new(home);
    let result = lint_repository(&repo);

    if result.diagnostics.is_empty() {
        ui::info(format!("{}", style("No issues found").green().bold())).ok();
        return Ok(());
    }

    for diag in &result.diagnostics {
        ui::info(format!("{diag}")).ok();
        ui::info("").ok();
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
    ui::info(format!("{} found", parts.join(", "))).ok();

    if result.has_errors() {
        anyhow::bail!(
            "lint found {} error{}",
            errors,
            if errors == 1 { "" } else { "s" },
        );
    }

    Ok(())
}
