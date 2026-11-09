use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow};
use clap::Args;
use dirs::home_dir;
use git2::{Commit, DiffOptions, Repository as GitRepository};

use crate::cli::CLI;
use crate::desired::{CheckoutScope, DesiredTree, Provenance};
use crate::exec::Context;
use crate::materialize::virtual_checkout::git as vc_git;
use crate::overlays::{Overlay, Repository};
use crate::ui;
use crate::ui::{emojis, style};

#[derive(Args, Debug)]
pub struct Params {
    #[clap(
        value_name = "NAME_OR_REVISION",
        help = "Select an overlay or Git revision/range (overlay names take precedence)"
    )]
    name: Option<String>,

    #[clap(
        long,
        help = "Select an overlay explicitly, interpreting the positional argument as a revision"
    )]
    overlay: Option<String>,

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

    let (name, revision) = select_input(&repo, args)?;
    let (git_repo, managed_path, label) = resolve_scope(&repo, &root, name)?;
    let entries = log_scoped_entries(&git_repo, &managed_path, args.limit, revision)?;
    let colors = log_colors_enabled();

    ui::info(format!(
        "{} {} {}",
        emojis::THREAD,
        style::bold("History for").force_styling(colors),
        style::cyan(&label).force_styling(colors),
    ))?;

    for entry in entries {
        ui::info(format!("  {}", entry.render(colors)))?;
    }

    Ok(())
}

fn select_input<'a>(
    repo: &Repository,
    args: &'a Params,
) -> Result<(Option<&'a str>, Option<&'a str>)> {
    if let Some(overlay) = args.overlay.as_deref() {
        return Ok((Some(overlay), args.name.as_deref()));
    }
    if let Some(input) = args.name.as_deref() {
        // Match Repository::get without hiding malformed overlays behind revision errors.
        if repo.root.join(input).try_exists()? {
            return Ok((Some(input), None));
        }
    }
    Ok((None, args.name.as_deref()))
}

fn log_colors_enabled() -> bool {
    // Forced color must not leak escape sequences into redirected history or override NO_COLOR.
    std::io::stdout().is_terminal()
        && std::env::var_os("NO_COLOR").is_none_or(|value| value.is_empty())
        && console::colors_enabled()
}

/// What `over log` scopes its history to: either a specific overlay's
/// virtual checkout (source repository + the managed path within it), or,
/// when no overlay is named/guessable, the whole source repository.
#[derive(Clone)]
struct HistoryScope {
    managed: PathBuf,
    scope: CheckoutScope,
}

fn resolve_scope(
    repo: &Repository,
    root: &Path,
    name: Option<&str>,
) -> Result<(GitRepository, Vec<HistoryScope>, String)> {
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
    let mut best: Option<(usize, GitRepository, HistoryScope, String)> = None;
    for overlay in repo.overlays()? {
        let ctx = Context::builder()
            .root(root.to_path_buf())
            .repository(repo.clone())
            .overlay(overlay.clone())
            .build();
        let ctx = ctx.resolve_data()?;
        let desired = match DesiredTree::build_own(&ctx, &overlay) {
            Ok(desired) => desired,
            Err(error) => {
                // Inference must not make unrelated machine-specific configs mandatory.
                tracing::debug!(overlay = %overlay.name, %error, "skipping unresolved history candidate");
                continue;
            }
        };
        let routing = overlay.resolve_mappings(&ctx)?;
        let Ok(logical) = routing.logical_path_for(&cwd) else {
            continue;
        };
        let routed_cwd = routing.target_for(&logical);
        for entry in desired
            .entries()
            .iter()
            .filter(|entry| entry.intent.is_virtual_checkout())
        {
            let specificity = entry.target.components().count();
            if crate::stage::owns(entry, &routed_cwd)
                && best.as_ref().is_none_or(|(depth, ..)| specificity > *depth)
            {
                let Provenance::Overlay { source, .. } = &entry.provenance else {
                    unreachable!()
                };
                let (git_repo, managed) = match vc_git::discover_source(source) {
                    Ok(source) => source,
                    Err(error) => {
                        tracing::debug!(overlay = %overlay.name, %error, "skipping unresolved history source");
                        continue;
                    }
                };
                best = Some((
                    specificity,
                    git_repo,
                    HistoryScope {
                        managed,
                        scope: entry.intent.checkout_scope(),
                    },
                    overlay.name.clone(),
                ));
            }
        }
    }

    if let Some((_, git_repo, scope, name)) = best {
        return Ok((git_repo, vec![scope], name));
    }

    // Nothing matched (or matched but isn't checkout-materialized): fall
    // back to the whole repository's own history.
    let git_repo = GitRepository::discover(&repo.root)
        .with_context(|| format!("'{}' is not inside a git repository", repo.root.display()))?;
    Ok((
        git_repo,
        vec![HistoryScope {
            managed: PathBuf::new(),
            scope: CheckoutScope::default(),
        }],
        repo.root.display().to_string(),
    ))
}

