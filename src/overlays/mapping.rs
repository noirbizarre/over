//! Bidirectional directory routing, independent of materialization strategy.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context as _, Result, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::exec;

use super::{MaterializationKind, Overlay};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DirectoryMapping {
    pub target: String,
}

#[derive(Debug, Clone)]
pub struct ResolvedMappings {
    pub base: PathBuf,
    pub directories: BTreeMap<PathBuf, PathBuf>,
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
    let mut sources: Vec<PathBuf> = Vec::new();
    for (key, config) in &overlay.map {
        let source = PathBuf::from(key);
        ensure!(
            !source.as_os_str().is_empty()
                && source
                    .components()
                    .all(|part| matches!(part, Component::Normal(_)))
                && !key.split('/').any(|part| part == "." || part.is_empty()),
            "mapping source '{key}' must be a relative directory without '.' or '..'"
        );
        match overlay.root.join(&source).symlink_metadata() {
            Ok(metadata) => ensure!(
                metadata.is_dir(),
                "mapping source '{key}' must be a directory"
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("failed to inspect mapping '{key}'"));
            }
        }
        ensure!(
            !config.target.trim().is_empty(),
            "mapping '{key}' has an empty target"
        );
        for other in &sources {
            ensure!(
                !source.starts_with(other) && !other.starts_with(&source),
                "nested mapping sources '{}' and '{key}' are not supported",
                other.display()
            );
        }
        // A symlink owns its complete subtree; it cannot leave a hole for a map.
        let mut ancestor = source.parent();
        while let Some(path) = ancestor {
            // The overlay root is a scaffold, not a directory symlink unit.
            if path.as_os_str().is_empty() {
                break;
            }
            ensure!(
                overlay.materialization_for(path) != MaterializationKind::SymlinkDirectory,
                "mapping '{key}' is inside whole-directory symlink '{}'",
                path.display()
            );
            ancestor = path.parent();
        }
        sources.push(source);
    }
    Ok(())
}

impl ResolvedMappings {
    pub fn new(overlay: &Overlay, ctx: &exec::Context) -> Result<Self> {
        validate_sources(overlay)?;
        let prepared = ctx.prepare_overlay(overlay)?;
        let ctx = prepared.as_ref();
        let base = ctx
            .current_target
            .clone()
            .context("prepared overlay has no target")?;
        let mut directories: BTreeMap<PathBuf, PathBuf> = BTreeMap::new();
        let mut physical_targets: Vec<PathBuf> = Vec::new();
        for (key, config) in &overlay.map {
            let source = PathBuf::from(key);
            let origin = overlay.mapping_origins.get(key).unwrap_or(&overlay.root);
            let target = absolute(&resolve_path(&config.target, origin, ctx)?)?;
            let root = ctx.absolute_path(&overlay.root)?;
            let physical_target = physical_destination(&target, &root.join(&source))?;
            let physical_base = physical(&base)?;
            ensure!(
                !base.starts_with(&target) && !physical_base.starts_with(&physical_target),
                "mapping '{key}' target '{}' contains or equals the overlay target",
                target.display()
            );
            let physical_root = physical(&root)?;
            ensure!(
                !target.starts_with(&root)
                    && !root.starts_with(&target)
                    && !physical_target.starts_with(&physical_root)
                    && !physical_root.starts_with(&physical_target),
                "mapping '{key}' target overlaps the overlay source"
            );
            for other_target in directories.values() {
                ensure!(
                    !target.starts_with(other_target) && !other_target.starts_with(&target),
                    "mapping targets '{}' and '{}' overlap",
                    other_target.display(),
                    target.display()
                );
            }
            for other in &physical_targets {
                ensure!(
                    !physical_target.starts_with(other) && !other.starts_with(&physical_target),
                    "mapping targets physically overlap at '{}' and '{}'",
                    other.display(),
                    physical_target.display()
                );
            }
            physical_targets.push(physical_target);
            directories.insert(source, target);
        }
        Ok(Self { base, directories })
    }

    pub fn target_for(&self, logical: &Path) -> PathBuf {
        for (source, target) in &self.directories {
            if let Ok(tail) = logical.strip_prefix(source) {
                // A trailing separator makes symlink creation treat a missing
                // destination as an existing directory rather than a link name.
                return if tail.as_os_str().is_empty() {
                    target.clone()
                } else {
                    target.join(tail)
                };
            }
        }
        self.base.join(logical)
    }

