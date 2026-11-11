//! A resolved, secret-safe inspection snapshot shared by overlay views.

use std::collections::{BTreeMap, BTreeSet};
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
    pub summary: Vec<Field>,
    pub details: Vec<Field>,
}

/// Presentation data stays unstyled so collecting a snapshot never depends on a terminal.
#[derive(Debug, Clone)]
pub enum Value {
    Text(String),
    Path(String),
    Overlay(String),
    Mode(String),
    Change {
        symbol: &'static str,
        path: PathBuf,
        staging: &'static str,
    },
}

#[derive(Debug, Clone)]
pub struct Field {
    pub key: String,
    pub annotation: Option<String>,
    pub values: Vec<Value>,
    pub children: Vec<Field>,
    pub sequence: Vec<String>,
}

impl Field {
    pub fn new(key: impl Into<String>, value: Value) -> Self {
        Self {
            key: key.into(),
            annotation: None,
            values: vec![value],
            children: Vec::new(),
            sequence: Vec::new(),
        }
    }

    fn annotated(mut self, annotation: impl Into<String>) -> Self {
        self.annotation = Some(annotation.into());
        self
    }

    fn list(key: &str, annotation: Option<String>, sequence: Vec<String>) -> Self {
        Self {
            key: key.into(),
            annotation,
            values: Vec::new(),
            children: Vec::new(),
            sequence,
        }
    }
}

