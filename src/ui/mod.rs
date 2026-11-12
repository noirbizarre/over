pub mod emojis;
pub mod json;
pub(crate) mod patch;
pub mod progress;
pub mod style;

pub mod log;

pub use log::{display_error, info, init_tracing, literal, warn};

/// Keep the question outside dialoguer's editable line so redraws stay single-line.
pub(crate) fn message_input(question: &str) -> anyhow::Result<String> {
    use dialoguer::theme::Theme;
    let theme = style::DialogTheme::default();
    let mut heading = String::new();
    theme.format_select_prompt(&mut heading, question)?;
    console::Term::stderr().write_line(&heading)?;
    Ok(dialoguer::Input::<String>::with_theme(&theme)
        .allow_empty(true)
        .interact_text()?)
}
