use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use clap::Args;
use dirs::home_dir;

use crate::cli::CLI;
use crate::exec::Context;
use crate::overlays::{Overlay, Repository};
use crate::status::Counts;
use crate::status::overlay::{OverlayReport, ResolvedConfiguration};
use crate::ui;

#[derive(Args, Debug)]
pub struct Params {
    #[clap(
        help = "Name of the overlay to check (infer context, then default_overlay, then all overlays)"
    )]
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
        short,
        conflicts_with = "name",
        help = "Check every overlay, retaining configured default_overlay data"
    )]
    all: bool,
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

    let overlays = match (&args.name, args.all) {
        (Some(name), _) => vec![repo.get(name).map_err(|error| anyhow!("{error}"))],
        (None, true) => repo
            .inspect_overlays()
            .map_err(|error| anyhow!("{error}"))?,
        (None, false) => {
            let ctx = Context::builder()
                .root(root.clone())
                .repository(repo.clone())
                .build();
            // Drop secret-bearing parser/template causes while preserving diagnostic context.
            match tokio::task::spawn_blocking(move || crate::selection::infer_or_default(&ctx))
                .await?
                .map_err(|error| anyhow!("{error}"))?
            {
                Some(overlay) => vec![Ok(overlay)],
                None => repo
                    .inspect_overlays()
                    .map_err(|error| anyhow!("{error}"))?,
            }
        }
    };

    if overlays.is_empty() {
        ui::info("No overlays found.")?;
        return Ok(());
    }

    let mut healthy = 0;
    let mut unhealthy = 0;
    let mut failed = 0;
    let mut failures = Vec::new();
    let mut counts = Counts::default();
    let mut exclude_problems = 0;
    for overlay in &overlays {
        let snapshot = overlay
            .as_ref()
            .map_err(|error| anyhow!("{error}"))
            .and_then(|overlay| inspect_overlay(&repo, &root, overlay, args.no_uses));
        match snapshot {
            Ok(snapshot) => {
                write_output(&render_overlay(&snapshot, cli.verbose))?;
                match &snapshot.health {
                    Ok(report) => {
                        let current = report.counts();
                        exclude_problems += report
                            .exclude_problems()
                            .iter()
                            .filter(|problem| problem.needs_attention())
                            .count();
                        counts.applied += current.applied;
                        counts.missing += current.missing;
                        counts.modified += current.modified;
                        counts.broken += current.broken;
                        counts.conflict += current.conflict;
                        counts.ahead += current.ahead;
                        counts.behind += current.behind;
                        counts.diverged += current.diverged;
                        if report.needs_attention() {
                            unhealthy += 1;
                        } else {
                            healthy += 1;
                        }
                    }
                    Err(error) => {
                        failures.push(error.to_string());
                        failed += 1;
                    }
                }
            }
            Err(error) => {
                write_output(&format!("Check failed: {error}\n"))?;
                failures.push(error.to_string());
                failed += 1;
            }
        }
    }
    if overlays.len() > 1 {
        write_output(&format!(
            "Overall: {} overlays; {healthy} fully applied, {unhealthy} need attention, {failed} could not be checked\n  {counts}\n  {exclude_problems} exclude problem(s)\n  Entry counts are summed per overlay, including shared dependencies.",
            overlays.len()
        ))?;
    }
    if unhealthy + failed > 0 {
        return Err(anyhow!(
            "selected overlays are not fully applied ({unhealthy} need attention, {failed} could not be checked){}",
            if failures.is_empty() {
                String::new()
            } else {
                format!(": {}", failures.join("; "))
            }
        ));
    }
    Ok(())
}

fn inspect_overlay(
    repo: &Repository,
    root: &Path,
    overlay: &Overlay,
    no_uses: bool,
) -> Result<OverlayReport> {
    let ctx = Context::builder()
        .root(root.to_path_buf())
        .repository(repo.clone())
        .overlay(overlay.clone())
        .no_uses(no_uses)
        .build();
    let ctx = ctx
        .resolve_data()
        .map_err(|error| anyhow!("cannot resolve overlay '{}': {error}", overlay.name))?;
    let target = overlay
        .resolve_target(&ctx)
        .map_err(|_| anyhow!("cannot resolve target for overlay '{}'", overlay.name))?;
    let ctx = ctx.with_resolved_overlay(overlay.name.clone(), target.to_string_lossy().to_string());

    OverlayReport::inspect(ctx, overlay)
        .map_err(|error| anyhow!("cannot inspect overlay '{}': {error}", overlay.name))
}

fn render_configuration(
    configuration: &ResolvedConfiguration,
    verbose: bool,
    lines: &mut Vec<String>,
) {
    lines.push(format!("  source: {}", configuration.source.display()));
    lines.push(format!("  target: {}", configuration.target.display()));
    lines.extend(configuration.summary.iter().map(|line| format!("  {line}")));
    if verbose {
        lines.extend(
            configuration
                .details
                .iter()
                .map(|line| format!("    {line}")),
        );
    }
}

