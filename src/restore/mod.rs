//! Restore explicit owned checkout paths without reconciling or advancing state.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, ensure};

use crate::actions::git::restore::{RestorePath, restore_paths};
use crate::desired::{DesiredTree, MaterializationIntent, Provenance};
use crate::exec::Ctx;
use crate::materialize::virtual_checkout::{git, state};
use crate::overlays::Overlay;

#[derive(Clone, Debug, Default)]
pub struct RestoreOptions {
    pub source: Option<String>,
    pub staged: bool,
    pub worktree: bool,
}

impl RestoreOptions {
    pub(crate) fn writes_worktree(&self) -> bool {
        self.worktree || !self.staged
    }
}

/// Fail closed unless a path has exactly one own virtual-checkout owner.
pub(crate) fn owner<'a>(
    tree: &'a DesiredTree,
    path: &Path,
) -> Result<&'a crate::desired::DesiredEntry> {
    ensure!(
        !tree.entries().iter().any(|entry| {
            !entry.intent.is_virtual_checkout()
                && !matches!(entry.intent, MaterializationIntent::Directory)
                && path.starts_with(&entry.target)
        }),
        "'{}' belongs to an independent materialization",
        path.display()
    );
    let owners: Vec<_> = tree
        .entries()
        .iter()
        .filter(|entry| entry.intent.is_virtual_checkout() && crate::stage::owns(entry, path))
        .collect();
    ensure!(
        owners.len() == 1,
        "'{}' must be owned by exactly one virtual checkout",
        path.display()
    );
    Ok(owners[0])
}

pub async fn restore(
    ctx: &Ctx,
    overlay: &Overlay,
    paths: &[PathBuf],
    options: &RestoreOptions,
) -> Result<()> {
    let (ctx, overlay, paths, options) = (
        ctx.clone(),
        overlay.clone(),
        paths.to_vec(),
        options.clone(),
    );
    tokio::task::spawn_blocking(move || restore_blocking(&ctx, &overlay, &paths, &options)).await?
}

fn restore_blocking(
    ctx: &Ctx,
    overlay: &Overlay,
    paths: &[PathBuf],
    options: &RestoreOptions,
) -> Result<()> {
    ensure!(!paths.is_empty(), "specify at least one file to restore");
    let _ownership = if ctx.dry_run {
        None
    } else {
        Some(state::mutation_guard()?)
    };
    let tree = DesiredTree::build_own(ctx, overlay)?;
    let routing = overlay.resolve_mappings(ctx)?;
    let policy = crate::overlays::ignore::SourcePolicy::new(&ctx.repository.root, overlay)?;
    let mut checked = BTreeSet::new();
    let mut requests = Vec::new();
    let mut associations = Vec::new();
    let mut generated_requests: std::collections::BTreeMap<
        PathBuf,
        (&crate::desired::DesiredEntry, Vec<PathBuf>),
    > = std::collections::BTreeMap::new();
    for input in paths {
        let logical = routing.logical_path_for(input)?;
        ensure!(
            !crate::secrets::is_encrypted(&logical),
            "encrypted checkout restore is unsupported"
        );
        let target = routing.target_for(&logical);
        if !checked.insert(target.clone()) {
            continue;
        }
        ensure!(
            policy.includes(&overlay.root.join(&logical), false),
            "'{}' is ignored or is control metadata",
            target.display()
        );
        let entry = owner(&tree, &target)?;
        let relative = target.strip_prefix(&entry.target)?;
        if crate::materialize::virtual_checkout::generated::generated_paths(entry)?
            .contains(relative)
        {
            ensure!(
                !options.staged && options.source.is_none(),
                "generated output can only be restored from its generated desired snapshot; edit the source template for source changes"
            );
            ensure!(
                !relative.as_os_str().is_empty(),
                "specify generated destination files to restore"
            );
            generated_requests
                .entry(entry.target.clone())
                .or_insert_with(|| (entry, Vec::new()))
                .1
                .push(relative.to_path_buf());
            continue;
        }
        let scope = entry.intent.checkout_scope();
        ensure!(
            !relative.as_os_str().is_empty()
                && scope.owns_source(relative)
                && !git::is_overlay_metadata(relative),
            "'{}' is not an owned checkout file",
            target.display()
        );
        let record = state::record_for_entry(entry)?.with_context(|| {
            format!(
                "no checkout at '{}'; run over apply first",
                entry.target.display()
            )
        })?;
        state::verify(entry, &record)?;
        let Provenance::Overlay { source, .. } = &entry.provenance else {
            unreachable!("checkout entries carry overlay provenance")
        };
        let (repo, managed) = git::discover_source(source)?;
        requests.push(RestorePath {
            repository: repo.path().to_path_buf(),
            source: managed.join(relative),
            root: entry.target.clone(),
            relative: relative.to_path_buf(),
        });
        associations.push((entry, record));
    }
    for (entry, paths) in generated_requests.values() {
        crate::materialize::virtual_checkout::generated::restore(entry, paths, true)?;
    }
    restore_paths(&requests, options, ctx.dry_run, || {
        for (entry, original) in &associations {
            let current = state::record_for_entry(entry)?
                .context("checkout association disappeared while preparing restore")?;
            state::verify(entry, &current)?;
            ensure!(
                current == *original,
                "checkout state changed while preparing restore; rerun over restore"
            );
        }
        Ok(())
    })?;
    for (entry, paths) in generated_requests.values() {
        crate::materialize::virtual_checkout::generated::restore(entry, paths, ctx.dry_run)?;
    }
    Ok(())
}
