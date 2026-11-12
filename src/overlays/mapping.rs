//! Bidirectional rule routing, resolved before materialization strategy.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context as _, Result, bail, ensure};

use crate::exec;

use super::{MaterializationKind, Overlay};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleOrigin {
    pub descriptor: PathBuf,
    pub index: usize,
}

#[derive(Debug, Clone)]
struct TargetAssignment {
    source: PathBuf,
    target: PathBuf,
    origin: RuleOrigin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReverseMatch {
    pub source: PathBuf,
    pub rule: Option<RuleOrigin>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReverseResolution {
    Absent,
    Unique(ReverseMatch),
    Ambiguous(Vec<ReverseMatch>),
}

#[derive(Debug, Clone)]
pub struct ResolvedMappings {
    pub base: PathBuf,
    /// Only layout discontinuities become independently walked roots.
    pub boundaries: BTreeMap<PathBuf, PathBuf>,
    assignments: Vec<TargetAssignment>,
}

/// Normalize lexically: resolving symlinks would turn managed target links
/// into source paths and make reverse routing impossible.
pub(crate) fn absolute(path: &Path) -> Result<PathBuf> {
    // std canonicalization uses verbatim Windows paths, while libgit2 reports
    // ordinary drive/UNC prefixes. They must share one component namespace.
    #[cfg(windows)]
    let ordinary = {
        let raw = path.to_string_lossy();
        if let Some(tail) = raw.strip_prefix(r"\\?\UNC\") {
            PathBuf::from(format!(r"\\{tail}"))
        } else if let Some(tail) = raw.strip_prefix(r"\\?\") {
            PathBuf::from(tail)
        } else {
            path.to_path_buf()
        }
    };
    #[cfg(windows)]
    let path = ordinary.as_path();
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    Ok(lexical(&path))
}

/// Shared lexical normalization avoids following managed symlinks while resolving selectors.
pub(crate) fn lexical(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(part),
        }
    }
    normalized
}

/// Resolve existing ancestors even when the destination has not been created.
/// Validation uses physical identity; routing must retain the lexical spelling.
pub(crate) fn physical(path: &Path) -> Result<PathBuf> {
    let mut ancestor = absolute(path)?;
    let mut suffix = Vec::new();
    loop {
        match ancestor.symlink_metadata() {
            Ok(_) => {
                let mut resolved = ancestor.canonicalize().with_context(|| {
                    format!("failed to resolve physical path '{}'", ancestor.display())
                })?;
                for part in suffix.iter().rev() {
                    resolved.push(part);
                }
                return absolute(&resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                suffix.push(
                    ancestor
                        .file_name()
                        .context("path has no existing ancestor")?
                        .to_os_string(),
                );
                ancestor.pop();
            }
            Err(error) => return Err(error).context("failed to inspect physical path"),
        }
    }
}

/// Resolve aliases in existing parents, but never dereference the selected
/// leaf: an added symlink must still name its deployment location, not source.
pub(crate) fn location(path: &Path) -> Result<PathBuf> {
    let path = absolute(path)?;
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => Ok(physical(parent)?.join(name)),
        _ => physical(&path),
    }
}

/// An already-deployed managed symlink owns its link location, not its source.
pub(crate) fn physical_destination(target: &Path, source: &Path) -> Result<PathBuf> {
    if target.is_symlink() {
        let resolved = physical(target)?;
        if resolved == physical(source)? {
            let target = absolute(target)?;
            return Ok(physical(target.parent().context("target has no parent")?)?
                .join(target.file_name().context("target has no filename")?));
        }
        return Ok(resolved);
    }
    physical(target)
}

pub(crate) fn render_target(raw: &str, ctx: &exec::Context) -> Result<PathBuf> {
    let state = exec::templates::TemplateContext::new(ctx)?;
    let rendered = exec::templates::render_path(raw, &state)
        .with_context(|| format!("failed to render target template '{raw}'"))?;
    if rendered == "~" || rendered.starts_with("~/") {
        // An unspecified execution root is not permission to deploy into invocation cwd.
        ensure!(
            !ctx.root.as_os_str().is_empty(),
            "cannot expand '~' without an execution root"
        );
    }
    Ok(if rendered == "~" {
        ctx.absolute_path(&ctx.root)?
    } else if let Some(tail) = rendered.strip_prefix("~/") {
        ctx.absolute_path(&ctx.root)?.join(tail)
    } else {
        PathBuf::from(rendered)
    })
}

