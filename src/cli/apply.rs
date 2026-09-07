use std::path::PathBuf;

use anyhow::{Result, anyhow};
use clap::Args;
use dirs::home_dir;

use crate::actions;
use crate::cli::CLI;
use crate::cli::common::select_overlay;
use crate::exec::Context;
use crate::overlays::Repository;
use crate::ui;
use crate::ui::{emojis, style};
#[derive(Args, Debug)]
pub struct Params {
    #[clap(help = "Name of the overlay to apply (uses default_overlay if configured)")]
    name: Option<String>,

    #[clap(short, long, help = "The target root directory (~)")]
    root: Option<PathBuf>,

    #[clap(long, short = 'n', help = "Run without applying changes")]
    dry_run: bool,

    #[clap(long, short, help = "Overwrite without prompting")]
    force: bool,

    #[clap(long, help = "Fail on conflict instead of prompting")]
    no_prompt: bool,

    #[clap(long, help = "Do not process uses")]
    no_uses: bool,

    #[clap(long, short, help = "Install associated applications")]
    install: bool,
}

pub async fn execute(cli: &CLI, args: &Params) -> Result<()> {
    if cli.debug {
        tracing::debug!(?cli, "CLI args");
    }

    let home = cli.resolve_home()?;
    let repo = Repository::new(home);
    if cli.debug {
        tracing::debug!(?repo, "repository");
    }

    let root = args
        .root
        .clone()
        .or_else(home_dir)
        .ok_or_else(|| anyhow!("could not determine home directory"))?;

    let ctx = Context::builder()
        .dry_run(args.dry_run)
        .debug(cli.debug)
        .verbose(cli.verbose)
        .force(args.force)
        .no_prompt(args.no_prompt)
        .no_uses(args.no_uses)
        .root(root)
        .repository(repo)
        .build();

    let overlay = select_overlay(
        &ctx.repository,
        &ctx,
        args.name.as_deref(),
        "Choose the overlay to apply",
    )?;
    if cli.debug {
        tracing::debug!(?overlay, "resolved overlay");
    }

    let ctx = ctx.with_overlay(overlay.clone());

    if args.install {
        actions::install::install(&ctx, &overlay).await?;
    }

    overlay.apply(&ctx).await.inspect_err(|e| {
        let _ = ui::warn(format!(
            "{} {} {}: {}",
            emojis::CROSSMARK,
            style::white_b("Failed to apply overlay"),
            style::cyan(&overlay.name),
            e,
        ));
    })?;

    Ok(())
}
