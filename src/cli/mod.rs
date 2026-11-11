use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand, crate_name};

use crate::ui::style::clap_styles;

mod add;
mod apply;
mod commit;
pub(crate) mod common;
mod completion;
mod data;
mod diff;
mod doctor;
mod git;
mod init;
mod lint;
mod list;
mod log;
mod new;
mod restore;
mod secret;
mod status;
mod sync;
mod unapply;

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
    #[clap(subcommand, about = "Manage repository-local encrypted secrets")]
    Secret(secret::Commands),
    #[clap(
        name = "init",
        about = "Initialize an Over repository from a Git source"
    )]
    Init(init::Params),

    #[clap(name = "add", about = "Add files or directories to an overlay")]
    Add(add::Params),

    #[clap(name = "new", about = "Create a new overlay")]
    New(new::Params),

    #[clap(name = "list", about = "List known overlays", alias = "ls")]
    List(list::Params),

    #[clap(name = "data", about = "Export resolved overlay data as JSON")]
    Data(data::Params),

    #[clap(name = "apply", about = "Apply a given overlay")]
    Apply(apply::Params),

    #[clap(
        name = "lint",
        about = "Check static overlay declarations and source content"
    )]
    Lint(lint::Params),

    #[clap(
        name = "doctor",
        about = "Summarize global environment and overlay health (use status or lint for details)"
    )]
    Doctor(doctor::Params),

    #[clap(name = "completion", about = "Generate shell completion scripts")]
    Completion(completion::Params),

    #[clap(
        name = "status",
        visible_alias = "show",
        about = "Show resolved overlay configuration, deployment health and Git prerequisites"
    )]
    Status(status::Params),

    #[clap(name = "diff", about = "Show Git changes for managed overlay paths")]
    Diff(diff::Params),

    #[clap(
        name = "restore",
        about = "Discard or recover selected virtual-checkout file changes"
    )]
    Restore(restore::Params),

    #[clap(
        name = "sync",
        about = "Reconcile virtual checkouts, optionally pulling/pushing the Over source repository"
    )]
    Sync(sync::Params),

    #[clap(
        name = "unapply",
        about = "Remove a given overlay's own entries from the target"
    )]
    Unapply(unapply::Params),

    #[clap(
        name = "commit",
        about = "Commit staged changes in a checkout-materialized overlay back to its source repository"
    )]
    Commit(commit::Params),

    #[clap(
        name = "log",
        about = "Show the git history behind a checkout-materialized overlay"
    )]
    Log(log::Params),

    #[clap(subcommand, about = "Manage git repository overlays")]
    Git(git::Commands),
}

impl CLI {
    /// Resolve the home directory: flag/env > default (~/.dotfiles)
    pub fn resolve_home(&self) -> Result<PathBuf> {
        crate::utils::resolve_home(self.home.as_ref())
    }
}

