use std::path::PathBuf;

use anyhow::{Context as _, Result, anyhow, bail};
use clap::Args;
use dirs::home_dir;

use crate::cli::CLI;
use crate::cli::common::select_overlay;
use crate::desired::DesiredTree;
use crate::exec::Context;
use crate::git_exclude;
use crate::overlays::Repository;
use crate::ui;
use crate::ui::{emojis, style};
use crate::unapply::{Outcome, Report};
use crate::utils::short_path;

#[derive(Args, Debug)]
pub struct Params {
    #[clap(
        help = "Name of the overlay to unapply (otherwise infer context, then use default_overlay)"
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

    #[clap(
        long,
        short = 'n',
        help = "Report what would be removed without removing anything"
    )]
    dry_run: bool,

    #[clap(
        long,
        help = "Discard owned real checkouts, including local files and Git history"
    )]
    discard_checkout: bool,

    #[clap(long, help = "Leave risky checkouts untouched instead of prompting")]
    no_prompt: bool,
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

    let ctx = Context::builder()
        .dry_run(args.dry_run)
        .discard_checkout(args.discard_checkout)
        .no_prompt(args.no_prompt)
        .debug(cli.debug)
        .verbose(cli.verbose)
        .root(root)
        .repository(repo)
        .build();

    let selection_ctx = ctx.clone();
    let name = args.name.clone();
    let overlay =
        tokio::task::spawn_blocking(move || select_overlay(&selection_ctx, name.as_deref()))
            .await??;
    if cli.debug {
        tracing::debug!(?overlay, "resolved overlay");
    }

    let ctx = ctx.with_overlay(overlay.clone()).resolve_data()?;
    let target = overlay.resolve_target(&ctx)?;
    let ctx = ctx.with_resolved_overlay(overlay.name.clone(), target.to_string_lossy().to_string());

    // This overlay's own entries only — no `uses` recursion. Unapplying a
    // dependency here could silently break another overlay that still
    // `uses` it (a diamond dependency): removing a shared dependency's
    // entries is left to explicitly unapplying it by name.
    let desired = DesiredTree::build_own(&ctx, &overlay)?;
    let report = Report::build(&desired)?;
    let report = if ctx.dry_run {
        report.execute(ctx.clone()).await?
    } else {
        report
    };

    ui::info(format!(
        "{} {} {} {} {}",
        emojis::PACKAGE,
        style::heading(if ctx.dry_run {
            "Unapply preview for overlay"
        } else {
            "Overlay"
        }),
        style::overlay(&overlay.name),
        style::white_b("->"),
        style::path(short_path(&target.to_string_lossy())),
    ))
    .ok();
    ui::info(format!("  {}", report.counts())).ok();

    for entry_outcome in report.entries() {
        if cli.verbose || !matches!(entry_outcome.outcome, Outcome::AlreadyAbsent) {
            ui::info(format!("  {entry_outcome}")).ok();
        }
    }
    ui::info("").ok();

    let report = report.execute(ctx.clone()).await?;
    if !ctx.dry_run {
        ui::info("Unapply results:").ok();
        for outcome in report.entries() {
            ui::info(format!("  {outcome}")).ok();
        }
    }

    // #147: this overlay's own entries are unapplied — remove its exclude
    // block from every repo `desired` says it touched, reusing the same
    // `desired` tree `Report::build` used above. Retained plaintext is the
    // exception: keep its exclusion rather than exposing it to accidental staging.
    // Dry-run must not alter either block.
    if !ctx.dry_run
        && !report.entries().iter().any(|outcome| {
            matches!(
                outcome.entry.intent,
                crate::desired::MaterializationIntent::EncryptedFile(_)
            ) && !matches!(outcome.outcome, Outcome::Removed | Outcome::AlreadyAbsent)
        })
    {
        git_exclude::unreconcile_desired(&desired).with_context(|| {
            format!(
                "failed to remove git excludes for overlay '{}'",
                overlay.name
            )
        })?;
    }

    if report.needs_attention() {
        bail!("one or more entries were left untouched — see `skip:` lines above");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
            discard_checkout: false,
            no_prompt: true,
        }
    }

    /// Apply `name` from the repository rooted at `home` into `root`,
    /// bypassing the CLI layer (mirrors the pattern already used by
    /// `plan::reconcile`/`status`/`diff`'s own tests).
    async fn apply_overlay(home: &std::path::Path, root: &std::path::Path, name: &str) {
        let repo = Repository::new(home.to_path_buf());
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

    /// The `.git/info/exclude` block content for `myoverlay` written by
    /// `over apply`'s own git-exclude wiring (#146) when `root` is itself a
    /// git repository.
    fn exclude_content(root: &std::path::Path) -> String {
        fs::read_to_string(root.join(".git/info/exclude")).unwrap_or_default()
    }

    #[tokio::test]
    async fn unapply_removes_the_overlays_exclude_block() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.child("root");
        root.create_dir_all().unwrap();
        git2::Repository::init(root.path()).unwrap();
        let ov = tmp.path().join("myoverlay");
        fs::create_dir_all(&ov).unwrap();
        fs::write(ov.join("over.toml"), "target = \"~\"").unwrap();
        fs::write(ov.join("file.txt"), "content").unwrap();

        apply_overlay(tmp.path(), root.path(), "myoverlay").await;
        assert!(
            exclude_content(root.path()).contains("exclude:myoverlay"),
            "apply should have excluded the symlink from this repo's git status"
        );

        let cli = make_cli(tmp.path().to_path_buf());
        let result = execute(
            &cli,
            &params(Some("myoverlay"), root.path().to_path_buf(), false),
        )
        .await;
        assert!(result.is_ok(), "unapply should succeed: {:?}", result.err());
        assert!(
            !exclude_content(root.path()).contains("exclude:myoverlay"),
            "unapply should have removed this overlay's exclude block"
        );
    }

    #[tokio::test]
    async fn unapply_dry_run_does_not_touch_the_exclude_file() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.child("root");
        root.create_dir_all().unwrap();
        git2::Repository::init(root.path()).unwrap();
        let ov = tmp.path().join("myoverlay");
        fs::create_dir_all(&ov).unwrap();
        fs::write(ov.join("over.toml"), "target = \"~\"").unwrap();
        fs::write(ov.join("file.txt"), "content").unwrap();

        apply_overlay(tmp.path(), root.path(), "myoverlay").await;
        assert!(exclude_content(root.path()).contains("exclude:myoverlay"));

        let cli = make_cli(tmp.path().to_path_buf());
        let result = execute(
            &cli,
            &params(Some("myoverlay"), root.path().to_path_buf(), true),
        )
        .await;
        assert!(result.is_ok());
        assert!(
            exclude_content(root.path()).contains("exclude:myoverlay"),
            "dry-run must not touch the exclude block"
        );
    }
}
