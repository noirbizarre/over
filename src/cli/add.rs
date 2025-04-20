use std::path::PathBuf;

use clap::Args;

use crate::cli::CLI;
use crate::exec::Context;
use crate::overlays::Repository;
use crate::ui::{emojis, style};
use anyhow::Result;
use dialoguer::FuzzySelect;
use dialoguer::theme::ColorfulTheme;
use dirs::home_dir;

#[derive(Args, Debug)]
pub struct Params {
    #[clap(help = "File to add")]
    file: PathBuf,

    #[clap(help = "Name of the target overlay")]
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

    let home = cli.home.as_ref().expect("Home directory not set");
    let repo = Repository::new(PathBuf::from(home));
    if cli.debug {
        println!("{:#?}", repo);
    }

    let overlay = match &args.overlay {
        Some(name) => repo.get(name)?,
        None => {
            let overlays = repo.overlays()?;
            let selection = FuzzySelect::with_theme(&ColorfulTheme::default())
                .with_prompt("Choose the target overlay")
                .default(0)
                .items(&overlays[..])
                .interact()
                .unwrap();
            overlays[selection].clone()
        }
    };

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

    let result = overlay.add_file(&ctx, &args.file).await;
    if let Err(e) = result {
        println!(
            "{} {} {} {} {}",
            emojis::CROSSMARK,
            style::white_b("Failed to add file overlay"),
            style::cyan(&args.file.to_str().unwrap()),
            style::white_b("to overlay"),
            style::cyan(&overlay.name),
        );
        println!("{:#?}", e);
    }

    Ok(())
}
