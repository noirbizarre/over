use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result};
use walkdir::WalkDir;

use crate::actions::git::config::ROOT_PATH;
use crate::actions::{partial, symlink};
use crate::exec;
use crate::overlays::{
    MaterializationKind, Overlay,
    ignore::SourcePolicy,
    mapping::{self, ResolvedMappings},
};

use super::entry::{DesiredEntry, MaterializationIntent, Provenance};

/// The canonical desired filesystem state for an overlay and everything it
/// transitively `uses`. See the [module docs](super) for the overall model.
#[derive(Debug, Clone, Default)]
pub struct DesiredTree {
    entries: Vec<DesiredEntry>,
    pub(crate) sources: Vec<SourceSelection>,
}

/// Retain resolved policy even for paths deleted from today's filesystem.
#[derive(Debug, Clone)]
pub(crate) struct SourceSelection {
    pub overlay: Overlay,
    pub routing: ResolvedMappings,
    pub policy: Arc<SourcePolicy>,
}

impl DesiredTree {
    /// Build the desired tree for `overlay` and everything it transitively
    /// `uses` (flat union, ADR-008), honoring `ctx.no_uses`.
    ///
    /// Mirrors [`Overlay::apply`]'s traversal and cycle detection, but only
    /// reads the filesystem (to resolve `.link.*` sidecar targets and detect
    /// directory-vs-file symlink targets) and never writes anything.
    pub fn build(ctx: &exec::Context, overlay: &Overlay) -> Result<Self> {
        Self::build_mode(ctx, overlay, false)
    }

    /// Reporting validates Git content through tracked inventories, not an unrelated source walk.
    pub(crate) fn build_reporting(ctx: &exec::Context, overlay: &Overlay) -> Result<Self> {
        Self::build_mode(ctx, overlay, true)
    }

    fn build_mode(ctx: &exec::Context, overlay: &Overlay, reporting: bool) -> Result<Self> {
        let mut entries = Vec::new();
        let mut visited = HashSet::new();
        let mut stack = Vec::new();
        let mut sources = Vec::new();
        build_inner(
            ctx,
            overlay,
            &mut visited,
            &mut stack,
            &mut entries,
            &mut sources,
            reporting,
        )?;
        // Canonical, deterministic ordering — useful for #109 diff and tests.
        entries.sort_by(|a, b| a.target.cmp(&b.target));
        Ok(Self { entries, sources })
    }

    /// This overlay's own entries only — no `uses` recursion.
    ///
    /// `Overlay::apply` (#13) uses this so its per-overlay-node
    /// orchestration (progress banners, cycle detection, git cloning) stays
    /// untouched while the directory/symlink surface goes through a
    /// [`crate::plan::Plan`]. [`Self::build`] (the full, recursive graph)
    /// remains the entry point for whole-overlay status/diff (#12/#109),
    /// which need every `uses` dependency's entries too.
    pub fn build_own(ctx: &exec::Context, overlay: &Overlay) -> Result<Self> {
        // Stage selections are absolute; relative roots must share their namespace.
        let target = mapping::absolute(&overlay.resolve_target(ctx)?)?;
        let mut sources = Vec::new();
        let mut entries = collect_own_entries(ctx, overlay, &target, &mut sources, false)?;
        // Same deterministic ordering as `build()`.
        entries.sort_by(|a, b| a.target.cmp(&b.target));
        Ok(Self { entries, sources })
    }

    pub fn entries(&self) -> &[DesiredEntry] {
        &self.entries
    }

    /// Reconstruct checkout ownership from Git names, never from untracked directories.
    pub(crate) fn build_commit(ctx: &exec::Context, overlay: &Overlay) -> Result<Self> {
        let (repo, managed) =
            crate::materialize::virtual_checkout::git::discover_source(&overlay.root)?;
        let head = repo.head()?.peel_to_tree()?;
        let mut paths = std::collections::BTreeSet::new();
        let mut control_candidates = std::collections::BTreeSet::new();
        head.walk(git2::TreeWalkMode::PreOrder, |root, entry| {
            if entry.kind() != Some(git2::ObjectType::Tree)
                && let Ok(name) = entry.name()
                && let Ok(relative) = Path::new(root).join(name).strip_prefix(&managed)
            {
                paths.insert(overlay.root.join(relative));
            }
            git2::TreeWalkResult::Ok
        })?;
        for entry in repo.index()?.iter() {
            if let Some(relative) = crate::materialize::virtual_checkout::git::relative_index_path(
                &entry.path,
                &managed,
            )? {
                if head
                    .get_path(&managed.join(&relative))
                    .ok()
                    .map(|old| (old.id(), old.filemode() as u32))
                    != Some((entry.id, entry.mode))
                {
                    control_candidates.insert(overlay.root.join(&relative));
                }
                paths.insert(overlay.root.join(relative));
            }
        }
        let state =
            crate::materialize::virtual_checkout::state::state_file()?.inspect_blocking()?;
        for record in state
            .checkouts
            .values()
            .filter(|record| record.overlay == overlay.name)
        {
            // State is shared across collections; same-named overlays are not the same source.
            if let Some(recorded) = &record.source_repository
                && mapping::physical(recorded)?
                    != mapping::physical(
                        repo.workdir().context("source has no working directory")?,
                    )?
            {
                continue;
            }
            if let Ok(relative) = record.managed_path.strip_prefix(&managed) {
                // Keep empty/deleted checkout roots discoverable without inspecting their children.
                paths.insert(overlay.root.join(relative).join(".over-commit-inventory"));
                control_candidates.insert(overlay.root.join(relative));
            }
        }
        let routing = overlay.resolve_mappings(ctx)?;
        for boundary in routing.boundaries.keys() {
            paths.insert(overlay.root.join(boundary).join(".over-commit-inventory"));
            control_candidates.insert(overlay.root.join(boundary));
        }
        // Relevant new control files can change ownership before they are themselves staged.
        let candidates: std::collections::BTreeSet<_> = control_candidates
            .iter()
            .flat_map(|path| {
                path.ancestors()
                    .take_while(|path| *path != overlay.root && path.starts_with(&overlay.root))
            })
            .collect();
        for candidate in candidates {
            for extension in crate::overlays::EXTENSIONS {
                for kind in ["link", "partial"] {
                    let mut name = candidate.as_os_str().to_os_string();
                    name.push(format!(".{kind}.{extension}"));
                    let sidecar = PathBuf::from(name);
                    match sidecar.symlink_metadata() {
                        Ok(_) => {
                            paths.insert(sidecar);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error.into()),
                    }
                }
            }
        }
        let paths: Vec<_> = paths.into_iter().collect();
        let policy = Arc::new(SourcePolicy::for_inventory(
            &ctx.repository.root,
            overlay,
            &paths,
        )?);
        let mut directories = std::collections::BTreeSet::from([PathBuf::new()]);
        for path in &paths {
            for parent in path.strip_prefix(&overlay.root)?.ancestors().skip(1) {
                directories.insert(parent.to_path_buf());
            }
        }
        let mut entries = Vec::new();
        let mut terminal = Vec::<PathBuf>::new();
        for directory in directories {
            let boundary = routing
                .boundaries
                .keys()
                .filter(|boundary| directory.starts_with(boundary))
                .max_by_key(|boundary| boundary.components().count());
            if terminal.iter().any(|parent| {
                directory.starts_with(parent)
                    && boundary
                        .is_none_or(|boundary| !boundary.starts_with(parent) || boundary == parent)
            }) {
                continue;
            }
            let source = overlay.root.join(&directory);
            if !policy.includes(&source, true) {
                continue;
            }
            let intent = match overlay.materialization_for(&directory) {
                MaterializationKind::Checkout => checkout_intent(&routing, &directory),
                MaterializationKind::SymlinkDirectory => MaterializationIntent::SymlinkDirectory {
                    source: source.clone(),
                    link_type: symlink::LinkType::Soft,
                },
                MaterializationKind::Symlink => continue,
            };
            terminal.push(directory.clone());
            entries.push(DesiredEntry {
                target: routing.target_for(&directory),
                provenance: Provenance::Overlay {
                    overlay: overlay.name.clone(),
                    source,
                },
                intent,
                permissions: None,
            });
        }
        build_git_entries(overlay, &routing, &mut entries);
        build_symlink_sidecars(
            ctx,
            overlay,
            &policy,
            &overlay.resolve_target(ctx)?,
            &mut entries,
            true,
        )?;
        build_partial_sidecars(ctx, overlay, &policy, &mut entries)?;
        attach_source_policy(&mut entries, &policy);
        finalize_entries(overlay, &routing, &mut entries, true)?;
        entries.retain(|entry| entry.intent.is_virtual_checkout());
        for entry in &entries {
            crate::materialize::virtual_checkout::state::verify_live_target_snapshot(
                &state, entry,
            )?;
        }
        Ok(Self {
            entries,
            sources: vec![SourceSelection {
                overlay: overlay.clone(),
                routing,
                policy,
            }],
        })
    }

