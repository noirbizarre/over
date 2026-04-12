use std::process::ExitCode;

use dot_over::cli::git_over;

#[tokio::main]
async fn main() -> ExitCode {
    if let Err(err) = git_over::main().await {
        dot_over::ui::display_error(&err);
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
