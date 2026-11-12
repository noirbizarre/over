use std::path::PathBuf;

use anyhow::{Result, anyhow, bail};
use clap::Args;
use dirs::home_dir;

use crate::cli::CLI;
use crate::commit::{self, CommitOptions, CommitOutcome};
use crate::desired::DesiredTree;
use crate::exec::Context;
use crate::overlays::Repository;
use crate::ui;
use crate::ui::{emojis, style};
use crate::utils::short_path;

#[derive(Args, Debug)]
pub struct Params {
    #[clap(
        long = "overlay",
        help = "Select the overlay (otherwise infer context, then use default_overlay)"
    )]
    name: Option<String>,

    #[clap(
        value_name = "PATH",
        help = "Commit staged paths (defaults to all owned staged changes)"
    )]
    paths: Vec<PathBuf>,

    #[clap(
        short = 'r',
        long = "target",
        value_name = "TARGET",
        env = "OVER_TARGET",
        help = "Set the target directory (defaults to ~)"
    )]
    root: Option<PathBuf>,

    #[clap(short, long, help = "Commit message")]
    message: Option<String>,

    #[clap(
        long,
        help = "Never prompt; use --message or an auto-generated message"
    )]
    no_prompt: bool,
}

pub async fn execute(cli: &CLI, args: &Params) -> Result<()> {
    if cli.debug {
        tracing::debug!(?cli, ?args, "CLI args");
    }

    let home = cli.resolve_home()?;
    let repo = Repository::new(home).collecting_warnings().scoped_loading();
    let root = args
        .root
        .clone()
        .or_else(home_dir)
        .ok_or_else(|| anyhow!("could not determine home directory"))?;

    let ctx = Context::builder()
        .debug(cli.debug)
        .verbose(cli.verbose)
        .no_prompt(args.no_prompt)
        .root(root)
        .repository(repo)
        .build();

    let selection_ctx = ctx.clone();
    let name = args.name.clone();
    let overlay = tokio::task::spawn_blocking(move || {
        crate::selection::select_commit(&selection_ctx, name.as_deref())
    })
    .await??;
    if cli.debug {
        tracing::debug!(?overlay, "resolved overlay");
    }

    let ctx = ctx.with_overlay(overlay.clone()).resolve_data()?;
    let target = overlay.resolve_target(&ctx)?;
    let ctx = ctx.with_resolved_overlay(overlay.name.clone(), target.to_string_lossy().to_string());

    // This overlay's own entries only — never a `uses` dependency's: that
    // content belongs to a different overlay (and possibly a different
    // source repository entirely), which the user didn't name here.
    let prepared_ctx = ctx.clone();
    let prepared_overlay = overlay.clone();
    let desired = tokio::task::spawn_blocking(move || {
        DesiredTree::build_commit(&prepared_ctx, &prepared_overlay)
    })
    .await??;
    // Metadata inheritance may load dependencies, but their diagnostics belong to their own commands.
    for warning in ctx
        .repository
        .warnings
        .warnings()
        .into_iter()
        .filter(|warning| {
            warning
                .file
                .parent()
                .is_some_and(|directory| overlay.root.starts_with(directory))
        })
    {
        tracing::warn!(
            "{}: {}; {}",
            warning.file.display(),
            warning.message,
            warning.hint
        );
    }
    let checkout_entries: Vec<_> = desired
        .entries()
        .iter()
        .filter(|e| e.intent.is_virtual_checkout())
        .collect();

    if checkout_entries.is_empty() {
        bail!(
            "overlay '{}' is not checkout-materialized (no `checkout` materialization rule \
             applies) — nothing for `over commit` to do",
            overlay.name,
        );
    }

    ui::info(format!(
        "{} {} {} {} {}",
        emojis::PACKAGE,
        style::heading("Overlay"),
        style::overlay(&overlay.name),
        style::white_b("->"),
        style::path(short_path(&target.to_string_lossy())),
    ))
    .ok();

    let mut needs_attention = false;
    let owned_entries: Vec<_> = checkout_entries.into_iter().cloned().collect();
    let cwd = ctx.absolute_path(std::path::Path::new("."))?;
    let physical_source = crate::overlays::mapping::physical(&overlay.root)?;
    let routing = overlay.resolve_mappings(&ctx)?;
    let contextual = cwd.starts_with(&overlay.root)
        || cwd.starts_with(&physical_source)
        || !matches!(
            routing.reverse(&cwd)?,
            crate::overlays::mapping::ReverseResolution::Absent
        );
    let anchor = if contextual { &cwd } else { &overlay.root };
    let paths = args
        .paths
        .iter()
        .map(|path| -> Result<_> {
            let path = crate::overlays::mapping::absolute(&anchor.join(path))?;
            if path.starts_with(&overlay.root) {
                return Ok(path);
            }
            let location = crate::overlays::mapping::location(&path)?;
            if let Ok(relative) = location.strip_prefix(&physical_source) {
                return Ok(overlay.root.join(relative));
            }
            match routing.reverse(&path)? {
                crate::overlays::mapping::ReverseResolution::Unique(found) => {
                    Ok(routing.target_for(&found.source))
                }
                crate::overlays::mapping::ReverseResolution::Ambiguous(_) => {
                    routing.logical_path_for(&path)?;
                    unreachable!("ambiguous reverse routing returns an error")
                }
                crate::overlays::mapping::ReverseResolution::Absent => Ok(path),
            }
        })
        .collect::<Result<Vec<_>>>()?;
    for path in &paths {
        if !path.starts_with(&overlay.root)
            && !owned_entries
                .iter()
                .any(|entry| path.starts_with(&entry.target) || entry.target.starts_with(path))
        {
            bail!(
                "path '{}' is outside overlay '{}'",
                path.display(),
                overlay.name
            );
        }
    }
    let mut options = CommitOptions {
        paths,
        ..Default::default()
    };
    // Only bother asking for a message once we know there's actually
    // something to commit — never prompt for a no-op.
    let preflight_entries = owned_entries.clone();
    let preflight_options = options.clone();
    let (pending, conflicting_files, snapshot) =
        tokio::task::spawn_blocking(move || -> Result<_> {
            let snapshot = commit::snapshot(&preflight_entries)?;
            let pending = commit::pending(&preflight_entries, &preflight_options)?;
            let conflicts = preflight_entries
                .iter()
                .map(|entry| {
                    crate::status::virtual_checkout::commit_conflicts(
                        entry,
                        &preflight_options.paths,
                    )
                })
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();
            anyhow::ensure!(
                commit::snapshot(&preflight_entries)? == snapshot,
                "source HEAD or index changed while preparing the commit; retry the command"
            );
            Ok((pending, conflicts, snapshot))
        })
        .await??;
    let outcome = if !conflicting_files.is_empty() {
        CommitOutcome::Blocked { conflicting_files }
    } else if !pending {
        CommitOutcome::NothingToCommit
    } else {
        let message = resolve_message(args, &target)?;
        options.message = message;
        commit::commit_entries_checked(&owned_entries, &options, Some(snapshot)).await?
    };
    ui::info(format!(
        "  {} {}",
        short_path(&target.to_string_lossy()),
        outcome
    ))
    .ok();
    needs_attention |= outcome.needs_attention();

    if needs_attention {
        bail!(
            "one or more virtual checkouts have conflicting files — resolve them manually, \
             then re-run `over commit`"
        );
    }

    Ok(())
}

