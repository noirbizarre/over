use std::env::current_dir;
use std::path::PathBuf;

use clap::Args;

use crate::cli::CLI;
use crate::exec::Context;
use crate::overlays::Repository;
use crate::ui::{emojis, style};
use anyhow::{Result, anyhow};
use dialoguer::FuzzySelect;
use dialoguer::theme::ColorfulTheme;
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

/// Check whether a string contains glob metacharacters.
fn is_glob_pattern(s: &str) -> bool {
    s.contains('*') || s.contains('?') || s.contains('[') || s.contains('{')
}

/// Expand a tilde prefix in a path string to the home directory.
fn expand_tilde(s: &str) -> PathBuf {
    if let Some(rest) = s.strip_prefix("~/")
        && let Some(home) = home_dir()
    {
        return home.join(rest);
    } else if s == "~"
        && let Some(home) = home_dir()
    {
        return home;
    }
    PathBuf::from(s)
}

/// Resolve a list of input strings (which may be globs, tildes, relative paths,
/// directories, or plain files) into a flat list of absolute paths.
fn resolve_inputs(inputs: &[String]) -> Result<Vec<PathBuf>> {
    let cwd = current_dir()?;
    let mut resolved = Vec::new();

    for input in inputs {
        let expanded = expand_tilde(input);
        let pattern_str = expanded.to_string_lossy();

        if is_glob_pattern(&pattern_str) {
            let matches: Vec<_> = glob::glob(&pattern_str)
                .map_err(|e| anyhow!("Invalid glob pattern '{}': {}", input, e))?
                .filter_map(|entry| entry.ok())
                .collect();

            if matches.is_empty() {
                return Err(anyhow!("No files matched pattern '{}'", input));
            }

            for path in matches {
                let abs = if path.is_relative() {
                    cwd.join(&path)
                } else {
                    path
                };
                resolved.push(abs);
            }
        } else {
            let abs = if expanded.is_relative() {
                cwd.join(&expanded)
            } else {
                expanded
            };
            if !abs.exists() {
                return Err(anyhow!("{} does not exist", abs.display()));
            }
            resolved.push(abs);
        }
    }

    Ok(resolved)
}

pub async fn execute(cli: &CLI, args: &Params) -> Result<()> {
    if cli.debug {
        println!("{:#?}", cli);
        println!("{:#?}", args);
    }

    let home = cli.resolve_home()?;
    let repo = Repository::new(home);
    if cli.debug {
        println!("{:#?}", repo);
    }

    let overlay = match &args.overlay {
        Some(name) => repo.get(name)?,
        None => {
            let overlays = repo.overlays()?;
            if overlays.is_empty() {
                return Err(anyhow!("no overlays found in repository"));
            }
            let selection = FuzzySelect::with_theme(&ColorfulTheme::default())
                .with_prompt("Choose the target overlay")
                .default(0)
                .items(&overlays[..])
                .interact()
                .map_err(|e| anyhow!("overlay selection cancelled: {}", e))?;
            overlays[selection].clone()
        }
    };

    if cli.debug {
        println!("{:#?}", overlay);
    }

    let ctx = Context::new(
        args.dry_run,
        cli.debug,
        cli.verbose,
        args.force,
        false,
        args.root
            .clone()
            .or_else(home_dir)
            .ok_or_else(|| anyhow!("could not determine home directory"))?,
        repo,
        Some(overlay.clone()),
    );

    let resolved = resolve_inputs(&args.files)?;

    overlay.add_files(&ctx, &resolved).await.map_err(|e| {
        println!(
            "{} {} {} {}",
            emojis::CROSSMARK,
            style::white_b("Failed to add to overlay"),
            style::cyan(&overlay.name),
            style::white_b(&format!(": {}", e)),
        );
        e
    })?;

    Ok(())
}
