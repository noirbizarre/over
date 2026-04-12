use std::path::{Path, PathBuf};

use clap::Args;
use dialoguer::Input;

use crate::actions::symlink::SymlinkConfig;
use crate::cli::CLI;
use crate::cli::common::resolve_inputs;
use crate::exec::Context;
use crate::overlays::Repository;
use crate::ui::emojis;
use crate::ui::style::{self, DialogTheme};
use anyhow::{Result, anyhow};
use dialoguer::FuzzySelect;
use dirs::home_dir;

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
                return Err(anyhow!("no overlays found in repository"));
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

    let resolved = resolve_inputs(&args.files)?;

    let (symlinks, regulars): (Vec<_>, Vec<_>) = resolved.iter().partition(|p| p.is_symlink());

    for symlink_path in &symlinks {
        add_symlink(&overlay, &home, symlink_path, cli)?;
    }

    if !regulars.is_empty() {
        let regular_paths: Vec<PathBuf> = regulars.into_iter().cloned().collect();
        overlay
            .add_files(&ctx, &regular_paths)
            .await
            .inspect_err(|e| {
                eprintln!(
                    "{} {} {}: {}",
                    emojis::CROSSMARK,
                    style::white_b("Failed to add to overlay"),
                    style::cyan(&overlay.name),
                    e,
                );
            })?;
    }

    Ok(())
}

fn add_symlink(
    overlay: &crate::overlays::Overlay,
    home: &PathBuf,
    symlink_path: &PathBuf,
    _cli: &CLI,
) -> Result<()> {
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

    let rel_path = symlink_path.strip_prefix(home).unwrap_or(symlink_path);
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

    println!(
        "{} {} {} {}",
        emojis::CHECKMARK,
        style::white_b("Created symlink config"),
        style::cyan(config_path.display()),
        style::white_b("in overlay"),
    );

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
