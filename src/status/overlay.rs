//! A resolved, secret-safe inspection snapshot shared by overlay views.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, ensure};

use crate::desired::{DesiredEntry, DesiredTree, MaterializationIntent, Provenance};
use crate::exec::Ctx;
use crate::hooks::Hooks;
use crate::overlays::{Overlay, Repository};

use super::{Report, Status, file_change_symbol, virtual_checkout};

/// Values are allowlisted summaries, never serialized configuration or scripts.
#[derive(Debug, Clone)]
pub struct ResolvedConfiguration {
    pub name: String,
    pub source: PathBuf,
    pub target: PathBuf,
    pub summary: Vec<String>,
    pub details: Vec<String>,
}

/// Configuration survives a reconciliation failure, so an unapplied or damaged
/// machine still gets an explanation of what the overlay intends to manage.
pub struct OverlayReport {
    pub configuration: ResolvedConfiguration,
    pub dependencies: Vec<ResolvedConfiguration>,
    pub health: Result<Report>,
    pub entry_details: Vec<Vec<String>>,
}

impl OverlayReport {
    pub fn inspect(ctx: Ctx, overlay: &Overlay) -> Result<Self> {
        let mut ordered = Vec::new();
        compose(
            &ctx.repository,
            overlay,
            &mut BTreeSet::new(),
            &mut BTreeSet::new(),
            &mut ordered,
        )?;
        let mut configurations = ordered
            .iter()
            .map(|item| configuration(&ctx, item))
            .collect::<Result<Vec<_>>>()?;
        let mut configuration = configurations
            .pop()
            .ok_or_else(|| anyhow!("overlay composition is empty"))?;
        configuration.summary.push(format!(
            "composition: {}{}",
            ordered
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>()
                .join(" -> "),
            if ctx.no_uses {
                " (dependency operations skipped; data retained)"
            } else {
                ""
            }
        ));
        let health = DesiredTree::build(&ctx, overlay).and_then(|desired| Report::build(&desired));
        let mut entry_details = Vec::new();
        let health = health
            .and_then(|report| {
                for entry in report.entries() {
                    let mut details = vec![explain(&entry.entry, &ordered)];
                    if entry.entry.intent.is_virtual_checkout()
                        && matches!(entry.status, Status::Modified | Status::Diverged { .. } | Status::Conflict)
                    {
                        for (label, changes) in [
                            ("staged", virtual_checkout::staged_changes(&entry.entry)?),
                            ("unstaged", virtual_checkout::unstaged_changes(&entry.entry)?),
                        ] {
                            details.extend(changes.into_iter().map(|change| {
                                format!("{} {} ({label})", file_change_symbol(change.kind), change.path.display())
                            }));
                        }
                    }
                    entry_details.push(details);
                }
                Ok(report)
            })
            // Git/parser errors may quote credentials, scripts, or rendered data.
            .map_err(|_| anyhow!("cannot check overlay '{}': inspect source files, Git configuration/index and checkout association", overlay.name));
        Ok(Self {
            configuration,
            dependencies: configurations,
            health,
            entry_details,
        })
    }
}

fn compose(
    repo: &Repository,
    overlay: &Overlay,
    visited: &mut BTreeSet<String>,
    stack: &mut BTreeSet<String>,
    ordered: &mut Vec<Overlay>,
) -> Result<()> {
    ensure!(
        !stack.contains(&overlay.name),
        "dependency cycle at overlay '{}'",
        overlay.name
    );
    if visited.contains(&overlay.name) {
        return Ok(());
    }
    stack.insert(overlay.name.clone());
    for name in overlay.uses.iter().flatten() {
        compose(repo, &repo.get(name)?, visited, stack, ordered)?;
    }
    stack.remove(&overlay.name);
    visited.insert(overlay.name.clone());
    ordered.push(overlay.clone());
    Ok(())
}

fn origin(overlay: &Overlay, key: &str) -> String {
    overlay
        .field_origins
        .get(key)
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "built-in default".into())
}

