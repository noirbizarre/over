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
    use clap::CommandFactory;
    use clap_complete::{Shell, generate};
    use rstest::rstest;

    use crate::cli::CLI;

    #[rstest]
    #[case::bash(Shell::Bash)]
    #[case::zsh(Shell::Zsh)]
    #[case::fish(Shell::Fish)]
    #[case::powershell(Shell::PowerShell)]
    #[case::elvish(Shell::Elvish)]
    fn generate_completions(#[case] shell: Shell) {
        let mut cmd = CLI::command();
        let mut buf = Vec::new();
        generate(shell, &mut cmd, "over", &mut buf);
        let output = String::from_utf8(buf).unwrap();
        assert!(!output.is_empty());
        assert!(output.contains("over"));
    }
}
