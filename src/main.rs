use std::process::ExitCode;

use over::cli;

#[tokio::main]
async fn main() -> ExitCode {
    if let Err(err) = cli::main().await {
        over::ui::display_error(&err);
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
