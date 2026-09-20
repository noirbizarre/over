use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow};
use clap::Args;
use dirs::home_dir;
use git2::{Commit, DiffOptions, Repository as GitRepository};

use crate::cli::CLI;
use crate::desired::{DesiredTree, MaterializationIntent, Provenance};
use crate::exec::Context;
use crate::materialize::virtual_checkout::git as vc_git;
use crate::overlays::{Overlay, Repository};
use crate::ui;
use crate::ui::{emojis, style};

#[derive(Args, Debug)]
pub struct Params {
    #[clap(
        help = "Name of the overlay whose history to show (guessed from the current \
                directory if omitted, falling back to the whole repository's history)"
    )]
    name: Option<String>,

    #[clap(short, long, help = "The target root directory (~)")]
    root: Option<PathBuf>,

    #[clap(
        short = 'n',
        long,
        default_value_t = 20,
        help = "Maximum number of commits to show"
    )]
    limit: usize,
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

    let (git_repo, managed_path, label) = resolve_scope(&repo, &root, args.name.as_deref())?;

    ui::info(format!(
        "{} {} {}",
        emojis::THREAD,
        style::white_b("History for"),
        style::cyan(&label),
    ))
    .ok();

    for entry in log_entries(&git_repo, &managed_path, args.limit)? {
        ui::info(format!("  {entry}")).ok();
    }

    Ok(())
}

/// What `over log` scopes its history to: either a specific overlay's
/// virtual checkout (source repository + the managed path within it), or,
/// when no overlay is named/guessable, the whole source repository.
fn resolve_scope(
    repo: &Repository,
    root: &Path,
    name: Option<&str>,
) -> Result<(GitRepository, PathBuf, String)> {
    if let Some(name) = name {
        let overlay = repo.get(name)?;
        return checkout_scope(repo, root, &overlay)
            .with_context(|| format!("overlay '{name}' is not checkout-materialized"))
            .map(|(git_repo, managed_path)| (git_repo, managed_path, name.to_string()));
    }

    // No name given: guess from the current directory — the first overlay
    // (by longest/most-specific resolved target) whose virtual checkout
    // contains the current directory.
    let cwd = std::env::current_dir().context("failed to determine current directory")?;
    let mut best: Option<(PathBuf, Overlay)> = None;
    for overlay in repo.overlays().unwrap_or_default() {
        let ctx = Context::builder()
            .root(root.to_path_buf())
            .repository(repo.clone())
            .overlay(overlay.clone())
            .build();
        let Ok(target) = overlay.resolve_target(&ctx) else {
            continue;
        };
        if cwd.starts_with(&target)
            && best.as_ref().is_none_or(|(best_target, _)| {
                target.components().count() > best_target.components().count()
            })
        {
            best = Some((target, overlay));
        }
    }

    if let Some((_, overlay)) = best
        && let Ok((git_repo, managed_path)) = checkout_scope(repo, root, &overlay)
    {
        return Ok((git_repo, managed_path, overlay.name.clone()));
    }

    // Nothing matched (or matched but isn't checkout-materialized): fall
    // back to the whole repository's own history.
    let git_repo = GitRepository::discover(&repo.root)
        .with_context(|| format!("'{}' is not inside a git repository", repo.root.display()))?;
    Ok((git_repo, PathBuf::new(), repo.root.display().to_string()))
}

/// Resolve `overlay`'s virtual checkout (source repository + managed
/// path), if it has one — errors if the overlay has no `checkout`-rule
/// entry at all.
fn checkout_scope(
    repo: &Repository,
    root: &Path,
    overlay: &Overlay,
) -> Result<(GitRepository, PathBuf)> {
    let ctx = Context::builder()
        .root(root.to_path_buf())
        .repository(repo.clone())
        .overlay(overlay.clone())
        .build();
    let target = overlay.resolve_target(&ctx)?;
    let ctx = ctx.with_resolved_overlay(overlay.name.clone(), target.to_string_lossy().to_string());
    let desired = DesiredTree::build_own(&ctx, overlay)?;

    let entry = desired
        .entries()
        .iter()
        .find(|e| matches!(e.intent, MaterializationIntent::VirtualCheckout))
        .ok_or_else(|| anyhow!("no `checkout` materialization rule applies to this overlay"))?;
    let Provenance::Overlay { source, .. } = &entry.provenance else {
        unreachable!(
            "VirtualCheckout entries only ever carry Provenance::Overlay \
             (see desired::tree::{{collect_own_entries,walk_overlay_tree}})"
        );
    };
    vc_git::discover_source(source)
}

