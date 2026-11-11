use std::path::PathBuf;

use anyhow::{Result, anyhow, bail};
use clap::Args;
use dirs::home_dir;

use crate::cli::CLI;
use crate::doctor::{self, Finding, OverlayHealth, Report};
use crate::overlays::Repository;
use crate::ui;
use crate::ui::{emojis, style};

#[derive(Args, Debug)]
pub struct Params {
    #[clap(help = "Name of the overlay to check (every overlay otherwise)")]
    name: Option<String>,

    #[clap(
        short = 'r',
        long = "target",
        value_name = "TARGET",
        env = "OVER_TARGET",
        help = "Set the target directory (defaults to ~)"
    )]
    root: Option<PathBuf>,

    #[clap(long, help = "Skip dependency operations, retaining dependency data")]
    no_uses: bool,

    #[clap(
        long,
        help = "Repair malformed git-exclude blocks (over's own corrupted markers only)"
    )]
    fix: bool,
}

pub async fn execute(cli: &CLI, args: &Params) -> Result<()> {
    if cli.debug {
        tracing::debug!(?cli, ?args, "CLI args");
    }

    let home = cli.resolve_home()?;
    let repo = Repository::new(home);
    let root = args
        .root
        .clone()
        .or_else(home_dir)
        .ok_or_else(|| anyhow!("could not determine home directory"))?;

    let opts = doctor::Options {
        root,
        overlay: args.name.clone(),
        no_uses: args.no_uses,
        fix: args.fix,
    };
    let report = doctor::check(&repo, &opts).await?;

    print_section(
        cli,
        &format!("{} Environment", emojis::STETHOSCOPE),
        &report.environment,
    );
    ui::info(format!(
        "{}",
        style::heading(format!("{} Overlay health", emojis::PACKAGE))
    ))?;
    for line in render_overlay_health(&report.overlay_health, cli.verbose).lines() {
        ui::info(line)?;
    }
    ui::info("")?;
    print_section(
        cli,
        &format!("{} XDG state", emojis::DIRECTORY),
        &report.xdg_state,
    );

    if !report.fixed.is_empty() {
        ui::info(format!("{}", style::heading("Fixed"))).ok();
        for line in &report.fixed {
            ui::info(format!("  {} {line}", emojis::checkmark())).ok();
        }
        ui::info("").ok();
    }

    print_summary(&report);

    if report.has_errors() {
        let errors = report.error_count();
        bail!(
            "doctor found {} error{}",
            errors,
            if errors == 1 { "" } else { "s" }
        );
    }

    Ok(())
}

fn render_overlay_health(overlays: &[OverlayHealth], verbose: bool) -> String {
    let lines: Vec<_> = overlays
        .iter()
        .filter(|overlay| verbose || overlay.needs_attention())
        .map(|overlay| {
            let name = if overlay.name.is_empty() {
                "."
            } else {
                &overlay.name
            };
            let mut severities = Vec::new();
            if overlay.errors > 0 {
                severities.push(format!(
                    "{} {}",
                    emojis::crossmark(),
                    style::severity(
                        format!("{} error(s)", overlay.errors),
                        style::Severity::Error
                    )
                ));
            }
            if overlay.warnings > 0 {
                severities.push(format!(
                    "{} {}",
                    emojis::warning(),
                    style::severity(
                        format!("{} warning(s)", overlay.warnings),
                        style::Severity::Warning
                    )
                ));
            }
            if severities.is_empty() {
                severities.push(emojis::checkmark().to_string());
            }
            format!("  {}: {}", style::overlay(name), severities.join(", "))
        })
        .collect();
    if lines.is_empty() {
        format!(
            "  {} {}",
            emojis::checkmark(),
            style::severity("No issues found", style::Severity::Success)
        )
    } else {
        lines.join("\n")
    }
}

fn print_section(cli: &CLI, title: &str, findings: &[Finding]) {
    ui::info(format!("{}", style::heading(title))).ok();
    let visible: Vec<&Finding> = findings
        .iter()
        .filter(|f| cli.verbose || f.needs_attention())
        .collect();
    if visible.is_empty() {
        ui::info(format!("  {}", style::green("No issues found"))).ok();
    } else {
        for finding in visible {
            ui::info(format!("  {finding}")).ok();
        }
    }
    ui::info("").ok();
}