/// Resolve `overlay`'s virtual checkout (source repository + managed
/// path), if it has one — errors if the overlay has no `checkout`-rule
/// entry at all.
fn checkout_scope(
    repo: &Repository,
    root: &Path,
    overlay: &Overlay,
) -> Result<(GitRepository, Vec<HistoryScope>)> {
    let ctx = Context::builder()
        .root(root.to_path_buf())
        .repository(repo.clone())
        .overlay(overlay.clone())
        .build();
    let ctx = ctx.resolve_data()?;
    let target = overlay.resolve_target(&ctx)?;
    let ctx = ctx.with_resolved_overlay(overlay.name.clone(), target.to_string_lossy().to_string());
    let desired = DesiredTree::build_own(&ctx, overlay)?;

    let mut repository: Option<GitRepository> = None;
    let mut scopes = Vec::new();
    for entry in desired
        .entries()
        .iter()
        .filter(|entry| entry.intent.is_virtual_checkout())
    {
        let Provenance::Overlay { source, .. } = &entry.provenance else {
            unreachable!(
                "VirtualCheckout entries only ever carry Provenance::Overlay \
             (see desired::tree::{{collect_own_entries,walk_overlay_tree}})"
            );
        };
        let (repo, managed) = vc_git::discover_source(source)?;
        if let Some(existing) = &repository {
            anyhow::ensure!(
                existing.path() == repo.path(),
                "checkout scopes belong to different repositories"
            );
        } else {
            repository = Some(repo);
        }
        scopes.push(HistoryScope {
            managed,
            scope: entry.intent.checkout_scope(),
        });
    }
    Ok((
        repository.context("no `checkout` materialization rule applies to this overlay")?,
        scopes,
    ))
}

/// One rendered commit line.
struct LogEntry {
    short_oid: String,
    summary: String,
    author: String,
    when: String,
    decorations: Vec<Decoration>,
}

impl LogEntry {
    fn render(&self, colors: bool) -> String {
        let decorations = self
            .decorations
            .iter()
            .map(|decoration| {
                let styled = if decoration.tag {
                    style::yellow(&decoration.label)
                } else {
                    style::cyan(&decoration.label)
                };
                styled.force_styling(colors).to_string()
            })
            .collect::<Vec<_>>();
        let refs = if decorations.is_empty() {
            String::new()
        } else {
            format!(" ({})", decorations.join(", "))
        };
        format!(
            "{} {}{} {}",
            style::yellow(&self.short_oid).force_styling(colors),
            style::bold(&self.summary).force_styling(colors),
            refs,
            style::dim(format!("— ({}, {})", self.author, self.when)).force_styling(colors),
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
            decorations: Vec::new(),
        }
    }
}

/// Every commit reachable from `HEAD`, most recent first, restricted to
/// `managed_path` when non-empty (a commit is included if its tree diff
/// against its first parent touches anything under that path — root
/// commits are compared against an empty tree). Capped at `limit`.
#[cfg(test)]
fn log_entries(repo: &GitRepository, managed_path: &Path, limit: usize) -> Result<Vec<LogEntry>> {
    log_scoped_entries(
        repo,
        &[HistoryScope {
            managed: managed_path.to_path_buf(),
            scope: CheckoutScope::default(),
        }],
        limit,
        None,
    )
}