fn render_overlay(snapshot: &OverlayReport, verbose: bool) -> String {
    let mut lines = vec![
        format!("Overlay {}", snapshot.configuration.name),
        "Configuration:".into(),
    ];
    render_configuration(&snapshot.configuration, verbose, &mut lines);
    if verbose {
        for dependency in &snapshot.dependencies {
            lines.push(format!("Dependency {}:", dependency.name));
            render_configuration(dependency, true, &mut lines);
        }
    }
    lines.push("Reconciliation:".into());
    match &snapshot.health {
        Ok(report) => {
            lines.push(format!(
                "  {} — {}",
                if report.needs_attention() {
                    "Needs attention"
                } else {
                    "Fully applied"
                },
                report.counts()
            ));
            for (entry, details) in report.entries().iter().zip(&snapshot.entry_details) {
                if verbose || entry.status.needs_attention() {
                    lines.push(format!("  {entry}"));
                    lines.extend(details.iter().map(|line| format!("    {line}")));
                }
            }
            for problem in report.exclude_problems() {
                if verbose || problem.needs_attention() {
                    lines.push(format!("  {problem}"));
                }
            }
        }
        Err(error) => lines.push(format!("  Check failed: {error}")),
    }
    lines.push(String::new());
    lines.join("\n")
}

