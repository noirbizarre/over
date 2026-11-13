//! Read-only overlay selection from configured ownership and invocation paths.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow, bail, ensure};

use crate::actions::git::association;
use crate::desired::{DesiredEntry, DesiredTree, MaterializationIntent};
use crate::exec::Context;
use crate::overlays::Overlay;
use crate::overlays::mapping::{self, ResolvedMappings, ReverseMatch, ReverseResolution};

struct Candidate {
    overlay: Overlay,
    routing: ResolvedMappings,
    tree: Result<DesiredTree>,
}

// Declaration order is policy: weaker containment must not dilute concrete ownership.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Evidence {
    Ownership,
    Sibling,
    Target,
}

/// The source correspondence is retained for callers operating from a redirected target.
pub struct InferredOverlay {
    pub overlay: Overlay,
    pub paths: Vec<(PathBuf, ReverseMatch)>,
}

/// Infer from paths (or captured cwd when empty). Ambiguity is never absence.
pub fn infer_selection(ctx: &Context, paths: &[PathBuf]) -> Result<Option<InferredOverlay>> {
    let paths = if paths.is_empty() {
        vec![ctx.absolute_path(Path::new("."))?]
    } else {
        paths
            .iter()
            .map(|path| ctx.absolute_path(path))
            .collect::<Result<_>>()?
    };
    let mut candidates = None;
    let mut locations = None;
    let mut selected = BTreeMap::new();
    let mut resolved_paths = Vec::new();
    for path in &paths {
        if let Some(name) = git_association(path)? {
            let overlay = ctx.repository.get(&name).with_context(|| {
                format!("over.overlay references unavailable overlay '{name}'; specify an overlay explicitly or fix the repository config")
            })?;
            selected.insert(overlay.name.clone(), overlay);
            continue;
        }
        if locations.is_none() {
            locations = Some(ctx.repository.overlay_dirs(true)?);
        }
        let directories = locations.as_ref().expect("locations initialized above");
        let sources = directories
            .iter()
            .map(|directory| Ok((directory, paths_inside_source(path, directory)?)))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .filter_map(|(directory, owns)| owns.then_some(directory))
            .collect::<Vec<_>>();
        ensure!(
            sources.len() <= 1,
            "ambiguous source overlay for '{}'; specify NAME or --overlay NAME explicitly",
            path.display()
        );
        if let Some(directory) = sources.first() {
            // Source context is authoritative: resolving unrelated targets here defeats lazy loading.
            let name = directory
                .strip_prefix(&ctx.repository.root)?
                .to_string_lossy();
            let overlay = (|| -> Result<_> {
                let overlay = ctx.repository.get(&name)?;
                let own_ctx = ctx.with_overlay(overlay.clone()).resolve_data()?;
                overlay.resolve_mappings(&own_ctx)?;
                Ok(overlay)
            })()
            .with_context(|| {
                format!("cannot resolve overlay '{name}' owning {}", path.display())
            })?;
            selected.insert(overlay.name.clone(), overlay);
            continue;
        }
        if candidates.is_none() {
            let mut prepared = Vec::new();
            for directory in directories {
                let candidate = (|| -> Result<_> {
                    let overlay = ctx.repository.get(
                        &directory
                            .strip_prefix(&ctx.repository.root)?
                            .to_string_lossy(),
                    )?;
                    let ctx = ctx.with_overlay(overlay.clone()).resolve_data()?;
                    Ok(Candidate {
                        overlay: overlay.clone(),
                        routing: overlay.resolve_mappings(&ctx)?,
                        // A broken sidecar must not erase an already-identifiable target.
                        tree: DesiredTree::build_own(&ctx, &overlay),
                    })
                })();
                match candidate {
                    Ok(candidate) => prepared.push(candidate),
                    Err(error) => {
                        // An unrelated machine-specific target must not block local inference.
                        for path in &paths {
                            if paths_inside_source(path, directory)? {
                                return Err(error).with_context(|| {
                                    format!(
                                        "cannot resolve overlay '{}' owning {}",
                                        directory
                                            .strip_prefix(&ctx.repository.root)
                                            .unwrap_or(directory)
                                            .display(),
                                        path.display()
                                    )
                                });
                            }
                        }
                        tracing::debug!(overlay = %directory.display(), %error, "skipping unresolved selection candidate");
                    }
                }
            }
            candidates = Some(prepared);
        }
        let mut tiers = BTreeMap::<Evidence, BTreeMap<String, &Candidate>>::new();
        for candidate in candidates.as_ref().expect("candidates prepared above") {
            let mut owns = paths_inside_source(path, &candidate.overlay.root)?;
            let mut sibling = false;
            for entry in candidate
                .tree
                .as_ref()
                .map(|tree| tree.entries())
                .unwrap_or_default()
            {
                // Broken deployment ancestors cannot provide alias evidence for unrelated paths.
                match owns_path(entry, path).and_then(|owned| {
                    owns |= owned;
                    is_sibling(entry, path)
                }) {
                    Ok(found) => sibling |= found,
                    Err(error) => {
                        tracing::debug!(overlay = %candidate.overlay.name, target = %entry.target.display(), %error, "skipping unresolved destination alias")
                    }
                }
            }
            let tier = if owns {
                Some(Evidence::Ownership)
            } else if sibling {
                Some(Evidence::Sibling)
            } else if !matches!(candidate.routing.reverse(path)?, ReverseResolution::Absent) {
                Some(Evidence::Target)
            } else {
                None
            };
            if let Some(tier) = tier {
                tiers
                    .entry(tier)
                    .or_default()
                    .insert(candidate.overlay.name.clone(), candidate);
            }
        }
        if let Some(tier) = tiers.into_values().next() {
            ensure!(
                tier.len() == 1,
                "ambiguous overlay for '{}': {}; specify NAME or --overlay NAME explicitly",
                path.display(),
                tier.keys().cloned().collect::<Vec<_>>().join(", ")
            );
            for (name, candidate) in tier {
                match candidate.routing.reverse(path)? {
                    ReverseResolution::Unique(found) => resolved_paths.push((path.clone(), found)),
                    ReverseResolution::Ambiguous(_) => {
                        // Naming the overlay cannot disambiguate two source paths within it.
                        candidate.routing.logical_path_for(path)?;
                    }
                    ReverseResolution::Absent => {}
                }
                if let Err(error) = &candidate.tree {
                    return Err(anyhow!(
                        "cannot resolve overlay '{name}' for '{}': {error:#}",
                        path.display()
                    ));
                }
                selected.insert(name, candidate.overlay.clone());
            }
        }
    }
    ensure!(
        selected.len() <= 1,
        "ambiguous overlay across selected paths: {}; specify NAME or --overlay NAME explicitly",
        selected.keys().cloned().collect::<Vec<_>>().join(", ")
    );
    Ok(selected
        .into_values()
        .next()
        .map(|overlay| InferredOverlay {
            overlay,
            paths: resolved_paths,
        }))
}

