use std::io;

use anyhow::Result;
use clap::Args;
use clap_complete::{Shell, generate};

use crate::cli::CLI;

#[derive(Args, Debug)]
pub struct Params {
    /// Shell to generate completions for
    shell: Shell,
}

pub async fn execute(_cli: &CLI, args: &Params) -> Result<()> {
    use clap::CommandFactory;
    let mut cmd = CLI::command();
    generate(args.shell, &mut cmd, "over", &mut io::stdout());
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use clap_complete::Shell;
    use rstest::rstest;

    use crate::cli::CLI;

    #[rstest]
    #[case::bash("bash", Shell::Bash)]
    #[case::zsh("zsh", Shell::Zsh)]
    #[case::fish("fish", Shell::Fish)]
    #[case::powershell("powershell", Shell::PowerShell)]
    #[case::elvish("elvish", Shell::Elvish)]
    fn parse_shell_argument(#[case] input: &str, #[case] expected: Shell) {
        let args = CLI::parse_from(["over", "completion", input]);
        match args.cmd {
            Some(crate::cli::Commands::Completion(params)) => {
                assert_eq!(params.shell, expected);
            }
            other => panic!("expected Completion variant, got {other:?}"),
        }
    }
}