fn print_summary(report: &Report) {
    let errors = report.error_count();
    let warnings = report.warning_count();
    if errors == 0 && warnings == 0 {
        ui::info(format!("{}", style::green("No issues found"))).ok();
        return;
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    use clap::Parser;
    use std::fs;

    #[test]
    fn overlay_health_uses_semantic_colors_and_only_nonzero_severities() {
        if crate::cli::common::run_in_isolated_process() {
            return;
        }
        // Isolate console's global color switch from concurrently rendered tests.
        console::set_colors_enabled(true);
        let rendered = render_overlay_health(
            &[
                OverlayHealth {
                    name: "healthy".into(),
                    errors: 0,
                    warnings: 0,
                },
                OverlayHealth {
                    name: "warning".into(),
                    errors: 0,
                    warnings: 1,
                },
                OverlayHealth {
                    name: "error".into(),
                    errors: 2,
                    warnings: 0,
                },
                OverlayHealth {
                    name: "mixed".into(),
                    errors: 1,
                    warnings: 3,
                },
            ],
            true,
        );
        assert!(rendered.contains("\x1b[36mhealthy\x1b[0m: \x1b[32m✔\x1b[0m"));
        assert!(rendered.contains("\x1b[33m⚠\x1b[0m \x1b[33m1 warning(s)\x1b[0m"));
        assert!(rendered.contains("\x1b[31m✘\x1b[0m \x1b[31m2 error(s)\x1b[0m"));
        let plain = console::strip_ansi_codes(&rendered);
        assert!(plain.contains("mixed: ✘ 1 error(s), ⚠ 3 warning(s)"));
        assert!(!plain.contains("0 error"));
        assert!(!plain.contains("0 warning"));
        assert!(!plain.contains("over status"));
        assert!(
            console::strip_ansi_codes(&render_overlay_health(&[], false))
                .contains("✔ No issues found")
        );
    }

    fn make_cli(home: PathBuf) -> CLI {
        CLI::parse_from(vec!["over", "--home", home.to_str().unwrap()])
    }

    fn params(name: Option<&str>, root: PathBuf, fix: bool) -> Params {
        Params {
            name: name.map(String::from),
            root: Some(root),
            no_uses: false,
            fix,
        }
    }

    /// Points `$XDG_STATE_HOME` at a fresh, writable temp dir for the
    /// duration of the returned guard's lifetime, so these in-process unit
    /// tests never depend on (or write to) the real environment's XDG
    /// state — mirrors `xdg::tests`' own `set_env` justification: safe
    /// because `cargo nextest` runs each test in its own process.
    ///
    /// # Safety
    /// `env::set_var` is only unsound when other threads read/write the
    /// process environment concurrently; each `nextest` test owns its own
    /// process.
    fn isolate_xdg_state() -> TempDir {
        let tmp = TempDir::new().unwrap();
        unsafe {
            std::env::set_var("XDG_STATE_HOME", tmp.path());
        }
        tmp
    }

    #[tokio::test]
    async fn doctor_empty_repository() {
        let _xdg = isolate_xdg_state();
        let tmp = TempDir::new().unwrap();
        let cli = make_cli(tmp.path().to_path_buf());
        let result = execute(&cli, &params(None, tmp.path().to_path_buf(), false)).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn doctor_clean_overlay_has_no_errors() {
        let _xdg = isolate_xdg_state();
        let tmp = TempDir::new().unwrap();
        let root = tmp.child("root");
        root.create_dir_all().unwrap();
        let ov = tmp.path().join("clean");
        fs::create_dir_all(&ov).unwrap();
        fs::write(ov.join("over.toml"), "target = \"~\"").unwrap();

        let cli = make_cli(tmp.path().to_path_buf());
        let result = execute(&cli, &params(None, root.path().to_path_buf(), false)).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn doctor_reports_lint_errors_and_exits_nonzero() {
        let _xdg = isolate_xdg_state();
        let tmp = TempDir::new().unwrap();
        let root = tmp.child("root");
        root.create_dir_all().unwrap();
        let ov = tmp.path().join("broken");
        fs::create_dir_all(&ov).unwrap();
        fs::write(ov.join("over.toml"), "this is not valid toml [[[").unwrap();

        let cli = make_cli(tmp.path().to_path_buf());
        let result = execute(&cli, &params(None, root.path().to_path_buf(), false)).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn doctor_unknown_overlay_errors() {
        let _xdg = isolate_xdg_state();
        let tmp = TempDir::new().unwrap();
        let root = tmp.child("root");
        root.create_dir_all().unwrap();

        let cli = make_cli(tmp.path().to_path_buf());
        let result = execute(
            &cli,
            &params(Some("does-not-exist"), root.path().to_path_buf(), false),
        )
        .await;
        assert!(result.is_err());
    }
}