/// Render before anchoring so a template producing a relative path keeps its declaration base.
pub(crate) fn resolve_path(raw: &str, origin: &Path, ctx: &exec::Context) -> Result<PathBuf> {
    Ok(lexical(
        &ctx.absolute_path(origin)?.join(render_target(raw, ctx)?),
    ))
}

/// Context-free checks also used by lint, which must not render machine-specific
/// templates merely to diagnose an invalid source namespace.
pub(crate) fn validate_sources(overlay: &Overlay) -> Result<()> {
    for scope in &overlay.scopes {
        for rule in &scope.rules {
            rule.validate()?;
        }
    }
    Ok(())
}

impl ResolvedMappings {
    pub fn new(overlay: &Overlay, ctx: &exec::Context) -> Result<Self> {
        // Both planning and clone provisioning must reject escapes before resolving destinations.
        if let Some(repositories) = &overlay.git {
            for key in repositories.keys() {
                crate::actions::git::config::validate_logical_key(key)?;
            }
        }
        validate_sources(overlay)?;
        let prepared = ctx.prepare_overlay(overlay)?;
        let ctx = prepared.as_ref();
        let base = ctx
            .current_target
            .clone()
            .context("prepared overlay has no target")?;
        let root = absolute(&overlay.root)?;
        let mut assignments = Vec::new();
        for scope in &overlay.scopes {
            for (index, rule) in scope.rules.iter().enumerate() {
                let Some(raw) = &rule.target else { continue };
                let prefix = lexical(&scope.root.join(&rule.path));
                if !root.starts_with(&prefix) && !prefix.starts_with(&root) {
                    // Policy for another overlay must not require its machine-specific template data.
                    continue;
                }
                let target = absolute(&resolve_path(raw, &scope.root, ctx)?)?;
                // An inherited selector above this overlay retains the suffix from its own root.
                let (source, target) = if let Ok(tail) = root.strip_prefix(&prefix) {
                    (PathBuf::new(), join_suffix(&target, tail))
                } else if let Ok(source) = prefix.strip_prefix(&root) {
                    (source.to_path_buf(), target)
                } else {
                    continue;
                };
                assignments.push(TargetAssignment {
                    source,
                    target,
                    origin: RuleOrigin {
                        descriptor: scope.descriptor.clone(),
                        index,
                    },
                });
            }
        }
        let mut routing = Self {
            base,
            boundaries: BTreeMap::new(),
            assignments,
        };
        let mut prefixes: Vec<_> = routing
            .assignments
            .iter()
            .map(|item| item.source.clone())
            .collect();
        prefixes.sort_by_key(|path| path.components().count());
        prefixes.dedup();
        for source in prefixes {
            if source.as_os_str().is_empty() {
                continue;
            }
            let target = routing.target_for(&source);
            let parent = routing
                .boundaries
                .keys()
                .filter(|prefix| source.starts_with(prefix))
                .max_by_key(|prefix| prefix.components().count());
            let inherited = match parent {
                Some(parent) => {
                    join_suffix(&routing.boundaries[parent], source.strip_prefix(parent)?)
                }
                None => join_suffix(&routing.target_for(Path::new("")), &source),
            };
            if target != inherited {
                routing.boundaries.insert(source, target);
            }
        }
        // Validate final regions, not dead declarations superseded by later rules.
        for (source, target) in &routing.boundaries {
            let key = source.display();
            let physical_target = physical_destination(target, &root.join(source))?;
            let physical_root = physical(&root)?;
            ensure!(
                !overlay
                    .git
                    .as_ref()
                    .is_some_and(|repos| repos.contains_key(".")),
                "target rule '{key}' is inside indivisible Git checkout '.'"
            );
            ensure!(
                !target.starts_with(&root)
                    && !root.starts_with(target)
                    && !physical_target.starts_with(&physical_root)
                    && !physical_root.starts_with(&physical_target),
                "mapping '{key}' target overlaps the overlay source"
            );
            let mut encrypted_source = root.join(source).into_os_string();
            encrypted_source.push(".age");
            if root.join(source).is_file() || Path::new(&encrypted_source).is_file() {
                ensure!(
                    overlay.materialization_for(source) != MaterializationKind::Checkout,
                    "target rule for file '{key}' inherits mode: checkout; redirect its containing directory or use mode: symlink"
                );
            }
            for ancestor in source
                .ancestors()
                .skip(1)
                .filter(|path| !path.as_os_str().is_empty())
            {
                ensure!(
                    overlay.materialization_for(ancestor) != MaterializationKind::SymlinkDirectory,
                    "target rule '{key}' is inside whole-directory symlink '{}'",
                    ancestor.display()
                );
                if overlay.git.as_ref().is_some_and(|repos| {
                    repos.contains_key(&ancestor.to_string_lossy().replace('\\', "/"))
                }) {
                    bail!(
                        "target rule '{key}' is inside indivisible Git checkout '{}'",
                        ancestor.display()
                    );
                }
            }
        }
        if routing.assignment_for(Path::new("")).is_some()
            && routing.target_for(Path::new("")) != routing.base
        {
            let root_target = routing.target_for(Path::new(""));
            let physical_target = physical_destination(&root_target, &root)?;
            let physical_root = physical(&root)?;
            ensure!(
                !root_target.starts_with(&root)
                    && !root.starts_with(&root_target)
                    && !physical_target.starts_with(&physical_root)
                    && !physical_root.starts_with(&physical_target),
                "overlay target overlaps the overlay source"
            );
        }
        Ok(routing)
    }