fn hook_summary(hooks: &Hooks) -> String {
    format!(
        "post-clone: {}, post-init: {}, post-apply: {}, install: {}",
        hooks.post_clone.len(),
        hooks.post_init.len(),
        hooks.post_apply.len(),
        hooks.install.len()
    )
}

fn configuration(ctx: &Ctx, overlay: &Overlay) -> Result<ResolvedConfiguration> {
    // with_overlay retains the selected root's resolved data snapshot.
    let own_ctx = ctx.with_overlay(overlay.clone());
    let mappings = overlay.resolve_mappings(&own_ctx).map_err(|_| {
        anyhow!(
            "cannot resolve target/mappings for overlay '{}'",
            overlay.name
        )
    })?;
    let mut summary = vec![
        format!("target origin: {}", origin(overlay, "target")),
        format!(
            "uses: {} (scope: {})",
            overlay
                .uses
                .as_ref()
                .filter(|uses| !uses.is_empty())
                .map(|uses| uses.join(", "))
                .unwrap_or_else(|| "none".into()),
            origin(overlay, "uses")
        ),
    ];
    if let Some(description) = &overlay.description {
        summary.push(format!("description: {description}"));
    }
    if mappings.directories.is_empty() {
        summary.push("mappings: none".into());
    }
    for (source, target) in &mappings.directories {
        let key = source.to_string_lossy().replace('\\', "/");
        summary.push(format!(
            "map: {} -> {} (origin: {})",
            source.display(),
            target.display(),
            overlay
                .mapping_origins
                .get(&key)
                .unwrap_or(&overlay.root)
                .display()
        ));
    }
    let baseline = overlay
        .scopes
        .iter()
        .rev()
        .find_map(|scope| scope.mode)
        .unwrap_or_default();
    let rule_count: usize = overlay.scopes.iter().map(|scope| scope.rules.len()).sum();
    summary.push(format!(
        "mode: {}; baseline: {baseline}; {rule_count} path rule(s)",
        overlay.materialization_for(Path::new(""))
    ));
    let mut details = Vec::new();
    for scope in &overlay.scopes {
        if scope.mode.is_some()
            || scope.permissions.is_some()
            || !scope.rules.is_empty()
            || scope.exclude.is_some()
        {
            details.push(format!(
                "scope: {} (base: {})",
                scope.descriptor.display(),
                scope.root.display()
            ));
            if let Some(mode) = scope.mode {
                summary.push(format!(
                    "mode scope: {} = {mode}",
                    scope.descriptor.display()
                ));
            }
            if let Some(permissions) = scope.permissions {
                details.push(format!("permissions: {permissions}"));
            }
            for (index, rule) in scope.rules.iter().enumerate() {
                details.push(format!(
                    "rule {}: {} mode={} permissions={}",
                    index + 1,
                    rule.path,
                    rule.mode
                        .map(|mode| mode.to_string())
                        .unwrap_or_else(|| "unchanged".into()),
                    rule.permissions
                        .map(|mode| mode.to_string())
                        .unwrap_or_else(|| "unchanged".into())
                ));
            }
            if let Some(exclude) = &scope.exclude {
                details.push(format!("exclude: {}", exclude.join(", ")));
            }
        }
    }
    if overlay.git.as_ref().is_none_or(|git| git.is_empty()) {
        summary.push("git: none".into());
    }
    let mut git_keys = overlay
        .git
        .iter()
        .flat_map(|git| git.keys())
        .collect::<Vec<_>>();
    git_keys.sort();
    for key in git_keys {
        let logical = if key == "." {
            Path::new("")
        } else {
            Path::new(key)
        };
        summary.push(format!(
            "git: {key} -> {} (configuration present; latest contributing scope: {})",
            mappings.target_for(logical).display(),
            origin(overlay, "git")
        ));
        if let Some(config) = overlay.git.as_ref().and_then(|git| git.get(key)) {
            details.push(format!("git settings for {key}: worktree={}, submodules={}, revision configured={}, extra remotes={}", config.worktree, config.recurse_submodules, config.branch.is_some() || config.tag.is_some() || config.rev.is_some(), config.remotes.as_ref().map_or(0, |remotes| remotes.len())));
            let mut worktrees = config
                .worktrees
                .iter()
                .flat_map(|worktrees| worktrees.keys())
                .collect::<Vec<_>>();
            worktrees.sort();
            for name in worktrees {
                summary.push(format!(
                    "git worktree: {}",
                    mappings.target_for(logical).join(name).display()
                ));
            }
        }
    }
    summary.push(format!(
        "hooks (overlay local: {}): {}",
        overlay.root.display(),
        hook_summary(&overlay.hooks)
    ));
    summary.push(format!(
        "hooks (repository: {}): {}",
        ctx.repository.root.display(),
        hook_summary(&ctx.repository.hooks()?)
    ));
    Ok(ResolvedConfiguration {
        name: overlay.name.clone(),
        source: overlay.root.clone(),
        target: mappings.base,
        summary,
        details,
    })
}

