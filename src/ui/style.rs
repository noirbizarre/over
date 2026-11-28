use std::fmt;
use std::sync::LazyLock;

use choisir::theme::MinimalTheme;
use clap::builder::styling;
use console::{StyledObject, style};
use dialoguer::theme::Theme;

pub static TICK_CHARS_BRAILLE_4_6_DOWN: LazyLock<String> = LazyLock::new(|| String::from("⠶⢲⣰⣤⣆⡖"));
pub static TICK_CHARS_BRAILLE_4_6_UP: LazyLock<String> = LazyLock::new(|| String::from("⠛⠹⠼⠶⠧⠏"));
pub static BRAILLE_6: LazyLock<String> = LazyLock::new(|| String::from("⠿"));
pub static THIN_PROGRESS: LazyLock<String> = LazyLock::new(|| String::from("━>-"));
pub static THIN_DUAL_PROGRESS: LazyLock<String> = LazyLock::new(|| String::from("=>-"));
pub static DOTS_4: LazyLock<String> = LazyLock::new(|| String::from("::"));

pub fn white<D>(value: D) -> StyledObject<D> {
    style(value)
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
pub fn bold<D>(value: D) -> StyledObject<D> {
    style(value).bold()
}
pub fn dim<D>(value: D) -> StyledObject<D> {
    style(value).dim()
}
pub fn yellow<D>(value: D) -> StyledObject<D> {
    style(value).yellow()
}
pub fn green<D>(value: D) -> StyledObject<D> {
    style(value).green()
}
pub fn red<D>(value: D) -> StyledObject<D> {
    style(value).red()
}

// Semantic roles reserve foreground colors for identities and diagnostic severity.
pub fn heading<D>(value: D) -> StyledObject<D> {
    style(value).white().bold()
}
pub fn key<D>(value: D) -> StyledObject<D> {
    style(value).white().bold()
}
pub fn path<D: fmt::Display>(value: D) -> StyledObject<String> {
    style(crate::utils::short_path(&value.to_string())).blue()
}
pub fn overlay<D>(value: D) -> StyledObject<D> {
    cyan(value)
}
pub fn repository<D: fmt::Display>(value: D) -> StyledObject<String> {
    style(crate::utils::short_path(&value.to_string())).blue()
}
pub fn mode<D>(value: D) -> StyledObject<D> {
    style(value)
}
pub fn annotation<D>(value: D) -> StyledObject<D> {
    dim(value)
}

#[derive(Debug, Clone, Copy)]
pub enum Severity {
    Success,
    Warning,
    Error,
}

pub fn severity<D>(value: D, severity: Severity) -> StyledObject<D> {
    match severity {
        Severity::Success => green(value),
        Severity::Warning => yellow(value),
        Severity::Error => red(value),
    }
}

/// Keep path presentation and credential masking local; choisir owns widget styling.
#[derive(Default)]
pub struct DialogTheme(MinimalTheme);

impl Theme for DialogTheme {
    fn format_prompt(&self, f: &mut dyn fmt::Write, prompt: &str) -> fmt::Result {
        self.0.format_prompt(f, &crate::utils::short_paths(prompt))
    }

    fn format_confirm_prompt(
        &self,
        f: &mut dyn fmt::Write,
        prompt: &str,
        default: Option<bool>,
    ) -> fmt::Result {
        self.0
            .format_confirm_prompt(f, &crate::utils::short_paths(prompt), default)
    }

    fn format_confirm_prompt_selection(
        &self,
        f: &mut dyn fmt::Write,
        prompt: &str,
        selection: Option<bool>,
    ) -> fmt::Result {
        self.0
            .format_confirm_prompt_selection(f, &crate::utils::short_paths(prompt), selection)
    }

    fn format_input_prompt(
        &self,
        f: &mut dyn fmt::Write,
        prompt: &str,
        default: Option<&str>,
    ) -> fmt::Result {
        let default = default.map(crate::utils::short_paths);
        self.0
            .format_input_prompt(f, &crate::utils::short_paths(prompt), default.as_deref())
    }

    fn format_input_prompt_selection(
        &self,
        f: &mut dyn fmt::Write,
        prompt: &str,
        selection: &str,
    ) -> fmt::Result {
        self.0.format_input_prompt_selection(
            f,
            &crate::utils::short_paths(prompt),
            &crate::utils::short_paths(selection),
        )
    }

    fn format_password_prompt_selection(
        &self,
        f: &mut dyn fmt::Write,
        prompt: &str,
    ) -> fmt::Result {
        // A fixed marker confirms input without disclosing the credential's length.
        self.format_input_prompt_selection(f, prompt, "***")
    }

    fn format_select_prompt(&self, f: &mut dyn fmt::Write, prompt: &str) -> fmt::Result {
        self.0
            .format_select_prompt(f, &crate::utils::short_paths(prompt))
    }

    fn format_select_prompt_selection(
        &self,
        f: &mut dyn fmt::Write,
        prompt: &str,
        selection: &str,
    ) -> fmt::Result {
        self.0.format_select_prompt_selection(
            f,
            &crate::utils::short_paths(prompt),
            &crate::utils::short_paths(selection),
        )
    }

    fn format_select_prompt_item(
        &self,
        f: &mut dyn fmt::Write,
        text: &str,
        active: bool,
    ) -> fmt::Result {
        self.0
            .format_select_prompt_item(f, &crate::utils::short_paths(text), active)
    }

    fn format_multi_select_prompt(&self, f: &mut dyn fmt::Write, prompt: &str) -> fmt::Result {
        self.0
            .format_multi_select_prompt(f, &crate::utils::short_paths(prompt))
    }

    fn format_multi_select_prompt_selection(
        &self,
        f: &mut dyn fmt::Write,
        prompt: &str,
        selections: &[&str],
    ) -> fmt::Result {
        let selections: Vec<_> = selections
            .iter()
            .map(|selection| crate::utils::short_paths(selection))
            .collect();
        let selections: Vec<_> = selections.iter().map(String::as_str).collect();
        self.0.format_multi_select_prompt_selection(
            f,
            &crate::utils::short_paths(prompt),
            &selections,
        )
    }

    fn format_multi_select_prompt_item(
        &self,
        f: &mut dyn fmt::Write,
        text: &str,
        checked: bool,
        active: bool,
    ) -> fmt::Result {
        self.0
            .format_multi_select_prompt_item(f, &crate::utils::short_paths(text), checked, active)
    }
}

pub fn clap_styles() -> styling::Styles {
    styling::Styles::plain()
        .header(styling::AnsiColor::Cyan.on_default().bold())
        .usage(styling::AnsiColor::Cyan.on_default().bold())
        .literal(styling::AnsiColor::Green.on_default().bold())
        .error(styling::AnsiColor::Red.on_default().bold())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_completion_uses_a_fixed_mask() {
        let mut rendered = String::new();
        DialogTheme::default()
            .format_password_prompt_selection(&mut rendered, "Passphrase")
            .unwrap();
        assert_eq!(console::strip_ansi_codes(&rendered), "Passphrase · ***");
    }

    #[test]
    fn input_defaults_abbreviate_home_paths() {
        let Some(home) = dirs::home_dir() else { return };
        let value = home.join("identity.txt").display().to_string();
        let mut rendered = String::new();
        DialogTheme::default()
            .format_input_prompt(&mut rendered, "Identity", Some(&value))
            .unwrap();
        assert!(console::strip_ansi_codes(&rendered).contains("[~/identity.txt]"));
    }
}