    fn assignment_for(&self, logical: &Path) -> Option<&TargetAssignment> {
        self.assignments
            .iter()
            .rev()
            .find(|assignment| logical.starts_with(&assignment.source))
    }

    pub fn rule_for(&self, logical: &Path) -> Option<&RuleOrigin> {
        self.assignment_for(logical)
            .map(|assignment| &assignment.origin)
    }

    pub fn target_for(&self, logical: &Path) -> PathBuf {
        if let Some(assignment) = self.assignment_for(logical) {
            return join_suffix(
                &assignment.target,
                logical
                    .strip_prefix(&assignment.source)
                    .expect("matched prefix"),
            );
        }
        join_suffix(&self.base, logical)
    }

    pub fn reverse(&self, target: &Path) -> Result<ReverseResolution> {
        let target = absolute(target)?;
        if self.assignments.is_empty()
            && let Ok(source) = target.strip_prefix(&self.base)
        {
            // A single lexical namespace cannot have competing inverse explanations.
            return Ok(ReverseResolution::Unique(ReverseMatch {
                source: source.to_path_buf(),
                rule: None,
            }));
        }
        let mut matches = BTreeMap::new();
        // Lexical routing remains usable beneath a broken link or obstructed scaffold.
        // Such paths still need status/repair; an unavailable alias adds no evidence.
        let identity = alias_location(&target)?;
        for (source, destination) in std::iter::once((Path::new(""), &self.base)).chain(
            self.assignments
                .iter()
                .map(|assignment| (assignment.source.as_path(), &assignment.target)),
        ) {
            let destination_identity = if identity.is_some() {
                alias_location(destination)?
            } else {
                None
            };
            let mut reserved = false;
            for (boundary_source, boundary_target) in &self.boundaries {
                if boundary_source == source || !boundary_source.starts_with(source) {
                    continue;
                }
                reserved |= target.starts_with(boundary_target);
                if let (Some(identity), Some(boundary)) =
                    (&identity, alias_location(boundary_target)?)
                {
                    reserved |= identity.starts_with(boundary);
                }
                if reserved {
                    break;
                }
            }
            if reserved {
                // Descendant routing units reserve destination holes in their enclosing region.
                // Disjoint source regions retain competing inverses rather than choosing by depth.
                continue;
            }
            for (candidate, prefix) in std::iter::once((&target, destination))
                .chain(identity.as_ref().zip(destination_identity.as_ref()))
            {
                if let Ok(tail) = candidate.strip_prefix(prefix) {
                    let logical = join_suffix(source, tail);
                    let forward = self.target_for(&logical);
                    // Every inverse must survive the final cascade, including shadowed rules.
                    let aliases_match = if forward != target
                        && let Some(identity) = &identity
                    {
                        alias_location(&forward)?.as_ref() == Some(identity)
                    } else {
                        false
                    };
                    if forward == target || aliases_match {
                        matches.insert(
                            logical.clone(),
                            ReverseMatch {
                                rule: self.rule_for(&logical).cloned(),
                                source: logical,
                            },
                        );
                    }
                }
            }
        }
        let mut matches: Vec<_> = matches.into_values().collect();
        Ok(match matches.len() {
            0 => ReverseResolution::Absent,
            1 => ReverseResolution::Unique(matches.remove(0)),
            _ => ReverseResolution::Ambiguous(matches),
        })
    }