fn log_scoped_entries(
    repo: &GitRepository,
    scopes: &[HistoryScope],
    limit: usize,
    revision: Option<&str>,
) -> Result<Vec<LogEntry>> {
    let mut revwalk = repo.revwalk()?;
    configure_walk(repo, &mut revwalk, revision).with_context(|| {
        format!(
            "invalid or unsupported revision '{}'",
            revision.unwrap_or("HEAD")
        )
    })?;
    revwalk.set_sorting(git2::Sort::TIME)?;
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut decorations = reference_decorations(repo)?;

    let mut entries = Vec::new();
    for oid in revwalk {
        let oid = oid?;
        let commit = repo.find_commit(oid)?;
        let mut touches = false;
        for scope in scopes {
            touches |= commit_touches_scope(repo, &commit, scope)?;
        }
        if !touches {
            continue;
        }
        let mut entry = LogEntry::from_commit(&commit);
        entry.decorations = decorations.remove(&oid).unwrap_or_default();
        entries.push(entry);
        if entries.len() >= limit {
            break;
        }
    }
    Ok(entries)
}

fn configure_walk(
    repo: &GitRepository,
    walk: &mut git2::Revwalk<'_>,
    revision: Option<&str>,
) -> Result<()> {
    let Some(revision) = revision else {
        walk.push_head()?;
        return Ok(());
    };
    anyhow::ensure!(
        !revision.contains("^@") && !revision.contains("^!") && !revision.contains("^-"),
        "revision sets are unsupported; use a single commit-ish or two-/three-dot range"
    );
    let spec = repo.revparse(revision)?;
    let from = spec
        .from()
        .context("revision has no starting object")?
        .peel_to_commit()?;
    if spec.mode().contains(git2::RevparseMode::RANGE) {
        let to = spec
            .to()
            .context("range has no ending object")?
            .peel_to_commit()?;
        walk.push(to.id())?;
        if spec.mode().contains(git2::RevparseMode::MERGE_BASE) {
            walk.push(from.id())?;
            match repo.merge_bases(from.id(), to.id()) {
                Ok(bases) => {
                    // Hiding only one base leaks shared history in criss-cross merges.
                    for base in bases.iter() {
                        walk.hide(*base)?;
                    }
                }
                Err(error) if error.code() == git2::ErrorCode::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        } else {
            walk.hide(from.id())?;
        }
    } else {
        let (_, reference) = repo.revparse_ext(revision)?;
        // Only literal branch refs are shorthand; ancestry/reflog expressions keep Git semantics.
        let branch = if let Some(reference) = reference {
            let name = reference.name()?;
            revision != "HEAD"
                && (reference.is_branch() || reference.is_remote())
                && (name == revision
                    || reference.shorthand()? == revision
                    || name.strip_prefix("refs/") == Some(revision))
        } else {
            false
        };
        if branch {
            walk.push_head()?;
            walk.hide(from.id())?;
        } else {
            walk.push(from.id())?;
        }
    }
    Ok(())
}

struct Decoration {
    label: String,
    tag: bool,
}

fn reference_decorations(repo: &GitRepository) -> Result<BTreeMap<git2::Oid, Vec<Decoration>>> {
    let mut decorations: BTreeMap<_, Vec<_>> = BTreeMap::new();
    let head = match repo.head() {
        Ok(head) => Some(head),
        Err(error)
            if matches!(
                error.code(),
                git2::ErrorCode::UnbornBranch | git2::ErrorCode::NotFound
            ) =>
        {
            None
        }
        Err(error) => return Err(error.into()),
    };
    let mut refs = repo
        .references()?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    refs.sort_by(|left, right| left.name_bytes().cmp(right.name_bytes()));
    for reference in refs {
        if !(reference.is_branch() || reference.is_remote() || reference.is_tag()) {
            continue;
        }
        // Symbolic remote HEAD aliases duplicate the actual branch decoration.
        if reference.symbolic_target()?.is_some() {
            continue;
        }
        let object = reference.peel(git2::ObjectType::Any)?;
        if object.kind() != Some(git2::ObjectType::Commit) {
            continue;
        }
        let name = reference.shorthand().unwrap_or("<non-UTF-8 ref>");
        let current = head
            .as_ref()
            .is_some_and(|head| head.is_branch() && head.name_bytes() == reference.name_bytes());
        let label = if current {
            format!("HEAD -> {name}")
        } else if reference.is_tag() {
            format!("tag: {name}")
        } else {
            name.to_string()
        };
        decorations
            .entry(object.id())
            .or_default()
            .push(Decoration {
                label,
                tag: reference.is_tag(),
            });
    }
    if repo.head_detached()?
        && let Some(head) = head
    {
        decorations
            .entry(head.peel_to_commit()?.id())
            .or_default()
            .insert(
                0,
                Decoration {
                    label: "HEAD".into(),
                    tag: false,
                },
            );
    }
    Ok(decorations)
}

/// Whether `commit` changed anything under `path`, compared against its
/// first parent (or an empty tree for a root commit), without Git's
/// path-history simplification.
fn commit_touches_scope(
    repo: &GitRepository,
    commit: &Commit,
    scope: &HistoryScope,
) -> Result<bool> {
    let tree = commit.tree()?;
    let parent_tree = match commit.parent(0) {
        Ok(parent) => Some(parent.tree()?),
        Err(_) => None,
    };
    let mut opts = DiffOptions::new();
    if !scope.managed.as_os_str().is_empty() {
        opts.pathspec(scope.managed.to_string_lossy().as_ref());
    }
    let diff = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut opts))?;
    Ok(diff.deltas().any(|delta| {
        [delta.old_file().path(), delta.new_file().path()]
            .into_iter()
            .flatten()
            .any(|path| {
                path.strip_prefix(&scope.managed)
                    .is_ok_and(|relative| scope.scope.owns_source(relative))
            })
    }))
}