pub async fn main() -> Result<()> {
    let args = CLI::parse();

    crate::ui::init_tracing(args.verbose, args.debug);

    let progress_message = match &args.cmd {
        Some(Commands::Apply(_)) => Some("Preparing overlays"),
        Some(Commands::Lint(_)) => Some("Checking overlays"),
        Some(Commands::Doctor(_)) => Some("Checking overlay health"),
        Some(Commands::Status(_)) => Some("Inspecting overlays"),
        Some(Commands::Sync(_)) => Some("Synchronizing overlays"),
        Some(Commands::Unapply(_)) => Some("Unapplying overlay"),
        Some(Commands::Restore(_)) => Some("Restoring overlay files"),
        Some(Commands::Commit(_)) => Some("Recording overlay changes"),
        Some(Commands::Init(_)) => Some("Initializing repository"),
        Some(Commands::Add(_)) => Some("Adding overlay files"),
        Some(Commands::New(_)) => Some("Creating overlay"),
        Some(Commands::Diff(_)) => Some("Comparing overlay files"),
        Some(Commands::Log(_)) => Some("Reading overlay history"),
        Some(Commands::Git(_)) => Some("Inspecting git overlay"),
        _ => None,
    };
    let _progress = progress_message.map(crate::ui::progress::Command::start);

    match args.cmd {
        Some(Commands::Secret(ref command)) => secret::execute(&args, command).await?,
        Some(Commands::Init(ref opt)) => {
            init::execute(&args, opt).await?;
        }
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
        Some(Commands::Doctor(ref opt)) => {
            doctor::execute(&args, opt).await?;
        }
        Some(Commands::Completion(ref opt)) => {
            completion::execute(&args, opt).await?;
        }
        Some(Commands::Data(ref opt)) => {
            data::execute(&args, opt).await?;
        }
        Some(Commands::Status(ref opt)) => {
            status::execute(&args, opt).await?;
        }
        Some(Commands::Diff(ref opt)) => {
            diff::execute(&args, opt).await?;
        }
        Some(Commands::Restore(ref opt)) => {
            restore::execute(&args, opt).await?;
        }
        Some(Commands::Sync(ref opt)) => {
            sync::execute(&args, opt).await?;
        }
        Some(Commands::Unapply(ref opt)) => {
            unapply::execute(&args, opt).await?;
        }
        Some(Commands::Commit(ref opt)) => {
            commit::execute(&args, opt).await?;
        }
        Some(Commands::Log(ref opt)) => {
            log::execute(&args, opt).await?;
        }
        Some(Commands::Git(ref cmd)) => {
            git::execute(&args, cmd).await?;
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
    use clap::CommandFactory;
    use rstest::rstest;
    use std::path::PathBuf;

    #[rstest]
    #[case("add")]
    #[case("apply")]
    #[case("commit")]
    #[case("diff")]
    #[case("doctor")]
    #[case("lint")]
    #[case("log")]
    #[case("restore")]
    #[case("status")]
    #[case("show")]
    #[case("sync")]
    #[case("unapply")]
    fn every_destination_option_respects_flag_environment_and_default_precedence(
        #[case] command: &str,
        #[values("environment", "flag", "short_flag", "default")] selection: &str,
    ) {
        // Clap reads the process environment: isolate each parse so parallel tests
        // never race over environment mutations (which are unsafe in edition 2024).
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "cli::tests::destination_selection_is_checked_in_an_isolated_process",
                "--nocapture",
            ])
            .env("OVER_TEST_COMMAND", command)
            .env("OVER_TEST_SELECTION", selection)
            .env_remove("OVER_TARGET");
        if selection != "default" {
            child.env(
                "OVER_TARGET",
                std::env::temp_dir().join("environment-target"),
            );
        }
        let output = child.output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn destination_selection_is_checked_in_an_isolated_process() {
        let Ok(command) = std::env::var("OVER_TEST_COMMAND") else {
            return;
        };
        let selection = std::env::var("OVER_TEST_SELECTION").unwrap();
        let environment_target = std::env::temp_dir().join("environment-target");
        let flag_target = std::env::temp_dir().join("flag-target");
        let mut args = vec![std::ffi::OsString::from("over"), command.into()];
        args.push("file".into());
        let expected = match selection.as_str() {
            "flag" | "short_flag" => {
                args.push(
                    if selection == "flag" {
                        "--target"
                    } else {
                        "-r"
                    }
                    .into(),
                );
                args.push(flag_target.clone().into());
                Some(flag_target)
            }
            "environment" => Some(environment_target),
            "default" => None,
            _ => panic!("unknown target selection"),
        };
        let matches = CLI::command().try_get_matches_from(args).unwrap();
        let (_, matches) = matches.subcommand().unwrap();
        assert_eq!(matches.get_one::<PathBuf>("root"), expected.as_ref());
    }

    #[test]
    fn resolve_home_with_flag() {
        let expected = std::env::temp_dir().join("test-home");
        let args = CLI::parse_from(["over", "--home", expected.to_str().unwrap()]);
        let home = args.resolve_home().unwrap();
        assert_eq!(home, expected);
    }

    #[test]
    fn resolve_home_with_short_flag() {
        let expected = std::env::temp_dir().join("short-home");
        let args = CLI::parse_from(["over", "-H", expected.to_str().unwrap()]);
        let home = args.resolve_home().unwrap();
        assert_eq!(home, expected);
    }

    #[test]
    fn resolve_home_default_uses_home_dir() {
        // Note: if OVER_HOME env var is set, it will be used instead of default
        let args = CLI::parse_from(["over"]);
        let home = args.resolve_home().unwrap();
        assert!(home.is_absolute());
        if std::env::var_os("OVER_HOME").is_none() {
            assert!(home.ends_with(".dotfiles"));
        }
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
        assert!(matches!(args.cmd, Some(Commands::Status(_))));
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
    fn lint_accepts_an_overlay_and_no_uses_and_json_flags() {
        let args = CLI::parse_from(["over", "lint", "myoverlay", "--no-uses", "--json"]);
        let Some(Commands::Lint(params)) = args.cmd else {
            panic!("expected lint command");
        };
        assert_eq!(params.overlay.as_deref(), Some("myoverlay"));
        assert!(params.no_uses);
        assert!(params.json);
    }

    #[test]
    fn cli_doctor_subcommand() {
        let args = CLI::parse_from(["over", "doctor"]);
        assert!(matches!(args.cmd, Some(Commands::Doctor(_))));
    }

    #[test]
    fn cli_status_subcommand() {
        let args = CLI::parse_from(["over", "status"]);
        assert!(matches!(args.cmd, Some(Commands::Status(_))));
    }

    #[test]
    fn cli_diff_subcommand() {
        let args = CLI::parse_from(["over", "diff"]);
        assert!(matches!(args.cmd, Some(Commands::Diff(_))));
    }

    #[test]
    fn cli_unapply_subcommand() {
        let args = CLI::parse_from(["over", "unapply", "myoverlay"]);
        assert!(matches!(args.cmd, Some(Commands::Unapply(_))));
    }

    #[test]
    fn cli_commit_subcommand() {
        let args = CLI::parse_from(["over", "commit", "myoverlay"]);
        assert!(matches!(args.cmd, Some(Commands::Commit(_))));
    }

    #[test]
    fn cli_log_subcommand() {
        let args = CLI::parse_from(["over", "log"]);
        assert!(matches!(args.cmd, Some(Commands::Log(_))));
    }

    #[test]
    fn cli_log_subcommand_with_overlay_name() {
        let args = CLI::parse_from(["over", "log", "myoverlay"]);
        assert!(matches!(args.cmd, Some(Commands::Log(_))));
    }

    #[test]
    fn cli_git_subcommand() {
        let args = CLI::parse_from(["over", "git", "status"]);
        assert!(matches!(args.cmd, Some(Commands::Git(_))));
    }

    #[test]
    fn cli_completion_subcommand() {
        let args = CLI::parse_from(["over", "completion", "bash"]);
        assert!(matches!(args.cmd, Some(Commands::Completion(_))));
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
