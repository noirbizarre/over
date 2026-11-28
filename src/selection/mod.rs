//! Read-only overlay selection from configured ownership and invocation paths.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow, bail, ensure};

use crate::actions::git::association;
use crate::desired::{DesiredEntry, DesiredTree, MaterializationIntent};
use crate::exec::Context;
use crate::overlays::Overlay;
use crate::overlays::exclusions::Exclusions;
use crate::overlays::mapping::{self, ResolvedMappings, ReverseMatch, ReverseResolution};

struct Candidate {
    overlay: Overlay,
    routing: ResolvedMappings,
    tree: Result<DesiredTree>,
    deployment_blocked: bool,
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

/// Preserve history's ambiguity fallback, including ambiguities that a picker cannot resolve.
#[derive(Debug)]
pub(crate) struct AmbiguousSelection(String);

impl std::fmt::Display for AmbiguousSelection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for AmbiguousSelection {}

/// Only these failures can be resolved by deliberately choosing an overlay identity.
#[derive(Debug)]
pub(crate) struct SelectionRequired {
    /// Empty only when context is missing, allowing the CLI to discover available names lazily.
    pub candidates: Vec<String>,
    /// Recorded target operands must retain their namespace after an interactive choice.
    pub absolute_paths: bool,
    message: String,
}

impl std::fmt::Display for SelectionRequired {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SelectionRequired {}

pub(crate) fn missing(message: impl Into<String>) -> anyhow::Error {
    SelectionRequired {
        candidates: Vec::new(),
        absolute_paths: false,
        message: message.into(),
    }
    .into()
}

fn ambiguous(message: String, candidates: BTreeSet<String>, absolute_paths: bool) -> anyhow::Error {
    anyhow::Error::new(SelectionRequired {
        candidates: candidates.into_iter().collect(),
        absolute_paths,
        message: message.clone(),
    })
    .context(AmbiguousSelection(message))
}

fn compatible_candidates(constraints: &[BTreeSet<String>]) -> BTreeSet<String> {
    let Some(first) = constraints.first() else {
        return BTreeSet::new();
    };
    constraints
        .iter()
        .skip(1)
        .fold(first.clone(), |names, constraint| {
            names.intersection(constraint).cloned().collect()
        })
}

/// Infer from paths (or captured cwd when empty). Ambiguity is never absence.
pub fn infer_selection(ctx: &Context, paths: &[PathBuf]) -> Result<Option<InferredOverlay>> {
    infer_selection_with_exclusions(ctx, paths, None, &mut false, false)
}

pub(crate) fn infer_history(ctx: &Context) -> Result<Option<Overlay>> {
    Ok(
        infer_selection_with_exclusions(ctx, &[], None, &mut false, true)?
            .map(|selection| selection.overlay),
    )
}

fn infer_selection_with_exclusions(
    ctx: &Context,
    paths: &[PathBuf],
    mut exclusions: Option<&mut Exclusions>,
    excluded_context: &mut bool,
    history: bool,
) -> Result<Option<InferredOverlay>> {
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
    let mut constraints = Vec::new();
    let mut ambiguity = None;
    for path in &paths {
        if let Some(name) = git_association(path)? {
            if let Some(policy) = exclusions.as_deref_mut()
                && policy.excludes(&ctx.repository.canonical_name(&name)?)
            {
                *excluded_context = true;
                return Ok(None);
            }
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
        if sources.len() > 1 {
            ambiguity = Some(format!(
                "ambiguous source overlay for '{}'; specify NAME or --overlay NAME explicitly",
                path.display()
            ));
            constraints.push(
                sources
                    .iter()
                    .map(|directory| {
                        ctx.repository.canonical_name(
                            &directory
                                .strip_prefix(&ctx.repository.root)?
                                .to_string_lossy(),
                        )
                    })
                    .collect::<Result<BTreeSet<_>>>()?,
            );
            continue;
        }
        if let Some(directory) = sources.first() {
            // Source context is authoritative: resolving unrelated targets here defeats lazy loading.
            let name = directory
                .strip_prefix(&ctx.repository.root)?
                .to_string_lossy();
            if let Some(policy) = exclusions.as_deref_mut()
                && policy.excludes(&ctx.repository.canonical_name(&name)?)
            {
                // Authoritative excluded context must never activate host fallback.
                *excluded_context = true;
                return Ok(None);
            }
            let overlay = (|| -> Result<_> {
                let overlay = ctx.repository.get(&name)?;
                let own_ctx = ctx.with_overlay(overlay.clone()).resolve_data()?;
                if let Some(policy) = exclusions.as_deref_mut() {
                    policy.validate(&own_ctx, std::slice::from_ref(&overlay))?;
                }
                if history {
                    ResolvedMappings::for_history(&overlay, &own_ctx)?;
                } else {
                    overlay.resolve_mappings(&own_ctx)?;
                }
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
                let name = ctx.repository.canonical_name(
                    &directory
                        .strip_prefix(&ctx.repository.root)?
                        .to_string_lossy(),
                )?;
                if let Some(policy) = exclusions.as_deref_mut()
                    && policy.excludes(&name)
                {
                    // Metadata routing can identify an excluded target without rendering its content.
                    let routing = (|| -> Result<_> {
                        let overlay = ctx.repository.get(&name)?;
                        let own_ctx = ctx.with_overlay(overlay.clone()).resolve_data()?;
                        overlay.resolve_mappings(&own_ctx)
                    })();
                    match routing {
                        Ok(routing) => {
                            for path in &paths {
                                if !matches!(routing.reverse(path)?, ReverseResolution::Absent) {
                                    *excluded_context = true;
                                }
                            }
                        }
                        Err(error) => {
                            tracing::debug!(overlay = %name, %error, "skipping unresolved excluded selection candidate");
                        }
                    }
                    continue;
                }
                let candidate = (|| -> Result<_> {
                    let overlay = ctx.repository.get(
                        &directory
                            .strip_prefix(&ctx.repository.root)?
                            .to_string_lossy(),
                    )?;
                    let ctx = ctx.with_overlay(overlay.clone()).resolve_data()?;
                    // Keep conflicts attached to routing evidence rather than hiding a winning candidate.
                    let validation = match exclusions.as_deref_mut() {
                        Some(policy) => policy.validate(&ctx, std::slice::from_ref(&overlay)),
                        None => Ok(()),
                    };
                    Ok(Candidate {
                        overlay: overlay.clone(),
                        routing: if history {
                            ResolvedMappings::for_history(&overlay, &ctx)?
                        } else {
                            overlay.resolve_mappings(&ctx)?
                        },
                        deployment_blocked: validation.is_err(),
                        // A broken sidecar must not erase an already-identifiable target.
                        tree: validation.and_then(|()| {
                            if history {
                                DesiredTree::build_own_history(&ctx, &overlay)
                            } else {
                                DesiredTree::build_own(&ctx, &overlay)
                            }
                        }),
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
                if candidate.deployment_blocked {
                    // Missing tree evidence must not let another root win and execute unexpectedly.
                    // An explicit name avoids preparing this blocked candidate's content.
                    if let Err(error) = &candidate.tree {
                        bail!(
                            "cannot safely infer an overlay for '{}': candidate '{}' has a deployment conflict: {error:#}; specify NAME explicitly to disambiguate",
                            path.display(),
                            candidate.overlay.name
                        );
                    }
                }
                tiers
                    .entry(tier)
                    .or_default()
                    .insert(candidate.overlay.name.clone(), candidate);
            }
        }
        if let Some(tier) = tiers.into_values().next() {
            constraints.push(tier.keys().cloned().collect());
            if tier.len() > 1 {
                ambiguity = Some(format!(
                    "ambiguous overlay for '{}': {}; specify NAME or --overlay NAME explicitly",
                    path.display(),
                    tier.keys().cloned().collect::<Vec<_>>().join(", ")
                ));
                // Inspect later operands before offering a choice, so ambiguity cannot hide conflicting ownership.
                continue;
            }
            for (name, candidate) in tier {
                match candidate.routing.reverse(path)? {
                    ReverseResolution::Unique(found) => resolved_paths.push((path.clone(), found)),
                    ReverseResolution::Ambiguous(_) => {
                        // Naming the overlay cannot disambiguate two source paths within it.
                        if let Err(error) = candidate.routing.logical_path_for(path) {
                            return Err(AmbiguousSelection(error.to_string()).into());
                        }
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
        AmbiguousSelection(format!(
            "ambiguous overlay across selected paths: {}; specify NAME or --overlay NAME explicitly",
            selected.keys().cloned().collect::<Vec<_>>().join(", ")
        ))
    );
    if let Some(message) = ambiguity {
        constraints.extend(selected.keys().map(|name| BTreeSet::from([name.clone()])));
        let candidates = compatible_candidates(&constraints);
        ensure!(
            !candidates.is_empty(),
            AmbiguousSelection(
                "paths belong to different overlays; specify NAME or --overlay NAME explicitly"
                    .into()
            )
        );
        return Err(ambiguous(message, candidates, false));
    }
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

/// Keep operand interpretation paired with the evidence that selected the overlay.
pub(crate) struct CommitSelection {
    pub overlay: Overlay,
    pub paths: Vec<PathBuf>,
}

/// Commit already-associated checkouts without discovering every overlay's contents.
pub(crate) fn select_commit(
    ctx: &Context,
    name: Option<&str>,
    paths: &[PathBuf],
) -> Result<CommitSelection> {
    let contextual = |overlay| CommitSelection {
        overlay,
        paths: paths.to_vec(),
    };
    if let Some(name) = name {
        return ctx.repository.get(name).map(contextual);
    }
    // macOS /var aliases and symlinked collections must share cwd's physical namespace.
    let cwd = mapping::physical(&ctx.absolute_path(Path::new("."))?)?;
    let collection = mapping::physical(&ctx.repository.root)?;
    // Operand ownership outranks an unrelated cwd association; no-path selection stays lazy.
    if paths.is_empty()
        && let Some(name) = git_association(&cwd)?
    {
        return ctx.repository.get(&name).map(contextual);
    }
    let state = crate::materialize::virtual_checkout::state::state_file()?.inspect_blocking()?;
    let mut operand_owners = std::collections::BTreeSet::new();
    let mut operand_constraints = Vec::new();
    let mut ambiguous_operand = false;
    for path in paths {
        let path = mapping::location(&ctx.absolute_path(path)?)?;
        let mut sources = std::collections::BTreeSet::new();
        let mut targets = std::collections::BTreeSet::new();
        for (key, record) in &state.checkouts {
            let source = record
                .source_repository
                .as_ref()
                .map(|root| mapping::physical(&root.join(&record.managed_path)))
                .transpose()?;
            if source
                .as_ref()
                .is_some_and(|source| !source.starts_with(&collection))
            {
                continue;
            }
            if source.as_ref().is_some_and(|source| {
                path.strip_prefix(source)
                    .is_ok_and(|relative| record.scope.owns_source_kind(relative, true))
            }) {
                sources.insert(record.overlay.clone());
            }
            let target = mapping::location(
                &crate::materialize::virtual_checkout::state::target_for_key(key)?,
            )?;
            // Prefix ownership also handles directories removed since application.
            if path
                .strip_prefix(&target)
                .is_ok_and(|relative| record.scope.owns_target_kind(relative, true))
            {
                targets.insert(record.overlay.clone());
            }
        }
        let owners = if sources.is_empty() { targets } else { sources };
        if !owners.is_empty() {
            ambiguous_operand |= owners.len() > 1;
            operand_constraints.push(owners.clone());
            if owners.len() == 1 {
                operand_owners.extend(owners);
            }
        }
    }
    ensure!(
        operand_owners.len() <= 1,
        "paths belong to different overlays; specify --overlay NAME explicitly"
    );
    if ambiguous_operand {
        let candidates = compatible_candidates(&operand_constraints);
        ensure!(
            !candidates.is_empty(),
            "paths belong to different overlays; specify --overlay NAME explicitly"
        );
        return Err(ambiguous(
            "ambiguous overlay for selected paths; specify --overlay NAME explicitly".into(),
            candidates,
            true,
        ));
    }
    if let Some(name) = operand_owners.into_iter().next() {
        // Re-anchoring after inference could silently commit a colliding source-relative file.
        return Ok(CommitSelection {
            overlay: ctx.repository.get(&name)?,
            paths: paths
                .iter()
                .map(|path| ctx.absolute_path(path))
                .collect::<Result<_>>()?,
        });
    }
    if let Some(name) = git_association(&cwd)? {
        return ctx.repository.get(&name).map(contextual);
    }
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
    if names.len() > 1 {
        return Err(ambiguous(
            "ambiguous overlay; specify --overlay NAME explicitly".into(),
            names,
            false,
        ));
    }
    if let Some(name) = names.into_iter().next() {
        return ctx.repository.get(&name).map(contextual);
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
                .get(&directory.strip_prefix(&collection)?.to_string_lossy())
                .map(contextual);
        }
    }
    Err(missing(
        "cannot infer an overlay; specify --overlay NAME or run from an active overlay context",
    ))
}

/// Select explicitly, then contextually, then optionally by host context.
pub fn select(
    ctx: &Context,
    name: Option<&str>,
    paths: &[PathBuf],
    use_host: bool,
) -> Result<Overlay> {
    if let Some(name) = name {
        return ctx.repository.get(name);
    }
    if let Some(overlay) = infer(ctx, paths)? {
        return Ok(overlay);
    }
    if use_host && let Some(overlay) = ctx.repository.host_overlay(ctx)? {
        return Ok(overlay);
    }
    Err(missing(format!(
        "cannot infer an overlay; provide NAME or --overlay NAME explicitly{}",
        if use_host {
            " or configure host_overlay"
        } else {
            ""
        }
    )))
}

/// Resolve a reporting command's active context before its host context.
pub fn infer_or_host(ctx: &Context) -> Result<Option<Overlay>> {
    match infer(ctx, &[])? {
        Some(overlay) => Ok(Some(overlay)),
        None => ctx.repository.host_overlay(ctx),
    }
}

/// Select a mutation target without turning machine identity into deployment intent.
pub fn select_context(ctx: &Context, name: Option<&str>, paths: &[PathBuf]) -> Result<Overlay> {
    select(ctx, name, paths, false)
}

/// Apply has its own opt-in for using the host without explicit or active selection.
pub fn select_apply(ctx: &Context, name: Option<&str>) -> Result<Overlay> {
    if let Some(name) = name {
        return ctx.repository.get(name);
    }
    if let Some(overlay) = infer(ctx, &[])? {
        return Ok(overlay);
    }
    if ctx.repository.apply_config()?.default_to_host
        && let Some(overlay) = ctx.repository.host_overlay(ctx)?
    {
        return Ok(overlay);
    }
    Err(missing(
        "cannot infer an overlay; run `over apply NAME`, use an active overlay context, or configure host_overlay and apply.default_to_host = true",
    ))
}

/// Select apply roots without treating an excluded authoritative context as absence.
pub(crate) fn select_apply_with_exclusions(
    ctx: &Context,
    exclusions: &mut Exclusions,
) -> Result<Option<Overlay>> {
    let mut excluded_context = false;
    if let Some(selection) =
        infer_selection_with_exclusions(ctx, &[], Some(exclusions), &mut excluded_context, false)?
    {
        return Ok(Some(selection.overlay));
    }
    if excluded_context {
        return Ok(None);
    }
    if ctx.repository.apply_config()?.default_to_host
        && let Some(overlay) = ctx.repository.host_overlay(ctx)?
    {
        return Ok((!exclusions.excludes(&overlay.name)).then_some(overlay));
    }
    Err(missing(
        "cannot infer an overlay; run `over apply NAME`, use an active overlay context, or configure host_overlay and apply.default_to_host = true",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_fs::prelude::*;

    #[test]
    fn ambiguity_only_offers_candidates_from_the_strongest_evidence_tier() {
        let temp = assert_fs::TempDir::new().unwrap();
        for name in ["one", "two", "weaker"] {
            temp.child(format!("repo/{name}/over.toml"))
                .write_str("target = '~'")
                .unwrap();
        }
        for name in ["one", "two"] {
            temp.child(format!("repo/{name}/file"))
                .write_str(name)
                .unwrap();
        }
        let ctx = Context::builder()
            .repository(crate::overlays::Repository::new(temp.path().join("repo")))
            .root(temp.path().join("home"))
            .build();
        let error = infer(&ctx, &[temp.path().join("home/file")]).unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<SelectionRequired>()
                .unwrap()
                .candidates,
            ["one", "two"]
        );
        assert!(error.is::<AmbiguousSelection>());
    }

    #[test]
    fn an_early_ambiguity_cannot_hide_later_conflicting_operand_ownership() {
        let temp = assert_fs::TempDir::new().unwrap();
        for name in ["one", "two", "three"] {
            temp.child(format!("repo/{name}/over.toml"))
                .write_str("target = '~'")
                .unwrap();
        }
        for name in ["one", "two"] {
            temp.child(format!("repo/{name}/ambiguous"))
                .write_str(name)
                .unwrap();
        }
        temp.child("repo/three/owned").write_str("three").unwrap();
        let ctx = Context::builder()
            .repository(crate::overlays::Repository::new(temp.path().join("repo")))
            .root(temp.path().join("home"))
            .build();
        let error = infer(
            &ctx,
            &[
                temp.path().join("home/ambiguous"),
                temp.path().join("home/owned"),
            ],
        )
        .unwrap_err();
        assert!(error.to_string().contains("different overlays"));
        assert!(!error.is::<SelectionRequired>());
    }
}
