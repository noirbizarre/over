//! A resolved, secret-safe inspection snapshot shared by overlay views.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, ensure};

use crate::desired::{DesiredEntry, DesiredTree, MaterializationIntent, Provenance};
use crate::exec::{Context, Ctx};
use crate::hooks::Hooks;
use crate::overlays::{Overlay, Repository};

use super::{Report, changes, virtual_checkout};

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
    Repository(String),
    Mode(String),
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
    pub changes: Vec<changes::Change>,
}

/// Cache only equivalent resolved claims, not overlay names: dependency targets
/// and checkout scopes can change with the selected consumer's data context.
#[derive(Default)]
pub struct InspectionCache {
    virtual_checkouts: BTreeMap<super::identity::ClaimIdentity, virtual_checkout::Inspection>,
    snapshots: virtual_checkout::Snapshots,
    native: changes::NativeCache,
}

/// Keep failed dependency identities for lint attribution even when no runtime
/// snapshot can be built. This collects graph names, not a second health check.
pub(crate) fn dependency_names(repo: &Repository, overlay: &Overlay) -> BTreeSet<String> {
    let mut names = BTreeSet::from([overlay.name.clone()]);
    let mut pending = overlay.uses.clone().unwrap_or_default();
    while let Some(name) = pending.pop() {
        // Match lint's graph identity even when the dependency cannot be loaded.
        let name = crate::overlays::mapping::lexical(Path::new(&name))
            .to_string_lossy()
            .replace('\\', "/");
        if names.insert(name.clone())
            && let Ok(dependency) = repo.get(&name)
        {
            pending.extend(dependency.uses.into_iter().flatten());
        }
    }
    names
}

impl OverlayReport {
    pub fn inspect_overlay(
        repo: &Repository,
        root: &Path,
        overlay: &Overlay,
        no_uses: bool,
    ) -> Result<Self> {
        Self::inspect_cached(
            repo,
            root,
            overlay,
            no_uses,
            &mut InspectionCache::default(),
        )
    }

    pub fn inspect_cached(
        repo: &Repository,
        root: &Path,
        overlay: &Overlay,
        no_uses: bool,
        cache: &mut InspectionCache,
    ) -> Result<Self> {
        Self::inspect_mode(repo, root, overlay, no_uses, cache, InspectionMode::Full)
    }

    pub(crate) fn inspect_mode(
        repo: &Repository,
        root: &Path,
        overlay: &Overlay,
        no_uses: bool,
        cache: &mut InspectionCache,
        mode: InspectionMode,
    ) -> Result<Self> {
        let ctx = Context::builder()
            .root(root.to_path_buf())
            .repository(repo.clone())
            .overlay(overlay.clone())
            .no_uses(no_uses)
            .build();
        let ctx = ctx
            .resolve_data()
            // Retain the configuration entry points without quoting rendered metadata values.
            .map_err(|_| {
                anyhow!(
                    "cannot resolve overlay '{}': check default_overlay and dependency metadata",
                    overlay.name
                )
            })?;
        let target = overlay
            .resolve_target(&ctx)
            .map_err(|_| anyhow!("cannot resolve target for overlay '{}'", overlay.name))?;
        let ctx =
            ctx.with_resolved_overlay(overlay.name.clone(), target.to_string_lossy().to_string());
        Self::inspect_with_cache(ctx, overlay, cache, mode)
            .map_err(|_| anyhow!("cannot inspect overlay '{}'", overlay.name))
    }

    pub fn inspect(ctx: Ctx, overlay: &Overlay) -> Result<Self> {
        Self::inspect_with_cache(
            ctx,
            overlay,
            &mut InspectionCache::default(),
            InspectionMode::Full,
        )
    }

