//! Terminal interaction for standard Age plugins and encrypted identity files.

use std::io::IsTerminal;

use age::secrecy::SecretString;
use zeroize::Zeroizing;

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
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

fn terminal_result<T, E>(result: Result<T, E>) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(_) => {
            // Raw UI errors can contain terminal input; report availability without echoing it.
            eprintln!("Age terminal interaction unavailable or cancelled");
            None
        }
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
        terminal_result(
            dialoguer::Select::new()
                .with_prompt(safe(message))
                .items(&labels)
                .default(1)
                .interact_opt(),
        )
        .flatten()
        .map(|selection| selection == 0)
    }

    fn request_public_string(&self, description: &str) -> Option<String> {
        if !available() {
            return None;
        }
        let _pause = terminal_result(super::progress::pause())?;
        terminal_result(
            dialoguer::Input::new()
                .with_prompt(safe(description))
                .interact_text(),
        )
    }

    fn request_passphrase(&self, description: &str) -> Option<SecretString> {
        if !available() {
            return None;
        }
        let _pause = terminal_result(super::progress::pause())?;
        let input = Zeroizing::new(terminal_result(
            dialoguer::Password::new()
                .with_prompt(safe(description))
                .interact(),
        )?);
        Some(SecretString::new(input.as_str().into()))
    }
}