/// Configuration survives a reconciliation failure, so an unapplied or damaged
/// machine still gets an explanation of what the overlay intends to manage.
pub struct OverlayReport {
    pub configuration: ResolvedConfiguration,
    pub dependencies: Vec<ResolvedConfiguration>,
    pub health: Result<Report>,
    pub entry_details: Vec<Vec<Field>>,
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
        configuration.summary.push(Field::list(
            "composition",
            ctx.no_uses
                .then(|| "dependency operations skipped; data retained".into()),
            ordered.iter().map(|item| item.name.clone()).collect(),
        ));
        // Reuse inspection results for details rather than cleaning every dirty file twice.
        let mut inspections = BTreeMap::new();
        let health = DesiredTree::build(&ctx, overlay).and_then(|desired| {
            Report::build_with_inspector(&desired, |entry| {
                let inspection = virtual_checkout::inspect_tracked(entry)?;
                let status = inspection.status.clone();
                inspections.insert(entry.target.clone(), inspection);
                Ok(status)
            })
        });
        let mut entry_details = Vec::new();
        let health = health
            .inspect(|report| {
                for entry in report.entries() {
                    let mut details = explain(&entry.entry, &ordered);
                    if entry.entry.intent.is_virtual_checkout()
                        && matches!(entry.status, Status::Modified | Status::Diverged { .. } | Status::Conflict)
                    {
                        for (label, changes) in [
                            ("staged", &inspections[&entry.entry.target].staged),
                            ("unstaged", &inspections[&entry.entry.target].unstaged),
                        ] {
                            details.extend(changes.iter().map(|change| {
                                Field::new("", Value::Change { symbol: file_change_symbol(change.kind), path: change.path.clone(), staging: label })
                            }));
                        }
                    }
                    entry_details.push(details);
                }
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

fn hook_summary(hooks: &Hooks) -> Field {
    let mut field = Field::new("hooks", Value::Text(String::new()));
    field.children = [
        ("post-clone", hooks.post_clone.len()),
        ("post-init", hooks.post_init.len()),
        ("post-apply", hooks.post_apply.len()),
        ("install", hooks.install.len()),
    ]
    .into_iter()
    .map(|(key, count)| Field::new(key, Value::Text(count.to_string())))
    .collect();
    field
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
        Field::new("target origin", Value::Path(origin(overlay, "target"))),
        Field::list(
            "uses",
            Some(format!("scope: {}", origin(overlay, "uses"))),
            overlay.uses.clone().unwrap_or_default(),
        ),
    ];
    if let Some(description) = &overlay.description {
        summary.push(Field::new("description", Value::Text(description.clone())));
    }
    if mappings.directories.is_empty() {
        summary.push(Field::new("mappings", Value::Text("none".into())));
    }
    for (source, target) in &mappings.directories {
        let key = source.to_string_lossy().replace('\\', "/");
        let mut field = Field::new("map", Value::Path(source.display().to_string()));
        field.values.extend([
            Value::Text(" → ".into()),
            Value::Path(target.display().to_string()),
        ]);
        field.children.push(Field::new(
            "origin",
            Value::Path(
                overlay
                    .mapping_origins
                    .get(&key)
                    .unwrap_or(&overlay.root)
                    .display()
                    .to_string(),
            ),
        ));
        summary.push(field);
    }
    let baseline = overlay
        .scopes
        .iter()
        .rev()
        .find_map(|scope| scope.mode)
        .unwrap_or_default();
    let rule_count: usize = overlay.scopes.iter().map(|scope| scope.rules.len()).sum();
    let mut mode = Field::new(
        "mode",
        Value::Mode(overlay.materialization_for(Path::new("")).to_string()),
    );
    mode.children = vec![
        Field::new("baseline", Value::Mode(baseline.to_string())),
        Field::new("path rules", Value::Text(rule_count.to_string())),
    ];
    summary.push(mode);
    let mut details = Vec::new();
    for scope in &overlay.scopes {
        if scope.mode.is_some()
            || scope.permissions.is_some()
            || !scope.rules.is_empty()
            || scope.exclude.is_some()
        {
            details.push(
                Field::new("scope", Value::Path(scope.descriptor.display().to_string()))
                    .annotated(format!("base: {}", scope.root.display())),
            );
            if let Some(mode) = scope.mode {
                let mut field = Field::new(
                    "mode scope",
                    Value::Path(scope.descriptor.display().to_string()),
                );
                field
                    .values
                    .extend([Value::Text(" = ".into()), Value::Mode(mode.to_string())]);
                summary.push(field);
            }
            if let Some(permissions) = scope.permissions {
                details.push(Field::new(
                    "permissions",
                    Value::Mode(permissions.to_string()),
                ));
            }
            for (index, rule) in scope.rules.iter().enumerate() {
                let mut field = Field::new(
                    format!("rule {}", index + 1),
                    Value::Path(rule.path.clone()),
                );
                field.children = vec![
                    Field::new(
                        "mode",
                        Value::Mode(
                            rule.mode
                                .map(|mode| mode.to_string())
                                .unwrap_or_else(|| "unchanged".into()),
                        ),
                    ),
                    Field::new(
                        "permissions",
                        Value::Mode(
                            rule.permissions
                                .map(|mode| mode.to_string())
                                .unwrap_or_else(|| "unchanged".into()),
                        ),
                    ),
                ];
                details.push(field);
            }
            if let Some(exclude) = &scope.exclude {
                details.push(Field::new("exclude", Value::Path(exclude.join(", "))));
            }
        }
    }
    if overlay.git.as_ref().is_none_or(|git| git.is_empty()) {
        summary.push(Field::new("git", Value::Text("none".into())));
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
        let mut field = Field::new("git", Value::Text(key.clone()));
        field.values.extend([
            Value::Text(" → ".into()),
            Value::Path(mappings.target_for(logical).display().to_string()),
        ]);
        field.children = vec![
            Field::new("configuration", Value::Text("present".into())),
            Field::new(
                "latest contributing scope",
                Value::Path(origin(overlay, "git")),
            ),
        ];
        summary.push(field);
        if let Some(config) = overlay.git.as_ref().and_then(|git| git.get(key)) {
            let mut settings = Field::new("git settings for", Value::Text(key.clone()));
            settings.children = vec![
                Field::new("worktree", Value::Text(config.worktree.to_string())),
                Field::new(
                    "submodules",
                    Value::Text(config.recurse_submodules.to_string()),
                ),
                Field::new(
                    "revision configured",
                    Value::Text(
                        (config.branch.is_some() || config.tag.is_some() || config.rev.is_some())
                            .to_string(),
                    ),
                ),
                Field::new(
                    "extra remotes",
                    Value::Text(
                        config
                            .remotes
                            .as_ref()
                            .map_or(0, |remotes| remotes.len())
                            .to_string(),
                    ),
                ),
            ];
            details.push(settings);
            let mut worktrees = config
                .worktrees
                .iter()
                .flat_map(|worktrees| worktrees.keys())
                .collect::<Vec<_>>();
            worktrees.sort();
            for name in worktrees {
                summary.push(Field::new(
                    "git worktree",
                    Value::Path(
                        mappings
                            .target_for(logical)
                            .join(name)
                            .display()
                            .to_string(),
                    ),
                ));
            }
        }
    }
    summary.push(
        hook_summary(&overlay.hooks)
            .annotated(format!("overlay local: {}", overlay.root.display())),
    );
    summary.push(
        hook_summary(&ctx.repository.hooks()?)
            .annotated(format!("repository: {}", ctx.repository.root.display())),
    );
    Ok(ResolvedConfiguration {
        name: overlay.name.clone(),
        source: overlay.root.clone(),
        target: mappings.base,
        summary,
        details,
    })
}

fn explain(entry: &DesiredEntry, overlays: &[Overlay]) -> Vec<Field> {
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
    let mut fields = vec![
        Field::new("overlay", Value::Overlay(owner.clone())),
        Field::new("source", Value::Path(source)),
        Field::new("scope", Value::Path(scope)),
        Field::new("mode", Value::Mode(intent.into())),
    ];
    if let Some(mode) = entry.permissions {
        fields.push(Field::new("permissions", Value::Mode(mode.to_string())));
    }
    fields
}