    fn inspect_with_cache(
        ctx: Ctx,
        overlay: &Overlay,
        cache: &mut InspectionCache,
        mode: InspectionMode,
    ) -> Result<Self> {
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
            .map(|item| {
                if mode == InspectionMode::Full {
                    configuration(&ctx, item)
                } else {
                    Ok(ResolvedConfiguration {
                        name: item.name.clone(),
                        source: item.root.clone(),
                        target: PathBuf::new(),
                        summary: Vec::new(),
                        details: Vec::new(),
                    })
                }
            })
            .collect::<Result<Vec<_>>>()?;
        let mut configuration = configurations
            .pop()
            .ok_or_else(|| anyhow!("overlay composition is empty"))?;
        if mode == InspectionMode::Full {
            configuration.summary.push(Field::list(
                "composition",
                ctx.no_uses
                    .then(|| "dependency operations skipped; data retained".into()),
                ordered.iter().map(|item| item.name.clone()).collect(),
            ));
        }
        let mut changes = Vec::new();
        let health = DesiredTree::build_reporting(&ctx, overlay).and_then(|desired| {
            if mode != InspectionMode::Health {
                changes.extend(changes::native_changes(&desired, &mut cache.native)?);
            }
            Report::build_reporting(&desired, mode != InspectionMode::Short, |entry| {
                let key = super::identity::ClaimIdentity::new(entry);
                let inspection = if let Some(inspection) = cache.virtual_checkouts.get(&key) {
                    inspection.clone()
                } else {
                    let inspection =
                        virtual_checkout::inspect_tracked_cached(entry, &mut cache.snapshots)?;
                    cache.virtual_checkouts.insert(key, inspection.clone());
                    inspection
                };
                let status = inspection.status.clone();
                if mode != InspectionMode::Health {
                    changes.extend(changes::virtual_changes(entry, &inspection)?);
                }
                Ok(status)
            })
        });
        let mut entry_details = Vec::new();
        let health = health
            .inspect(|report| {
                for entry in report.entries() {
                    let details = if mode == InspectionMode::Full { explain(&entry.entry, &ordered) } else { Vec::new() };
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
            changes,
        })
    }
}

/// Health and short consumers avoid preparing presentation data they never use.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum InspectionMode {
    Full,
    Health,
    Short,
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

fn origin_value(overlay: &Overlay, key: &str) -> Value {
    // Built-in defaults are provenance labels, not filesystem paths.
    overlay.field_origins.get(key).map_or_else(
        || Value::Text("built-in default".into()),
        |path| Value::Path(path.display().to_string()),
    )
}

fn hook_summary(hooks: &Hooks) -> Option<Field> {
    let mut field = Field::new("hooks", Value::Text(String::new()));
    field.children = [
        ("post-clone", hooks.post_clone.len()),
        ("post-init", hooks.post_init.len()),
        ("post-apply", hooks.post_apply.len()),
        ("install", hooks.install.len()),
    ]
    .into_iter()
    .filter(|(_, count)| *count > 0)
    .map(|(key, count)| Field::new(key, Value::Text(count.to_string())))
    .collect();
    // An empty parent would otherwise render as `hooks: none`.
    (!field.children.is_empty()).then_some(field)
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
        Field::new("target origin", origin_value(overlay, "target")),
        Field::list(
            "uses",
            Some(format!("scope: {}", origin(overlay, "uses"))),
            overlay.uses.clone().unwrap_or_default(),
        ),
    ];
    if let Some(description) = &overlay.description {
        summary.push(Field::new("description", Value::Text(description.clone())));
    }
    let effective_root = mappings.target_for(Path::new(""));
    if effective_root != mappings.base {
        // Keep the fallback visible: templates still resolve paths.target against it.
        summary.push(Field::new(
            "effective root",
            Value::Path(effective_root.display().to_string()),
        ));
    }
    if mappings.boundaries.is_empty() {
        summary.push(Field::new(
            "target rules",
            Value::Text("no independent boundaries".into()),
        ));
    }
    for (source, target) in &mappings.boundaries {
        let mut field = Field::new("target rule", Value::Path(source.display().to_string()));
        field.values.extend([
            Value::Text(" → ".into()),
            Value::Path(target.display().to_string()),
        ]);
        field.children.push(Field::new(
            "origin",
            Value::Path(
                mappings
                    .rule_for(source)
                    .map(|origin| origin.descriptor.as_path())
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
                        "target",
                        Value::Path(rule.target.clone().unwrap_or_else(|| "unchanged".into())),
                    ),
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
        let mut field = Field::new("git", Value::Repository(key.clone()));
        field.values.extend([
            Value::Text(" → ".into()),
            Value::Path(mappings.target_for(logical).display().to_string()),
        ]);
        field.children = vec![
            Field::new("configuration", Value::Text("present".into())),
            Field::new("latest contributing scope", origin_value(overlay, "git")),
        ];
        summary.push(field);
        if let Some(config) = overlay.git.as_ref().and_then(|git| git.get(key)) {
            let mut settings = Field::new("git settings for", Value::Repository(key.clone()));
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
    if let Some(field) = hook_summary(&overlay.hooks) {
        summary.push(field.annotated(format!("overlay local: {}", overlay.root.display())));
    }
    if let Some(field) = hook_summary(&ctx.repository.hooks()?) {
        summary.push(field.annotated(format!("repository: {}", ctx.repository.root.display())));
    }
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
            Some(Value::Text("sidecar declaration".into())),
        ),
        Provenance::PartialSidecar { overlay, config } => (
            overlay,
            config.display().to_string(),
            Some(Value::Text("sidecar declaration".into())),
        ),
        Provenance::Git {
            overlay, repo_key, ..
        } => (
            overlay,
            repo_key.clone(),
            Some(Value::Text("Git declaration".into())),
        ),
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
            return Value::Text("built-in scaffold".into());
        }
        let Some(overlay) = overlays.iter().find(|overlay| &overlay.name == owner) else {
            return Value::Text("built-in default".into());
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
                    .map(|(index, _)| {
                        Value::Path(format!("{} rule {}", scope.descriptor.display(), index + 1))
                    })
                    .or_else(|| {
                        scope
                            .mode
                            .map(|_| Value::Path(scope.descriptor.display().to_string()))
                    })
            })
            .unwrap_or_else(|| Value::Text("built-in default".into()))
    });
    let source = if matches!(entry.provenance, Provenance::Git { .. }) {
        Value::Repository(source)
    } else {
        Value::Path(source)
    };
    let mut fields = vec![
        Field::new("overlay", Value::Overlay(owner.clone())),
        Field::new("source", source),
        Field::new("scope", scope),
        Field::new("mode", Value::Mode(intent.into())),
    ];
    if let Some(mode) = entry.permissions {
        fields.push(Field::new("permissions", Value::Mode(mode.to_string())));
    }
    fields
}