pub fn infer(ctx: &Context, paths: &[PathBuf]) -> Result<Option<Overlay>> {
    Ok(infer_selection(ctx, paths)?.map(|selection| selection.overlay))
}

fn paths_inside_source(path: &Path, source: &Path) -> Result<bool> {
    Ok(
        path.starts_with(source)
            || mapping::location(path)?.starts_with(mapping::physical(source)?),
    )
}

fn is_sibling(entry: &DesiredEntry, path: &Path) -> Result<bool> {
    if matches!(entry.intent, MaterializationIntent::Directory) {
        return Ok(false);
    }
    let directory = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(path)
    };
    // Check the deployment namespace before resolving possibly-broken directory links.
    Ok(
        (entry.target.parent() == Some(directory) && entry.target != path)
            || (mapping::location(&entry.target)?.parent()
                == Some(mapping::physical(directory)?.as_path())
                && mapping::location(&entry.target)? != mapping::location(path)?),
    )
}

fn owns_path(entry: &DesiredEntry, path: &Path) -> Result<bool> {
    if matches!(entry.intent, MaterializationIntent::Directory) {
        // Every overlay scaffolds its target; treating that as ownership defeats sibling inference.
        return Ok(false);
    }
    // Try the deployment namespace before resolving parents inside directory symlinks.
    let owns = |path: &Path, target: &Path| match &entry.intent {
        MaterializationIntent::SymlinkDirectory { .. } | MaterializationIntent::Checkout => {
            path.starts_with(target)
        }
        intent if intent.is_virtual_checkout() => path.strip_prefix(target).is_ok_and(|relative| {
            intent
                .checkout_scope()
                .owns_target_kind(relative, path.is_dir())
        }),
        _ => path == target,
    };
    Ok(owns(path, &entry.target)
        || owns(
            &mapping::location(path)?,
            &mapping::location(&entry.target)?,
        ))
}

