use std::process::ExitCode;

use over::cli;

fn main() -> ExitCode {
    // Privileged dispatch must not load repository configuration or initialize the normal CLI.
    if let Some(result) = over::target_fs::dispatch_helper() {
        return match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("over privileged helper: {error}");
                ExitCode::FAILURE
            }
        };
    }
    match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime.block_on(run()),
        Err(error) => {
            eprintln!("could not initialize runtime: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> ExitCode {
    let result = cli::main().await;
    if let Err(error) = over::target_fs::Session::shared().shutdown() {
        over::ui::display_error(&error);
        return ExitCode::FAILURE;
    }
    if let Err(err) = result {
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
