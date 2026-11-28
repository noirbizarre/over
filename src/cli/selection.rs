//! Interactive fallback belongs to the CLI, not read-only ownership inference.

use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::{Context as _, Result, ensure};
use choisir::{SearchMode, Select};

use crate::exec::Context;
use crate::overlays::Overlay;
use crate::overlays::exclusions::Exclusions;
use crate::selection::SelectionRequired;

pub(super) fn resolve<T>(
    ctx: &Context,
    result: Result<T>,
    mut allowed: impl FnMut(&str) -> bool,
    selected: impl FnOnce(&str, bool) -> Result<T>,
) -> Result<T> {
    let error = match result {
        Ok(value) => return Ok(value),
        Err(error) => error,
    };
    let Some(required) = error.downcast_ref::<SelectionRequired>() else {
        return Err(error);
    };
    if ctx.no_prompt || !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        return Err(error);
    }
    let mut names = required.candidates.clone();
    if names.is_empty() {
        names = ctx
            .repository
            .overlay_dirs(true)?
            .iter()
            .map(|directory| {
                ctx.repository.canonical_name(
                    &directory
                        .strip_prefix(&ctx.repository.root)?
                        .to_string_lossy(),
                )
            })
            .collect::<Result<_>>()?;
    }
    names.retain(|name| allowed(name));
    names.sort();
    names.dedup();
    ensure!(!names.is_empty(), "{error}; no selectable overlays found");
    let _pause = crate::ui::progress::pause()?;
    let name = Select::new()
        .with_theme(&crate::ui::style::DialogTheme::default())
        .with_prompt("Choose an overlay")
        .search(SearchMode::Typeahead)
        .items(names.into_iter().map(|name| (name.clone(), name)))
        .interact()
        .context("overlay selection cancelled or unavailable")?;
    selected(&name, required.absolute_paths)
}

pub(super) fn select(
    ctx: &Context,
    name: Option<&str>,
    paths: &[PathBuf],
    use_host: bool,
) -> Result<Overlay> {
    resolve(
        ctx,
        crate::selection::select(ctx, name, paths, use_host),
        |_| true,
        |name, _| chosen(ctx, name, paths),
    )
}

pub(super) fn chosen(ctx: &Context, name: &str, paths: &[PathBuf]) -> Result<Overlay> {
    let overlay = ctx.repository.get(name)?;
    let own_ctx = ctx.with_overlay(overlay.clone()).resolve_data()?;
    let mappings = overlay.resolve_mappings(&own_ctx)?;
    // Inference substitutes cwd for absent operands; choosing an identity must not bypass its mapping checks.
    let effective_paths = if paths.is_empty() {
        vec![ctx.absolute_path(std::path::Path::new("."))?]
    } else {
        paths
            .iter()
            .map(|path| ctx.absolute_path(path))
            .collect::<Result<_>>()?
    };
    for path in effective_paths {
        if matches!(
            mappings.reverse(&path)?,
            crate::overlays::mapping::ReverseResolution::Ambiguous(_)
        ) {
            mappings.logical_path_for(&path)?;
        }
    }
    Ok(overlay)
}

pub(super) fn apply(ctx: &Context) -> Result<Overlay> {
    resolve(
        ctx,
        crate::selection::select_apply(ctx, None),
        |_| true,
        |name, _| chosen(ctx, name, &[]),
    )
}

pub(super) fn apply_with_exclusions(
    ctx: &Context,
    exclusions: &mut Exclusions,
) -> Result<Option<Overlay>> {
    let result = crate::selection::select_apply_with_exclusions(ctx, exclusions);
    resolve(
        ctx,
        result,
        |name| !exclusions.excludes(name),
        |name, _| chosen(ctx, name, &[]).map(Some),
    )
}

pub(super) fn commit(
    ctx: &Context,
    name: Option<&str>,
    paths: &[PathBuf],
) -> Result<crate::selection::CommitSelection> {
    resolve(
        ctx,
        crate::selection::select_commit(ctx, name, paths),
        |_| true,
        |name, absolute| {
            // Recorded target ownership must not be re-anchored onto a source-relative collision.
            let paths = if absolute {
                paths
                    .iter()
                    .map(|path| ctx.absolute_path(path))
                    .collect::<Result<Vec<_>>>()?
            } else {
                paths.to_vec()
            };
            crate::selection::select_commit(ctx, Some(name), &paths)
        },
    )
}
