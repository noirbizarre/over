//! The over binary.

#![allow(clippy::result_large_err)]

use std::process::ExitCode;

use clap::Parser;

mod cli;

use cli::{Cli, Command};

fn main() -> ExitCode {
    let args = Cli::parse();

    let result = match args.command {
        Command::Run => over::run(),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // Rendered here rather than returned as `miette::Result`, so the
            // exit code is chosen explicitly instead of inherited from
            // whatever the report wrapper decides.
            eprintln!("{:?}", miette::Report::new(error));
            ExitCode::FAILURE
        }
    }
}
