use std::path::PathBuf;

use anyhow::{Result, anyhow};
use clap::Args;
use dialoguer::Input;
use dirs::home_dir;

use crate::cli::CLI;
use crate::cli::common::select_overlay;
use crate::commit::{self, CommitOptions, CommitOutcome};
use crate::desired::{DesiredTree, MaterializationIntent};
use crate::exec::Context;
use crate::overlays::Repository;
use crate::ui;
use crate::ui::style::DialogTheme;
use crate::ui::{emojis, style};
use crate::utils::short_path;

#[derive(Args, Debug)]
pub struct Params {
    #[clap(help = "Name of the overlay to commit (uses default_overlay if configured)")]
    name: Option<String>,

    #[clap(short, long, help = "The target root directory (~)")]
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
    let repo = Repository::new(home);
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

    let overlay = select_overlay(
        &ctx.repository,
        &ctx,
        args.name.as_deref(),
        "Choose the overlay to commit",
    )?;
    if cli.debug {
        tracing::debug!(?overlay, "resolved overlay");
    }

    let ctx = ctx.with_overlay(overlay.clone());
    let target = overlay.resolve_target(&ctx)?;
    let ctx = ctx.with_resolved_overlay(overlay.name.clone(), target.to_string_lossy().to_string());

    // This overlay's own entries only — never a `uses` dependency's: that
    // content belongs to a different overlay (and possibly a different
    // source repository entirely), which the user didn't name here.
    let desired = DesiredTree::build_own(&ctx, &overlay)?;
    let checkout_entries: Vec<_> = desired
        .entries()
        .iter()
        .filter(|e| matches!(e.intent, MaterializationIntent::VirtualCheckout))
        .collect();

    if checkout_entries.is_empty() {
        return Err(anyhow!(
            "overlay '{}' is not checkout-materialized (no `checkout` materialization rule \
             applies) — nothing for `over commit` to do",
            overlay.name,
        ));
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

    let mut needs_attention = false;
    for entry in checkout_entries {
        // Only bother asking for a message once we know there's actually
        // something to commit — never prompt for a no-op.
        let pending = crate::status::virtual_checkout::file_changes(entry)?;
        let outcome = if pending.is_empty() {
            CommitOutcome::NothingToCommit
        } else {
            let message = resolve_message(args, &entry.target)?;
            commit::commit(entry, &CommitOptions { message }).await?
        };
        ui::info(format!(
            "  {} {}",
            short_path(&entry.target.to_string_lossy()),
            outcome
        ))
        .ok();
        needs_attention |= outcome.needs_attention();
    }

    if needs_attention {
        return Err(anyhow!(
            "one or more virtual checkouts have conflicting files — resolve them manually, \
             then re-run `over commit`"
        ));
    }

    Ok(())
}

/// `--message` always wins; `--no-prompt` falls through to
/// [`commit::default_message`]'s auto-generated text (by passing `None`
/// through); otherwise ask interactively, allowing an empty answer to mean
/// the same "use the default" (never forces the user to type something
/// just to accept the default).
fn resolve_message(args: &Params, target: &std::path::Path) -> Result<Option<String>> {
    if let Some(message) = &args.message {
        return Ok(Some(message.clone()));
    }
    if args.no_prompt {
        return Ok(None);
    }
    let input = Input::<String>::with_theme(&DialogTheme::default())
        .with_prompt(format!(
            "Commit message for {}",
            short_path(&target.to_string_lossy())
        ))
        .allow_empty(true)
        .interact_text()
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

    fn make_cli(home: PathBuf) -> CLI {
        CLI::parse_from(vec!["over", "--home", home.to_str().unwrap()])
    }

    fn params(name: Option<&str>, root: PathBuf, message: Option<&str>) -> Params {
        Params {
            name: name.map(String::from),
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
        let tmp = TempDir::new().unwrap();
        let ov = tmp.path().join("dotfiles");
        fs::create_dir_all(&ov).unwrap();
        fs::write(
            ov.join("over.toml"),
            "target = \"~\"\n[defaults]\nmaterialization = \"checkout\"",
        )
        .unwrap();
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
        let tmp = TempDir::new().unwrap();
        let ov = tmp.path().join("dotfiles");
        fs::create_dir_all(&ov).unwrap();
        fs::write(
            ov.join("over.toml"),
            "target = \"~\"\n[defaults]\nmaterialization = \"checkout\"",
        )
        .unwrap();
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
}