    pub(crate) fn filter_entries(&self, mut keep: impl FnMut(&DesiredEntry) -> bool) -> Self {
        Self {
            entries: self
                .entries
                .iter()
                .filter(|entry| keep(entry))
                .cloned()
                .collect(),
            sources: self.sources.clone(),
        }
    }

    /// Find the outermost scaffold that must be replaced before inspecting a child.
    pub(crate) fn blocking_scaffold(&self, target: &Path) -> Result<Option<PathBuf>> {
        let ancestors: Vec<_> = target.ancestors().skip(1).collect();
        for ancestor in ancestors.into_iter().rev() {
            if !self.entries.iter().any(|entry| {
                entry.target == ancestor && matches!(entry.intent, MaterializationIntent::Directory)
            }) {
                continue;
            }
            match ancestor.symlink_metadata() {
                Ok(metadata) if !metadata.is_dir() => return Ok(Some(ancestor.to_path_buf())),
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(None)
    }

    /// Reject incompatible claims independently of deployment authorization.
    pub fn validate_ownership(&self) -> Result<()> {
        let issues = super::ownership::check(self);
        anyhow::ensure!(
            issues.is_empty(),
            "{}",
            issues
                .into_iter()
                .map(|issue| issue.message)
                .collect::<Vec<_>>()
                .join("\n")
        );
        Ok(())
    }

    /// Test-only: build a tree directly from a list of entries, bypassing
    /// `Overlay`/config resolution. Used by other modules' tests
    /// (`crate::sync`) that exercise entry-level behavior without needing
    /// a full overlay/repository/target-resolution fixture.
    #[cfg(test)]
    pub(crate) fn from_entries(entries: Vec<DesiredEntry>) -> Self {
        Self {
            entries,
            sources: Vec::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

fn build_inner(
    ctx: &exec::Context,
    overlay: &Overlay,
    visited: &mut HashSet<String>,
    stack: &mut Vec<String>,
    entries: &mut Vec<DesiredEntry>,
    sources: &mut Vec<SourceSelection>,
    reporting: bool,
) -> Result<()> {
    // Already fully processed via another dependency path — skip silently.
    if visited.contains(&overlay.name) {
        return Ok(());
    }
    // Currently in the recursion stack — true cycle.
    if stack.contains(&overlay.name) {
        stack.push(overlay.name.clone());
        anyhow::bail!(
            "Cycle detected: overlay '{}' forms a cycle (path: {})",
            overlay.name,
            stack.join(" -> ")
        );
    }
    stack.push(overlay.name.clone());

    let target = mapping::absolute(&overlay.resolve_target(ctx)?)?;

    if let Some(uses) = &overlay.uses {
        if ctx.no_uses {
            tracing::debug!(overlay = %overlay.name, "skipping uses (--no-uses)");
        } else {
            for name in uses {
                let used = ctx
                    .repository
                    .get(name)
                    .with_context(|| format!("used overlay '{}' not found", name))?;
                let sub_ctx = ctx.with_overlay(used.clone());
                build_inner(&sub_ctx, &used, visited, stack, entries, sources, reporting)?;
            }
        }
    }

    // Preserve every claim: uses is a flat union, so traversal order must
    // never silently choose a winner for conflicting target ownership.
    entries.extend(collect_own_entries(
        ctx, overlay, &target, sources, reporting,
    )?);

    stack.pop();
    visited.insert(overlay.name.clone());

    Ok(())
}

/// This overlay's own entries: its root directory, its declared `git`
/// checkouts, its own file tree, and its `.link.*` sidecars. Excludes
/// `uses` — callers that need the full transitive graph recurse separately
/// (see [`build_inner`]); [`DesiredTree::build_own`] calls this directly for
/// a single node.
fn collect_own_entries(
    ctx: &exec::Context,
    overlay: &Overlay,
    target: &Path,
    sources: &mut Vec<SourceSelection>,
    reporting: bool,
) -> Result<Vec<DesiredEntry>> {
    // Explicit ownership wins over any overlay selected by the caller.
    let prepared = ctx
        .with_overlay(overlay.clone())
        .with_target(target.to_path_buf());
    let ctx = prepared.as_ref();
    let mut entries = Vec::new();
    let routing = overlay.resolve_mappings(ctx)?;
    let policy = ctx.repository.source_policy(overlay)?;
    sources.push(SourceSelection {
        overlay: overlay.clone(),
        routing: routing.clone(),
        policy: policy.clone(),
    });

    // #141: the whole overlay resolves to a virtual checkout (via
    // `mode = "checkout"`, including inherited policy) — one entry owns
    // the entire subtree; the base `Directory` entry
    // and the per-file/per-directory walk below are both skipped, since
    // `VirtualCheckoutMaterializer` handles the whole managed path itself
    // (mirrors `SymlinkDirectory`'s "children not separately enumerated",
    // just anchored at the overlay root instead of a subdirectory).
    if overlay.materialization_for(Path::new("")) == MaterializationKind::Checkout {
        entries.push(DesiredEntry {
            target: routing.target_for(Path::new("")),
            provenance: Provenance::Overlay {
                overlay: overlay.name.clone(),
                source: overlay.root.clone(),
            },
            intent: checkout_intent(&routing, Path::new("")),
            permissions: None,
        });
        for source in routing.boundaries.keys() {
            walk_overlay_tree(ctx, overlay, &routing, &policy, source, &mut entries)?;
        }
        build_git_entries(overlay, &routing, &mut entries);
        let ctx_with_target =
            ctx.with_resolved_overlay(overlay.name.clone(), target.to_string_lossy().to_string());
        build_symlink_sidecars(
            &ctx_with_target,
            overlay,
            &policy,
            target,
            &mut entries,
            false,
        )?;
        build_partial_sidecars(&ctx_with_target, overlay, &policy, &mut entries)?;
        attach_source_policy(&mut entries, &policy);
        finalize_entries(overlay, &routing, &mut entries, reporting)?;
        return Ok(entries);
    }

    // Every overlay needs a place to live, even one with no files of its own
    // (a `uses`-only or git-only overlay) — mirrors the unconditional
    // `EnsureDir` at the top of `Overlay::apply_inner`.
    entries.push(DesiredEntry {
        target: routing.target_for(Path::new("")),
        provenance: Provenance::Overlay {
            overlay: overlay.name.clone(),
            source: overlay.root.clone(),
        },
        intent: MaterializationIntent::Directory,
        permissions: None,
    });

    build_git_entries(overlay, &routing, &mut entries);

    walk_overlay_tree(ctx, overlay, &routing, &policy, Path::new(""), &mut entries)?;
    for source in routing.boundaries.keys() {
        walk_overlay_tree(ctx, overlay, &routing, &policy, source, &mut entries)?;
    }

    // Mirrors the same resolved-overlay bookkeeping `Overlay::apply` uses so
    // `.link.*` sidecar templates get the same (limited) `{{ overlays[...] }}`
    // support as before #13.
    let ctx_with_target =
        ctx.with_resolved_overlay(overlay.name.clone(), target.to_string_lossy().to_string());
    build_symlink_sidecars(
        &ctx_with_target,
        overlay,
        &policy,
        target,
        &mut entries,
        false,
    )?;
    build_partial_sidecars(&ctx_with_target, overlay, &policy, &mut entries)?;

    attach_source_policy(&mut entries, &policy);
    finalize_entries(overlay, &routing, &mut entries, reporting)?;
    Ok(entries)
}

fn attach_source_policy(entries: &mut [DesiredEntry], policy: &Arc<SourcePolicy>) {
    for entry in entries.iter_mut() {
        if entry.intent.is_virtual_checkout()
            && let Provenance::Overlay { source, .. } = &entry.provenance
        {
            let mut scope = entry.intent.checkout_scope();
            scope.source_policy = Some((source.clone(), policy.clone()));
            entry.intent = MaterializationIntent::ScopedVirtualCheckout(scope);
        }
    }
}

fn finalize_entries(
    overlay: &Overlay,
    routing: &ResolvedMappings,
    entries: &mut [DesiredEntry],
    reporting: bool,
) -> Result<()> {
    for entry in entries.iter() {
        if !reporting
            && entry.intent.is_virtual_checkout()
            && let Provenance::Overlay { source, .. } = &entry.provenance
        {
            // Mapped checkouts may deliberately have no source directory until reverse add.
            if source
                .symlink_metadata()
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
            {
                continue;
            }
            let scope = entry.intent.checkout_scope();
            for item in WalkDir::new(source).min_depth(1) {
                let item = item?;
                if !item.file_type().is_dir()
                    && scope.owns_source(item.path().strip_prefix(source)?)
                {
                    anyhow::ensure!(
                        !crate::secrets::is_encrypted(item.path()),
                        "Age-encrypted files require ordinary file-level materialization; virtual checkouts are unsupported"
                    );
                }
            }
        }
    }
    if !overlay.has_target_rules() {
        for entry in entries {
            crate::materialize::virtual_checkout::state::resolve_projection(entry, !reporting)?;
        }
        return Ok(());
    }
    let independent: Vec<_> = entries
        .iter()
        .filter(|entry| !matches!(entry.intent, MaterializationIntent::Directory))
        .map(|entry| (entry.target.clone(), entry.intent.clone()))
        .collect();
    for entry in entries.iter_mut() {
        if entry.intent.is_virtual_checkout() {
            let mut scope = entry.intent.checkout_scope();
            for (target, _) in &independent {
                if target != &entry.target
                    && let Ok(relative) = target.strip_prefix(&entry.target)
                {
                    scope.target_excludes.push(relative.to_path_buf());
                }
            }
            scope.target_excludes.sort();
            scope.target_excludes.dedup();
            if !scope.target_excludes.is_empty() {
                entry.intent = MaterializationIntent::ScopedVirtualCheckout(scope);
            }
        }
        if matches!(
            entry.intent,
            MaterializationIntent::SymlinkDirectory { .. } | MaterializationIntent::Checkout
        ) {
            let physical_entry = match &entry.intent {
                MaterializationIntent::SymlinkDirectory { source, .. } => {
                    mapping::physical_destination(&entry.target, source)?
                }
                _ => mapping::physical(&entry.target)?,
            };
            for destination in routing.boundaries.values() {
                // Lexically distinct aliases must not punch holes into indivisible units.
                let source = routing.logical_path_for(destination)?;
                let physical_destination =
                    mapping::physical_destination(destination, &overlay.root.join(source))?;
                anyhow::ensure!(
                    destination == &entry.target
                        || (!destination.starts_with(&entry.target)
                            && !physical_destination.starts_with(&physical_entry)),
                    "mapped destination '{}' lies inside indivisible target '{}'",
                    destination.display(),
                    entry.target.display()
                );
            }
        }
    }
    for entry in entries.iter_mut() {
        crate::materialize::virtual_checkout::state::resolve_projection(entry, !reporting)?;
    }
    for (index, entry) in entries.iter().enumerate() {
        for other in &entries[index + 1..] {
            // Mapping does not change the compatibility of shared partial files.
            if entry.target == other.target
                && let Some((target, reason)) = super::ownership::conflict(entry, other)?
            {
                anyhow::bail!(
                    "multiple entries own mapped target '{}': {reason}",
                    target.display()
                );
            }
        }
    }
    Ok(())
}

/// Git-managed paths declared via `overlay.git`: materialized by
/// `CheckoutMaterializer` (#110), which ensures presence/configuration
/// only — manage their content with ordinary Git. The CLI's `over sync`
/// handles local virtual checkouts; the broader library API in [`crate::sync`]
/// is separate. Unrelated to `MaterializationIntent::VirtualCheckout`
/// (#141, see its doc) — both can coexist at different paths within the
/// same overlay, including when the overlay root itself resolves to a
/// virtual checkout.
fn build_git_entries(
    overlay: &Overlay,
    routing: &ResolvedMappings,
    entries: &mut Vec<DesiredEntry>,
) {
    let Some(git_repos) = &overlay.git else {
        return;
    };
    for (repo_key, config) in git_repos {
        let repo_target = if repo_key == ROOT_PATH {
            routing.target_for(Path::new(""))
        } else {
            routing.target_for(Path::new(repo_key))
        };
        entries.push(DesiredEntry {
            target: repo_target,
            provenance: Provenance::Git {
                overlay: overlay.name.clone(),
                repo_key: repo_key.clone(),
                config: Box::new(config.clone()),
            },
            intent: MaterializationIntent::Checkout,
            permissions: None,
        });
    }
}

/// Walk the overlay's own tree, producing one entry per file/directory —
/// overlay descriptor files, `.link.*` sidecars, and `exclude` globs are
/// skipped, mirroring what `Overlay::apply` used to filter for inline
/// before #13 moved materialization behind a `Plan`.
fn walk_overlay_tree(
    ctx: &exec::Context,
    overlay: &Overlay,
    routing: &ResolvedMappings,
    policy: &SourcePolicy,
    start: &Path,
    entries: &mut Vec<DesiredEntry>,
) -> Result<()> {
    let mut walk_root = overlay.root.join(start);
    if !walk_root.exists() {
        let mut encrypted = walk_root.clone().into_os_string();
        encrypted.push(".age");
        if Path::new(&encrypted).is_file() {
            // Target selectors address decrypted logical names, not their storage suffix.
            walk_root = PathBuf::from(encrypted);
        }
    }
    if policy.is_ignored(&walk_root, !walk_root.is_file()) {
        return Ok(());
    }
    // An absent mapped directory can be populated later by reverse add.
    if !walk_root.exists() {
        if sidecar_config_path(&overlay.root, &start.to_string_lossy(), policy).is_file() {
            // A link sidecar materializes its logical name later; a directory here would conflict.
            return Ok(());
        }
        if !start.as_os_str().is_empty() && policy.includes(&walk_root, true) {
            entries.push(DesiredEntry {
                target: routing.target_for(start),
                provenance: Provenance::Overlay {
                    overlay: overlay.name.clone(),
                    source: walk_root,
                },
                intent: if overlay.materialization_for(start) == MaterializationKind::Checkout {
                    checkout_intent(routing, start)
                } else {
                    MaterializationIntent::Directory
                },
                permissions: None,
            });
        }
        return Ok(());
    }
    let mut walker = WalkDir::new(&walk_root)
        .min_depth(usize::from(start.as_os_str().is_empty()))
        .into_iter();
    while let Some(entry) = walker.next() {
        // An incomplete source walk cannot produce a trustworthy health report or plan.
        let entry = entry?;
        let path = entry.path();
        if !policy.includes(path, entry.file_type().is_dir()) {
            if entry.file_type().is_dir() {
                walker.skip_current_dir();
            }
            continue;
        }

        let rel_path = match path.strip_prefix(&overlay.root) {
            Ok(r) => r,
            Err(_) => continue,
        };

        let encrypted = !entry.file_type().is_dir() && crate::secrets::is_encrypted(path);
        let logical = if encrypted {
            anyhow::ensure!(
                entry.file_type().is_file(),
                "encrypted source must be a regular file"
            );
            let stem = rel_path
                .file_stem()
                .filter(|stem| !stem.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!("encrypted filename must have a nonempty basename")
                })?;
            let logical = rel_path.with_file_name(stem);
            anyhow::ensure!(
                logical.file_name().is_some_and(|name| name != ".age")
                    && !crate::overlays::ignore::is_control_file(&logical),
                "encrypted control files are unsupported"
            );
            logical
        } else {
            rel_path.to_path_buf()
        };
        if logical != start && routing.boundaries.contains_key(&logical) {
            if entry.file_type().is_dir() {
                walker.skip_current_dir();
            }
            continue;
        }
        let entry_target = routing.target_for(&logical);
        anyhow::ensure!(
            routing.logical_path_for(&entry_target)? == logical,
            "residual path '{}' collides with a mapped destination",
            rel_path.display()
        );

        // Terminal modes own their subtree; enumerating children would create overlapping units.
        if path.is_dir() {
            match overlay.materialization_for(rel_path) {
                MaterializationKind::SymlinkDirectory => {
                    policy.validate_directory_link(path, path)?;
                    if entry_target
                        .symlink_metadata()
                        .is_ok_and(|metadata| metadata.is_dir())
                    {
                        // A clean managed view does not authorize deleting newly ignored target files.
                        policy.validate_directory_link(&entry_target, path)?;
                    }
                    entries.push(DesiredEntry {
                        target: entry_target,
                        provenance: Provenance::Overlay {
                            overlay: overlay.name.clone(),
                            source: path.to_path_buf(),
                        },
                        intent: MaterializationIntent::SymlinkDirectory {
                            source: path.to_path_buf(),
                            link_type: symlink::LinkType::Soft,
                        },
                        permissions: None,
                    });
                    walker.skip_current_dir();
                    continue;
                }
                MaterializationKind::Checkout => {
                    // #141: a subtree-level virtual checkout — the whole
                    // directory is one materialization unit owned by
                    // `VirtualCheckoutMaterializer`; its files are not
                    // separately enumerated here (it does its own
                    // git-tree-based enumeration instead).
                    entries.push(DesiredEntry {
                        target: entry_target,
                        provenance: Provenance::Overlay {
                            overlay: overlay.name.clone(),
                            source: path.to_path_buf(),
                        },
                        intent: checkout_intent(routing, rel_path),
                        permissions: None,
                    });
                    walker.skip_current_dir();
                    continue;
                }
                MaterializationKind::Symlink => {}
            }
        }

        if path.is_dir() {
            entries.push(DesiredEntry {
                target: entry_target,
                provenance: Provenance::Overlay {
                    overlay: overlay.name.clone(),
                    source: path.to_path_buf(),
                },
                intent: MaterializationIntent::Directory,
                permissions: None,
            });
        } else {
            entries.push(DesiredEntry {
                target: entry_target,
                provenance: Provenance::Overlay {
                    overlay: overlay.name.clone(),
                    source: path.to_path_buf(),
                },
                intent: if encrypted {
                    MaterializationIntent::EncryptedFile(crate::secrets::EncryptedFile {
                        source: path.to_path_buf(),
                        crypto: ctx.crypto.clone(),
                    })
                } else {
                    MaterializationIntent::SymlinkFile {
                        source: path.to_path_buf(),
                        link_type: symlink::LinkType::Soft,
                    }
                },
                permissions: if encrypted {
                    overlay.permission_for(&logical)
                } else {
                    None
                },
            });
        }
    }

    Ok(())
}

/// Resolve `.link.{toml,yaml,yml}` sidecars into entries (`.link.*` config
/// files declaring an arbitrary extra symlink, soft or hard).
fn build_symlink_sidecars(
    ctx: &exec::Context,
    overlay: &Overlay,
    policy: &SourcePolicy,
    _target: &Path,
    entries: &mut Vec<DesiredEntry>,
    inventory_only: bool,
) -> Result<()> {
    let symlinks = symlink::discover_symlinks_selected(&overlay.root, Some(policy))?;
    let routing = overlay.resolve_mappings(ctx)?;
    for (name, config) in symlinks {
        let resolved = crate::overlays::mapping::resolve_path(&config.target, &overlay.root, ctx)?;
        let resolved_str = resolved.to_string_lossy();
        let is_dir = resolved.is_dir()
            || resolved_str.ends_with(std::path::MAIN_SEPARATOR_STR)
            || resolved_str.ends_with('/');
        let entry_target = routing.target_for(Path::new(&name));
        let config_path = sidecar_config_path(&overlay.root, &name, policy);

        let provenance = Provenance::SymlinkSidecar {
            overlay: overlay.name.clone(),
            config: config_path,
            template: config.target.clone(),
            resolved: resolved.clone(),
        };
        let intent = if is_dir {
            // Commit needs ownership holes, not validation of unrelated link contents.
            if !inventory_only {
                crate::secrets::reject_encrypted_directory(&resolved)?;
                if resolved.starts_with(&overlay.root) {
                    policy.validate_directory_link(&resolved, &resolved)?;
                }
            }
            MaterializationIntent::SymlinkDirectory {
                source: resolved,
                link_type: config.r#type,
            }
        } else {
            MaterializationIntent::SymlinkFile {
                source: resolved,
                link_type: config.r#type,
            }
        };

        entries.push(DesiredEntry {
            target: entry_target,
            provenance,
            intent,
            // Symlink sidecars carry a source outside the overlay's own
            // walked tree, but the same reasoning applies: no independent
            // target permission without mutating that source (ADR-020).
            permissions: None,
        });
    }
    Ok(())
}

fn checkout_intent(routing: &ResolvedMappings, logical: &Path) -> MaterializationIntent {
    let target = routing.target_for(logical);
    let source_excludes: Vec<_> = routing
        .boundaries
        .keys()
        .filter(|source| source.as_path() != logical)
        .filter_map(|source| source.strip_prefix(logical).ok().map(Path::to_path_buf))
        .collect();
    let mut target_excludes = source_excludes.clone();
    for destination in routing.boundaries.values() {
        if destination != &target
            && let Ok(tail) = destination.strip_prefix(&target)
        {
            target_excludes.push(tail.to_path_buf());
        }
    }
    target_excludes.sort();
    target_excludes.dedup();
    if source_excludes.is_empty() && target_excludes.is_empty() {
        MaterializationIntent::VirtualCheckout
    } else {
        MaterializationIntent::ScopedVirtualCheckout(super::CheckoutScope {
            source_excludes,
            target_excludes,
            source_policy: None,
            projection: None,
        })
    }
}

/// Reconstruct the sidecar file path for a symlink `stem`, since
/// `discover_symlinks` only returns the stem and parsed config, not the
/// originating file. Mirrors its own TOML > YAML > YML precedence.
fn sidecar_config_path(overlay_root: &Path, stem: &str, policy: &SourcePolicy) -> PathBuf {
    for ext in ["toml", "yaml", "yml"] {
        let candidate = overlay_root.join(format!("{stem}.link.{ext}"));
        if candidate.exists() && !policy.is_ignored(&candidate, false) {
            return candidate;
        }
    }
    // Shouldn't happen: `discover_symlinks` found this stem from one of
    // these three files. Fall back to the canonical (TOML) form for a
    // stable, if slightly inaccurate, diagnostic path.
    overlay_root.join(format!("{stem}.link.toml"))
}

/// Resolve `.partial.{toml,yaml,yml}` sidecars into
/// [`MaterializationIntent::PartialFile`] entries (#66): a managed block
/// injected into `target` (rendered the same way a `.link.*` sidecar's
/// `target` is — this reuses `symlink::render_symlink_target` directly,
/// since it's already a generic path-template renderer, not
/// symlink-specific in implementation).
///
/// Unlike `.link.*` sidecars, the entry's target is the rendered path
/// *itself*, not `target_root.join(stem)`: a managed block's target is an
/// arbitrary external file (e.g. `~/.zshrc`), not something living under
/// the overlay's own target root by naming convention.
fn build_partial_sidecars(
    ctx: &exec::Context,
    overlay: &Overlay,
    policy: &SourcePolicy,
    entries: &mut Vec<DesiredEntry>,
) -> Result<()> {
    let partials = partial::discover_partials_selected(&overlay.root, Some(policy))?;
    for (name, config) in partials {
        let resolved = crate::overlays::mapping::resolve_path(&config.target, &overlay.root, ctx)?;
        let config_path = partial_sidecar_config_path(&overlay.root, &name, policy);
        let marker = config.marker.unwrap_or_else(|| name.clone());
        // The only intent `over` writes real target content for directly
        // (#65) — resolved against the sidecar's own overlay-relative stem
        // (not the rendered, possibly-external `target`), so `permissions`
        // rules match the same identity `.link.*`/`.partial.*` naming
        // already uses elsewhere.
        let permissions = overlay.permission_for(Path::new(&name));

        entries.push(DesiredEntry {
            target: resolved,
            provenance: Provenance::PartialSidecar {
                overlay: overlay.name.clone(),
                config: config_path,
            },
            intent: MaterializationIntent::PartialFile {
                content: config.content,
                marker,
            },
            permissions,
        });
    }
    Ok(())
}

/// Reconstruct the sidecar file path for a partial `stem`, mirroring
/// [`sidecar_config_path`]'s own TOML > YAML > YML precedence.
fn partial_sidecar_config_path(overlay_root: &Path, stem: &str, policy: &SourcePolicy) -> PathBuf {
    for ext in ["toml", "yaml", "yml"] {
        let candidate = overlay_root.join(format!("{stem}.partial.{ext}"));
        if candidate.exists() && !policy.is_ignored(&candidate, false) {
            return candidate;
        }
    }
    overlay_root.join(format!("{stem}.partial.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::symlink::LinkType;
    use crate::exec::Context;
    use crate::overlays::Repository;
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    use rstest::rstest;

    fn repo_and_root() -> (TempDir, Repository) {
        let td = TempDir::new().unwrap();
        let repo = Repository::new(td.path().to_path_buf());
        (td, repo)
    }

    #[cfg(unix)]
    #[test]
    fn a_mapped_alias_cannot_enter_an_indivisible_git_checkout() {
        let (temp, repo) = repo_and_root();
        temp.child("home/base").create_dir_all().unwrap();
        std::os::unix::fs::symlink(temp.path().join("home/base"), temp.path().join("alias"))
            .unwrap();
        temp.child("ov/over.toml")
            .write_str(&format!(
                "target='~/base'\ngit='https://example.com/repo.git'\n[[rules]]\npath='config'\ntarget='{}'",
                temp.path().join("alias/new").display()
            ))
            .unwrap();
        let overlay = repo.get("ov").unwrap();
        let ctx = ctx(temp.path().join("home"), repo);
        assert!(
            DesiredTree::build_own(&ctx, &overlay)
                .unwrap_err()
                .to_string()
                .contains("indivisible Git checkout")
        );
        assert!(!temp.path().join("home/base/new").exists());
    }

    fn ctx(root: PathBuf, repo: Repository) -> exec::Ctx {
        Context::builder().root(root).repository(repo).build()
    }

    fn ctx_no_uses(root: PathBuf, repo: Repository) -> exec::Ctx {
        Context::builder()
            .root(root)
            .repository(repo)
            .no_uses(true)
            .build()
    }

    #[rstest]
    #[case("one", "two", "600", "600", None)]
    #[case("one", "one", "600", "600", Some("duplicate partial marker"))]
    #[case("one", "two", "600", "644", Some("incompatible file-wide permissions"))]
    #[tokio::test]
    async fn unrelated_mappings_preserve_partial_compatibility_and_refuse_invalid_claims(
        #[case] first: &str,
        #[case] second: &str,
        #[case] first_mode: &str,
        #[case] second_mode: &str,
        #[case] expected_error: Option<&str>,
    ) {
        let (td, repo) = repo_and_root();
        td.child("ov/over.yaml").write_str(&format!(
            "rules:\n  - {{path: config, target: '~/mapped'}}\n  - {{path: first, permissions: '{first_mode}'}}\n  - {{path: second, permissions: '{second_mode}'}}"
        )).unwrap();
        td.child("ov/config/file")
            .write_str("mapped content")
            .unwrap();
        for (name, marker) in [("first", first), ("second", second)] {
            td.child(format!("ov/{name}.partial.yaml"))
                .write_str(&format!(
                    "target: '~/shell'\nmarker: {marker}\ncontent: {name}"
                ))
                .unwrap();
        }
        let overlay = repo.get("ov").unwrap();
        let root = td.path().join("home");
        let context = Context::builder()
            .repository(repo)
            .root(root.clone())
            .force(true)
            .no_prompt(true)
            .build();
        if let Some(expected) = expected_error {
            let error = overlay.apply(&context).await.unwrap_err();
            assert!(error.to_string().contains(expected), "{error:#}");
            assert!(!root.exists());
        } else {
            overlay.apply(&context).await.unwrap();
            let content = std::fs::read_to_string(root.join("shell")).unwrap();
            assert!(content.contains("first"), "{content}");
            assert!(content.contains("second"), "{content}");
            assert_eq!(
                std::fs::read_to_string(root.join("mapped/file")).unwrap(),
                "mapped content"
            );
            overlay.apply(&context).await.unwrap();
            assert_eq!(
                std::fs::read_to_string(root.join("shell")).unwrap(),
                content
            );
        }
    }

    #[rstest]
    fn empty_overlay_has_only_root_directory_entry() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());

        let tree = DesiredTree::build(&c, &overlay).unwrap();
        assert_eq!(tree.len(), 1);
        let entry = &tree.entries()[0];
        assert_eq!(entry.target, td.path().to_path_buf());
        assert!(matches!(entry.intent, MaterializationIntent::Directory));
    }

    #[rstest]
    fn plain_files_and_nested_dirs_produce_expected_entries() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        overlay_dir.child("file.txt").write_str("content").unwrap();
        overlay_dir.child("sub").create_dir_all().unwrap();
        overlay_dir
            .child("sub/nested.txt")
            .write_str("nested")
            .unwrap();

        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let tree = DesiredTree::build(&c, &overlay).unwrap();

        let root = td.path().to_path_buf();
        // root dir + file.txt + sub (dir) + sub/nested.txt
        assert_eq!(tree.len(), 4);

        let file_entry = tree
            .entries()
            .iter()
            .find(|e| e.target == root.join("file.txt"))
            .expect("file.txt entry present");
        match &file_entry.intent {
            MaterializationIntent::SymlinkFile { source, link_type } => {
                assert_eq!(source, &overlay.root.join("file.txt"));
                assert_eq!(*link_type, LinkType::Soft);
            }
            other => panic!("unexpected intent: {other:?}"),
        }

        let sub_entry = tree
            .entries()
            .iter()
            .find(|e| e.target == root.join("sub"))
            .expect("sub dir entry present");
        assert!(matches!(sub_entry.intent, MaterializationIntent::Directory));

        let nested_entry = tree
            .entries()
            .iter()
            .find(|e| e.target == root.join("sub/nested.txt"))
            .expect("sub/nested.txt entry present");
        assert!(matches!(
            nested_entry.intent,
            MaterializationIntent::SymlinkFile { .. }
        ));
    }

    #[rstest]
    fn excluded_files_are_absent() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"\nexclude = \"*.bak\"")
            .unwrap();
        overlay_dir.child("file.txt").write_str("keep").unwrap();
        overlay_dir.child("file.bak").write_str("skip").unwrap();

        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let tree = DesiredTree::build(&c, &overlay).unwrap();

        let root = td.path().to_path_buf();
        assert!(
            tree.entries()
                .iter()
                .any(|e| e.target == root.join("file.txt"))
        );
        assert!(
            !tree
                .entries()
                .iter()
                .any(|e| e.target == root.join("file.bak"))
        );
    }

    #[rstest]
    fn a_directory_link_rule_produces_one_entry_without_children() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"\n[[rules]]\npath = 'mydir'\nmode = 'symlink-directory'")
            .unwrap();
        overlay_dir.child("mydir").create_dir_all().unwrap();
        overlay_dir
            .child("mydir/inner.txt")
            .write_str("content")
            .unwrap();

        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let tree = DesiredTree::build(&c, &overlay).unwrap();