fn explain(entry: &DesiredEntry, overlays: &[Overlay]) -> String {
    let (owner, source, scope) = match &entry.provenance {
        Provenance::Overlay { overlay, source } => (overlay, source.display().to_string(), None),
        Provenance::SymlinkSidecar {
            overlay,
            config,
            resolved,
            ..
        } => (
            overlay,
            format!("{} (referent: {})", config.display(), resolved.display()),
            Some("sidecar declaration".into()),
        ),
        Provenance::PartialSidecar { overlay, config } => (
            overlay,
            config.display().to_string(),
            Some("sidecar declaration".into()),
        ),
        Provenance::Git {
            overlay, repo_key, ..
        } => (overlay, repo_key.clone(), Some("Git declaration".into())),
    };
    let intent = match entry.intent {
        MaterializationIntent::EncryptedFile(_) => "encrypted file",
        MaterializationIntent::Directory => "directory scaffold",
        MaterializationIntent::SymlinkFile { .. } => "symlink",
        MaterializationIntent::SymlinkDirectory { .. } => "symlink-directory",
        MaterializationIntent::Checkout => "declared Git checkout",
        MaterializationIntent::VirtualCheckout
        | MaterializationIntent::ScopedVirtualCheckout(_) => "virtual checkout",
        MaterializationIntent::PartialFile { .. } => "partial file",
    };
    let scope = scope.unwrap_or_else(|| {
        if matches!(entry.intent, MaterializationIntent::Directory) {
            return "built-in scaffold".into();
        }
        let Some(overlay) = overlays.iter().find(|overlay| &overlay.name == owner) else {
            return "built-in default".into();
        };
        // Match against the source namespace: mappings must not change rule provenance.
        let path = Path::new(&source);
        let is_directory = matches!(
            entry.intent,
            MaterializationIntent::SymlinkDirectory { .. }
                | MaterializationIntent::VirtualCheckout
                | MaterializationIntent::ScopedVirtualCheckout(_)
        );
        overlay
            .scopes
            .iter()
            .rev()
            .find_map(|scope| {
                scope
                    .rules
                    .iter()
                    .enumerate()
                    .rev()
                    .find(|(_, rule)| {
                        rule.mode.is_some() && scope.matches(rule, path, is_directory)
                    })
                    .map(|(index, _)| format!("{} rule {}", scope.descriptor.display(), index + 1))
                    .or_else(|| scope.mode.map(|_| scope.descriptor.display().to_string()))
            })
            .unwrap_or_else(|| "built-in default".into())
    });
    let permissions = entry
        .permissions
        .map(|mode| format!("; permissions: {mode}"))
        .unwrap_or_default();
    format!("source: {source}; overlay: {owner}; mode: {intent}; scope: {scope}{permissions}")
}
