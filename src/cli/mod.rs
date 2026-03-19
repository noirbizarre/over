use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand, crate_name};
use dirs::home_dir;

use crate::ui::style::clap_styles;

mod add;
mod apply;
pub mod git_over;
mod lint;
mod list;
mod show;
mod status;

#[derive(Parser, Debug)]
#[clap(
    author,
    version,
    about,
    name = crate_name!(),
    long_about = None,
    styles = clap_styles(),
	// after_help = "over allows you to version your configuration files and workspaces settings",
)]
pub struct CLI {
    #[clap(
        long,
        short = 'H',
        global = true,
        required = false,
        env = "OVER_HOME",
        help = "Configuration and overlays root"
    )]
    home: Option<PathBuf>,

    #[clap(long, short, global = true, help = "Toggle debug traces")]
    debug: bool,

    #[clap(long, short, global = true, help = "Toggle verbose output")]
    verbose: bool,

    #[clap(subcommand)]
    cmd: Option<Commands>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    #[clap(name = "add", about = "Add files or directories to an overlay")]
    Add(add::Params),

    #[clap(name = "list", about = "List known overlays", alias = "ls")]
    List(list::Params),

    #[clap(name = "show", about = "Display details about an overlay")]
    Show(show::Params),

    #[clap(name = "apply", about = "Apply a given overlay")]
    Apply(apply::Params),

    #[clap(name = "lint", about = "Check overlays for configuration issues")]
    Lint(lint::Params),

    #[clap(
        name = "status",
        about = "Get the current repository/directory overlays status"
    )]
    Status,
}

impl CLI {
    /// Resolve the home directory: flag/env > default (~/.over)
    pub fn resolve_home(&self) -> Result<PathBuf> {
        if let Some(ref home) = self.home {
            Ok(home.clone())
        } else {
            let default = home_dir()
                .ok_or_else(|| anyhow::anyhow!("could not determine home directory"))?
                .join(".over");
            Ok(default)
        }
    }
}

pub async fn main() -> Result<()> {
    let args = CLI::parse();
    match args.cmd {
        Some(Commands::Add(ref opt)) => {
            add::execute(&args, opt).await?;
        }

        Some(Commands::List(ref opt)) => {
            list::execute(&args, opt).await?;
        }
        Some(Commands::Apply(ref opt)) => {
            apply::execute(&args, opt).await?;
        }
        Some(Commands::Lint(ref opt)) => {
            lint::execute(&args, opt).await?;
        }
        Some(Commands::Show(ref opt)) => {
            show::execute(&args, opt).await?;
        }
        Some(Commands::Status) => {
            status::execute(&args).await?;
        }
        None => {
            use clap::CommandFactory;
            CLI::command().print_help()?;
        }
    }
    Ok(())
}
