use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};
use clap::Args;
use dialoguer::{FuzzySelect, Input};
use dirs::home_dir;

use crate::actions::symlink::SymlinkConfig;
use crate::cli::CLI;
use crate::cli::common::resolve_inputs_with_missing;
use crate::exec::Context;
use crate::overlays::Repository;
use crate::ui;
use crate::ui::emojis;
use crate::ui::style::{self, DialogTheme};

#[derive(Args, Debug)]
pub struct Params {
    #[clap(required = true, help = "Files, directories, or glob patterns to add")]
    files: Vec<String>,

    #[clap(short, long, help = "Name of the target overlay")]
    overlay: Option<String>,

    #[clap(short, long, help = "The target root directory (~)")]
    root: Option<PathBuf>,

    #[clap(long, short = 'n', help = "Run without applying changes")]
    dry_run: bool,

    #[clap(long, short, help = "Overwrite without prompting")]
    force: bool,

    #[clap(
        short = 'p',
        long,
        help = "Interactively stage virtual-checkout changes by hunk"
    )]
    patch: bool,
}

pub async fn execute(cli: &CLI, args: &Params) -> Result<()> {
    if cli.debug {
        tracing::debug!(?cli, ?args, "add command");
    }

    let home = cli.resolve_home()?;
    let repo = Repository::new(home.clone());
    if cli.debug {
        tracing::debug!(?repo, "repository");
    }

    let overlay = match &args.overlay {
        Some(name) => repo.get(name)?,
        None => {
            let overlays = repo.overlays()?;
            if overlays.is_empty() {
                bail!("no overlays found in repository");
            }
            let selection = FuzzySelect::with_theme(&DialogTheme::default())
                .with_prompt("Choose the target overlay")
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

    let ctx = Context::builder()
        .dry_run(args.dry_run)
        .debug(cli.debug)
        .verbose(cli.verbose)
        .force(args.force)
        .root(
            args.root
                .clone()
                .or_else(home_dir)
                .ok_or_else(|| anyhow!("could not determine home directory"))?,
        )
        .repository(repo)
        .overlay(overlay.clone())
        .build();

    let resolved = resolve_inputs_with_missing(&args.files, true)?;
    let ctx = ctx.resolve_data()?;
    let routing = overlay.resolve_mappings(&ctx)?;
    for path in &resolved {
        routing.logical_path_for(path)?;
    }

    if args.patch {
        // Patch selection must never fall through to move-and-link ingestion.
        let overlay = overlay.clone();
        return tokio::task::spawn_blocking(move || {
            crate::stage::patch::run(&ctx, &overlay, &resolved, crate::ui::patch::select)
        })
        .await?;
    }

    let (symlinks, regulars): (Vec<_>, Vec<_>) = resolved.iter().partition(|p| p.is_symlink());

    // Validate the entire selection before the first prompt can publish a declaration.
    for symlink_path in &symlinks {
        validate_symlink_ingestion(&overlay, symlink_path, &ctx)?;
    }
    for symlink_path in &symlinks {
        add_symlink(&overlay, &home, symlink_path, &ctx)?;
    }

    if !regulars.is_empty() {
        let regular_paths: Vec<PathBuf> = regulars.into_iter().cloned().collect();
        overlay
            .add_files(&ctx, &regular_paths)
            .await
            .inspect_err(|e| {
                ui::warn(format!(
                    "{} {} {}: {}",
                    emojis::CROSSMARK,
                    style::white_b("Failed to add to overlay"),
                    style::cyan(&overlay.name),
                    e,
                ))
                .ok();
            })?;
    }

    Ok(())
}

fn validate_symlink_ingestion(
    overlay: &crate::overlays::Overlay,
    symlink_path: &Path,
    ctx: &Context,
) -> Result<PathBuf> {
    let rel_path = overlay
        .resolve_mappings(ctx)?
        .logical_path_for(symlink_path)?;
    let source = overlay.root.join(&rel_path);
    let policy = crate::overlays::ignore::SourcePolicy::new(&ctx.repository.root, overlay)?;
    anyhow::ensure!(
        policy.includes(&source, symlink_path.is_dir()),
        "cannot ingest ignored content or control metadata '{}'",
        rel_path.display()
    );
    if symlink_path.is_dir() {
        policy.validate_directory_link(symlink_path, &source)?;
    }
    Ok(rel_path)
}

fn add_symlink(
    overlay: &crate::overlays::Overlay,
    home: &Path,
    symlink_path: &Path,
    ctx: &crate::exec::Context,
) -> Result<()> {
    let rel_path = validate_symlink_ingestion(overlay, symlink_path, ctx)?;
    let desired = crate::desired::DesiredTree::build_own(ctx, overlay)?;
    anyhow::ensure!(
        !desired
            .entries()
            .iter()
            .any(|entry| entry.intent.is_virtual_checkout()
                && crate::stage::owns(entry, symlink_path)),
        "cannot stage a symlink in a virtual checkout"
    );
    if ctx.dry_run {
        return Ok(());
    }
    let link_target = std::fs::read_link(symlink_path)
        .map_err(|e| anyhow!("failed to read symlink {}: {}", symlink_path.display(), e))?;

    let default_target = compute_default_target(&link_target, &overlay.root, home);

    let target: String = Input::with_theme(&DialogTheme::default())
        .with_prompt(format!(
            "Target for {}",
            style::cyan(symlink_path.display())
        ))
        .default(default_target)
        .interact_text()
        .map_err(|e| anyhow!("target input cancelled: {}", e))?;

    let link_config_name = rel_path.to_string_lossy();
    let config_path = overlay.root.join(format!("{}.link.toml", link_config_name));

    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let config = SymlinkConfig {
        target,
        r#type: crate::actions::symlink::LinkType::Soft,
    };
    let toml_content = toml::to_string_pretty(&config)?;
    std::fs::write(&config_path, toml_content)?;

    ui::info(format!(
        "{} {} {} {}",
        emojis::CHECKMARK,
        style::white_b("Created symlink config"),
        style::cyan(config_path.display()),
        style::white_b("in overlay"),
    ))
    .ok();

    Ok(())
}

fn compute_default_target(link_target: &Path, overlay_root: &Path, home: &Path) -> String {
    if let Ok(rel) = link_target.strip_prefix(overlay_root) {
        return rel.to_string_lossy().to_string();
    }

    if let Ok(rel) = link_target.strip_prefix(home) {
        return format!("~/{}", rel.display());
    }

    link_target.to_string_lossy().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[rstest::rstest]
    #[case("secret", false)]
    #[case("over.toml", false)]
    #[case("directory", true)]
    fn refused_symlink_ingestion_leaves_sources_and_declarations_untouched(
        #[case] name: &str,
        #[case] directory: bool,
    ) {
        let temp = tempfile::TempDir::new().unwrap();
        let repo_root = temp.path().join("repo");
        let root = temp.path().join("target");
        std::fs::create_dir_all(repo_root.join("ov")).unwrap();
        std::fs::create_dir(&root).unwrap();
        std::fs::write(repo_root.join("ov/.overignore"), "secret\n*.pem\n").unwrap();
        let referent = temp.path().join("referent");
        if directory {
            std::fs::create_dir(&referent).unwrap();
            std::fs::write(referent.join("private.pem"), "private").unwrap();
        } else {
            std::fs::write(&referent, "private").unwrap();
        }
        let link = root.join(name);
        std::os::unix::fs::symlink(&referent, &link).unwrap();
        let repo = Repository::new(repo_root.clone());
        let overlay = repo.get("ov").unwrap();
        let ctx = Context::builder()
            .repository(repo)
            .root(root)
            .dry_run(true)
            .build();
        let result = add_symlink(&overlay, &repo_root, &link, &ctx);
        assert!(result.is_err(), "ingestion must reject {name}");
        assert_eq!(std::fs::read_link(&link).unwrap(), referent);
        assert!(!overlay.root.join(format!("{name}.link.toml")).exists());
        assert_eq!(std::fs::read_dir(&overlay.root).unwrap().count(), 1);
    }

    #[test]
    fn compute_default_target_inside_overlay() {
        let overlay_root = PathBuf::from("/home/user/.over/myoverlay");
        let home = PathBuf::from("/home/user");
        let link_target = PathBuf::from("/home/user/.over/myoverlay/subdir/file.txt");
        let result = compute_default_target(&link_target, &overlay_root, &home);
        assert_eq!(result, "subdir/file.txt");
    }

    #[test]
    fn compute_default_target_inside_home() {
        let overlay_root = PathBuf::from("/home/user/.over/myoverlay");
        let home = PathBuf::from("/home/user");
        let link_target = PathBuf::from("/home/user/.config/nvim");
        let result = compute_default_target(&link_target, &overlay_root, &home);
        assert_eq!(result, "~/.config/nvim");
    }

    #[test]
    fn compute_default_target_outside_home() {
        let overlay_root = PathBuf::from("/home/user/.over/myoverlay");
        let home = PathBuf::from("/home/user");
        let link_target = PathBuf::from("/opt/nvim/bin/nvim");
        let result = compute_default_target(&link_target, &overlay_root, &home);
        assert_eq!(result, "/opt/nvim/bin/nvim");
    }

    #[test]
    fn compute_default_target_overlay_root_same_as_home() {
        let overlay_root = PathBuf::from("/home/user");
        let home = PathBuf::from("/home/user");
        let link_target = PathBuf::from("/home/user/.config/nvim");
        let result = compute_default_target(&link_target, &overlay_root, &home);
        assert_eq!(result, ".config/nvim");
    }

    #[test]
    fn compute_default_target_link_target_is_overlay_root() {
        let overlay_root = PathBuf::from("/home/user/.over/myoverlay");
        let home = PathBuf::from("/home/user");
        let link_target = PathBuf::from("/home/user/.over/myoverlay");
        let result = compute_default_target(&link_target, &overlay_root, &home);
        assert_eq!(result, "");
    }
}
