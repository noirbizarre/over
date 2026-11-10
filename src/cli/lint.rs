use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;

use anyhow::{Context as _, Result, bail};
use clap::Args;
use serde::Serialize;

use crate::cli::CLI;
use crate::exec::Context;
use crate::lint::{
    Diagnostic, LintResult, lint_overlay_with_context, lint_repository_with_context,
};
use crate::overlays::Repository;
use crate::ui;
use crate::ui::style;

#[derive(Args, Debug)]
pub struct Params {
    /// Set the target root directory (~)
    #[arg(short, long)]
    pub root: Option<PathBuf>,

    /// Check only this overlay and its uses dependencies
    #[arg(value_name = "OVERLAY")]
    pub overlay: Option<String>,

    /// Skip linting the selected overlay's uses dependencies
    #[arg(long)]
    pub no_uses: bool,

    /// Output diagnostics as JSON
    #[arg(long)]
    pub json: bool,
}

pub async fn execute(cli: &CLI, args: &Params) -> Result<()> {
    let home = cli.resolve_home()?;
    let repo = Repository::new(home);
    let root = args
        .root
        .clone()
        .or_else(dirs::home_dir)
        .ok_or_else(|| anyhow::anyhow!("could not determine home directory"))?;
    let ctx = Context::builder().repository(repo).root(root).build();
    let result = match &args.overlay {
        Some(name) => lint_overlay_with_context(&ctx, name, !args.no_uses)?,
        None => lint_repository_with_context(&ctx),
    };

    if args.json {
        let terminal = io::stdout().is_terminal();
        let colors = terminal
            && std::env::var_os("NO_COLOR").is_none_or(|value| value.is_empty())
            && console::colors_enabled();
        let json = format_json(&result, terminal, colors)?;
        let mut stdout = io::stdout().lock();
        writeln!(stdout, "{json}").context("failed to write lint JSON to stdout")?;
        stdout.flush().context("failed to flush lint JSON")?;
    } else {
        print_human(&result);
    }

    if result.has_errors() {
        let errors = result.error_count();
        bail!(
            "lint found {} error{}",
            errors,
            if errors == 1 { "" } else { "s" },
        );
    }

    Ok(())
}

fn print_human(result: &LintResult) {
    if result.diagnostics.is_empty() {
        ui::info(format!("{}", style::green("No issues found"))).ok();
        return;
    }

    ui::info(format!("{}", result.grouped())).ok();

    // Summary — matches `cli::doctor`'s `print_summary` (both render the
    // same `lint::Diagnostic`-derived counts; kept identical so the two
    // commands can't drift into two different formats again).
    let errors = result.error_count();
    let warnings = result.warning_count();
    let mut parts = Vec::new();
    if errors > 0 {
        parts.push(format!(
            "{}",
            style::red(format!(
                "{errors} error{}",
                if errors == 1 { "" } else { "s" }
            ))
        ));
    }
    if warnings > 0 {
        parts.push(format!(
            "{}",
            style::yellow(format!(
                "{warnings} warning{}",
                if warnings == 1 { "" } else { "s" }
            ))
        ));
    }
    ui::info(format!("{} found", parts.join(", "))).ok();
}

#[derive(Serialize)]
struct JsonReport<'a> {
    diagnostics: &'a [Diagnostic],
    error_count: usize,
    warning_count: usize,
}

fn format_json(result: &LintResult, terminal: bool, colors: bool) -> Result<String> {
    let report = JsonReport {
        diagnostics: &result.diagnostics,
        error_count: result.error_count(),
        warning_count: result.warning_count(),
    };
    // Serialize completely before writing so failures cannot leave a partial document.
    let json = if terminal {
        serde_json::to_string_pretty(&report)
    } else {
        serde_json::to_string(&report)
    }
    .context("failed to serialize lint diagnostics as JSON")?;
    // Pipes must stay machine-readable even when the environment forces colors.
    Ok(if terminal && colors {
        highlight_json(&json)
    } else {
        json
    })
}

fn highlight_json(json: &str) -> String {
    let bytes = json.as_bytes();
    let mut output = String::new();
    let mut index = 0;
    while index < bytes.len() {
        let start = index;
        if bytes[index] == b'"' {
            index += 1;
            while index < bytes.len() {
                match bytes[index] {
                    // An escaped quote must not terminate a JSON string. Skipping
                    // escape pairs also handles even/odd runs of backslashes.
                    b'\\' => index += 2,
                    b'"' => {
                        index += 1;
                        break;
                    }
                    _ => index += 1,
                }
            }
            let token = &json[start..index];
            let key = json[index..].trim_start().starts_with(':');
            let styled = if key {
                console::style(token).cyan()
            } else {
                console::style(token).green()
            };
            output.push_str(&styled.force_styling(true).to_string());
        } else if bytes[index].is_ascii_digit() || matches!(bytes[index], b'-' | b't' | b'f' | b'n')
        {
            while index < bytes.len()
                && !matches!(
                    bytes[index],
                    b',' | b']' | b'}' | b' ' | b'\n' | b'\r' | b'\t'
                )
            {
                index += 1;
            }
            output.push_str(
                &console::style(&json[start..index])
                    .yellow()
                    .force_styling(true)
                    .to_string(),
            );
        } else {
            // Outside strings serialized JSON contains only ASCII punctuation/whitespace.
            output.push(char::from(bytes[index]));
            index += 1;
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_layout_and_color_follow_the_output_destination() {
        let result = LintResult {
            diagnostics: vec![Diagnostic::warning(
                "desktop",
                "unknown key `targt` ignored",
            )],
        };
        let compact = format_json(&result, false, true).unwrap();
        assert!(!compact.contains('\n'));
        assert!(!compact.contains('\x1b'));
        let value: serde_json::Value = serde_json::from_str(&compact).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "diagnostics": [{
                    "severity": "warning", "overlay": "desktop",
                    "message": "unknown key `targt` ignored", "hint": null, "file": null
                }],
                "error_count": 0, "warning_count": 1
            })
        );
        let pretty = format_json(&result, true, false).unwrap();
        assert!(pretty.contains('\n'));
        assert!(!pretty.contains('\x1b'));
        let colored = format_json(&result, true, true).unwrap();
        assert!(colored.contains('\x1b'));
        assert_eq!(console::strip_ansi_codes(&colored), pretty);
    }

    #[test]
    fn syntax_coloring_preserves_json_escapes_and_unicode_exactly() {
        let value = serde_json::json!({
            "escaped\"key": "Unicode: é 🦀; quotes: \"; slash: \\; pair: \\\"; newline:\n; `over lint`",
            "punctuation": "{\"key\":null}",
            "values": [true, false, null, -2, 1.25e30]
        });
        let pretty = serde_json::to_string_pretty(&value).unwrap();
        let colored = highlight_json(&pretty);
        let stripped = console::strip_ansi_codes(&colored);
        assert_eq!(stripped, pretty);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&stripped).unwrap(),
            value
        );
    }
}
