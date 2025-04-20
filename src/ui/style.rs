use std::fmt;

use clap::builder::styling;
use console::{Style, StyledObject, style};
use dialoguer::theme::Theme;
use once_cell::sync::Lazy;

pub static TICK_CHARS_BRAILLE_4_6_DOWN: Lazy<String> = Lazy::new(|| String::from("⠶⢲⣰⣤⣆⡖"));
pub static TICK_CHARS_BRAILLE_4_6_UP: Lazy<String> = Lazy::new(|| String::from("⠛⠹⠼⠶⠧⠏"));
pub static BRAILLE_6: Lazy<String> = Lazy::new(|| String::from("⠿"));

pub static THIN_PROGRESS: Lazy<String> = Lazy::new(|| String::from("━>-"));
pub static THIN_DUAL_PROGRESS: Lazy<String> = Lazy::new(|| String::from("=>-"));

pub static DOTS_4: Lazy<String> = Lazy::new(|| String::from("::"));

// pub static ref SPINNER: ProgressStyle = ProgressStyle::default_spinner()
//     .template("{prefix:.bold.dim} {spinner.green} {wide_msg}")
//     .tick_chars("⠁⠂⠄⡀⢀⠠⠐⠈ ");
// }

pub fn white<D>(value: D) -> StyledObject<D> {
    style(value).white()
}

pub fn white_b<D>(value: D) -> StyledObject<D> {
    white(value).bold()
}

pub fn white_bi<D>(value: D) -> StyledObject<D> {
    white_b(value).italic()
}

pub fn cyan<D>(value: D) -> StyledObject<D> {
    style(value).cyan()
}

pub fn yellow<D>(value: D) -> StyledObject<D> {
    style(value).yellow()
}

pub struct DialogTheme {
    /// The style for default values
    pub defaults_style: Style,
    /// The style for prompt
    pub prompt_style: Style,
    /// Prompt prefix value and style
    pub prompt_prefix: StyledObject<String>,
    /// Prompt suffix value and style
    pub prompt_suffix: StyledObject<String>,
    /// Prompt on success prefix value and style
    pub success_prefix: StyledObject<String>,
    /// Prompt on success suffix value and style
    pub success_suffix: StyledObject<String>,
    /// Error prefix value and style
    pub error_prefix: StyledObject<String>,
    /// The style for error message
    pub error_style: Style,
    /// The style for hints
    pub hint_style: Style,
    /// The style for values on prompt success
    pub values_style: Style,
}

impl Default for DialogTheme {
    fn default() -> DialogTheme {
        DialogTheme {
            defaults_style: Style::new().for_stderr().cyan(),
            prompt_style: Style::new().for_stderr().bold(),
            prompt_prefix: style("?".to_string()).for_stderr().yellow(),
            prompt_suffix: style("›".to_string()).for_stderr().black().bright(),
            success_prefix: style("✔".to_string()).for_stderr().green(),
            success_suffix: style("·".to_string()).for_stderr().black().bright(),
            error_prefix: style("✘".to_string()).for_stderr().red(),
            error_style: Style::new().for_stderr().red(),
            hint_style: Style::new().for_stderr().black().bright(),
            values_style: Style::new().for_stderr().green(),
        }
    }
}

impl Theme for DialogTheme {
    /// Formats a confirm prompt.
    fn format_confirm_prompt(
        &self,
        f: &mut dyn fmt::Write,
        prompt: &str,
        default: Option<bool>,
    ) -> fmt::Result {
        if !prompt.is_empty() {
            write!(
                f,
                "{} {} ",
                &self.prompt_prefix,
                self.prompt_style.apply_to(prompt)
            )?;
        }

        match default {
            None => write!(
                f,
                "{} {}",
                self.hint_style.apply_to("(y/n)"),
                &self.prompt_suffix
            ),
            Some(true) => write!(
                f,
                "{} {} {}",
                self.hint_style.apply_to("(y/n)"),
                &self.prompt_suffix,
                self.defaults_style.apply_to("yes")
            ),
            Some(false) => write!(
                f,
                "{} {} {}",
                self.hint_style.apply_to("(y/n)"),
                &self.prompt_suffix,
                self.defaults_style.apply_to("no")
            ),
        }
    }

    /// Formats a confirm prompt after selection.
    fn format_confirm_prompt_selection(
        &self,
        f: &mut dyn fmt::Write,
        prompt: &str,
        selection: Option<bool>,
    ) -> fmt::Result {
        let selection = selection.map(|b| if b { "yes" } else { "no" });
        let prefix = match selection {
            Some("yes") => &self.success_prefix,
            _ => &self.error_prefix,
        };
        let style = match selection {
            Some("yes") => &self.values_style,
            _ => &self.error_style,
        };

        if !prompt.is_empty() {
            write!(f, "{} {} ", prefix, self.prompt_style.apply_to(prompt))?;
        }

        match selection {
            Some(selection) => {
                write!(f, "{}", style.apply_to(selection))
            }
            None => {
                write!(f, "{}", &self.success_suffix)
            }
        }
    }
}

pub fn clap_styles() -> styling::Styles {
    styling::Styles::styled()
        .header(styling::AnsiColor::Green.on_default() | styling::Effects::BOLD)
        .usage(styling::AnsiColor::Green.on_default() | styling::Effects::BOLD)
        .literal(styling::AnsiColor::Blue.on_default() | styling::Effects::BOLD)
        .placeholder(styling::AnsiColor::Cyan.on_default())
}
