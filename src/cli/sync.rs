use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};
use clap::Args;
use dirs::home_dir;

use crate::actions::git::source_sync::SourceRepository;
use crate::cli::CLI;
use crate::desired::DesiredTree;
use crate::exec::Context;
use crate::overlays::{Overlay, Repository, SyncConfig};
use crate::sync::{self, SyncOptions, SyncOutcome};
use crate::ui;
use crate::ui::{emojis, style};
use crate::utils::short_path;

#[derive(Args, Debug)]
pub struct Params {
    #[clap(
        help = "Name of the overlay to sync (infer context, then default_overlay, then all overlays)"
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
        help = "Sync every overlay, retaining configured default_overlay data"
    )]
    all: bool,

    #[clap(
        long,
        conflicts_with = "no_pull",
        help = "Fetch and rebase the Over source repository before local sync"
    )]
    pull: bool,

    #[clap(
        long,
        conflicts_with = "pull",
        help = "Disable source-repository pull for this invocation"
    )]
    no_pull: bool,

    #[clap(
        long,
        conflicts_with = "no_push",
        help = "Push existing source-repository commits after successful local sync"
    )]
    push: bool,

    #[clap(
        long,
        conflicts_with = "push",
        help = "Disable source-repository push for this invocation"
    )]
    no_push: bool,

    #[clap(
        long,
        conflicts_with = "push_only",
        help = "Only refresh virtual checkouts from the local source"
    )]
    pull_only: bool,

    #[clap(
        long,
        conflicts_with = "pull_only",
        help = "Only record virtual-checkout changes in the local source"
    )]
    push_only: bool,

    // Native Git owns recovery state; this legacy flag is only a rerun alias.
    #[clap(
        long = "continue",
        conflicts_with = "abort",
        help = "Re-attempt a sync after resolving conflicts manually (alias for a plain re-run)"
    )]
    continue_: bool,

    #[clap(
        long,
        help = "Report no abortable virtual-checkout operation; resolve source rebases with Git"
    )]
    abort: bool,

    #[clap(long, short = 'n', help = "Report what would happen without syncing")]
    dry_run: bool,

    #[clap(
        long,
        help = "Never prompt to commit a virtual checkout's local changes; report them as blocked instead"
    )]
    no_prompt: bool,
}

impl Params {
    fn source_options(&self, config: SyncConfig) -> SyncConfig {
        SyncConfig {
            pull: self.pull || (config.pull && !self.no_pull),
            push: self.push || (config.push && !self.no_push),
        }
    }

    fn options(&self) -> SyncOptions {
        SyncOptions {
            pull: !self.push_only,
            push: !self.pull_only,
            abort: self.abort,
            dry_run: self.dry_run,
            no_prompt: self.no_prompt,
        }
    }
}

