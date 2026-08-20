use clap::Args;

use crate::cli::CLI;
use crate::overlays::Repository;
use crate::ui::style;
use crate::utils::short_path;
use anyhow::Result;

#[derive(Args, Debug)]
pub struct Params {
    #[clap(help = "Name of the overlay to display")]
    name: String,
}

pub async fn execute(cli: &CLI, args: &Params) -> Result<()> {
    if cli.debug {
        println!("{:#?}", cli);
        println!("{:#?}", args);
    }

    let home = cli.resolve_home()?;
    let repo = Repository::new(home);
    let overlay = repo.get(&args.name)?;

    println!("{}", style::white_b(&overlay.name));
    println!("  root:   {}", short_path(&overlay.root.to_string_lossy()));
    println!("  target: {}", overlay.target);
    if let Some(desc) = &overlay.description {
        println!("  desc:   {}", desc);
    }
    if let Some(uses) = &overlay.uses {
        println!("  uses:   {}", uses.join(", "));
    }
    if let Some(link_dirs) = &overlay.link_dirs {
        println!("  link_dirs: {}", link_dirs.join(", "));
    }
    if let Some(git) = &overlay.git {
        println!("  git repositories:");
        for (path, cfg) in git {
            println!("    {}: {}", path, cfg.url);
        }
    }
    if overlay.install.is_some() {
        println!("  install: configured");
    }
    Ok(())
}
