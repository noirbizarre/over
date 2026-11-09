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
    if !overlay.map.is_empty() {
        ui::info("  map:").ok();
        for (source, mapping) in &overlay.map {
            ui::info(format!("    {source}: {}", mapping.target)).ok();
        }
    }
    if let Some(desc) = &overlay.description {
        ui::info(format!("  desc:   {}", desc)).ok();
    }
    if let Some(uses) = &overlay.uses {
        ui::info(format!("  uses:   {}", uses.join(", "))).ok();
    }
    for scope in &overlay.scopes {
        if scope.mode.is_none() && scope.permissions.is_none() && scope.rules.is_empty() {
            continue;
        }
        ui::info(format!("  policy ({}):", scope.descriptor.display())).ok();
        if let Some(mode) = scope.mode {
            ui::info(format!("    mode: {mode}")).ok();
        }
        if let Some(permissions) = scope.permissions {
            ui::info(format!("    permissions: {permissions}")).ok();
        }
        if !scope.rules.is_empty() {
            ui::info("    rules:").ok();
        }
        for rule in &scope.rules {
            let mut properties = Vec::new();
            if let Some(mode) = rule.mode {
                properties.push(format!("mode: {mode}"));
            }
            if let Some(permissions) = rule.permissions {
                properties.push(format!("permissions: {permissions}"));
            }
            ui::info(format!("    {}: {}", rule.path, properties.join(", "))).ok();
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