/// `--message` always wins; `--no-prompt` falls through to
/// [`commit::default_message`]'s auto-generated text (by passing `None`
/// through); otherwise ask interactively, allowing an empty answer to mean
/// the same "use the default" (never forces the user to type something
/// just to accept the default).
fn resolve_message(args: &Params, target: &std::path::Path) -> Result<Option<String>> {
    let _pause = crate::ui::progress::pause()?;
    if let Some(message) = &args.message {
        return Ok(Some(message.clone()));
    }
    if args.no_prompt {
        return Ok(None);
    }
    let input = ui::message_input(&format!(
        "Commit message for {}",
        short_path(&target.to_string_lossy())
    ))
    .map_err(|e| anyhow!("commit message prompt cancelled: {}", e))?;
    Ok(if input.trim().is_empty() {
        None
    } else {
        Some(input)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    use clap::Parser;
    use git2::Signature;
    use std::fs;

    #[test]
    fn commit_paths_are_never_interpreted_as_overlay_names() {
        let cli = CLI::try_parse_from([
            "over",
            "commit",
            "--overlay",
            "work",
            "-m",
            "message",
            "one",
            "two",
            "--",
            "-three",
        ])
        .unwrap();
        let Some(crate::cli::Commands::Commit(args)) = cli.cmd else {
            panic!("expected commit");
        };
        assert_eq!(args.name.as_deref(), Some("work"));
        assert_eq!(args.message.as_deref(), Some("message"));
        assert_eq!(
            args.paths,
            [
                PathBuf::from("one"),
                PathBuf::from("two"),
                PathBuf::from("-three")
            ]
        );
    }

    fn make_cli(home: PathBuf) -> CLI {
        CLI::parse_from(vec!["over", "--home", home.to_str().unwrap()])
    }

    /// Points `$XDG_STATE_HOME` at a fresh, writable temp dir for the
    /// duration of the returned guard's lifetime — `execute` runs the real
    /// `over commit` command end to end, which persists
    /// `VirtualCheckoutState` via the real, non-injectable
    /// `XdgDirs::new()`, so every test here must isolate this or it
    /// silently pollutes the real
    /// `$XDG_STATE_HOME/over/virtual_checkout.toml`.
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

    fn params(name: Option<&str>, root: PathBuf, message: Option<&str>) -> Params {
        Params {
            name: name.map(String::from),
            paths: Vec::new(),
            root: Some(root),
            message: message.map(String::from),
            no_prompt: message.is_none(),
        }
    }

    fn init_committed_repo(path: &std::path::Path) {
        let repo = git2::Repository::init(path).unwrap();
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "Test").unwrap();
        cfg.set_str("user.email", "test@test.com").unwrap();
        drop(cfg);
        let sig = Signature::now("Test", "test@test.com").unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        {
            let tree = repo.find_tree(tree_id).unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
                .unwrap();
        }
    }

    #[tokio::test]
    async fn commit_non_checkout_overlay_errors() {
        let _xdg = isolate_xdg_state();
        let tmp = TempDir::new().unwrap();
        let root = tmp.child("root");
        root.create_dir_all().unwrap();
        let ov = tmp.path().join("plain");
        fs::create_dir_all(&ov).unwrap();
        fs::write(ov.join("over.toml"), "target = \"~\"").unwrap();

        let cli = make_cli(tmp.path().to_path_buf());
        let result = execute(
            &cli,
            &params(Some("plain"), root.path().to_path_buf(), Some("msg")),
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn commit_end_to_end_records_a_commit_in_the_source_repository() {
        let _xdg = isolate_xdg_state();
        let tmp = TempDir::new().unwrap();
        let ov = tmp.path().join("dotfiles");
        fs::create_dir_all(&ov).unwrap();
        fs::write(ov.join("over.toml"), "target = \"~\"\nmode = \"checkout\"").unwrap();
        fs::write(ov.join("a.txt"), "original\n").unwrap();
        init_committed_repo(&ov);

        let root = tmp.child("root");
        root.create_dir_all().unwrap();

        // Materialize the virtual checkout first (mirrors `over apply`).
        let repo = Repository::new(tmp.path().to_path_buf());
        let overlay = repo.get("dotfiles").unwrap();
        let ctx = Context::builder()
            .root(root.path().to_path_buf())
            .repository(repo)
            .overlay(overlay.clone())
            .build();
        overlay.apply(&ctx).await.unwrap();

        assert!(!root.path().join(".git").exists());
        assert_eq!(
            fs::read_to_string(root.path().join("a.txt")).unwrap(),
            "original\n"
        );

        fs::write(root.path().join("a.txt"), "edited locally\n").unwrap();
        overlay
            .add_files(&ctx, &[root.path().join("a.txt")])
            .await
            .unwrap();

        let cli = make_cli(tmp.path().to_path_buf());
        let result = execute(
            &cli,
            &params(
                Some("dotfiles"),
                root.path().to_path_buf(),
                Some("edit a.txt"),
            ),
        )
        .await;
        assert!(result.is_ok(), "commit should succeed: {:?}", result.err());

        let source_repo = git2::Repository::open(&ov).unwrap();
        let head = source_repo.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(head.message(), Ok("edit a.txt"));
        let tree = head.tree().unwrap();
        let blob = tree
            .get_path(std::path::Path::new("a.txt"))
            .unwrap()
            .to_object(&source_repo)
            .unwrap();
        assert_eq!(blob.as_blob().unwrap().content(), b"edited locally\n");
    }

    #[tokio::test]
    async fn commit_with_no_local_changes_reports_nothing_to_commit_and_succeeds() {
        let _xdg = isolate_xdg_state();
        let tmp = TempDir::new().unwrap();
        let ov = tmp.path().join("dotfiles");
        fs::create_dir_all(&ov).unwrap();
        fs::write(ov.join("over.toml"), "target = \"~\"\nmode = \"checkout\"").unwrap();
        fs::write(ov.join("a.txt"), "original\n").unwrap();
        init_committed_repo(&ov);

        let root = tmp.child("root");
        root.create_dir_all().unwrap();
        let repo = Repository::new(tmp.path().to_path_buf());
        let overlay = repo.get("dotfiles").unwrap();
        let ctx = Context::builder()
            .root(root.path().to_path_buf())
            .repository(repo)
            .overlay(overlay.clone())
            .build();
        overlay.apply(&ctx).await.unwrap();

        let cli = make_cli(tmp.path().to_path_buf());
        let result = execute(
            &cli,
            &params(Some("dotfiles"), root.path().to_path_buf(), None),
        )
        .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn commit_no_prompt_with_no_message_uses_an_auto_generated_message() {
        let _xdg = isolate_xdg_state();
        let tmp = TempDir::new().unwrap();
        let ov = tmp.path().join("dotfiles");
        fs::create_dir_all(&ov).unwrap();
        fs::write(ov.join("over.toml"), "target = \"~\"\nmode = \"checkout\"").unwrap();
        fs::write(ov.join("a.txt"), "original\n").unwrap();
        init_committed_repo(&ov);

        let root = tmp.child("root");
        root.create_dir_all().unwrap();
        let repo = Repository::new(tmp.path().to_path_buf());
        let overlay = repo.get("dotfiles").unwrap();
        let ctx = Context::builder()
            .root(root.path().to_path_buf())
            .repository(repo)
            .overlay(overlay.clone())
            .build();
        overlay.apply(&ctx).await.unwrap();

        // A real local change, but no `--message` and `--no-prompt` set —
        // must fall through to `commit::default_message` rather than
        // reaching the interactive prompt.
        fs::write(root.path().join("a.txt"), "edited\n").unwrap();
        overlay
            .add_files(&ctx, &[root.path().join("a.txt")])
            .await
            .unwrap();

        let cli = make_cli(tmp.path().to_path_buf());
        let result = execute(
            &cli,
            &params(Some("dotfiles"), root.path().to_path_buf(), None),
        )
        .await;
        assert!(result.is_ok(), "commit should succeed: {:?}", result.err());

        let source_repo = git2::Repository::open(&ov).unwrap();
        let head = source_repo.head().unwrap().peel_to_commit().unwrap();
        // `commit::default_message`'s own wording — asserting the exact
        // text here would couple this test too tightly to that module's
        // implementation detail, so just confirm *a* message landed.
        assert!(head.message().unwrap_or_default().contains("file"));
    }

    #[tokio::test]
    async fn commit_with_a_conflicting_file_fails_and_leaves_it_untouched() {
        let _xdg = isolate_xdg_state();
        let tmp = TempDir::new().unwrap();
        let ov = tmp.path().join("dotfiles");
        fs::create_dir_all(&ov).unwrap();
        fs::write(ov.join("over.toml"), "target = \"~\"\nmode = \"checkout\"").unwrap();
        fs::write(ov.join("a.txt"), "original\n").unwrap();
        init_committed_repo(&ov);

        let root = tmp.child("root");
        root.create_dir_all().unwrap();
        let repo = Repository::new(tmp.path().to_path_buf());
        let overlay = repo.get("dotfiles").unwrap();
        let ctx = Context::builder()
            .root(root.path().to_path_buf())
            .repository(repo)
            .overlay(overlay.clone())
            .build();
        overlay.apply(&ctx).await.unwrap();

        // The same file changed on both sides, to different content.
        fs::write(root.path().join("a.txt"), "local edit\n").unwrap();
        fs::write(ov.join("a.txt"), "source edit\n").unwrap();
        let source_repo = git2::Repository::open(&ov).unwrap();
        let sig = Signature::now("Test", "test@test.com").unwrap();
        let mut index = source_repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = source_repo.find_tree(tree_id).unwrap();
        let parent = source_repo.head().unwrap().peel_to_commit().unwrap();
        source_repo
            .commit(
                Some("HEAD"),
                &sig,
                &sig,
                "advance differently",
                &tree,
                &[&parent],
            )
            .unwrap();

        let cli = make_cli(tmp.path().to_path_buf());
        let result = execute(
            &cli,
            &params(Some("dotfiles"), root.path().to_path_buf(), Some("attempt")),
        )
        .await;
        assert!(result.is_err());
        // Untouched: still exactly the local content, never overwritten.
        assert_eq!(
            fs::read_to_string(root.path().join("a.txt")).unwrap(),
            "local edit\n"
        );
    }
}
