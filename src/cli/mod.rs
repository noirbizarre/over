use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand, crate_name};
use dirs::home_dir;

use crate::ui::style::clap_styles;

mod add;
mod apply;
pub(crate) mod common;
pub mod git_over;
mod lint;
mod list;
mod new;
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

    #[clap(name = "new", about = "Create a new overlay")]
    New(new::Params),

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

        Some(Commands::New(ref opt)) => {
            new::execute(&args, opt).await?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn resolve_home_with_flag() {
        let args = CLI::parse_from(["over", "--home", "/tmp/test-home"]);
        let home = args.resolve_home().unwrap();
        assert_eq!(home, PathBuf::from("/tmp/test-home"));
    }

    #[test]
    fn resolve_home_with_short_flag() {
        let args = CLI::parse_from(["over", "-H", "/tmp/short-home"]);
        let home = args.resolve_home().unwrap();
        assert_eq!(home, PathBuf::from("/tmp/short-home"));
    }

    #[test]
    fn resolve_home_default_uses_home_dir() {
        // Note: if OVER_HOME env var is set, it will be used instead of default
        let args = CLI::parse_from(["over"]);
        let home = args.resolve_home().unwrap();
        assert!(home.is_absolute());
        assert!(home.ends_with(".over") || home.ends_with(".dotfiles"));
    }

    #[test]
    fn cli_debug_flag() {
        let args = CLI::parse_from(["over", "--debug"]);
        assert!(args.debug);
    }

    #[test]
    fn cli_short_debug_flag() {
        let args = CLI::parse_from(["over", "-d"]);
        assert!(args.debug);
    }

    #[test]
    fn cli_verbose_flag() {
        let args = CLI::parse_from(["over", "--verbose"]);
        assert!(args.verbose);
    }

    #[test]
    fn cli_short_verbose_flag() {
        let args = CLI::parse_from(["over", "-v"]);
        assert!(args.verbose);
    }

    #[test]
    fn cli_no_subcommand() {
        let args = CLI::parse_from(["over"]);
        assert!(args.cmd.is_none());
    }

    #[test]
    fn cli_add_subcommand() {
        let args = CLI::parse_from(["over", "add", "file.txt", "-o", "myoverlay"]);
        assert!(matches!(args.cmd, Some(Commands::Add(_))));
    }

    #[test]
    fn cli_new_subcommand() {
        let args = CLI::parse_from(["over", "new", "myoverlay", "~"]);
        assert!(matches!(args.cmd, Some(Commands::New(_))));
    }

    #[test]
    fn cli_list_subcommand() {
        let args = CLI::parse_from(["over", "list"]);
        assert!(matches!(args.cmd, Some(Commands::List(_))));
    }

    #[test]
    fn cli_list_alias_ls() {
        let args = CLI::parse_from(["over", "ls"]);
        assert!(matches!(args.cmd, Some(Commands::List(_))));
    }

    #[test]
    fn cli_show_subcommand() {
        let args = CLI::parse_from(["over", "show", "myoverlay"]);
        assert!(matches!(args.cmd, Some(Commands::Show(_))));
    }

    #[test]
    fn cli_apply_subcommand() {
        let args = CLI::parse_from(["over", "apply", "myoverlay"]);
        assert!(matches!(args.cmd, Some(Commands::Apply(_))));
    }

    #[test]
    fn cli_lint_subcommand() {
        let args = CLI::parse_from(["over", "lint"]);
        assert!(matches!(args.cmd, Some(Commands::Lint(_))));
    }

    #[test]
    fn cli_status_subcommand() {
        let args = CLI::parse_from(["over", "status"]);
        assert!(matches!(args.cmd, Some(Commands::Status)));
    }

    #[test]
    fn cli_global_flags_with_subcommand() {
        let args = CLI::parse_from([
            "over",
            "--home",
            "/tmp/test",
            "--debug",
            "--verbose",
            "list",
        ]);
        assert_eq!(args.home, Some(PathBuf::from("/tmp/test")));
        assert!(args.debug);
        assert!(args.verbose);
        assert!(matches!(args.cmd, Some(Commands::List(_))));
    }
}
