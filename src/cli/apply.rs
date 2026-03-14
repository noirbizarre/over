use std::path::PathBuf;

use anyhow::Result;
use clap::Args;
use dirs::home_dir;

use crate::actions;
use crate::cli::CLI;
use crate::exec::Context;
use crate::overlays::Repository;
use crate::ui::{emojis, style};
#[derive(Args, Debug)]
pub struct Params {
    #[clap(help = "Name of the overlay to apply")]
    name: String,

    #[clap(short, long, help = "The target root directory (~)")]
    root: Option<PathBuf>,

    #[clap(long, short = 'n', help = "Run without applying changes")]
    dry_run: bool,

    #[clap(long, short, help = "Overwrite without prompting")]
    force: bool,

    #[clap(long, help = "Do not process uses")]
    no_uses: bool,

    #[clap(long, short, help = "Install associated applications")]
    install: bool,
}

pub async fn execute(cli: &CLI, args: &Params) -> Result<()> {
    if cli.debug {
        println!("{:#?}", cli);
    }

    let home = cli.resolve_home()?;
    let repo = Repository::new(home.clone());
    if cli.debug {
        println!("{:#?}", repo);
    }
    let overlay = repo.get(&args.name)?;
    if cli.debug {
        println!("{:#?}", overlay);
    }

    let ctx = Context::new(
        args.dry_run,
        cli.debug,
        cli.verbose,
        args.force,
        args.root
            .clone()
            .or_else(home_dir)
            .ok_or_else(|| anyhow::anyhow!("could not determine home directory"))?,
        repo,
        Some(overlay.clone()),
    );

    if args.install {
        actions::install::install(&ctx, &overlay).await?;
    }

    overlay.apply(&ctx).await.inspect_err(|_| {
        eprintln!(
            "{} {} {}",
            emojis::CROSSMARK,
            style::white_b("Failed to apply overlay"),
            style::white_bi(&overlay.name),
        );
    })?;

    Ok(())
}