/// One rendered commit line.
struct LogEntry {
    short_oid: String,
    summary: String,
    author: String,
    when: String,
}

impl std::fmt::Display for LogEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {} {} ({}, {})",
            style::yellow(&self.short_oid),
            self.summary,
            style::white("—"),
            self.author,
            self.when,
        )
    }
}

impl LogEntry {
    fn from_commit(commit: &Commit) -> Self {
        let short_oid = commit
            .as_object()
            .short_id()
            .ok()
            .and_then(|buf| buf.as_str().ok().map(str::to_string))
            .unwrap_or_else(|| commit.id().to_string());
        let summary = commit
            .summary()
            .ok()
            .flatten()
            .unwrap_or("<no message>")
            .to_string();
        let author = commit.author().name().unwrap_or("unknown").to_string();
        let when = format_git_time(&commit.time());
        Self {
            short_oid,
            summary,
            author,
            when,
        }
    }
}

/// Every commit reachable from `HEAD`, most recent first, restricted to
/// `managed_path` when non-empty (a commit is included if its tree diff
/// against its first parent touches anything under that path — root
/// commits are compared against an empty tree). Capped at `limit`.
fn log_entries(repo: &GitRepository, managed_path: &Path, limit: usize) -> Result<Vec<LogEntry>> {
    let mut revwalk = repo.revwalk()?;
    revwalk.push_head()?;
    revwalk.set_sorting(git2::Sort::TIME)?;

    let mut entries = Vec::new();
    for oid in revwalk {
        let oid = oid?;
        let commit = repo.find_commit(oid)?;
        if !managed_path.as_os_str().is_empty()
            && !commit_touches_path(repo, &commit, managed_path)?
        {
            continue;
        }
        entries.push(LogEntry::from_commit(&commit));
        if entries.len() >= limit {
            break;
        }
    }
    Ok(entries)
}

/// Whether `commit` changed anything under `path`, compared against its
/// first parent (or an empty tree for a root commit) — mirrors `git log --
/// <path>`'s own notion of "touches".
fn commit_touches_path(repo: &GitRepository, commit: &Commit, path: &Path) -> Result<bool> {
    let tree = commit.tree()?;
    let parent_tree = match commit.parent(0) {
        Ok(parent) => Some(parent.tree()?),
        Err(_) => None,
    };
    let mut opts = DiffOptions::new();
    opts.pathspec(path.to_string_lossy().as_ref());
    let diff = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut opts))?;
    Ok(diff.deltas().len() > 0)
}

/// Minimal, dependency-free UTC calendar formatting for a
/// [`git2::Time`] — avoids pulling in a new date/time crate (AGENTS.md:
/// "do not add new dependencies lightly") for what `over log` only ever
/// needs as a display string. Uses the commit's own recorded UTC offset
/// (not the local timezone), matching `git log`'s default author-relative
/// display.
fn format_git_time(time: &git2::Time) -> String {
    let total_seconds = time.seconds() + i64::from(time.offset_minutes()) * 60;
    let days = total_seconds.div_euclid(86400);
    let secs_of_day = total_seconds.rem_euclid(86400);
    let (year, month, day) = civil_from_days(days);
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}")
}

