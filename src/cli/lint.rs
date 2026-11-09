use anyhow::{Result, bail};
use clap::Args;

use crate::cli::CLI;
use crate::lint::{lint_overlay, lint_repository};
use crate::overlays::Repository;
use crate::ui;
use crate::ui::style;

#[derive(Args, Debug)]
pub struct Params {
    /// Check only this overlay and its uses dependencies
    #[arg(value_name = "OVERLAY")]
    pub overlay: Option<String>,

    /// Skip linting the selected overlay's uses dependencies
    #[arg(long)]
    pub no_uses: bool,
}

pub async fn execute(cli: &CLI, args: &Params) -> Result<()> {
    let home = cli.resolve_home()?;
    let repo = Repository::new(home);
    let result = match &args.overlay {
        Some(name) => lint_overlay(&repo, name, !args.no_uses)?,
        None => lint_repository(&repo),
    };

    if result.diagnostics.is_empty() {
        ui::info(format!("{}", style::green("No issues found"))).ok();
        return Ok(());
    }

    for diag in &result.diagnostics {
        ui::info(format!("{diag}")).ok();
        ui::info("").ok();
    }

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

    if result.has_errors() {
        bail!(
            "lint found {} error{}",
            errors,
            if errors == 1 { "" } else { "s" },
        );
    }

    Ok(())
}