    pub fn logical_path_for(&self, target: &Path) -> Result<PathBuf> {
        match self.reverse(target)? {
            ReverseResolution::Unique(found) => return Ok(found.source),
            ReverseResolution::Ambiguous(matches) => bail!(
                "ambiguous source path for '{}': {}",
                target.display(),
                matches
                    .iter()
                    .map(|found| found.source.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            ReverseResolution::Absent => {}
        }
        bail!(
            "{} is not included in {} or its mapped targets",
            target.display(),
            self.base.display()
        )
    }
}

fn join_suffix(prefix: &Path, suffix: &Path) -> PathBuf {
    // Joining an empty suffix appends a separator that changes symlink creation semantics.
    if suffix.as_os_str().is_empty() {
        prefix.to_path_buf()
    } else {
        prefix.join(suffix)
    }
}

fn alias_location(path: &Path) -> Result<Option<PathBuf>> {
    match location(path) {
        Ok(identity) => Ok(Some(identity)),
        Err(error)
            if error.downcast_ref::<std::io::Error>().is_some_and(|error| {
                matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                )
            }) =>
        {
            // A broken link or obstructed scaffold has lexical ownership but no alias evidence.
            tracing::debug!(%error, path = %path.display(), "path has no physical alias evidence");
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::Context;
    use crate::overlays::Repository;
    use assert_fs::prelude::*;
    use rstest::rstest;

    #[rstest]
    #[case(
        "- {path: config/editor, target: '~/editor'}\n  - {path: config, target: '~/settings'}",
        "settings/editor/init",
        1
    )]
    #[case(
        "- {path: config, target: '~/settings'}\n  - {path: config/editor, target: '~/editor'}",
        "editor/init",
        2
    )]
    #[case(
        "- {path: config, target: '~/settings'}\n  - {path: config/editor, target: '~/settings/editor'}",
        "settings/editor/init",
        1
    )]
    #[case(
        "- {path: config, target: '~/settings'}\n  - {path: config/editor, target: '~/settings/renamed'}",
        "settings/renamed/init",
        2
    )]
    fn final_ordered_targets_determine_layout_boundaries(
        #[case] rules: &str,
        #[case] expected: &str,
        #[case] boundaries: usize,
    ) {
        let temp = assert_fs::TempDir::new().unwrap();
        temp.child("repo/ov/over.yaml")
            .write_str(&format!("target: ~/base\nrules:\n  {rules}\n"))
            .unwrap();
        let overlay = Repository::new(temp.path().join("repo")).get("ov").unwrap();
        let ctx = Context::builder().root(temp.path().join("home")).build();
        let routing = overlay.resolve_mappings(&ctx).unwrap();
        let logical = Path::new("config/editor/init");
        assert_eq!(
            routing.target_for(logical),
            temp.path().join("home").join(expected)
        );
        assert_eq!(routing.boundaries.len(), boundaries);
        assert_eq!(
            routing
                .logical_path_for(&routing.target_for(logical))
                .unwrap(),
            logical
        );
        assert_eq!(
            routing.target_for(Path::new("config-old/file")),
            temp.path().join("home/base/config-old/file")
        );
    }

    #[test]
    fn targets_and_permissions_cascade_independently_against_original_sources() {
        let temp = assert_fs::TempDir::new().unwrap();
        temp.child("repo/ov/over.yaml").write_str("rules:\n  - {path: dotfiles/ssh, target: '~/.ssh', permissions: '0644'}\n  - {path: 'dotfiles/ssh/*.pub', permissions: '0666'}\n  - {path: dotfiles/ssh/id_rsa, permissions: '0600'}\n  - {path: .ssh, target: '~/wrong', permissions: '0777'}\n  - {path: dotfiles/ssh/id_rsa, target: '~/private/key'}\n").unwrap();
        let overlay = Repository::new(temp.path().join("repo")).get("ov").unwrap();
        let ctx = Context::builder().root(temp.path().join("home")).build();
        let routing = overlay.resolve_mappings(&ctx).unwrap();
        for (source, target, mode) in [
            ("dotfiles/ssh/id_rsa", "private/key", "0600"),
            ("dotfiles/ssh/key.pub", ".ssh/key.pub", "0666"),
            ("dotfiles/ssh/config", ".ssh/config", "0644"),
        ] {
            assert_eq!(
                routing.target_for(Path::new(source)),
                temp.path().join("home").join(target)
            );
            assert_eq!(
                overlay.permission_for(Path::new(source)),
                Some(super::super::FileMode::parse(mode).unwrap())
            );
        }
    }

    #[test]
    fn inherited_selectors_above_the_overlay_preserve_their_own_suffix() {
        let temp = assert_fs::TempDir::new().unwrap();
        temp.child("repo/over.yaml")
            .write_str("target: ~/fallback\nrules:\n  - {path: hosts, target: '~/machines'}\n")
            .unwrap();
        temp.child("repo/hosts/laptop/over.yaml")
            .write_str("target: ~/nearer-fallback\n")
            .unwrap();
        let overlay = Repository::new(temp.path().join("repo"))
            .get("hosts/laptop")
            .unwrap();
        let ctx = Context::builder().root(temp.path().join("home")).build();
        let routing = overlay.resolve_mappings(&ctx).unwrap();
        assert_eq!(
            routing.target_for(Path::new("config/file")),
            temp.path().join("home/machines/laptop/config/file")
        );
        assert!(routing.boundaries.is_empty());
        let ReverseResolution::Unique(found) = routing
            .reverse(&temp.path().join("home/machines/laptop/config"))
            .unwrap()
        else {
            panic!("unique inherited target")
        };
        assert_eq!(found.source, Path::new("config"));
        assert_eq!(
            found.rule.unwrap().descriptor,
            temp.path().join("repo/over.yaml")
        );
    }

    #[rstest]
    #[case("", "home/file")]
    #[case("target: ~/root\n", "home/root/file")]
    fn rules_without_targets_preserve_the_existing_fallback(
        #[case] root: &str,
        #[case] expected: &str,
    ) {
        let temp = assert_fs::TempDir::new().unwrap();
        temp.child("repo/over.yaml").write_str(root).unwrap();
        temp.child("repo/ov/over.yaml")
            .write_str("rules: [{path: file, permissions: '0600'}]\n")
            .unwrap();
        let overlay = Repository::new(temp.path().join("repo")).get("ov").unwrap();
        let ctx = Context::builder().root(temp.path().join("home")).build();
        assert_eq!(
            overlay
                .resolve_mappings(&ctx)
                .unwrap()
                .target_for(Path::new("file")),
            temp.path().join(expected)
        );
    }

    #[test]
    fn shadowed_targets_cannot_infer_a_source_path() {
        let temp = assert_fs::TempDir::new().unwrap();
        temp.child("repo/ov/over.yaml").write_str("target: ~/base\nrules:\n  - {path: config/editor, target: '~/old'}\n  - {path: config, target: '~/new'}\n").unwrap();
        let overlay = Repository::new(temp.path().join("repo")).get("ov").unwrap();
        let ctx = Context::builder().root(temp.path().join("home")).build();
        let routing = overlay.resolve_mappings(&ctx).unwrap();
        assert_eq!(
            routing
                .rule_for(Path::new("config/editor/file"))
                .unwrap()
                .index,
            1
        );
        assert!(!routing.boundaries.contains_key(Path::new("config/editor")));
        assert_eq!(
            routing.reverse(&temp.path().join("home/old/file")).unwrap(),
            ReverseResolution::Absent
        );
    }

    #[test]
    fn standalone_mappings_bind_the_supplied_overlays_base_target() {
        let temp = assert_fs::TempDir::new().unwrap();
        for name in ["first", "second"] {
            temp.child(format!("repo/{name}/over.toml")).write_str(
                "target = '~/{{ overlay.name }}'\n[[rules]]\npath = 'config'\ntarget = '{{ paths.target }}-config'\n"
            ).unwrap();
            temp.child(format!("repo/{name}/config/file"))
                .write_str("content")
                .unwrap();
        }
        let repo = Repository::new(temp.path().join("repo"));
        let first = repo.get("first").unwrap();
        let second = repo.get("second").unwrap();
        let ctx = Context::builder()
            .repository(repo)
            .root(temp.path().join("home"))
            .build()
            .prepare_overlay(&first)
            .unwrap();
        for overlay in [first, second] {
            let routing = overlay.resolve_mappings(&ctx).unwrap();
            assert_eq!(routing.base, temp.path().join("home").join(&overlay.name));
            assert_eq!(
                routing.target_for(Path::new("config/file")),
                temp.path()
                    .join("home")
                    .join(format!("{}-config/file", overlay.name))
            );
        }
    }

    #[test]
    fn an_unspecified_tilde_root_never_becomes_the_invocation_directory() {
        assert!(render_target("~", &Context::default()).is_err());
        assert!(render_target("~/file", &Context::default()).is_err());
    }

    #[rstest]
    #[case("over.toml", "target = '{{ env.OVER_MISSING_PATH_VALUE }}'")]
    #[case(
        "over.toml",
        "[[rules]]\npath = 'config'\ntarget = '{{ overlays.missing }}'"
    )]
    #[case("link.link.toml", "target = '{{ env.OVER_MISSING_PATH_VALUE }}'")]
    #[case(
        "block.partial.toml",
        "target = '{{ overlays.missing }}'\ncontent = 'block'"
    )]
    fn strict_desired_path_resolution_reports_missing_namespace_values(
        #[case] file: &str,
        #[case] content: &str,
    ) {
        let temp = assert_fs::TempDir::new().unwrap();
        temp.child(format!("ov/{file}")).write_str(content).unwrap();
        let repo = Repository::new(temp.path().to_path_buf());
        let overlay = repo.get("ov").unwrap();
        let ctx = Context::builder()
            .repository(repo)
            .root(temp.path().join("home"))
            .build();
        let error = crate::desired::DesiredTree::build_own(&ctx, &overlay).unwrap_err();
        assert!(format!("{error:#}").contains("undefined"));
    }

    #[test]
    fn desired_paths_share_environment_overlay_and_execution_context_namespaces() {
        let temp = assert_fs::TempDir::new().unwrap();
        // Use a built-in environment value without mutating process-global test state.
        temp.child("repo/ov/over.toml").write_str(
            "target = '{{ root }}/base'\n[[rules]]\npath = 'config'\ntarget = '{{ overlays.shared }}/mapped'\n"
        ).unwrap();
        temp.child("repo/ov/config/file")
            .write_str("content")
            .unwrap();
        temp.child("repo/ov/link.link.toml")
            .write_str("target = '{{ root }}/referent'\n")
            .unwrap();
        temp.child("repo/ov/block.partial.toml")
            .write_str(
                "target = '{{ overlays.ov }}/{{ env.PATH | length }}/partial'\ncontent = 'block'\n",
            )
            .unwrap();
        let repo = Repository::new(temp.path().join("repo"));
        let overlay = repo.get("ov").unwrap();
        let root = temp.path().join("home");
        let shared = temp.path().join("shared");
        let ctx = Context::builder()
            .repository(repo)
            .root(root.clone())
            .resolved_overlays(std::sync::Arc::new(std::collections::HashMap::from([(
                "shared".to_owned(),
                shared.to_string_lossy().into_owned(),
            )])))
            .build();
        let tree = crate::desired::DesiredTree::build_own(&ctx, &overlay).unwrap();
        assert!(
            tree.entries()
                .iter()
                .any(|entry| entry.target == shared.join("mapped/file"))
        );
        assert!(tree.entries().iter().any(|entry| matches!(&entry.intent,
            crate::desired::MaterializationIntent::SymlinkFile { source, .. } if source == &root.join("referent"))));
        let path_length = std::env::var("PATH").unwrap().chars().count();
        assert!(
            tree.entries()
                .iter()
                .any(|entry| entry.target == root.join(format!("base/{path_length}/partial")))
        );
    }

    #[test]
    fn reverse_alias_routing_preserves_a_symlink_leaf_identity() {
        let temp = assert_fs::TempDir::new().unwrap();
        temp.child("real/client").create_dir_all().unwrap();
        symlink::symlink_dir(temp.child("real").path(), temp.child("alias").path()).unwrap();
        temp.child("outside").write_str("external").unwrap();
        symlink::symlink_file(
            temp.child("outside").path(),
            temp.child("real/client/link").path(),
        )
        .unwrap();
        temp.child("ov/over.toml")
            .write_str(&format!(
                "target='~/workspace'\n[[rules]]\npath='config'\ntarget={:?}\n",
                temp.child("alias/client").path().to_string_lossy()
            ))
            .unwrap();
        let overlay = Repository::new(temp.path().to_path_buf())
            .get("ov")
            .unwrap();
        let ctx = Context::builder().root(temp.path().join("home")).build();
        let routing = overlay.resolve_mappings(&ctx).unwrap();
        assert_eq!(
            routing
                .logical_path_for(temp.child("real/client/link").path())
                .unwrap(),
            Path::new("config/link")
        );
        assert!(
            routing
                .logical_path_for(temp.child("outside").path())
                .is_err()
        );
    }

    #[test]
    fn mappings_replace_prefixes_and_reverse_without_shadowed_fallbacks() {
        let temp = assert_fs::TempDir::new().unwrap();
        temp.child("ov/over.yaml").write_str("target: ~/Workspace/Client\nrules:\n  - path: etc\n    target: ~/system/client\n  - path: config\n    target: ../settings\n").unwrap();
        let repo = Repository::new(temp.path().to_path_buf());
        let overlay = repo.get("ov").unwrap();
        let ctx = Context::builder()
            .root(temp.path().join("home"))
            .repository(repo)
            .build();
        let routing = overlay.resolve_mappings(&ctx).unwrap();
        for logical in ["etc/config.toml", "config/settings.json", "main/app"] {
            let target = routing.target_for(Path::new(logical));
            assert_eq!(
                routing.logical_path_for(&target).unwrap(),
                Path::new(logical)
            );
        }
        assert_eq!(
            routing.target_for(Path::new("config/a")),
            temp.path().join("settings/a")
        );
        assert_eq!(
            routing.target_for(Path::new("etc/a")),
            temp.path().join("home/system/client/a")
        );
        assert!(
            routing
                .logical_path_for(&routing.base.join("config/a"))
                .is_err()
        );
        assert!(
            routing
                .logical_path_for(&routing.base.join("etc/a"))
                .is_err()
        );
    }

    #[test]
    fn target_rules_cannot_enter_the_source_namespace() {
        let temp = assert_fs::TempDir::new().unwrap();
        temp.child("ov/over.toml")
            .write_str("target='~/target'\n[[rules]]\npath='config'\ntarget='inside-source'")
            .unwrap();
        let repo = Repository::new(temp.path().to_path_buf());
        let overlay = repo.get("ov").unwrap();
        let ctx = Context::builder().root(temp.path().join("home")).build();
        assert!(
            overlay
                .resolve_mappings(&ctx)
                .unwrap_err()
                .to_string()
                .contains("overlaps the overlay source")
        );
    }

    #[test]
    fn nearer_mapping_targets_override_inherited_keys_and_preserve_siblings() {
        let temp = assert_fs::TempDir::new().unwrap();
        temp.child("over.toml")
            .write_str("[[rules]]\npath='ov/config'\ntarget='settings'\n[[rules]]\npath='ov/etc'\ntarget='system'")
            .unwrap();
        temp.child("ov/over.yaml")
            .write_str("rules:\n  - path: config\n    target: ../custom\n")
            .unwrap();
        let overlay = Repository::new(temp.path().to_path_buf())
            .get("ov")
            .unwrap();
        let ctx = Context::builder().root(temp.path().join("home")).build();
        let routing = overlay.resolve_mappings(&ctx).unwrap();
        assert_eq!(
            routing.target_for(Path::new("config/a")),
            temp.path().join("custom/a")
        );
        assert_eq!(
            routing.target_for(Path::new("etc/a")),
            temp.path().join("system/a")
        );
    }

    #[cfg(unix)]
    #[rstest]
    #[case("real/new", "alias/new")]
    #[case("real/new", "alias/new/nested")]
    fn absent_target_aliases_preserve_all_inverse_explanations(
        #[case] first: &str,
        #[case] second: &str,
    ) {
        let temp = assert_fs::TempDir::new().unwrap();
        temp.child("real").create_dir_all().unwrap();
        std::os::unix::fs::symlink(temp.path().join("real"), temp.path().join("alias")).unwrap();
        temp.child("ov/over.toml")
            .write_str(&format!(
                "target='~/base'\n[[rules]]\npath='config'\ntarget='{}'\n[[rules]]\npath='etc'\ntarget='{}'",
                temp.path().join(first).display(),
                temp.path().join(second).display()
            ))
            .unwrap();
        let overlay = Repository::new(temp.path().to_path_buf())
            .get("ov")
            .unwrap();
        let ctx = Context::builder().root(temp.path().join("home")).build();
        let routing = overlay.resolve_mappings(&ctx).unwrap();
        assert!(matches!(
            routing.reverse(&temp.path().join(second)).unwrap(),
            ReverseResolution::Ambiguous(_)
        ));
        assert!(!temp.path().join("real/new").exists());
    }

    #[cfg(unix)]
    #[test]
    fn absent_mapped_leaves_cannot_enter_the_source_through_an_alias() {
        let temp = assert_fs::TempDir::new().unwrap();
        temp.child("ov").create_dir_all().unwrap();
        std::os::unix::fs::symlink(temp.path().join("ov"), temp.path().join("alias")).unwrap();
        temp.child("ov/over.toml")
            .write_str(&format!(
                "target='~/base'\n[[rules]]\npath='config'\ntarget='{}'",
                temp.path().join("alias/new").display()
            ))
            .unwrap();
        let overlay = Repository::new(temp.path().to_path_buf())
            .get("ov")
            .unwrap();
        let ctx = Context::builder().root(temp.path().join("home")).build();
        assert!(
            overlay
                .resolve_mappings(&ctx)
                .unwrap_err()
                .to_string()
                .contains("overlaps the overlay source")
        );
        assert!(!temp.path().join("ov/new").exists());
    }

    #[cfg(unix)]
    #[test]
    fn physical_validation_preserves_lexical_routing_and_deployed_directory_links() {
        let temp = assert_fs::TempDir::new().unwrap();
        temp.child("real").create_dir_all().unwrap();
        temp.child("ov/config/file").write_str("source").unwrap();
        std::os::unix::fs::symlink(temp.path().join("real"), temp.path().join("alias")).unwrap();
        temp.child("ov/over.toml")
            .write_str(&format!(
                "target='~/base'\n[[rules]]\npath='config'\nmode='symlink-directory'\ntarget='{}'",
                temp.path().join("alias/new").display()
            ))
            .unwrap();
        let overlay = Repository::new(temp.path().to_path_buf())
            .get("ov")
            .unwrap();
        let ctx = Context::builder().root(temp.path().join("home")).build();
        let routing = overlay.resolve_mappings(&ctx).unwrap();
        let target = temp.path().join("alias/new/file");
        assert_eq!(routing.target_for(Path::new("config/file")), target);
        assert_eq!(
            routing.logical_path_for(&target).unwrap(),
            Path::new("config/file")
        );
        std::os::unix::fs::symlink(temp.path().join("ov/config"), temp.path().join("real/new"))
            .unwrap();
        assert!(overlay.resolve_mappings(&ctx).is_ok());
    }
}
