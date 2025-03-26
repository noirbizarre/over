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

    #[clap(long, short, help = "Do not process uses")]
    no_uses: bool,

    #[clap(long, short, help = "Install associated applications")]
    install: bool,
}

pub async fn execute(cli: &CLI, args: &Params) -> Result<()> {
    if cli.debug {
        println!("{:#?}", cli);
    }

    let repo = Repository::new(PathBuf::from(&cli.home));
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
        args.root.clone().unwrap_or(home_dir().unwrap()),
        repo,
        Some(overlay.clone()),
    );
        
    if args.install {
        actions::install::install(&ctx, &overlay).await?;
    }

    let result = overlay.apply(&ctx).await;
    if let Err(e) = result {
        println!(
            "{} {} {}",
            emojis::CROSSMARK,
            style::white_b("Failed to apply overlay"),
            style::white_bi(&overlay.name),
        );
        println!("{:#?}", e);
    }

    Ok(())
}
