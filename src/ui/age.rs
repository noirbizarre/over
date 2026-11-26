//! Terminal interaction for standard Age plugins and encrypted identity files.

use std::io::IsTerminal;

use age::secrecy::SecretString;
use zeroize::Zeroizing;

use crate::crypt::age::InteractionError;

#[derive(Clone, Copy)]
pub struct Callbacks;

fn safe(text: &str) -> String {
    // Plugin messages are untrusted terminal data, not terminal escape sequences.
    text.chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .take(4096)
        .collect()
}

fn available() -> bool {
    let available = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    if !available {
        eprintln!("{0}", InteractionError::Unavailable);
    }
    available
}

fn terminal_result<T, E>(result: Result<T, E>) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(_) => {
            // Raw UI errors can contain terminal input; report availability without echoing it.
            eprintln!("{0}", InteractionError::Failed);
            None
        }
    }
}

fn prompt_result<T>(result: dialoguer::Result<T>) -> Option<T> {
    result
        .map_err(|error| eprintln!("{}", prompt_error(error)))
        .ok()
}

pub(crate) fn prompt_error(error: dialoguer::Error) -> InteractionError {
    let dialoguer::Error::IO(error) = error;
    // Only a structured interruption establishes cancellation; never inspect error text.
    if error.kind() == std::io::ErrorKind::Interrupted {
        InteractionError::Cancelled
    } else {
        InteractionError::Failed
    }
}

impl age::Callbacks for Callbacks {
    fn display_message(&self, message: &str) {
        let pause = super::progress::pause();
        if pause.is_err() {
            eprintln!("Could not suspend progress output for the Age plugin message");
        }
        // Device insertion/touch instructions remain essential even if progress suspension fails.
        eprintln!("{}", safe(message));
    }

    fn confirm(&self, message: &str, yes: &str, no: Option<&str>) -> Option<bool> {
        if !available() {
            return None;
        }
        let _pause = terminal_result(super::progress::pause())?;
        let labels = [safe(yes), safe(no.unwrap_or("Cancel"))];
        let selection = prompt_result(
            dialoguer::Select::with_theme(&super::style::DialogTheme::default())
                .with_prompt(safe(message))
                .items(&labels)
                .default(1)
                .interact_opt(),
        )?;
        if selection != Some(0) {
            eprintln!("{0}", InteractionError::Cancelled);
        }
        selection.map(|selection| selection == 0)
    }

    fn request_public_string(&self, description: &str) -> Option<String> {
        if !available() {
            return None;
        }
        let _pause = terminal_result(super::progress::pause())?;
        prompt_result(
            dialoguer::Input::with_theme(&super::style::DialogTheme::default())
                .with_prompt(safe(description))
                .interact_text(),
        )
    }

    fn request_passphrase(&self, description: &str) -> Option<SecretString> {
        if !available() {
            return None;
        }
        let _pause = terminal_result(super::progress::pause())?;
        let input = Zeroizing::new(prompt_result(
            dialoguer::Password::with_theme(&super::style::DialogTheme::default())
                .with_prompt(safe(description))
                .interact(),
        )?);
        Some(SecretString::new(input.as_str().into()))
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::prompt_error;
    use crate::crypt::age::{InteractionError, interaction_error};

    #[rstest]
    #[case(std::io::ErrorKind::Interrupted, "cancelled or denied")]
    #[case(std::io::ErrorKind::Other, "unavailable or cancelled")]
    fn terminal_errors_preserve_only_structured_safe_outcomes(
        #[case] kind: std::io::ErrorKind,
        #[case] category: &str,
    ) {
        let error = prompt_error(dialoguer::Error::from(std::io::Error::new(
            kind,
            "private-input-sentinel",
        )));
        let message = interaction_error(&anyhow::Error::new(error)).to_string();
        assert!(message.contains(category));
        assert!(!message.contains("sentinel"));
        let unknown = anyhow::anyhow!("private-input-sentinel");
        assert_eq!(
            interaction_error(&unknown).to_string(),
            InteractionError::Failed.to_string()
        );
    }
}
