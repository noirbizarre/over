use std::path::PathBuf;

use anyhow::{Result, anyhow};
use clap::Args;
use dialoguer::FuzzySelect;
use dirs::home_dir;

use crate::actions;
use crate::cli::CLI;
use crate::exec::Context;
use crate::overlays::Repository;
use crate::ui::style::DialogTheme;
use crate::ui::{emojis, style};
#[derive(Args, Debug)]
pub struct Params {
    #[clap(help = "Name of the overlay to apply")]
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
        println!("{:#?}", cli);
    }

    let home = cli.resolve_home()?;
    let repo = Repository::new(home.clone());
    if cli.debug {
        println!("{:#?}", repo);
    }
    let overlay = match &args.name {
        Some(name) => repo.get(name)?,
        None => {
            let overlays = repo.overlays()?;
            if overlays.is_empty() {
                return Err(anyhow!("no overlays found in repository"));
            }
            let selection = FuzzySelect::with_theme(&DialogTheme::default())
                .with_prompt("Choose the overlay to apply")
                .default(0)
                .items(&overlays[..])
                .interact()
                .map_err(|e| anyhow!("overlay selection cancelled: {}", e))?;
            overlays[selection].clone()
        }
    };
    if cli.debug {
        println!("{:#?}", overlay);
    }

    let ctx = Context::builder()
        .dry_run(args.dry_run)
        .debug(cli.debug)
        .verbose(cli.verbose)
        .force(args.force)
        .no_prompt(args.no_prompt)
        .root(
            args.root
                .clone()
                .or_else(home_dir)
                .ok_or_else(|| anyhow::anyhow!("could not determine home directory"))?,
        )
        .repository(repo)
        .overlay(overlay.clone())
        .build();

    if args.install {
        actions::install::install(&ctx, &overlay).await?;
    }

    overlay.apply(&ctx).await.inspect_err(|e| {
        eprintln!(
            "{} {} {}: {}",
            emojis::CROSSMARK,
            style::white_b("Failed to apply overlay"),
            style::cyan(&overlay.name),
            e,
        );
    })?;

    Ok(())
}
