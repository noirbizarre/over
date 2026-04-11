use std::process::ExitCode;

use dot_over::cli;

#[tokio::main]
async fn main() -> ExitCode {
    if let Err(err) = cli::main().await {
        dot_over::ui::display_error(&err);
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