        let root = td.path().to_path_buf();
        let dir_entry = tree
            .entries()
            .iter()
            .find(|e| e.target == root.join("mydir"))
            .expect("mydir entry present");
        match &dir_entry.intent {
            MaterializationIntent::SymlinkDirectory { source, link_type } => {
                assert_eq!(source, &overlay.root.join("mydir"));
                assert_eq!(*link_type, LinkType::Soft);
            }
            other => panic!("unexpected intent: {other:?}"),
        }
        // The child must NOT be separately enumerated.
        assert!(
            !tree
                .entries()
                .iter()
                .any(|e| e.target == root.join("mydir/inner.txt"))
        );
    }

    /// Explicit rules select whole-directory ownership, not individual child links.
    #[rstest]
    fn a_materialization_rule_produces_a_symlink_directory_entry() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"\n[[rules]]\npath = \"mydir\"\nmode = \"symlink-directory\"")
            .unwrap();
        overlay_dir.child("mydir").create_dir_all().unwrap();
        overlay_dir
            .child("mydir/inner.txt")
            .write_str("content")
            .unwrap();

        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let tree = DesiredTree::build(&c, &overlay).unwrap();

        let root = td.path().to_path_buf();
        let dir_entry = tree
            .entries()
            .iter()
            .find(|e| e.target == root.join("mydir"))
            .expect("mydir entry present");
        assert!(matches!(
            dir_entry.intent,
            MaterializationIntent::SymlinkDirectory { .. }
        ));
        assert!(
            !tree
                .entries()
                .iter()
                .any(|e| e.target == root.join("mydir/inner.txt"))
        );
    }

    /// The baseline applies to every directory without a later rule override.
    #[rstest]
    fn the_mode_baseline_applies_to_every_directory_without_a_rule() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"\nmode = \"symlink-directory\"")
            .unwrap();
        overlay_dir.child("sub").create_dir_all().unwrap();
        overlay_dir
            .child("sub/inner.txt")
            .write_str("content")
            .unwrap();

        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let tree = DesiredTree::build(&c, &overlay).unwrap();

        let root = td.path().to_path_buf();
        let sub_entry = tree
            .entries()
            .iter()
            .find(|e| e.target == root.join("sub"))
            .expect("sub entry present");
        assert!(matches!(
            sub_entry.intent,
            MaterializationIntent::SymlinkDirectory { .. }
        ));
    }

    /// #113: a file added to a source subtree governed by a file-level
    /// rule appears in the next `DesiredTree` build with no rule change —
    /// `walk_overlay_tree` re-walks the filesystem every call, so this is
    /// a regression test, not new behavior.
    #[rstest]
    fn a_newly_added_file_is_picked_up_without_any_rule_change() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();

        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let root = td.path().to_path_buf();

        let before = DesiredTree::build(&c, &overlay).unwrap();
        assert!(
            !before
                .entries()
                .iter()
                .any(|e| e.target == root.join("new.txt"))
        );

        overlay_dir.child("new.txt").write_str("content").unwrap();

        let after = DesiredTree::build(&c, &overlay).unwrap();
        let entry = after
            .entries()
            .iter()
            .find(|e| e.target == root.join("new.txt"))
            .expect("newly added file should appear in the next build");
        assert!(matches!(
            entry.intent,
            MaterializationIntent::SymlinkFile { .. }
        ));
    }

    #[rstest]
    fn symlink_sidecar_file_target_produces_symlink_file_entry() {
        let (td, repo) = repo_and_root();
        let referent = td.path().join("nvim-config");
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        overlay_dir
            .child("nvim.link.toml")
            .write_str(&format!("target = {:?}", referent.to_str().unwrap()))
            .unwrap();

        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let tree = DesiredTree::build(&c, &overlay).unwrap();

        let root = td.path().to_path_buf();
        let entry = tree
            .entries()
            .iter()
            .find(|e| e.target == root.join("nvim"))
            .expect("nvim entry present");
        match &entry.intent {
            MaterializationIntent::SymlinkFile { source, link_type } => {
                assert_eq!(source, &referent);
                assert_eq!(*link_type, LinkType::Soft);
            }
            other => panic!("unexpected intent: {other:?}"),
        }
        match &entry.provenance {
            Provenance::SymlinkSidecar {
                config,
                template,
                resolved,
                ..
            } => {
                assert_eq!(config, &overlay.root.join("nvim.link.toml"));
                assert_eq!(template, referent.to_str().unwrap());
                assert_eq!(resolved, &referent);
            }
            other => panic!("unexpected provenance: {other:?}"),
        }
    }

    #[rstest]
    fn symlink_sidecar_directory_target_produces_symlink_directory_entry() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let real_dir = td.child("real_target_dir");
        real_dir.create_dir_all().unwrap();
        // Escape backslashes so Windows paths don't get misparsed as TOML
        // unicode escape sequences (see `Overlay::resolve_target` tests).
        let target_toml = real_dir.path().to_string_lossy().replace('\\', "\\\\");
        overlay_dir
            .child("app.link.toml")
            .write_str(&format!("target = \"{}\"", target_toml))
            .unwrap();

        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let tree = DesiredTree::build(&c, &overlay).unwrap();

        let root = td.path().to_path_buf();
        let entry = tree
            .entries()
            .iter()
            .find(|e| e.target == root.join("app"))
            .expect("app entry present");
        assert!(matches!(
            entry.intent,
            MaterializationIntent::SymlinkDirectory { .. }
        ));
    }

    #[rstest]
    fn symlink_sidecar_hard_type_is_preserved() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        overlay_dir
            .child("hard.link.toml")
            .write_str("target = \"/opt/hard-target\"\ntype = \"hard\"")
            .unwrap();

        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let tree = DesiredTree::build(&c, &overlay).unwrap();

        let root = td.path().to_path_buf();
        let entry = tree
            .entries()
            .iter()
            .find(|e| e.target == root.join("hard"))
            .expect("hard entry present");
        match &entry.intent {
            MaterializationIntent::SymlinkFile { link_type, .. } => {
                assert_eq!(*link_type, LinkType::Hard);
            }
            other => panic!("unexpected intent: {other:?}"),
        }
    }

    #[rstest]
    fn partial_sidecar_produces_partial_file_entry_at_rendered_target() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let external = td.child("external.zshrc");
        external.write_str("export FOO=bar\n").unwrap();
        let target_toml = external.path().to_string_lossy().replace('\\', "\\\\");
        overlay_dir
            .child("aliases.partial.toml")
            .write_str(&format!(
                "target = \"{}\"\ncontent = \"alias x=y\"",
                target_toml
            ))
            .unwrap();

        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let tree = DesiredTree::build(&c, &overlay).unwrap();

        let entry = tree
            .entries()
            .iter()
            .find(|e| e.target == external.path().to_path_buf())
            .expect("partial entry present at the rendered target");
        match &entry.intent {
            MaterializationIntent::PartialFile { content, marker } => {
                assert_eq!(content, "alias x=y");
                assert_eq!(marker, "aliases");
            }
            other => panic!("unexpected intent: {other:?}"),
        }
        match &entry.provenance {
            Provenance::PartialSidecar { config, .. } => {
                assert_eq!(config, &overlay.root.join("aliases.partial.toml"));
            }
            other => panic!("unexpected provenance: {other:?}"),
        }
    }

    #[rstest]
    fn partial_sidecar_custom_marker_is_used() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let target_toml = td
            .path()
            .join("out.txt")
            .to_string_lossy()
            .replace('\\', "\\\\");
        overlay_dir
            .child("aliases.partial.toml")
            .write_str(&format!(
                "target = \"{}\"\ncontent = \"x\"\nmarker = \"custom\"",
                target_toml
            ))
            .unwrap();

        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let tree = DesiredTree::build(&c, &overlay).unwrap();

        let entry = tree
            .entries()
            .iter()
            .find(|e| e.target == td.path().join("out.txt"))
            .expect("partial entry present");
        match &entry.intent {
            MaterializationIntent::PartialFile { marker, .. } => assert_eq!(marker, "custom"),
            other => panic!("unexpected intent: {other:?}"),
        }
    }

    // ── #65: permission resolution ───────────────────────────────────────

    #[rstest]
    fn a_partial_sidecar_resolves_the_permission_baseline() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"\npermissions = \"600\"")
            .unwrap();
        let target_toml = td
            .path()
            .join("out.txt")
            .to_string_lossy()
            .replace('\\', "\\\\");
        overlay_dir
            .child("aliases.partial.toml")
            .write_str(&format!("target = \"{}\"\ncontent = \"x\"", target_toml))
            .unwrap();

        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let tree = DesiredTree::build(&c, &overlay).unwrap();

        let entry = tree
            .entries()
            .iter()
            .find(|e| e.target == td.path().join("out.txt"))
            .expect("partial entry present");
        assert_eq!(
            entry.permissions,
            Some(crate::overlays::FileMode::parse("600").unwrap())
        );
    }

    #[rstest]
    fn a_partial_sidecar_rule_overrides_the_permission_baseline() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str(
                "target = \"~\"\npermissions = \"644\"\n\n[[rules]]\npath = \"aliases\"\npermissions = \"600\"",
            )
            .unwrap();
        let target_toml = td
            .path()
            .join("out.txt")
            .to_string_lossy()
            .replace('\\', "\\\\");
        overlay_dir
            .child("aliases.partial.toml")
            .write_str(&format!("target = \"{}\"\ncontent = \"x\"", target_toml))
            .unwrap();

        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let tree = DesiredTree::build(&c, &overlay).unwrap();

        let entry = tree
            .entries()
            .iter()
            .find(|e| e.target == td.path().join("out.txt"))
            .expect("partial entry present");
        assert_eq!(
            entry.permissions,
            Some(crate::overlays::FileMode::parse("600").unwrap())
        );
    }

    #[rstest]
    fn partial_sidecar_without_any_rule_has_no_permission() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let target_toml = td
            .path()
            .join("out.txt")
            .to_string_lossy()
            .replace('\\', "\\\\");
        overlay_dir
            .child("aliases.partial.toml")
            .write_str(&format!("target = \"{}\"\ncontent = \"x\"", target_toml))
            .unwrap();

        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let tree = DesiredTree::build(&c, &overlay).unwrap();

        let entry = tree
            .entries()
            .iter()
            .find(|e| e.target == td.path().join("out.txt"))
            .expect("partial entry present");
        assert_eq!(entry.permissions, None);
    }

    /// Symlinked entries share their overlay source's inode — there is no
    /// independent target permission to manage without mutating that
    /// source (ADR-020), so a `permissions` rule matching a plain file's
    /// path must never populate `DesiredEntry.permissions` for it, even
    /// when the rule's `path` matches exactly.
    #[rstest]
    fn symlinked_file_never_carries_a_permission_even_with_a_matching_rule() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str(
                "target = \"~\"\npermissions = \"600\"\n\n[[rules]]\npath = \"file.txt\"\npermissions = \"600\"",
            )
            .unwrap();
        overlay_dir.child("file.txt").write_str("content").unwrap();

        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let tree = DesiredTree::build(&c, &overlay).unwrap();

        let root = td.path().to_path_buf();
        let file_entry = tree
            .entries()
            .iter()
            .find(|e| e.target == root.join("file.txt"))
            .expect("file.txt entry present");
        assert!(matches!(
            file_entry.intent,
            MaterializationIntent::SymlinkFile { .. }
        ));
        assert_eq!(file_entry.permissions, None);
    }

    #[rstest]
    fn partial_sidecar_config_files_are_not_entries() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let target_toml = td
            .path()
            .join("out.txt")
            .to_string_lossy()
            .replace('\\', "\\\\");
        overlay_dir
            .child("aliases.partial.toml")
            .write_str(&format!("target = \"{}\"\ncontent = \"x\"", target_toml))
            .unwrap();

        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let tree = DesiredTree::build(&c, &overlay).unwrap();

        assert!(
            !tree
                .entries()
                .iter()
                .any(|e| e.target == overlay.root.join("aliases.partial.toml"))
        );
    }

    #[rstest]
    fn git_root_path_produces_checkout_entry_at_target_root() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"\ngit = \"https://example.com/repo.git\"")
            .unwrap();

        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let tree = DesiredTree::build(&c, &overlay).unwrap();

        let root = td.path().to_path_buf();
        let entry = tree
            .entries()
            .iter()
            .find(|e| e.target == root && matches!(e.intent, MaterializationIntent::Checkout));
        let entry = entry.expect("checkout entry at target root present");
        match &entry.provenance {
            Provenance::Git {
                config, repo_key, ..
            } => {
                assert_eq!(config.url, "https://example.com/repo.git");
                assert_eq!(repo_key, ".");
            }
            other => panic!("unexpected provenance: {other:?}"),
        }
    }

    #[rstest]
    fn git_named_path_produces_checkout_entry_at_that_path() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"\n[git]\n\".config/nvim\" = \"https://example.com/nvim.git\"")
            .unwrap();

        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let tree = DesiredTree::build(&c, &overlay).unwrap();

        let root = td.path().to_path_buf();
        let entry = tree
            .entries()
            .iter()
            .find(|e| e.target == root.join(".config/nvim"))
            .expect("checkout entry at .config/nvim present");
        assert!(matches!(entry.intent, MaterializationIntent::Checkout));
    }

    #[rstest]
    fn uses_composition_includes_both_overlays_own_targets() {
        let (td, repo) = repo_and_root();

        let child_dir = td.child("child");
        child_dir.create_dir_all().unwrap();
        child_dir
            .child("over.toml")
            .write_str("target = \"~/child-target\"")
            .unwrap();
        child_dir.child("file.txt").write_str("content").unwrap();

        let parent_dir = td.child("parent");
        parent_dir.create_dir_all().unwrap();
        parent_dir
            .child("over.toml")
            .write_str("target = \"~/parent-target\"\nuses = [\"child\"]")
            .unwrap();
        parent_dir.child("own.txt").write_str("own").unwrap();

        let parent = repo.get("parent").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let tree = DesiredTree::build(&c, &parent).unwrap();

        let root = td.path().to_path_buf();
        assert!(
            tree.entries()
                .iter()
                .any(|e| e.target == root.join("parent-target/own.txt"))
        );
        assert!(
            tree.entries()
                .iter()
                .any(|e| e.target == root.join("child-target/file.txt"))
        );
    }

    #[rstest]
    fn no_uses_excludes_used_overlay_entries() {
        let (td, repo) = repo_and_root();

        let child_dir = td.child("child");
        child_dir.create_dir_all().unwrap();
        child_dir
            .child("over.toml")
            .write_str("target = \"~/child-target\"")
            .unwrap();
        child_dir.child("file.txt").write_str("content").unwrap();

        let parent_dir = td.child("parent");
        parent_dir.create_dir_all().unwrap();
        parent_dir
            .child("over.toml")
            .write_str("target = \"~/parent-target\"\nuses = [\"child\"]")
            .unwrap();

        let parent = repo.get("parent").unwrap();
        let c = ctx_no_uses(td.path().to_path_buf(), repo.clone());
        let tree = DesiredTree::build(&c, &parent).unwrap();

        let root = td.path().to_path_buf();
        assert!(
            !tree
                .entries()
                .iter()
                .any(|e| e.target == root.join("child-target/file.txt"))
        );
    }

    /// Diamond dependency: A uses B and C, both B and C use D.
    /// D's entries should appear only once and no false cycle error should occur.
    #[rstest]
    fn diamond_dependency_includes_shared_overlay_entries_once() {
        let (td, repo) = repo_and_root();

        let d = td.child("d");
        d.create_dir_all().unwrap();
        d.child("over.toml")
            .write_str("target = \"~/d-target\"")
            .unwrap();
        d.child("shared.txt").write_str("shared").unwrap();

        let b = td.child("b");
        b.create_dir_all().unwrap();
        b.child("over.toml")
            .write_str("target = \"~/b-target\"\nuses = [\"d\"]")
            .unwrap();

        let c_ov = td.child("c");
        c_ov.create_dir_all().unwrap();
        c_ov.child("over.toml")
            .write_str("target = \"~/c-target\"\nuses = [\"d\"]")
            .unwrap();

        let a = td.child("a");
        a.create_dir_all().unwrap();
        a.child("over.toml")
            .write_str("target = \"~/a-target\"\nuses = [\"b\", \"c\"]")
            .unwrap();

        let overlay_a = repo.get("a").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let tree = DesiredTree::build(&c, &overlay_a).unwrap();

        let root = td.path().to_path_buf();
        let matches = tree
            .entries()
            .iter()
            .filter(|e| e.target == root.join("d-target/shared.txt"))
            .count();
        assert_eq!(matches, 1, "d's entry should appear exactly once");
    }

    /// True cycle: A uses B, B uses A. Should produce a cycle error.
    #[rstest]
    fn true_cycle_is_detected() {
        let (td, repo) = repo_and_root();

        let a = td.child("a_cycle");
        a.create_dir_all().unwrap();
        a.child("over.toml")
            .write_str("target = \"~\"\nuses = [\"b_cycle\"]")
            .unwrap();

        let b = td.child("b_cycle");
        b.create_dir_all().unwrap();
        b.child("over.toml")
            .write_str("target = \"~\"\nuses = [\"a_cycle\"]")
            .unwrap();

        let overlay_a = repo.get("a_cycle").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let result = DesiredTree::build(&c, &overlay_a);
        assert!(result.is_err(), "cycle should be detected");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Cycle detected"),
            "error should mention cycle: {err_msg}"
        );
        assert!(
            err_msg.contains("a_cycle"),
            "error should mention the cycling overlay: {err_msg}"
        );
    }

    #[rstest]
    fn descriptor_and_sidecar_config_files_are_not_entries() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        overlay_dir
            .child("app.link.toml")
            .write_str("target = \"/opt/app\"")
            .unwrap();
        overlay_dir.child("file.txt").write_str("content").unwrap();

        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let tree = DesiredTree::build(&c, &overlay).unwrap();

        assert!(
            !tree
                .entries()
                .iter()
                .any(|e| e.target == overlay.root.join("over.toml"))
        );
        assert!(
            !tree
                .entries()
                .iter()
                .any(|e| e.target == overlay.root.join("app.link.toml"))
        );
    }

    #[rstest]
    fn entries_are_sorted_by_target() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        overlay_dir.child("zeta.txt").write_str("z").unwrap();
        overlay_dir.child("alpha.txt").write_str("a").unwrap();
        overlay_dir.child("mid").create_dir_all().unwrap();
        overlay_dir.child("mid/file.txt").write_str("m").unwrap();

        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let tree = DesiredTree::build(&c, &overlay).unwrap();

        let targets: Vec<_> = tree.entries().iter().map(|e| e.target.clone()).collect();
        let mut sorted = targets.clone();
        sorted.sort();
        assert_eq!(targets, sorted);
    }

    #[test]
    fn empty_tree_reports_empty() {
        let tree = DesiredTree::default();
        assert!(tree.is_empty());
        assert_eq!(tree.len(), 0);
    }

    #[rstest]
    #[cfg(unix)]
    fn an_unreadable_source_directory_prevents_a_false_complete_report() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        overlay_dir.child("good.txt").write_str("ok").unwrap();
        let bad_dir = overlay_dir.child("bad_dir");
        bad_dir.create_dir_all().unwrap();
        bad_dir.child("hidden.txt").write_str("secret").unwrap();
        fs::set_permissions(bad_dir.path(), fs::Permissions::from_mode(0o000)).unwrap();
        // Privileged runners can read mode-000 directories, so cannot exercise this failure.
        let inaccessible = fs::read_dir(bad_dir.path()).is_err();

        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let result = DesiredTree::build(&c, &overlay);

        // Restore permissions for cleanup regardless of outcome.
        fs::set_permissions(bad_dir.path(), fs::Permissions::from_mode(0o755)).unwrap();

        if inaccessible {
            assert!(result.is_err());
        }
    }
}