fn git_association(path: &Path) -> Result<Option<String>> {
    let mut directory = path;
    while !directory.is_dir() {
        let Some(parent) = directory.parent() else {
            return Ok(None);
        };
        directory = parent;
    }
    match git2::Repository::discover(directory) {
        Ok(repo) => association::read(&repo),
        Err(error) if error.code() == git2::ErrorCode::NotFound => Ok(None),
        Err(error) => Err(error).context("failed to inspect contextual Git repository"),
    }
}

/// Commit already-associated checkouts without discovering every overlay's contents.
pub(crate) fn select_commit(ctx: &Context, name: Option<&str>) -> Result<Overlay> {
    if let Some(name) = name {
        return ctx.repository.get(name);
    }
    // macOS /var aliases and symlinked collections must share cwd's physical namespace.
    let cwd = mapping::physical(&ctx.absolute_path(Path::new("."))?)?;
    let collection = mapping::physical(&ctx.repository.root)?;
    if let Some(name) = git_association(&cwd)? {
        return ctx.repository.get(&name);
    }
    let state = crate::materialize::virtual_checkout::state::state_file()?.inspect_blocking()?;
    let mut sources = std::collections::BTreeSet::new();
    let mut targets = std::collections::BTreeSet::new();
    for (target, record) in &state.checkouts {
        let target = crate::materialize::virtual_checkout::state::target_for_key(target)?;
        let source = record
            .source_repository
            .as_ref()
            .map(|root| mapping::physical(&root.join(&record.managed_path)))
            .transpose()?;
        // A record from another collection is not evidence for a same-named overlay here.
        if source
            .as_ref()
            .is_some_and(|source| !source.starts_with(&collection))
        {
            continue;
        }
        let target_owned = cwd
            .strip_prefix(&target)
            .is_ok_and(|relative| record.scope.owns_target_kind(relative, true));
        if target_owned {
            targets.insert(record.overlay.clone());
        }
        if source.as_ref().is_some_and(|source| {
            cwd.strip_prefix(source)
                .is_ok_and(|relative| record.scope.owns_source_kind(relative, true))
        }) {
            sources.insert(record.overlay.clone());
        }
    }
    // Source context remains authoritative even when another overlay targets that directory.
    let names = if sources.is_empty() { targets } else { sources };
    ensure!(
        names.len() <= 1,
        "ambiguous overlay; specify --overlay NAME explicitly"
    );
    if let Some(name) = names.into_iter().next() {
        return ctx.repository.get(&name);
    }
    // Descriptor probing is bounded by cwd depth, not by repository size.
    for directory in cwd
        .ancestors()
        .take_while(|directory| directory.starts_with(&collection))
    {
        if directory != collection
            && crate::overlays::EXTENSIONS
                .iter()
                .any(|ext| directory.join(format!("over.{ext}")).is_file())
        {
            return ctx
                .repository
                .get(&directory.strip_prefix(&collection)?.to_string_lossy());
        }
    }
    if let Some(overlay) = ctx.repository.default_overlay(ctx)? {
        return Ok(overlay);
    }
    bail!("cannot infer an overlay; specify --overlay NAME or configure default_overlay")
}

/// Select explicitly, then contextually, then optionally by configured default.
pub fn select(
    ctx: &Context,
    name: Option<&str>,
    paths: &[PathBuf],
    use_default: bool,
) -> Result<Overlay> {
    if let Some(name) = name {
        return ctx.repository.get(name);
    }
    if let Some(overlay) = infer(ctx, paths)? {
        return Ok(overlay);
    }
    if use_default && let Some(overlay) = ctx.repository.default_overlay(ctx)? {
        return Ok(overlay);
    }
    bail!(
        "cannot infer an overlay; provide NAME or --overlay NAME explicitly{}",
        if use_default {
            " or configure default_overlay"
        } else {
            ""
        }
    )
}

/// Resolve a reporting command's contextual selection before its default.
pub fn infer_or_default(ctx: &Context) -> Result<Option<Overlay>> {
    match infer(ctx, &[])? {
        Some(overlay) => Ok(Some(overlay)),
        None => ctx.repository.default_overlay(ctx),
    }
}