pub async fn execute(cli: &CLI, args: &Params) -> Result<()> {
    if cli.debug {
        tracing::debug!(?cli, ?args, "CLI args");
    }

    // This retained flag must not trigger unrelated configured remote mutations.
    if args.abort {
        bail!(
            "no abortable sync operation; resolve source rebases with `git rebase --abort` (or `git rebase --continue`), then rerun `over sync`"
        );
    }

    let home = cli.resolve_home()?;
    // Check Git state before parsing a descriptor that may contain rebase conflicts.
    let source = SourceRepository::discover(home.clone()).await?;
    let repo = Repository::new(home);
    let remote = args.source_options(repo.sync_config()?);
    let upstream = if remote.pull || remote.push {
        Some(
            source
                .as_ref()
                .ok_or_else(|| {
                    anyhow!("source synchronization requires a Git worktree containing OVER_HOME")
                })?
                .upstream()
                .await?,
        )
    } else {
        None
    };
    if remote.pull {
        let source = source
            .as_ref()
            .expect("upstream validation requires a source");
        if args.dry_run {
            ui::info("Would fetch and rebase the Over source repository (remote state may have changed since the last fetch).").ok();
        } else {
            source
                .pull(upstream.as_ref().expect("validated upstream"))
                .await?;
            // Re-read even for explicit/--all selections, which otherwise might miss an invalid root.
            repo.sync_config()?;
        }
    }
    let root = args
        .root
        .clone()
        .or_else(home_dir)
        .ok_or_else(|| anyhow!("could not determine home directory"))?;

    let overlays = match (&args.name, args.all) {
        (Some(name), _) => vec![repo.get(name)?],
        (None, true) => repo
            .inspect_overlays()?
            .into_iter()
            .collect::<Result<Vec<_>>>()?,
        (None, false) => {
            let ctx = Context::builder()
                .root(root.clone())
                .repository(repo.clone())
                .build();
            match tokio::task::spawn_blocking(move || crate::selection::infer_or_default(&ctx))
                .await??
            {
                Some(overlay) => vec![overlay],
                None => repo
                    .inspect_overlays()?
                    .into_iter()
                    .collect::<Result<Vec<_>>>()?,
            }
        }
    };

    if overlays.is_empty() {
        ui::info("No overlays found.").ok();
    }

    let opts = args.options();
    let mut needs_attention = false;
    for overlay in &overlays {
        needs_attention |= sync_overlay(cli, &repo, &root, overlay, args.no_uses, &opts).await?;
    }

    if needs_attention {
        bail!("one or more checkouts need attention — see `conflict`/`blocked` entries above");
    }
    if remote.push {
        if args.dry_run {
            ui::info(
                "Would push existing Over source commits after successful local reconciliation.",
            )
            .ok();
        } else {
            source
                .as_ref()
                .expect("validated source")
                .push(upstream.as_ref().expect("validated upstream"))
                .await?;
        }
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
    let ctx = ctx.resolve_data()?;
    let target = overlay.resolve_target(&ctx)?;
    let ctx = ctx.with_resolved_overlay(overlay.name.clone(), target.to_string_lossy().to_string());

    // Full `uses` graph (not `build_own`): sync reconciles everything an
    // overlay pulls in, exactly like `status`/`diff`/`Overlay::apply` do.
    let desired = DesiredTree::build(&ctx, overlay)?;
    let outcomes = sync::sync_local(&desired, opts).await?;

    if outcomes.is_empty() {
        if cli.verbose {
            ui::info(format!(
                "{} {} {} {}",
                emojis::PACKAGE,
                style::white_b("Overlay"),
                style::cyan(&overlay.name),
                style::white("has no virtual checkout to reconcile"),
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
        ("committed", 0),
        ("conflict(s)", 0),
        ("blocked", 0),
        ("aborted", 0),
        ("preview(s)", 0),
    ];
    for outcome in outcomes {
        let idx = match outcome {
            SyncOutcome::UpToDate { .. } => 0,
            SyncOutcome::FastForwarded { .. } => 1,
            SyncOutcome::Merged { .. } => 2,
            SyncOutcome::Pushed { .. } => 3,
            SyncOutcome::Committed { .. } => 4,
            SyncOutcome::Conflict { .. } => 5,
            SyncOutcome::Blocked { .. } => 6,
            SyncOutcome::Aborted { .. } => 7,
            SyncOutcome::Preview { .. } => 8,
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
            all: false,
            pull: false,
            no_pull: false,
            push: false,
            no_push: false,
            pull_only: false,
            push_only: false,
            continue_: false,
            abort: false,
            dry_run: false,
            no_prompt: false,
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
        if crate::cli::common::run_in_isolated_process() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        let cli = make_cli(tmp.path().to_path_buf());
        let result = execute(&cli, &params(None, tmp.path().to_path_buf())).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn sync_overlay_without_git_reports_nothing_to_sync() {
        if crate::cli::common::run_in_isolated_process() {
            return;
        }
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
        if crate::cli::common::run_in_isolated_process() {
            return;
        }
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

    /// An omitted `NAME` narrows to just the configured `default_overlay`
    /// instead of syncing every overlay (#128).
    #[tokio::test]
    async fn sync_default_overlay_narrows_omitted_name() {
        if crate::cli::common::run_in_isolated_process() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("over.toml"), "default_overlay = \"plain\"").unwrap();
        let root = tmp.child("root");
        root.create_dir_all().unwrap();
        for name in ["plain", "other"] {
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
    async fn sync_all_flag_overrides_configured_default() {
        if crate::cli::common::run_in_isolated_process() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("over.toml"), "default_overlay = \"plain\"").unwrap();
        let root = tmp.child("root");
        root.create_dir_all().unwrap();
        for name in ["plain", "other"] {
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
    async fn sync_misconfigured_default_overlay_errors() {
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

    #[test]
    fn options_default_syncs_both_directions() {
        let p = params(None, PathBuf::from("/tmp"));
        let opts = p.options();
        assert!(opts.pull);
        assert!(opts.push);
        assert!(!opts.abort);
        assert!(!opts.dry_run);
    }

    #[rstest::rstest]
    fn source_overrides_are_independent(
        #[values(false, true)] pull: bool,
        #[values(false, true)] push: bool,
        #[values(None, Some(true), Some(false))] pull_override: Option<bool>,
        #[values(None, Some(true), Some(false))] push_override: Option<bool>,
    ) {
        let mut p = params(None, PathBuf::new());
        p.pull = pull_override == Some(true);
        p.no_pull = pull_override == Some(false);
        p.push = push_override == Some(true);
        p.no_push = push_override == Some(false);
        assert_eq!(
            p.source_options(SyncConfig { pull, push }),
            SyncConfig {
                pull: pull_override.unwrap_or(pull),
                push: push_override.unwrap_or(push),
            }
        );
    }

    #[rstest::rstest]
    #[case(&["--pull", "--no-pull"])]
    #[case(&["--push", "--no-push"])]
    #[case(&["--fetch"])]
    #[case(&["--no-fetch"])]
    fn contradictory_or_unsupported_remote_flags_are_rejected(#[case] flags: &[&str]) {
        assert!(
            CLI::try_parse_from(["over", "sync"].into_iter().chain(flags.iter().copied())).is_err()
        );
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
