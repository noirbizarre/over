use std::path::PathBuf;

use anyhow::{Result, anyhow};
use clap::Args;
use dialoguer::FuzzySelect;
use dirs::home_dir;

use crate::cli::CLI;
use crate::desired::DesiredTree;
use crate::exec::Context;
use crate::overlays::Repository;
use crate::ui::style::DialogTheme;
use crate::ui::{emojis, style};
use crate::unapply::{Outcome, Report};
use crate::utils::short_path;

#[derive(Args, Debug)]
pub struct Params {
    #[clap(help = "Name of the overlay to unapply")]
    name: Option<String>,

    #[clap(short, long, help = "The target root directory (~)")]
    root: Option<PathBuf>,

    #[clap(
        long,
        short = 'n',
        help = "Report what would be removed without removing anything"
    )]
    dry_run: bool,
}

pub async fn execute(cli: &CLI, args: &Params) -> Result<()> {
    if cli.debug {
        tracing::debug!(?cli, ?args, "CLI args");
    }

    let home = cli.resolve_home()?;
    let repo = Repository::new(home);
    let overlay = match &args.name {
        Some(name) => repo.get(name)?,
        None => {
            let overlays = repo.overlays()?;
            if overlays.is_empty() {
                return Err(anyhow!("no overlays found in repository"));
            }
            let selection = FuzzySelect::with_theme(&DialogTheme::default())
                .with_prompt("Choose the overlay to unapply")
                .default(0)
                .items(&overlays[..])
                .interact()
                .map_err(|e| anyhow!("overlay selection cancelled: {}", e))?;
            overlays[selection].clone()
        }
    };
    if cli.debug {
        tracing::debug!(?overlay, "resolved overlay");
    }

    let root = args
        .root
        .clone()
        .or_else(home_dir)
        .ok_or_else(|| anyhow!("could not determine home directory"))?;

    let ctx = Context::builder()
        .dry_run(args.dry_run)
        .debug(cli.debug)
        .verbose(cli.verbose)
        .root(root)
        .repository(repo)
        .overlay(overlay.clone())
        .build();
    let target = overlay.resolve_target(&ctx)?;
    let ctx = ctx.with_resolved_overlay(overlay.name.clone(), target.to_string_lossy().to_string());

    // This overlay's own entries only — no `uses` recursion. Unapplying a
    // dependency here could silently break another overlay that still
    // `uses` it (a diamond dependency): removing a shared dependency's
    // entries is left to explicitly unapplying it by name.
    let desired = DesiredTree::build_own(&ctx, &overlay)?;
    let report = Report::build(&desired)?;

    println!(
        "{} {} {} {} {}",
        emojis::PACKAGE,
        style::white_b("Overlay"),
        style::cyan(&overlay.name),
        style::white_b("->"),
        style::cyan(&short_path(&target.to_string_lossy())),
    );
    println!("  {}", report.counts());

    for entry_outcome in report.entries() {
        if cli.verbose || !matches!(entry_outcome.outcome, Outcome::AlreadyAbsent) {
            println!("  {entry_outcome}");
        }
    }
    println!();

    report.execute(ctx).await?;

    if report.needs_attention() {
        return Err(anyhow!(
            "one or more entries were left untouched — see `skip:` lines above"
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlays::Repository as OverlayRepository;
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    use clap::Parser;
    use std::fs;

    fn make_cli(home: PathBuf) -> CLI {
        CLI::parse_from(vec!["over", "--home", home.to_str().unwrap()])
    }

    fn params(name: Option<&str>, root: PathBuf, dry_run: bool) -> Params {
        Params {
            name: name.map(String::from),
            root: Some(root),
            dry_run,
        }
    }

    /// Apply `name` from the repository rooted at `home` into `root`,
    /// bypassing the CLI layer (mirrors the pattern already used by
    /// `plan::reconcile`/`status`/`diff`'s own tests).
    async fn apply_overlay(home: &std::path::Path, root: &std::path::Path, name: &str) {
        let repo = OverlayRepository::new(home.to_path_buf());
        let overlay = repo.get(name).unwrap();
        let ctx = Context::builder()
            .root(root.to_path_buf())
            .repository(repo)
            .overlay(overlay.clone())
            .build();
        overlay.apply(&ctx).await.unwrap();
    }

    #[tokio::test]
    async fn unapply_unknown_overlay_errors() {
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

    #[tokio::test]
    async fn unapply_missing_target_reports_already_absent() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.child("root");
        root.create_dir_all().unwrap();
        let ov = tmp.path().join("myoverlay");
        fs::create_dir_all(&ov).unwrap();
        fs::write(ov.join("over.toml"), "target = \"~/sub\"").unwrap();

        let cli = make_cli(tmp.path().to_path_buf());
        let result = execute(
            &cli,
            &params(Some("myoverlay"), root.path().to_path_buf(), false),
        )
        .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn unapply_removes_applied_symlink_end_to_end() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.child("root");
        root.create_dir_all().unwrap();
        let ov = tmp.path().join("myoverlay");
        fs::create_dir_all(&ov).unwrap();
        fs::write(ov.join("over.toml"), "target = \"~\"").unwrap();
        fs::write(ov.join("file.txt"), "content").unwrap();

        apply_overlay(tmp.path(), root.path(), "myoverlay").await;
        let cli = make_cli(tmp.path().to_path_buf());
        let target = root.path().join("file.txt");
        assert!(target.is_symlink());

        let result = execute(
            &cli,
            &params(Some("myoverlay"), root.path().to_path_buf(), false),
        )
        .await;
        assert!(result.is_ok(), "unapply should succeed: {:?}", result.err());
        assert!(!target.exists() && !target.is_symlink());
        // Overlay source untouched.
        assert!(ov.join("file.txt").exists());
    }

    #[tokio::test]
    async fn unapply_dry_run_does_not_touch_filesystem() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.child("root");
        root.create_dir_all().unwrap();
        let ov = tmp.path().join("myoverlay");
        fs::create_dir_all(&ov).unwrap();
        fs::write(ov.join("over.toml"), "target = \"~\"").unwrap();
        fs::write(ov.join("file.txt"), "content").unwrap();

        apply_overlay(tmp.path(), root.path(), "myoverlay").await;
        let cli = make_cli(tmp.path().to_path_buf());
        let target = root.path().join("file.txt");
        assert!(target.is_symlink());

        let result = execute(
            &cli,
            &params(Some("myoverlay"), root.path().to_path_buf(), true),
        )
        .await;
        assert!(result.is_ok());
        assert!(target.is_symlink(), "dry-run must not remove anything");
    }

    #[tokio::test]
    async fn unapply_leaves_foreign_file_untouched() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.child("root");
        root.create_dir_all().unwrap();
        let ov = tmp.path().join("conflicted");
        fs::create_dir_all(&ov).unwrap();
        fs::write(ov.join("over.toml"), "target = \"~\"").unwrap();
        fs::write(ov.join("file.txt"), "overlay").unwrap();
        fs::write(root.path().join("file.txt"), "existing").unwrap();

        let cli = make_cli(tmp.path().to_path_buf());
        let result = execute(
            &cli,
            &params(Some("conflicted"), root.path().to_path_buf(), false),
        )
        .await;
        assert!(
            result.is_err(),
            "not-owned entries should be reported as needing attention"
        );
        assert_eq!(
            fs::read_to_string(root.path().join("file.txt")).unwrap(),
            "existing"
        );
    }
}
