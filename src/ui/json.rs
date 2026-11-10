//! Shared terminal-aware JSON output for CLI reports.

use std::io::{self, IsTerminal, Write};

use anyhow::{Context as _, Result};
use colored_json::ColorMode;
use serde::Serialize;

/// Print a JSON document, pretty and colored on terminals, compact in pipes.
///
/// A nonempty `NO_COLOR` disables terminal colors without disabling indentation.
pub fn print_json<T: Serialize>(value: &T) -> Result<()> {
    let terminal = io::stdout().is_terminal();
    let colors = terminal
        && std::env::var_os("NO_COLOR").is_none_or(|value| value.is_empty())
        && console::colors_enabled();
    // Finish serialization before writing so failures cannot leave a partial document.
    let json = format_json(value, terminal, colors).context("failed to serialize JSON output")?;
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{json}").context("failed to write JSON to stdout")?;
    stdout.flush().context("failed to flush JSON output")
}

fn format_json<T: Serialize>(
    value: &T,
    terminal: bool,
    colors: bool,
) -> serde_json::Result<String> {
    if terminal {
        colored_json::to_colored_json(
            value,
            if colors {
                ColorMode::On
            } else {
                ColorMode::Off
            },
        )
    } else {
        // Bypass all color policy in pipes, even when the environment forces colors.
        serde_json::to_string(value)
    }
}

#[cfg(test)]
mod tests {
    use super::format_json;

    #[test]
    fn json_layout_and_color_follow_the_output_destination() {
        let value = serde_json::json!({"key": "value", "count": 1});
        let compact = format_json(&value, false, true).unwrap();
        assert!(!compact.contains('\n'));
        assert!(!compact.contains('\x1b'));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&compact).unwrap(),
            value
        );

        let pretty = format_json(&value, true, false).unwrap();
        assert!(pretty.contains('\n'));
        assert!(!pretty.contains('\x1b'));
        let colored = format_json(&value, true, true).unwrap();
        assert!(colored.contains('\x1b'));
        assert_eq!(console::strip_ansi_codes(&colored), pretty);
    }

    #[test]
    fn colored_output_preserves_json_escapes_and_unicode_exactly() {
        let value = serde_json::json!({
            "escaped\"key": "Unicode: é 🦀; quotes: \"; slash: \\; pair: \\\"; newline:\n; `over lint`",
            "punctuation": "{\"key\":null}",
            "values": [true, false, null, -2, 1.25e30]
        });
        let pretty = serde_json::to_string_pretty(&value).unwrap();
        let colored = format_json(&value, true, true).unwrap();
        let stripped = console::strip_ansi_codes(&colored);
        assert_eq!(stripped, pretty);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&stripped).unwrap(),
            value
        );
    }
}
