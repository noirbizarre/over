use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use clap::Args;
use dirs::home_dir;

use crate::cli::CLI;
use crate::desired::DesiredTree;
use crate::exec::Context;
use crate::overlays::{Overlay, Repository};
use crate::sync::{self, SyncOptions, SyncOutcome};
use crate::ui;
use crate::ui::{emojis, style};
use crate::utils::short_path;

#[derive(Args, Debug)]
pub struct Params {
    #[clap(help = "Name of the overlay to sync (all overlays if omitted)")]
    name: Option<String>,

    #[clap(short, long, help = "The target root directory (~)")]
    root: Option<PathBuf>,

    #[clap(long, help = "Do not process uses")]
    no_uses: bool,

    #[clap(
        long,
        conflicts_with = "push_only",
        help = "Only pull upstream changes into the checkout"
    )]
    pull_only: bool,

    #[clap(
        long,
        conflicts_with = "pull_only",
        help = "Only push local commits to the overlay source"
    )]
    push_only: bool,

    // Deliberately has no effect on `SyncOptions`: a plain re-run of `over
    // sync` already behaves identically once conflicts are resolved (git's
    // own `repo.state()` is what's actually being "continued", not any
    // over-side flag) — kept purely for discoverability/intent-signaling,
    // per #110's ask for an explicit `sync --continue` workflow.
    #[clap(
        long = "continue",
        conflicts_with = "abort",
        help = "Re-attempt a sync after resolving conflicts manually (alias for a plain re-run)"
    )]
    continue_: bool,

    #[clap(long, help = "Abort an in-progress merge left by a conflicted sync")]
    abort: bool,

    #[clap(long, short = 'n', help = "Report what would happen without syncing")]
    dry_run: bool,
}

impl Params {
    fn options(&self) -> SyncOptions {
        SyncOptions {
            pull: !self.push_only,
            push: !self.pull_only,
            abort: self.abort,
            dry_run: self.dry_run,
        }
    }
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
        ui::info("No overlays found.").ok();
        return Ok(());
    }

    let opts = args.options();
    let mut needs_attention = false;
    for overlay in &overlays {
        needs_attention |= sync_overlay(cli, &repo, &root, overlay, args.no_uses, &opts).await?;
    }

    if needs_attention {
        return Err(anyhow!(
            "one or more checkouts need attention — see `conflict`/`blocked` entries above"
        ));
    }
    Ok(())
}

async fn sync_overlay(
    cli: &CLI,
    repo: &Repository,
    root: &Path,
    overlay: &Overlay,
    no_uses: bool,
    opts: &SyncOptions,
) -> Result<bool> {
    let ctx = Context::builder()
        .root(root.to_path_buf())
        .repository(repo.clone())
        .overlay(overlay.clone())
        .no_uses(no_uses)
        .build();
    let target = overlay.resolve_target(&ctx)?;
    let ctx = ctx.with_resolved_overlay(overlay.name.clone(), target.to_string_lossy().to_string());

    // Full `uses` graph (not `build_own`): sync reconciles everything an
    // overlay pulls in, exactly like `status`/`diff`/`Overlay::apply` do.
    let desired = DesiredTree::build(&ctx, overlay)?;
    let outcomes = sync::sync(&desired, opts).await?;

    if outcomes.is_empty() {
        if cli.verbose {
            ui::info(format!(
                "{} {} {} {}",
                emojis::PACKAGE,
                style::white_b("Overlay"),
                style::cyan(&overlay.name),
                style::white("has no checkout-materialized root entry"),
            ))
            .ok();
        }
        return Ok(false);
    }

    ui::info(format!(
        "{} {} {} {} {}",
        emojis::PACKAGE,
        style::white_b("Overlay"),
        style::cyan(&overlay.name),
        style::white_b("->"),
        style::cyan(&short_path(&target.to_string_lossy())),
    ))
    .ok();
    // Unconditional summary (mirrors `status::Report::counts()`), so a
    // plain `over sync` still reports e.g. "1 up to date" without needing
    // `--verbose` — only the noisier per-checkout lines below are gated.
    ui::info(format!("  {}", summarize(&outcomes))).ok();

    let mut needs_attention = false;
    for outcome in &outcomes {
        needs_attention |= outcome.needs_attention();
        if cli.verbose || !matches!(outcome, SyncOutcome::UpToDate { .. }) {
            ui::info(format!("  {outcome}")).ok();
        }
    }
    ui::info("").ok();

    Ok(needs_attention)
}

