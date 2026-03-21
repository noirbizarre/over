use std::fmt;

use clap::builder::styling;
use console::{Style, StyledObject, style};
use dialoguer::theme::Theme;
use std::sync::LazyLock;

pub static TICK_CHARS_BRAILLE_4_6_DOWN: LazyLock<String> = LazyLock::new(|| String::from("⠶⢲⣰⣤⣆⡖"));
pub static TICK_CHARS_BRAILLE_4_6_UP: LazyLock<String> = LazyLock::new(|| String::from("⠛⠹⠼⠶⠧⠏"));
pub static BRAILLE_6: LazyLock<String> = LazyLock::new(|| String::from("⠿"));

pub static THIN_PROGRESS: LazyLock<String> = LazyLock::new(|| String::from("━>-"));
pub static THIN_DUAL_PROGRESS: LazyLock<String> = LazyLock::new(|| String::from("=>-"));

pub static DOTS_4: LazyLock<String> = LazyLock::new(|| String::from("::"));

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
    /// The style for active items
    pub active_item_style: Style,
    /// The style for inactive items
    pub inactive_item_style: Style,
    /// Active item in select prefix value and style
    pub active_item_prefix: StyledObject<String>,
    /// Inactive item in select prefix value and style
    pub inactive_item_prefix: StyledObject<String>,
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
            active_item_style: Style::new().for_stderr().cyan(),
            inactive_item_style: Style::new().for_stderr(),
            active_item_prefix: style("❯".to_string()).for_stderr().green(),
            inactive_item_prefix: style(" ".to_string()).for_stderr(),
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

    /// Formats a select prompt.
    fn format_select_prompt(&self, f: &mut dyn fmt::Write, prompt: &str) -> fmt::Result {
        if !prompt.is_empty() {
            write!(
                f,
                "{} {} ",
                &self.prompt_prefix,
                self.prompt_style.apply_to(prompt)
            )?;
        }

        write!(f, "{}", &self.prompt_suffix)
    }

    /// Formats a select prompt after selection.
    fn format_select_prompt_selection(
        &self,
        f: &mut dyn fmt::Write,
        prompt: &str,
        sel: &str,
    ) -> fmt::Result {
        if !prompt.is_empty() {
            write!(
                f,
                "{} {} ",
                &self.success_prefix,
                self.prompt_style.apply_to(prompt)
            )?;
        }

        write!(
            f,
            "{} {}",
            &self.success_suffix,
            self.values_style.apply_to(sel)
        )
    }

    /// Formats an input prompt.
    fn format_input_prompt(
        &self,
        f: &mut dyn fmt::Write,
        prompt: &str,
        default: Option<&str>,
    ) -> fmt::Result {
        if !prompt.is_empty() {
            write!(
                f,
                "{} {} ",
                &self.prompt_prefix,
                self.prompt_style.apply_to(prompt),
            )?;
        }

        match default {
            Some(default) => write!(
                f,
                "{} {}",
                self.hint_style.apply_to(format!("({})", default)),
                &self.prompt_suffix,
            ),
            None => write!(f, "{}", &self.prompt_suffix),
        }
    }

    /// Formats an input prompt after selection.
    fn format_input_prompt_selection(
        &self,
        f: &mut dyn fmt::Write,
        prompt: &str,
        sel: &str,
    ) -> fmt::Result {
        if !prompt.is_empty() {
            write!(
                f,
                "{} {} ",
                &self.success_prefix,
                self.prompt_style.apply_to(prompt)
            )?;
        }

        write!(
            f,
            "{} {}",
            &self.success_suffix,
            self.values_style.apply_to(sel)
        )
    }

    /// Formats a multi-select prompt.
    fn format_multi_select_prompt(&self, f: &mut dyn fmt::Write, prompt: &str) -> fmt::Result {
        if !prompt.is_empty() {
            write!(
                f,
                "{} {} ",
                &self.prompt_prefix,
                self.prompt_style.apply_to(prompt),
            )?;
        }

        write!(f, "{}", &self.prompt_suffix)
    }

    /// Formats a multi-select prompt after selection.
    fn format_multi_select_prompt_selection(
        &self,
        f: &mut dyn fmt::Write,
        prompt: &str,
        selections: &[&str],
    ) -> fmt::Result {
        if !prompt.is_empty() {
            write!(
                f,
                "{} {} ",
                &self.success_prefix,
                self.prompt_style.apply_to(prompt)
            )?;
        }

        write!(
            f,
            "{} {}",
            &self.success_suffix,
            self.values_style.apply_to(selections.join(", "))
        )
    }

    /// Formats a select prompt item.
    fn format_select_prompt_item(
        &self,
        f: &mut dyn fmt::Write,
        text: &str,
        active: bool,
    ) -> fmt::Result {
        let details = if active {
            (
                &self.active_item_prefix,
                self.active_item_style.apply_to(text),
            )
        } else {
            (
                &self.inactive_item_prefix,
                self.inactive_item_style.apply_to(text),
            )
        };

        write!(f, "{} {}", details.0, details.1)
    }
}

pub fn clap_styles() -> styling::Styles {
    styling::Styles::styled()
        .header(styling::AnsiColor::Green.on_default() | styling::Effects::BOLD)
        .usage(styling::AnsiColor::Green.on_default() | styling::Effects::BOLD)
        .literal(styling::AnsiColor::Blue.on_default() | styling::Effects::BOLD)
        .placeholder(styling::AnsiColor::Cyan.on_default())
}