    pub fn logical_path_for(&self, target: &Path) -> Result<PathBuf> {
        let target = absolute(target)?;
        for (source, destination) in &self.directories {
            if let Ok(tail) = target.strip_prefix(destination) {
                return Ok(source.join(tail));
            }
        }
        if let Ok(logical) = target.strip_prefix(&self.base)
            && self.target_for(logical) == target
        {
            return Ok(logical.to_path_buf());
        }
        // CWD and Git discovery can spell a configured ancestor differently
        // (/private/var on macOS or long/verbatim paths on Windows).
        let identity = location(&target)?;
        for (source, destination) in &self.directories {
            if let Ok(tail) = identity.strip_prefix(location(destination)?) {
                let logical = source.join(tail);
                if location(&self.target_for(&logical))? == identity {
                    return Ok(logical);
                }
            }
        }
        if let Ok(logical) = identity.strip_prefix(location(&self.base)?)
            && location(&self.target_for(logical))? == identity
        {
            return Ok(logical.to_path_buf());
        }
        bail!(
            "{} is not included in {} or its mapped targets",
            target.display(),
            self.base.display()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::Context;
    use crate::overlays::Repository;
    use assert_fs::prelude::*;
    use rstest::rstest;

    #[test]
    fn standalone_mappings_bind_the_supplied_overlays_base_target() {
        let temp = assert_fs::TempDir::new().unwrap();
        for name in ["first", "second"] {
            temp.child(format!("repo/{name}/over.toml")).write_str(
                "target = '~/{{ overlay.name }}'\n[map.config]\ntarget = '{{ paths.target }}-config'\n"
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
    #[case("over.toml", "[map.config]\ntarget = '{{ overlays.missing }}'")]
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
            "target = '{{ root }}/base'\n[map.config]\ntarget = '{{ overlays.shared }}/mapped'\n"
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
                "target='~/workspace'\n[map.config]\ntarget={:?}\n",
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
        temp.child("ov/over.yaml").write_str("target: ~/Workspace/Client\nmap:\n  etc:\n    target: ~/system/client\n  config:\n    target: ../settings\n").unwrap();
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

    #[rstest]
    #[case(
        "[map.config]\ntarget='/tmp/client'\n[map.'config/editor']\ntarget='/tmp/editor'",
        "nested mapping"
    )]
    #[case("[map.'../config']\ntarget='/tmp/client'", "relative directory")]
    #[case(
        "[map.config]\ntarget='/tmp/client'\n[map.etc]\ntarget='/tmp/client/etc'",
        "targets"
    )]
    fn invalid_mapping_namespaces_are_rejected(#[case] mapping: &str, #[case] error: &str) {
        let temp = assert_fs::TempDir::new().unwrap();
        temp.child("ov/over.toml")
            .write_str(&format!("target='~/target'\n{mapping}"))
            .unwrap();
        let repo = Repository::new(temp.path().to_path_buf());
        let overlay = repo.get("ov").unwrap();
        let ctx = Context::builder().root(temp.path().join("home")).build();
        assert!(
            overlay
                .resolve_mappings(&ctx)
                .unwrap_err()
                .to_string()
                .contains(error)
        );
    }

    #[test]
    fn nearer_mapping_targets_override_inherited_keys_and_preserve_siblings() {
        let temp = assert_fs::TempDir::new().unwrap();
        temp.child("over.toml")
            .write_str("[map.'ov/config']\ntarget='settings'\n[map.'ov/etc']\ntarget='system'")
            .unwrap();
        temp.child("ov/over.yaml")
            .write_str("map:\n  config:\n    target: custom\n")
            .unwrap();
        let overlay = Repository::new(temp.path().to_path_buf())
            .get("ov")
            .unwrap();
        assert_eq!(overlay.map["config"].target, "custom");
        assert_eq!(overlay.map["etc"].target, "system");
    }

    #[cfg(unix)]
    #[rstest]
    #[case("real/new", "alias/new")]
    #[case("real/new", "alias/new/nested")]
    fn absent_mapped_leaves_cannot_overlap_through_an_ancestor_alias(
        #[case] first: &str,
        #[case] second: &str,
    ) {
        let temp = assert_fs::TempDir::new().unwrap();
        temp.child("real").create_dir_all().unwrap();
        std::os::unix::fs::symlink(temp.path().join("real"), temp.path().join("alias")).unwrap();
        temp.child("ov/over.toml")
            .write_str(&format!(
                "target='~/base'\n[map.config]\ntarget='{}'\n[map.etc]\ntarget='{}'",
                temp.path().join(first).display(),
                temp.path().join(second).display()
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
                .contains("physically overlap")
        );
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
                "target='~/base'\n[map.config]\ntarget='{}'",
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
                "target='~/base'\n[[rules]]\npath='config'\nmode='symlink-directory'\n[map.config]\ntarget='{}'",
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
