use std::path::PathBuf;

use anyhow::{Context as _, Result, ensure};
use clap::Args;

use crate::cli::CLI;
use crate::desired::DesiredTree;
use crate::exec::{Context, Ctx};
use crate::overlays::{Overlay, Repository};
use crate::restore::{self, RestoreOptions};

#[derive(Args, Debug)]
#[clap(
    after_help = "Discard selected changes permanently. By default, restore the worktree from the index.\nWith --staged, default to HEAD. With --source REV, default to worktree only;\nhistorical recovery remains unstaged unless --staged is requested.\nPaths are literal files, including deleted tracked files; directories and globs are not expanded."
)]
pub struct Params {
    #[clap(required = true, help = "Explicit file paths to restore (destructive)")]
    files: Vec<PathBuf>,
    #[clap(
        short,
        long,
        help = "Select an overlay (otherwise use default or infer ownership)"
    )]
    overlay: Option<String>,
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
        value_name = "REV",
        help = "Restore from a Git revision instead of the default source"
    )]
    source: Option<String>,
    #[clap(
        long,
        help = "Restore the source repository index (default source: HEAD)"
    )]
    staged: bool,
    #[clap(long, help = "Restore materialized working-tree files")]
    worktree: bool,
    #[clap(
        short = 'n',
        long,
        help = "Show destructive effects without applying changes"
    )]
    dry_run: bool,
}

pub async fn execute(cli: &CLI, args: &Params) -> Result<()> {
    let repository = Repository::new(cli.resolve_home()?);
    let ctx = Context::builder()
        .repository(repository)
        .root(
            args.root
                .clone()
                .or_else(dirs::home_dir)
                .context("could not determine home directory")?,
        )
        .debug(cli.debug)
        .verbose(cli.verbose)
        .dry_run(args.dry_run)
        .build();
    let paths = args
        .files
        .iter()
        .map(|path| {
            crate::overlays::mapping::absolute(&super::common::expand_tilde(
                &path.to_string_lossy(),
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    let explicit = args.overlay.clone();
    let selection_paths = paths.clone();
    let (ctx, overlay) =
        tokio::task::spawn_blocking(move || select(ctx, explicit.as_deref(), &selection_paths))
            .await??;
    restore::restore(
        &ctx,
        &overlay,
        &paths,
        &RestoreOptions {
            source: args.source.clone(),
            staged: args.staged,
            worktree: args.worktree,
        },
    )
    .await
}

fn resolved(ctx: &Ctx, overlay: &Overlay) -> Result<Ctx> {
    let ctx = ctx.with_overlay(overlay.clone()).resolve_data()?;
    let target = overlay.resolve_target(&ctx)?;
    Ok(ctx.with_resolved_overlay(overlay.name.clone(), target.to_string_lossy().to_string()))
}

fn select(ctx: Ctx, name: Option<&str>, paths: &[PathBuf]) -> Result<(Ctx, Overlay)> {
    let selected = match name {
        Some(name) => Some(ctx.repository.get(name)?),
        None => ctx.repository.default_overlay(&ctx)?,
    };
    if let Some(overlay) = selected {
        return Ok((resolved(&ctx, &overlay)?, overlay));
    }
    let mut candidates = Vec::new();
    for overlay in ctx.repository.overlays()? {
        // Unrelated machine-specific templates must not prevent ownership inference.
        let candidate = (|| -> Result<_> {
            let ctx = resolved(&ctx, &overlay)?;
            let tree = DesiredTree::build_own(&ctx, &overlay)?;
            let routing = overlay.resolve_mappings(&ctx)?;
            let mut specificity = Vec::new();
            for path in paths {
                let logical = routing.logical_path_for(path)?;
                let target = routing.target_for(&logical);
                let owner = restore::owner(&tree, &target)?;
                specificity.push(owner.target.components().count());
            }
            Ok((specificity, ctx))
        })();
        match candidate {
            Ok((specificity, ctx)) => candidates.push((specificity, ctx, overlay)),
            Err(error) => {
                tracing::debug!(overlay = %overlay.name, %error, "skipping restore inference candidate")
            }
        }
    }
    ensure!(
        !candidates.is_empty(),
        "cannot infer an owning virtual checkout; specify --overlay"
    );
    // Summing depths could choose opposite owners for different paths. Require
    // one overlay to be at least as specific for every requested file instead.
    let winner = candidates
        .iter()
        .enumerate()
        .position(|(index, (depths, _, _))| {
            candidates
                .iter()
                .enumerate()
                .all(|(other, (other_depths, _, _))| {
                    index == other
                        || (depths != other_depths
                            && depths.iter().zip(other_depths).all(|(a, b)| a >= b))
                })
        })
        .context("ambiguous checkout ownership; specify --overlay")?;
    let (_, ctx, overlay) = candidates.remove(winner);
    Ok((ctx, overlay))
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[test]
    fn restore_requires_explicit_paths_and_accepts_both_destinations() {
        assert!(CLI::try_parse_from(["over", "restore"]).is_err());
        assert!(CLI::try_parse_from(["over", "restore", "--staged", "--worktree", "file"]).is_ok());
        assert!(CLI::try_parse_from(["over", "restore", "--source", "HEAD~1", "deleted"]).is_ok());
    }
}
