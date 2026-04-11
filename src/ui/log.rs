use console::{Term, style};

use anyhow::Result;

pub fn info(msg: impl AsRef<str>) -> Result<()> {
    let term = Term::stdout();
    term.write_line(msg.as_ref())?;
    Ok(())
}

/// Initialize the tracing subscriber with the given verbosity level.
///
/// - Default (no flags): only warnings and errors
/// - `--verbose`: adds info-level messages
/// - `--debug`: adds debug-level messages
pub fn init_tracing(verbose: bool, debug: bool) {
    use tracing_subscriber::EnvFilter;

    let default_level = if debug {
        "debug"
    } else if verbose {
        "info"
    } else {
        "warn"
    };

    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level));

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(filter)
        .with_target(false)
        .without_time()
        .init();
}

/// Format an `anyhow::Error` for user-facing display on stderr.
///
/// Produces styled output like:
/// ```text
/// ✘ error: overlay "foo" not found
///   caused by: no descriptor file at path
/// ```
pub fn display_error(err: &anyhow::Error) {
    let term = Term::stderr();
    let mut chain = err.chain();

    // First error in chain is the top-level message
    if let Some(top) = chain.next() {
        let _ = term.write_line(&format!(
            "{} {} {}",
            style("✘").red().bold(),
            style("error:").red().bold(),
            top,
        ));
    }

    // Remaining causes
    for cause in chain {
        let _ = term.write_line(&format!("  {} {}", style("caused by:").dim(), cause,));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_error_single_message() {
        let err = anyhow::anyhow!("something went wrong");
        // Should not panic; output goes to stderr
        display_error(&err);
    }

    #[test]
    fn display_error_with_cause_chain() {
        let err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let err = anyhow::Error::new(err).context("failed to read config");
        // Exercises the `for cause in chain` loop (lines 60-62)
        display_error(&err);
    }

    #[test]
    fn display_error_deep_chain() {
        let err = anyhow::anyhow!("root cause")
            .context("middle layer")
            .context("top level");
        display_error(&err);
    }
}