fn write_output(output: &str) -> Result<()> {
    let _pause = ui::progress::pause()?;
    // Even forced console color must not leak ANSI escapes into captured status.
    if !std::io::stdout().is_terminal() || std::env::var_os("NO_COLOR").is_some() {
        ui::info(console::strip_ansi_codes(output))
    } else {
        ui::info(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    use clap::Parser;
    use std::fs;
    use std::path::PathBuf;

    fn make_cli(home: PathBuf) -> CLI {
        CLI::parse_from(vec!["over", "--home", home.to_str().unwrap()])
    }

    fn params(name: Option<&str>, root: PathBuf) -> Params {
        Params {
            name: name.map(String::from),
            root: Some(root),
            no_uses: false,
            all: false,
        }
    }

    #[test]
    fn rendering_a_collected_overlay_report_never_reopens_its_source_or_target() {
        let tmp = TempDir::new().unwrap();
        tmp.child("ov/over.yaml")
            .write_str("target: '~/target'")
            .unwrap();
        tmp.child("ov/file").write_str("managed").unwrap();
        let repo = Repository::new(tmp.path().to_owned());
        let overlay = repo.get("ov").unwrap();
        let snapshot = inspect_overlay(&repo, &tmp.path().join("home"), &overlay, false).unwrap();
        let before = render_overlay(&snapshot, true);
        tmp.close().unwrap();
        assert_eq!(render_overlay(&snapshot, true), before);
    }

    #[tokio::test]
    async fn status_empty_repository() {
        if crate::cli::common::run_in_isolated_process() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        let cli = make_cli(tmp.path().to_path_buf());
        let result = execute(&cli, &params(None, tmp.path().to_path_buf())).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn status_missing_overlay_target_reports_missing() {
        if crate::cli::common::run_in_isolated_process() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        let root = tmp.child("root");
        root.create_dir_all().unwrap();
        let ov = tmp.path().join("myoverlay");
        fs::create_dir_all(&ov).unwrap();
        fs::write(ov.join("over.toml"), "target = \"~/sub\"").unwrap();

        let cli = make_cli(tmp.path().to_path_buf());
        let result = execute(&cli, &params(None, root.path().to_path_buf())).await;
        assert!(result.is_err());
        assert!(!root.path().join("sub").exists());
    }

    #[tokio::test]
    async fn status_named_overlay_only() {
        if crate::cli::common::run_in_isolated_process() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        let root = tmp.child("root");
        root.create_dir_all().unwrap();
        for name in ["first", "second"] {
            let ov = tmp.path().join(name);
            fs::create_dir_all(&ov).unwrap();
            fs::write(ov.join("over.toml"), "target = \"~\"").unwrap();
        }

        let cli = make_cli(tmp.path().to_path_buf());
        let result = execute(&cli, &params(Some("first"), root.path().to_path_buf())).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn status_unknown_overlay_errors() {
        if crate::cli::common::run_in_isolated_process() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        let root = tmp.child("root");
        root.create_dir_all().unwrap();

        let cli = make_cli(tmp.path().to_path_buf());
        let result = execute(
            &cli,
            &params(Some("does-not-exist"), root.path().to_path_buf()),
        )
        .await;
        assert!(result.is_err());
    }

    /// End-to-end smoke test for #148: a managed target landing inside a
    /// git repository with no `.git/info/exclude` block yet must not
    /// create the block as a side effect, even though it returns an unhealthy exit.
    #[tokio::test]
    async fn status_reports_a_missing_exclude_block_with_failure_without_writing() {
        if crate::cli::common::run_in_isolated_process() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        let root = tmp.child("root");
        root.create_dir_all().unwrap();
        git2::Repository::init(root.path()).unwrap();
        let ov = tmp.path().join("dotfiles");
        fs::create_dir_all(&ov).unwrap();
        fs::write(ov.join("over.toml"), "target = \"~\"").unwrap();
        fs::write(ov.join("file.txt"), "content").unwrap();

        let cli = make_cli(tmp.path().to_path_buf());
        let result = execute(&cli, &params(None, root.path().to_path_buf())).await;
        assert!(result.is_err());

        // `git init` itself pre-populates `info/exclude` with a commented
        // template — assert our own marker never lands in it, rather than
        // asserting non-existence (a false negative).
        let exclude_path = root.path().join(".git/info/exclude");
        let content = fs::read_to_string(&exclude_path).unwrap_or_default();
        assert!(
            !content.contains("over: exclude:"),
            "`over status` must never write to .git/info/exclude:\n{content}"
        );
    }

    #[tokio::test]
    async fn status_conflicting_file_is_reported() {
        if crate::cli::common::run_in_isolated_process() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        let root = tmp.child("root");
        root.create_dir_all().unwrap();
        let ov = tmp.path().join("conflicted");
        fs::create_dir_all(&ov).unwrap();
        fs::write(ov.join("over.toml"), "target = \"~\"").unwrap();
        fs::write(ov.join("file.txt"), "overlay").unwrap();
        fs::write(root.path().join("file.txt"), "existing").unwrap();

        let cli = make_cli(tmp.path().to_path_buf());
        let result = execute(&cli, &params(None, root.path().to_path_buf())).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn status_multiple_overlays() {
        if crate::cli::common::run_in_isolated_process() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        let root = tmp.child("root");
        root.create_dir_all().unwrap();
        for name in ["alpha", "beta", "gamma"] {
            let ov = tmp.path().join(name);
            fs::create_dir_all(&ov).unwrap();
            fs::write(ov.join("over.toml"), "target = \"~\"").unwrap();
        }

        let cli = make_cli(tmp.path().to_path_buf());
        let result = execute(&cli, &params(None, root.path().to_path_buf())).await;
        assert!(result.is_ok());
    }

    /// An omitted `NAME` narrows to just the configured `default_overlay`
    /// instead of reporting on every overlay (#128).
    #[tokio::test]
    async fn status_default_overlay_narrows_omitted_name() {
        if crate::cli::common::run_in_isolated_process() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("over.toml"), "default_overlay = \"alpha\"").unwrap();
        let root = tmp.child("root");
        root.create_dir_all().unwrap();
        for name in ["alpha", "beta"] {
            let ov = tmp.path().join(name);
            fs::create_dir_all(&ov).unwrap();
            fs::write(ov.join("over.toml"), "target = \"~\"").unwrap();
        }

        let cli = make_cli(tmp.path().to_path_buf());
        let result = execute(&cli, &params(None, root.path().to_path_buf())).await;
        assert!(result.is_ok());
    }

    /// `--all` recovers today's "every overlay" behavior even when a
    /// `default_overlay` is configured (#128).
    #[tokio::test]
    async fn status_all_flag_overrides_configured_default() {
        if crate::cli::common::run_in_isolated_process() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("over.toml"), "default_overlay = \"alpha\"").unwrap();
        let root = tmp.child("root");
        root.create_dir_all().unwrap();
        for name in ["alpha", "beta"] {
            let ov = tmp.path().join(name);
            fs::create_dir_all(&ov).unwrap();
            fs::write(ov.join("over.toml"), "target = \"~\"").unwrap();
        }

        let cli = make_cli(tmp.path().to_path_buf());
        let mut p = params(None, root.path().to_path_buf());
        p.all = true;
        let result = execute(&cli, &p).await;
        assert!(result.is_ok());
    }

    /// A `default_overlay` naming a nonexistent overlay is a hard error,
    /// not a silent fallback to "all overlays" (#128).
    #[tokio::test]
    async fn status_misconfigured_default_overlay_errors() {
        if crate::cli::common::run_in_isolated_process() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("over.toml"),
            "default_overlay = \"does-not-exist\"",
        )
        .unwrap();
        let root = tmp.child("root");
        root.create_dir_all().unwrap();

        let cli = make_cli(tmp.path().to_path_buf());
        let result = execute(&cli, &params(None, root.path().to_path_buf())).await;
        assert!(result.is_err());
    }
}
