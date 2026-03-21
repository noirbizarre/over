use std::path::PathBuf;

use clap::Args;

use crate::cli::CLI;
use crate::cli::common::resolve_inputs;
use crate::exec::Context;
use crate::overlays::Repository;
use crate::ui::{emojis, style};
use anyhow::{Result, anyhow};
use dialoguer::FuzzySelect;
use dialoguer::theme::ColorfulTheme;
use dirs::home_dir;

#[derive(Args, Debug)]
pub struct Params {
    #[clap(required = true, help = "Files, directories, or glob patterns to add")]
    files: Vec<String>,

    #[clap(short, long, help = "Name of the target overlay")]
    overlay: Option<String>,

    #[clap(short, long, help = "The target root directory (~)")]
    root: Option<PathBuf>,

    #[clap(long, short = 'n', help = "Run without applying changes")]
    dry_run: bool,

    #[clap(long, short, help = "Overwrite without prompting")]
    force: bool,
}

pub async fn execute(cli: &CLI, args: &Params) -> Result<()> {
    if cli.debug {
        println!("{:#?}", cli);
        println!("{:#?}", args);
    }

    let home = cli.resolve_home()?;
    let repo = Repository::new(home);
    if cli.debug {
        println!("{:#?}", repo);
    }

    let overlay = match &args.overlay {
        Some(name) => repo.get(name)?,
        None => {
            let overlays = repo.overlays()?;
            if overlays.is_empty() {
                return Err(anyhow!("no overlays found in repository"));
            }
            let selection = FuzzySelect::with_theme(&ColorfulTheme::default())
                .with_prompt("Choose the target overlay")
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
        .root(
            args.root
                .clone()
                .or_else(home_dir)
                .ok_or_else(|| anyhow!("could not determine home directory"))?,
        )
        .repository(repo)
        .overlay(overlay.clone())
        .build();

    let resolved = resolve_inputs(&args.files)?;

    overlay.add_files(&ctx, &resolved).await.map_err(|e| {
        println!(
            "{} {} {} {}",
            emojis::CROSSMARK,
            style::white_b("Failed to add to overlay"),
            style::cyan(&overlay.name),
            style::white_b(&format!(": {}", e)),
        );
        e
    })?;

    Ok(())
}
