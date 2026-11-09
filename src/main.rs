use std::process::ExitCode;

use over::cli;

#[tokio::main]
async fn main() -> ExitCode {
    if let Err(err) = cli::main().await {
        over::ui::display_error(&err);
        if let Some(failure) = err.downcast_ref::<over::hooks::HookFailure>() {
            // ExitCode is byte-sized; signal termination has no portable numeric code.
            return ExitCode::from(
                failure
                    .status
                    .code()
                    .and_then(|code| u8::try_from(code).ok())
                    .unwrap_or(1),
            );
        }
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