/// One-line digest of every outcome for an overlay, e.g. "1 up to date, 1
/// pushed, 1 conflict(s)". Only non-zero counts are listed.
fn summarize(outcomes: &[SyncOutcome]) -> String {
    let mut counts: Vec<(&str, usize)> = vec![
        ("up to date", 0),
        ("fast-forwarded", 0),
        ("merged", 0),
        ("pushed", 0),
        ("conflict(s)", 0),
        ("blocked", 0),
        ("aborted", 0),
    ];
    for outcome in outcomes {
        let idx = match outcome {
            SyncOutcome::UpToDate { .. } => 0,
            SyncOutcome::FastForwarded { .. } => 1,
            SyncOutcome::Merged { .. } => 2,
            SyncOutcome::Pushed { .. } => 3,
            SyncOutcome::Conflict { .. } => 4,
            SyncOutcome::Blocked { .. } => 5,
            SyncOutcome::Aborted { .. } => 6,
        };
        counts[idx].1 += 1;
    }
    counts
        .into_iter()
        .filter(|(_, n)| *n > 0)
        .map(|(label, n)| format!("{n} {label}"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    use clap::Parser;
    use git2::Signature;
    use std::fs;

    fn make_cli(home: PathBuf) -> CLI {
        CLI::parse_from(vec!["over", "--home", home.to_str().unwrap()])
    }

    fn params(name: Option<&str>, root: PathBuf) -> Params {
        Params {
            name: name.map(String::from),
            root: Some(root),
            no_uses: false,
            pull_only: false,
            push_only: false,
            continue_: false,
            abort: false,
            dry_run: false,
        }
    }

    fn init_committed_repo(path: &std::path::Path) {
        let repo = git2::Repository::init(path).unwrap();
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "Test").unwrap();
        cfg.set_str("user.email", "test@test.com").unwrap();
        drop(cfg);
        let sig = Signature::now("Test", "test@test.com").unwrap();
        fs::write(path.join("README.md"), "# Test").unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
    }

    #[tokio::test]
    async fn sync_empty_repository() {
        let tmp = TempDir::new().unwrap();
        let cli = make_cli(tmp.path().to_path_buf());
        let result = execute(&cli, &params(None, tmp.path().to_path_buf())).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn sync_overlay_without_git_reports_nothing_to_sync() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.child("root");
        root.create_dir_all().unwrap();
        let ov = tmp.path().join("plain");
        fs::create_dir_all(&ov).unwrap();
        fs::write(ov.join("over.toml"), "target = \"~\"").unwrap();

        let cli = make_cli(tmp.path().to_path_buf());
        let result = execute(&cli, &params(None, root.path().to_path_buf())).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn sync_up_to_date_checkout_succeeds() {
        let tmp = TempDir::new().unwrap();
        let source_td = TempDir::new().unwrap();
        init_committed_repo(source_td.path());

        let root = tmp.child("root");
        root.create_dir_all().unwrap();
        let ov = tmp.path().join("dotfiles");
        fs::create_dir_all(&ov).unwrap();
        fs::write(
            ov.join("over.toml"),
            format!(
                "target = \"~\"\ngit = \"{}\"",
                source_td.path().to_str().unwrap().replace('\\', "\\\\")
            ),
        )
        .unwrap();

        // Materialize the checkout first (mirrors `over apply`'s clone
        // step) — `over sync` itself never clones.
        let root_target = root.path();
        git2::build::RepoBuilder::new()
            .clone(source_td.path().to_str().unwrap(), root_target)
            .unwrap();

        let cli = make_cli(tmp.path().to_path_buf());
        let result = execute(&cli, &params(Some("dotfiles"), root.path().to_path_buf())).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn sync_unknown_overlay_errors() {
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

    #[test]
    fn options_default_syncs_both_directions() {
        let p = params(None, PathBuf::from("/tmp"));
        let opts = p.options();
        assert!(opts.pull);
        assert!(opts.push);
        assert!(!opts.abort);
        assert!(!opts.dry_run);
    }

    #[test]
    fn options_pull_only_disables_push() {
        let mut p = params(None, PathBuf::from("/tmp"));
        p.pull_only = true;
        let opts = p.options();
        assert!(opts.pull);
        assert!(!opts.push);
    }

    #[test]
    fn options_push_only_disables_pull() {
        let mut p = params(None, PathBuf::from("/tmp"));
        p.push_only = true;
        let opts = p.options();
        assert!(!opts.pull);
        assert!(opts.push);
    }

    #[test]
    fn summarize_lists_only_non_zero_counts() {
        let outcomes = vec![
            SyncOutcome::UpToDate {
                path: PathBuf::from("/a"),
            },
            SyncOutcome::UpToDate {
                path: PathBuf::from("/b"),
            },
            SyncOutcome::Pushed {
                path: PathBuf::from("/c"),
            },
        ];
        let summary = summarize(&outcomes);
        assert_eq!(summary, "2 up to date, 1 pushed");
    }

    #[test]
    fn summarize_empty_outcomes_is_empty_string() {
        assert_eq!(summarize(&[]), "");
    }
}