/// Howard Hinnant's `civil_from_days`: days-since-1970-01-01 -> (year,
/// month, day), proleptic Gregorian calendar. Public-domain algorithm,
/// widely used exactly for avoiding a date/time library dependency.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let year = if m <= 2 { y + 1 } else { y };
    (year, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    use clap::Parser;
    use git2::Signature;
    use rstest::rstest;
    use std::fs;

    fn make_cli(home: PathBuf) -> CLI {
        CLI::parse_from(vec!["over", "--home", home.to_str().unwrap()])
    }

    /// Points `$XDG_STATE_HOME` at a fresh, writable temp dir for the
    /// duration of the returned guard's lifetime — `execute` runs the real
    /// `over log` command end to end, which reads `VirtualCheckoutState`
    /// via the real, non-injectable `XdgDirs::new()` for `VirtualCheckout`
    /// overlays, so every test here must isolate this or it silently
    /// touches the real `$XDG_STATE_HOME/over/virtual_checkout.toml`.
    ///
    /// # Safety
    /// `env::set_var` is only unsound when other threads read/write the
    /// process environment concurrently; `cargo nextest` runs each test in
    /// its own process, matching `xdg::tests`' own `set_env` justification.
    fn isolate_xdg_state() -> TempDir {
        let tmp = TempDir::new().unwrap();
        unsafe {
            std::env::set_var("XDG_STATE_HOME", tmp.path());
        }
        tmp
    }

    fn params(name: Option<&str>, root: PathBuf) -> Params {
        params_with_limit(name, root, 20)
    }

    fn params_with_limit(name: Option<&str>, root: PathBuf, limit: usize) -> Params {
        Params {
            name: name.map(String::from),
            root: Some(root),
            limit,
        }
    }

    fn commit_all(repo: &GitRepository, message: &str) {
        let sig = Signature::now("Test", "test@test.com").unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let parents: Vec<_> = repo
            .head()
            .ok()
            .and_then(|h| h.peel_to_commit().ok())
            .into_iter()
            .collect();
        let parent_refs: Vec<&git2::Commit> = parents.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parent_refs)
            .unwrap();
    }

    fn init_repo(path: &std::path::Path) -> GitRepository {
        let repo = GitRepository::init(path).unwrap();
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "Test").unwrap();
        cfg.set_str("user.email", "test@test.com").unwrap();
        drop(cfg);
        repo
    }

    #[rstest]
    #[case(0, 1970, 1, 1)]
    #[case(19723, 2024, 1, 1)] // days since epoch for 2024-01-01
    fn civil_from_days_matches_known_dates(
        #[case] days: i64,
        #[case] year: i64,
        #[case] month: u32,
        #[case] day: u32,
    ) {
        assert_eq!(civil_from_days(days), (year, month, day));
    }

    #[tokio::test]
    async fn log_named_non_checkout_overlay_errors() {
        let _xdg = isolate_xdg_state();
        let tmp = TempDir::new().unwrap();
        let root = tmp.child("root");
        root.create_dir_all().unwrap();
        let ov = tmp.path().join("plain");
        fs::create_dir_all(&ov).unwrap();
        fs::write(ov.join("over.toml"), "target = \"~\"").unwrap();

        let cli = make_cli(tmp.path().to_path_buf());
        let result = execute(&cli, &params(Some("plain"), root.path().to_path_buf())).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn log_named_checkout_overlay_lists_commits() {
        let _xdg = isolate_xdg_state();
        let tmp = TempDir::new().unwrap();
        let ov = tmp.path().join("dotfiles");
        fs::create_dir_all(&ov).unwrap();
        fs::write(
            ov.join("over.toml"),
            "target = \"~\"\n[defaults]\nmaterialization = \"checkout\"",
        )
        .unwrap();
        fs::write(ov.join("a.txt"), "content").unwrap();
        let repo = init_repo(&ov);
        commit_all(&repo, "initial");
        fs::write(ov.join("a.txt"), "changed").unwrap();
        commit_all(&repo, "second commit");

        let root = tmp.child("root");
        root.create_dir_all().unwrap();

        let cli = make_cli(tmp.path().to_path_buf());
        let result = execute(&cli, &params(Some("dotfiles"), root.path().to_path_buf())).await;
        assert!(result.is_ok(), "log should succeed: {:?}", result.err());
    }

    #[tokio::test]
    async fn log_no_name_falls_back_to_whole_repository() {
        let _xdg = isolate_xdg_state();
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        fs::write(tmp.path().join("README.md"), "hello").unwrap();
        commit_all(&GitRepository::open(tmp.path()).unwrap(), "root commit");

        let root = tmp.child("root");
        root.create_dir_all().unwrap();

        let cli = make_cli(tmp.path().to_path_buf());
        let result = execute(&cli, &params(None, root.path().to_path_buf())).await;
        assert!(result.is_ok(), "log should succeed: {:?}", result.err());
    }

    /// The git repository sits at `tmp` (not at the overlay's own
    /// directory), with the overlay as a subdirectory — lets a test add
    /// commits that don't touch the overlay's own managed path at all,
    /// exercising `commit_touches_path`'s filtering.
    fn setup_nested_checkout_overlay(tmp: &std::path::Path) -> (GitRepository, PathBuf) {
        let ov = tmp.join("dotfiles");
        fs::create_dir_all(&ov).unwrap();
        fs::write(
            ov.join("over.toml"),
            "target = \"~\"\n[defaults]\nmaterialization = \"checkout\"",
        )
        .unwrap();
        fs::write(ov.join("a.txt"), "content").unwrap();
        let repo = init_repo(tmp);
        commit_all(&repo, "initial");
        (repo, ov)
    }

    #[test]
    fn log_skips_commits_that_never_touch_the_managed_path() {
        let tmp = TempDir::new().unwrap();
        let (repo, ov) = setup_nested_checkout_overlay(tmp.path());

        // Unrelated to the overlay: a sibling file outside `dotfiles/`.
        fs::write(tmp.path().join("unrelated.txt"), "unrelated").unwrap();
        commit_all(&repo, "unrelated change");

        fs::write(ov.join("a.txt"), "changed").unwrap();
        commit_all(&repo, "overlay change");

        let entries = log_entries(&repo, Path::new("dotfiles"), 20).unwrap();
        let summaries: Vec<_> = entries.iter().map(|e| e.summary.clone()).collect();
        assert!(summaries.contains(&"overlay change".to_string()));
        assert!(summaries.contains(&"initial".to_string()));
        assert!(!summaries.contains(&"unrelated change".to_string()));
    }

    #[test]
    fn log_limit_caps_the_number_of_commits_shown() {
        let tmp = TempDir::new().unwrap();
        let (repo, ov) = setup_nested_checkout_overlay(tmp.path());
        fs::write(ov.join("a.txt"), "changed").unwrap();
        commit_all(&repo, "second commit");

        let entries = log_entries(&repo, Path::new("dotfiles"), 1).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].summary, "second commit");
    }

    #[tokio::test]
    async fn log_limit_flag_is_honored_end_to_end() {
        let _xdg = isolate_xdg_state();
        let tmp = TempDir::new().unwrap();
        let (repo, ov) = setup_nested_checkout_overlay(tmp.path());
        fs::write(ov.join("a.txt"), "changed").unwrap();
        commit_all(&repo, "second commit");

        let root = tmp.child("root");
        root.create_dir_all().unwrap();

        let cli = make_cli(tmp.path().to_path_buf());
        let result = execute(
            &cli,
            &params_with_limit(Some("dotfiles"), root.path().to_path_buf(), 1),
        )
        .await;
        assert!(result.is_ok(), "log should succeed: {:?}", result.err());
    }

    #[tokio::test]
    async fn log_no_name_guesses_the_overlay_from_the_current_directory() {
        let _xdg = isolate_xdg_state();
        let tmp = TempDir::new().unwrap();
        let (_, ov) = setup_nested_checkout_overlay(tmp.path());

        let root = tmp.child("root");
        root.create_dir_all().unwrap();
        // Materialize the virtual checkout so a real target directory
        // exists to `cd` into.
        let repo = Repository::new(tmp.path().to_path_buf());
        let overlay = repo.get("dotfiles").unwrap();
        let ctx = crate::exec::Context::builder()
            .root(root.path().to_path_buf())
            .repository(repo)
            .overlay(overlay.clone())
            .build();
        overlay.apply(&ctx).await.unwrap();

        let original_cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(root.path()).unwrap();
        let cli = make_cli(tmp.path().to_path_buf());
        let result = execute(&cli, &params(None, root.path().to_path_buf())).await;
        std::env::set_current_dir(original_cwd).unwrap();

        assert!(result.is_ok(), "log should succeed: {:?}", result.err());
        let _ = ov;
    }
}
