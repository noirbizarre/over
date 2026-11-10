use std::path::PathBuf;

use anyhow::{Result, anyhow};
use clap::Args;
use dirs::home_dir;

use crate::cli::CLI;
use crate::cli::common::select_overlay;
use crate::exec::Context;
use crate::overlays::Repository;
use crate::ui;
use crate::ui::{emojis, style};
#[derive(Args, Debug)]
pub struct Params {
    #[clap(
        help = "Name of the overlay to apply (otherwise infer context, then use default_overlay)"
    )]
    name: Option<String>,

    #[clap(
        short = 'r',
        long = "target",
        value_name = "TARGET",
        env = "OVER_TARGET",
        help = "Set the target directory (defaults to ~)"
    )]
    root: Option<PathBuf>,

    #[clap(long, short = 'n', help = "Run without applying changes")]
    dry_run: bool,

    #[clap(
        long,
        short,
        help = "Overwrite ordinary filesystem conflicts without prompting"
    )]
    force: bool,

    #[clap(
        long,
        help = "Discard owned real checkouts, including local files and Git history"
    )]
    discard_checkout: bool,

    #[clap(long, help = "Fail on conflict instead of prompting")]
    no_prompt: bool,

    #[clap(long, help = "Skip dependency operations, retaining dependency data")]
    no_uses: bool,

    #[clap(long, short, help = "Run install hooks after applying files")]
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
        .discard_checkout(args.discard_checkout)
        .no_prompt(args.no_prompt)
        .no_uses(args.no_uses)
        .install(args.install)
        .root(root)
        .repository(repo)
        .build();

    let selection_ctx = ctx.clone();
    let name = args.name.clone();
    let overlay =
        tokio::task::spawn_blocking(move || select_overlay(&selection_ctx, name.as_deref()))
            .await??;
    if cli.debug {
        tracing::debug!(?overlay, "resolved overlay");
    }

    let ctx = ctx.with_overlay(overlay.clone()).resolve_data()?;

    overlay.apply(&ctx).await.inspect_err(|e| {
        ui::warn(format!(
            "{} {} {}: {}",
            emojis::CROSSMARK,
            style::white_b("Failed to apply overlay"),
            style::cyan(&overlay.name),
            e,
        ))
        .ok();
    })?;

    Ok(())
}