/// Minimal, dependency-free UTC calendar formatting for a
/// [`git2::Time`] — avoids pulling in a new date/time crate (AGENTS.md:
/// "do not add new dependencies lightly") for what `over log` only ever
/// needs as a display string. Uses the commit's own recorded UTC offset
/// (not the local timezone), preserving the recorded committer timestamp.
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
            overlay: None,
            root: Some(root),
            limit,
        }
    }

    #[test]
    fn explicit_overlay_and_revision_parse_with_a_limit() {
        let cli = CLI::parse_from([
            "over",
            "log",
            "--overlay",
            "dotfiles",
            "main..HEAD",
            "-n",
            "3",
        ]);
        let Some(crate::cli::Commands::Log(args)) = cli.cmd else {
            panic!("expected log command");
        };
        assert_eq!(args.overlay.as_deref(), Some("dotfiles"));
        assert_eq!(args.name.as_deref(), Some("main..HEAD"));
        assert_eq!(args.limit, 3);
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

    fn graph_commit(repo: &GitRepository, message: &str, parents: &[git2::Oid]) -> git2::Oid {
        let blob = repo.blob(message.as_bytes()).unwrap();
        let mut builder = repo.treebuilder(None).unwrap();
        builder.insert("file.txt", blob, 0o100644).unwrap();
        let tree = repo.find_tree(builder.write().unwrap()).unwrap();
        let parents: Vec<_> = parents
            .iter()
            .map(|oid| repo.find_commit(*oid).unwrap())
            .collect();
        let parents: Vec<_> = parents.iter().collect();
        let signature = Signature::now("Test", "test@test.com").unwrap();
        repo.commit(None, &signature, &signature, message, &tree, &parents)
            .unwrap()
    }

    fn selected_commits(repo: &GitRepository, revision: &str) -> Vec<git2::Oid> {
        let mut walk = repo.revwalk().unwrap();
        configure_walk(repo, &mut walk, Some(revision)).unwrap();
        let mut commits = walk.collect::<std::result::Result<Vec<_>, _>>().unwrap();
        commits.sort();
        commits
    }

    #[test]
    fn branches_are_shorthand_while_tags_and_expressions_keep_git_semantics() {
        let tmp = TempDir::new().unwrap();
        let repo = init_repo(tmp.path());
        let base = graph_commit(&repo, "base", &[]);
        let main = graph_commit(&repo, "main only", &[base]);
        let feature = graph_commit(&repo, "feature only", &[base]);
        repo.reference("refs/heads/main", main, false, "test")
            .unwrap();
        repo.reference("refs/heads/feature", feature, false, "test")
            .unwrap();
        repo.reference("refs/remotes/origin/main", main, false, "test")
            .unwrap();
        repo.set_head("refs/heads/feature").unwrap();
        repo.tag_lightweight("light", &repo.find_object(base, None).unwrap(), false)
            .unwrap();
        repo.tag(
            "annotated",
            &repo.find_object(base, None).unwrap(),
            &Signature::now("Test", "test@test.com").unwrap(),
            "release",
            false,
        )
        .unwrap();
        for revision in [
            "main",
            "refs/heads/main",
            "heads/main",
            "origin/main",
            "main..HEAD",
            "light..HEAD",
            "annotated..HEAD",
            "HEAD~1..HEAD",
            "main..",
        ] {
            assert_eq!(
                selected_commits(&repo, revision),
                vec![feature],
                "{revision}"
            );
        }
        for revision in ["light", "annotated", "HEAD~1", &base.to_string()] {
            assert_eq!(selected_commits(&repo, revision), vec![base], "{revision}");
        }
        let mut symmetric = vec![main, feature];
        symmetric.sort();
        assert_eq!(selected_commits(&repo, "main...feature"), symmetric);
        let mut reachable = vec![base, feature];
        reachable.sort();
        assert_eq!(selected_commits(&repo, "HEAD"), reachable);
        assert_eq!(selected_commits(&repo, "@"), reachable);
        let attached = reference_decorations(&repo).unwrap();
        assert!(
            attached[&feature]
                .iter()
                .any(|item| item.label == "HEAD -> feature")
        );
        // Git resolves a colliding tag before a branch; it must not become branch shorthand.
        repo.tag_lightweight("main", &repo.find_object(base, None).unwrap(), false)
            .unwrap();
        assert_eq!(selected_commits(&repo, "main"), vec![base]);
        repo.set_head_detached(feature).unwrap();
        assert_eq!(selected_commits(&repo, "HEAD"), reachable);
        let decorations = reference_decorations(&repo).unwrap();
        assert!(
            decorations[&base]
                .iter()
                .any(|item| item.label == "tag: annotated")
        );
        assert!(
            decorations[&feature]
                .iter()
                .any(|item| item.label == "HEAD")
        );
    }

    #[test]
    fn symmetric_ranges_exclude_all_merge_bases_and_allow_unrelated_histories() {
        let tmp = TempDir::new().unwrap();
        let repo = init_repo(tmp.path());
        let base = graph_commit(&repo, "base", &[]);
        let a = graph_commit(&repo, "a", &[base]);
        let b = graph_commit(&repo, "b", &[base]);
        let left = graph_commit(&repo, "left", &[a, b]);
        let right = graph_commit(&repo, "right", &[b, a]);
        let unrelated = graph_commit(&repo, "unrelated", &[]);
        for (from, to, mut expected) in [
            (left, right, vec![left, right]),
            (a, unrelated, vec![base, a, unrelated]),
        ] {
            expected.sort();
            assert_eq!(selected_commits(&repo, &format!("{from}...{to}")), expected);
        }
    }

    #[rstest]
    #[case("missing")]
    #[case("HEAD^!")]
    #[case("HEAD^@")]
    #[case("HEAD^-")]
    #[case("HEAD^{tree}")]
    fn invalid_or_non_commit_revisions_report_errors(#[case] revision: &str) {
        let tmp = TempDir::new().unwrap();
        let repo = init_repo(tmp.path());
        let base = graph_commit(&repo, "base", &[]);
        repo.set_head_detached(base).unwrap();
        assert!(configure_walk(&repo, &mut repo.revwalk().unwrap(), Some(revision)).is_err());
    }

    #[test]
    fn overlay_precedence_preserves_errors_and_explicit_selection_accepts_revisions() {
        let tmp = TempDir::new().unwrap();
        tmp.child("main").create_dir_all().unwrap();
        let repository = Repository::new(tmp.path().to_path_buf());
        let mut args = params(Some("main"), tmp.path().to_path_buf());
        assert_eq!(
            select_input(&repository, &args).unwrap(),
            (Some("main"), None)
        );
        args.name = Some("missing".into());
        assert_eq!(
            select_input(&repository, &args).unwrap(),
            (None, Some("missing"))
        );
        args.overlay = Some("main".into());
        assert_eq!(
            select_input(&repository, &args).unwrap(),
            (Some("main"), Some("missing"))
        );
    }

    #[test]
    fn ranges_filter_paths_before_applying_the_limit() {
        let tmp = TempDir::new().unwrap();
        let (repo, overlay) = setup_nested_checkout_overlay(tmp.path());
        let base = repo.head().unwrap().target().unwrap();
        fs::write(overlay.join("a.txt"), "changed").unwrap();
        commit_all(&repo, "scoped change");
        fs::write(tmp.path().join("unrelated.txt"), "unrelated").unwrap();
        commit_all(&repo, "unrelated change");
        let scopes = [HistoryScope {
            managed: "dotfiles".into(),
            scope: CheckoutScope::default(),
        }];
        let range = format!("{base}..HEAD");
        let entries = log_scoped_entries(&repo, &scopes, 1, Some(&range)).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].summary, "scoped change");
        let plain = entries[0].render(false);
        assert!(plain.contains("Test"));
        assert!(!plain.contains('\x1b'));
        assert!(entries[0].render(true).contains('\x1b'));
        assert!(
            log_scoped_entries(&repo, &scopes, 0, Some(&range))
                .unwrap()
                .is_empty()
        );
    }

    #[rstest]
    #[case("target='~'\n[map.'../invalid']\ntarget='elsewhere'", "mapping source")]
    #[case("target='{{ missing_variable('", "render")]
    fn unnamed_history_skips_invalid_candidates_but_named_history_reports_them(
        #[case] config: &str,
        #[case] error: &str,
        #[values(false, true)] infer: bool,
    ) {
        let tmp = TempDir::new().unwrap();
        let (git_repo, _) = setup_nested_checkout_overlay(tmp.path());
        tmp.child("invalid/over.toml").write_str(config).unwrap();
        let repo = Repository::new(tmp.path().to_path_buf());
        // Matching the real cwd avoids process-global cwd changes while testing inference.
        let root = if infer {
            std::env::current_dir().unwrap()
        } else {
            tmp.path().join("unrelated-target")
        };
        let (resolved, scopes, label) = resolve_scope(&repo, &root, None).unwrap();
        assert_eq!(resolved.path(), git_repo.path());
        assert_eq!(scopes.len(), 1);
        if infer {
            assert_eq!(label, "dotfiles");
            assert_eq!(scopes[0].managed, Path::new("dotfiles"));
        } else {
            assert_eq!(label, tmp.path().display().to_string());
            assert!(scopes[0].managed.as_os_str().is_empty());
        }
        let explicit_error = resolve_scope(&repo, &root, Some("invalid")).err().unwrap();
        assert!(format!("{explicit_error:#}").contains(error));
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
        fs::write(ov.join("over.toml"), "target = \"~\"\nmode = \"checkout\"").unwrap();
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
        fs::write(ov.join("over.toml"), "target = \"~\"\nmode = \"checkout\"").unwrap();
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
