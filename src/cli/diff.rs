use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use clap::Args;
use dirs::home_dir;

use crate::actions::git::diff::View;
use crate::cli::CLI;
use crate::desired::DesiredTree;
use crate::diff::git::Report;
use crate::exec::Context;
use crate::overlays::{Overlay, Repository};
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
        help = "Diff every overlay, retaining configured default_overlay data"
    )]
    all: bool,

    #[clap(
        long,
        alias = "cached",
        conflicts_with = "all_changes",
        help = "Show staged changes instead of unstaged changes"
    )]
    staged: bool,

    #[clap(long, help = "Show both staged and unstaged changes")]
    all_changes: bool,

    #[clap(long, help = "Use Git's configured difftool")]
    difftool: bool,
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
        (Some(name), _) => vec![repo.get(name)?],
        (None, true) => repo.overlays()?,
        (None, false) => {
            let ctx = Context::builder()
                .root(root.clone())
                .repository(repo.clone())
                .build();
            match tokio::task::spawn_blocking(move || crate::selection::infer_or_default(&ctx))
                .await??
            {
                Some(overlay) => vec![overlay],
                None => repo.overlays()?,
            }
        }
    };

    if overlays.is_empty() {
        ui::info("No overlays found.").ok();
        return Ok(());
    }

    let views = if args.all_changes {
        vec![View::Staged, View::Unstaged]
    } else if args.staged {
        vec![View::Staged]
    } else {
        vec![View::Unstaged]
    };
    let mut seen = std::collections::BTreeSet::new();
    let mut linked_paths = std::collections::BTreeMap::<_, std::collections::BTreeSet<_>>::new();
    for overlay in &overlays {
        let report = build_overlay_diff(&repo, &root, overlay, args.no_uses, views.clone()).await?;
        let _pause = ui::progress::pause()?;
        for diagnostic in &report.diagnostics {
            eprintln!("{diagnostic}");
        }
        for mut comparison in report.comparisons {
            if comparison.trees.is_none() {
                let paths = linked_paths
                    .entry((comparison.repository.clone(), comparison.view))
                    .or_default();
                comparison.scope.retain(|path| paths.insert(path.clone()));
            }
            if comparison.scope.is_empty() {
                continue;
            }
            // Dependencies can occur in several selected roots; never launch a tool twice.
            let key = (
                comparison.repository.clone(),
                comparison.scope.clone(),
                comparison.view,
                comparison.trees,
            );
            if !seen.insert(key) {
                continue;
            }
            eprintln!(
                "Overlay {} — repository {} — {}{}\n  scope: {}",
                crate::ui::style::overlay(&overlay.name).for_stderr(),
                crate::ui::style::repository(comparison.repository.display()).for_stderr(),
                if comparison.view == View::Staged {
                    "staged"
                } else {
                    "unstaged"
                },
                comparison
                    .baseline
                    .as_ref()
                    .map(|base| format!(" (recorded base {base})"))
                    .unwrap_or_default(),
                comparison
                    .scope
                    .iter()
                    .map(|path| crate::ui::style::path(path.display())
                        .for_stderr()
                        .to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            comparison.present(args.difftool).await?;
        }
    }

    Ok(())
}

async fn build_overlay_diff(
    repo: &Repository,
    root: &Path,
    overlay: &Overlay,
    no_uses: bool,
    views: Vec<View>,
) -> Result<Report> {
    let ctx = Context::builder()
        .root(root.to_path_buf())
        .repository(repo.clone())
        .overlay(overlay.clone())
        .no_uses(no_uses)
        .build();
    let ctx = ctx.resolve_data()?;
    let target = overlay.resolve_target(&ctx)?;
    let ctx = ctx.with_resolved_overlay(overlay.name.clone(), target.to_string_lossy().to_string());

    // Full `uses` graph (not `build_own`): diff reports on everything an
    // overlay pulls in, exactly like `status` and `Overlay::apply` do.
    let overlay = overlay.clone();
    tokio::task::spawn_blocking(move || {
        let desired = DesiredTree::build(&ctx, &overlay)?;
        Report::build(&desired, &views)
    })
    .await?
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
            staged: false,
            all_changes: false,
            difftool: false,
        }
    }

    #[tokio::test]
    async fn diff_empty_repository() {
        if crate::cli::common::run_in_isolated_process() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        let cli = make_cli(tmp.path().to_path_buf());
        let result = execute(&cli, &params(None, tmp.path().to_path_buf())).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn diff_missing_overlay_target_reports_missing() {
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
        assert!(result.is_ok());
        assert!(!root.path().join("sub").exists());
    }

    #[tokio::test]
    async fn diff_named_overlay_only() {
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
    async fn diff_unknown_overlay_errors() {
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

    #[tokio::test]
    async fn diff_conflicting_file_shows_content_diff() {
        if crate::cli::common::run_in_isolated_process() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        let root = tmp.child("root");
        root.create_dir_all().unwrap();
        let ov = tmp.path().join("conflicted");
        fs::create_dir_all(&ov).unwrap();
        fs::write(ov.join("over.toml"), "target = \"~\"").unwrap();
        fs::write(ov.join("file.txt"), "overlay content").unwrap();
        fs::write(root.path().join("file.txt"), "existing content").unwrap();

        let cli = make_cli(tmp.path().to_path_buf());
        let result = execute(&cli, &params(None, root.path().to_path_buf())).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn diff_multiple_overlays() {
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
    async fn diff_default_overlay_narrows_omitted_name() {
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
    async fn diff_all_flag_overrides_configured_default() {
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
    async fn diff_misconfigured_default_overlay_errors() {
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
