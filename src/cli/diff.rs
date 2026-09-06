use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use clap::Args;
use dirs::home_dir;

use crate::cli::CLI;
use crate::desired::DesiredTree;
use crate::diff::Report;
use crate::exec::Context;
use crate::overlays::{Overlay, Repository};
use crate::ui::{emojis, style};
use crate::utils::short_path;

#[derive(Args, Debug)]
pub struct Params {
    #[clap(help = "Name of the overlay to check (all overlays if omitted)")]
    name: Option<String>,

    #[clap(short, long, help = "The target root directory (~)")]
    root: Option<PathBuf>,

    #[clap(long, help = "Do not process uses")]
    no_uses: bool,
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

    let overlays = match &args.name {
        Some(name) => vec![repo.get(name)?],
        None => repo.overlays()?,
    };

    if overlays.is_empty() {
        println!("No overlays found.");
        return Ok(());
    }

    for overlay in &overlays {
        print_overlay_diff(cli, &repo, &root, overlay, args.no_uses)?;
    }

    Ok(())
}

fn print_overlay_diff(
    cli: &CLI,
    repo: &Repository,
    root: &Path,
    overlay: &Overlay,
    no_uses: bool,
) -> Result<()> {
    let ctx = Context::builder()
        .root(root.to_path_buf())
        .repository(repo.clone())
        .overlay(overlay.clone())
        .no_uses(no_uses)
        .build();
    let target = overlay.resolve_target(&ctx)?;
    let ctx = ctx.with_resolved_overlay(overlay.name.clone(), target.to_string_lossy().to_string());

    // Full `uses` graph (not `build_own`): diff reports on everything an
    // overlay pulls in, exactly like `status` and `Overlay::apply` do.
    let desired = DesiredTree::build(&ctx, overlay)?;
    let report = Report::build(&desired)?;

    println!(
        "{} {} {} {} {}",
        emojis::PACKAGE,
        style::white_b("Overlay"),
        style::cyan(&overlay.name),
        style::white_b("->"),
        style::cyan(&short_path(&target.to_string_lossy())),
    );

    if !report.needs_attention() {
        println!("  {}", style::white("no differences"));
    }

    for diff_entry in report.entries() {
        if cli.verbose || diff_entry.needs_attention() {
            println!("  {diff_entry}");
        }
    }
    println!();

    Ok(())
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
        }
    }

    #[tokio::test]
    async fn diff_empty_repository() {
        let tmp = TempDir::new().unwrap();
        let cli = make_cli(tmp.path().to_path_buf());
        let result = execute(&cli, &params(None, tmp.path().to_path_buf())).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn diff_missing_overlay_target_reports_missing() {
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
}
