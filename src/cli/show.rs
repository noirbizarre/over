use anyhow::Result;
use clap::Args;

use crate::cli::CLI;
use crate::overlays::Repository;
use crate::ui;
use crate::ui::style;
use crate::utils::short_path;

#[derive(Args, Debug)]
pub struct Params {
    #[clap(help = "Name of the overlay to display")]
    name: String,
}

pub async fn execute(cli: &CLI, args: &Params) -> Result<()> {
    if cli.debug {
        tracing::debug!(?cli, ?args, "show command");
    }

    let home = cli.resolve_home()?;
    let repo = Repository::new(home);
    let overlay = repo.get(&args.name)?;

    ui::info(format!("{}", style::white_b(&overlay.name))).ok();
    ui::info(format!(
        "  root:   {}",
        short_path(&overlay.root.to_string_lossy())
    ))
    .ok();
    ui::info(format!("  target: {}", overlay.target)).ok();
    if let Some(desc) = &overlay.description {
        ui::info(format!("  desc:   {}", desc)).ok();
    }
    if let Some(uses) = &overlay.uses {
        ui::info(format!("  uses:   {}", uses.join(", "))).ok();
    }
    if let Some(link_dirs) = &overlay.link_dirs {
        ui::info(format!("  link_dirs: {}", link_dirs.join(", "))).ok();
    }
    if let Some(defaults) = &overlay.defaults {
        ui::info(format!(
            "  defaults: materialization = {}",
            defaults.materialization
        ))
        .ok();
    }
    if let Some(rules) = &overlay.rules {
        ui::info("  rules:").ok();
        for rule in rules {
            ui::info(format!("    {}: {}", rule.path, rule.materialization)).ok();
        }
    }
    if let Some(git) = &overlay.git {
        ui::info("  git repositories:").ok();
        for (path, cfg) in git {
            ui::info(format!("    {}: {}", path, cfg.url)).ok();
        }
    }
    if overlay.install.is_some() {
        ui::info("  install: configured").ok();
    }
    Ok(())
}
