use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as AnyhowContext, Result};
use async_trait::async_trait;

use serde::{Deserialize, Serialize};
use symlink::{remove_symlink_dir, remove_symlink_file, symlink_dir, symlink_file};

use crate::exec::{Action, Ctx};
use crate::ui::{emojis, style};
use crate::utils::short_path;

#[derive(Debug, Deserialize, Serialize, Clone, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LinkType {
    #[default]
    Soft,
    Hard,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct SymlinkConfig {
    pub target: String,
    #[serde(default)]
    pub r#type: LinkType,
}

pub struct EnsureSymlink {
    pub source: PathBuf,
    pub target: PathBuf,
    pub link_type: LinkType,
    pub is_dir: bool,
}

impl EnsureSymlink {
    pub fn new(source: PathBuf, target: PathBuf, link_type: LinkType, is_dir: bool) -> Self {
        Self {
            source,
            target,
            link_type,
            is_dir,
        }
    }
}

impl fmt::Display for EnsureSymlink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self.link_type {
            LinkType::Soft => "symlink",
            LinkType::Hard => "hardlink",
        };
        write!(
            f,
            "{} {} {} -> {} ({})",
            emojis::LINK,
            style::white(format!("{kind}:")),
            short_path(&self.source.to_string_lossy()),
            short_path(&self.target.to_string_lossy()),
            kind,
        )
    }
}

fn remove_target(target: &Path) -> Result<()> {
    if target.is_symlink() {
        if target.is_dir() {
            remove_symlink_dir(target)?;
        } else {
            remove_symlink_file(target)?;
        }
    } else if target.is_dir() {
        fs::remove_dir_all(target)?;
    } else {
        fs::remove_file(target)?;
    }
    Ok(())
}

fn resolve_conflict(source: &Path, target: &Path) -> Result<bool> {
    if target.is_symlink() {
        let existing = fs::read_link(target)?;
        if existing == source {
            return Ok(false);
        }
    }
    if target.exists() || target.is_symlink() {
        remove_target(target)?;
    }
    Ok(true)
}

#[async_trait]
impl Action for EnsureSymlink {
    async fn execute(&self, ctx: Ctx) -> Result<()> {
        if ctx.dry_run {
            return Ok(());
        }

        let source = self.source.clone();
        let target = self.target.clone();
        let link_type = self.link_type.clone();
        let is_dir = self.is_dir;

        tokio::task::spawn_blocking(move || -> Result<()> {
            let should_link = resolve_conflict(&source, &target)?;
            if !should_link {
                return Ok(());
            }

            match link_type {
                LinkType::Soft => {
                    if is_dir {
                        symlink_dir(&source, &target)?;
                    } else {
                        symlink_file(&source, &target)?;
                    }
                }
                LinkType::Hard => {
                    if is_dir {
                        tracing::warn!(
                            source = %source.display(),
                            target = %target.display(),
                            "hard links are not supported for directories, falling back to soft link",
                        );
                        symlink_dir(&source, &target)?;
                    } else {
                        match fs::hard_link(&source, &target) {
                            Ok(()) => {}
                            Err(e) => {
                                tracing::warn!(
                                    source = %source.display(),
                                    target = %target.display(),
                                    error = %e,
                                    "hard link failed, falling back to soft link",
                                );
                                symlink_file(&source, &target)?;
                            }
                        }
                    }
                }
            }

            Ok(())
        })
        .await?
    }
}

pub fn discover_symlinks(overlay_root: &Path) -> Result<Vec<(String, SymlinkConfig)>> {
    use globset::GlobBuilder;
    use walkdir::WalkDir;

    let glob = GlobBuilder::new("**/*.link.{toml,yaml,yml}")
        .literal_separator(true)
        .build()?
        .compile_matcher();

    let mut results: Vec<(String, SymlinkConfig)> = Vec::new();
    let mut seen_names: HashMap<String, PathBuf> = HashMap::new();

    for entry in WalkDir::new(overlay_root)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        let rel = path.strip_prefix(overlay_root)?;

        if !glob.is_match(rel) {
            continue;
        }

        let rel_str = rel.to_string_lossy();
        let stem = rel_str
            .strip_suffix(".link.toml")
            .or_else(|| rel_str.strip_suffix(".link.yaml"))
            .or_else(|| rel_str.strip_suffix(".link.yml"))
            .map(String::from)
            .ok_or_else(|| anyhow::anyhow!("unexpected symlink config file: {}", rel.display()))?;

        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read symlink config: {}", rel.display()))?;

        let config: SymlinkConfig = if rel_str.ends_with(".toml") {
            toml::from_str(&content)
                .with_context(|| format!("failed to parse {}", rel.display()))?
        } else {
            serde_yml::from_str(&content)
                .with_context(|| format!("failed to parse {}", rel.display()))?
        };

        if config.target.is_empty() {
            tracing::warn!(
                name = %stem,
                path = %rel.display(),
                "empty target in symlink config, skipping",
            );
            continue;
        }

        if let Some(existing_path) = seen_names.get(&stem) {
            let (toml_path, yaml_path) = if rel.to_string_lossy().ends_with(".toml") {
                (rel.to_path_buf(), existing_path.clone())
            } else {
                (existing_path.clone(), rel.to_path_buf())
            };
            tracing::warn!(
                name = %stem,
                toml = %toml_path.display(),
                yaml = %yaml_path.display(),
                "both TOML and YAML symlink configs exist; TOML takes precedence",
            );
            if rel.to_string_lossy().ends_with(".toml") {
                results.retain(|(name, _)| name != &stem);
                seen_names.insert(stem.clone(), rel.to_path_buf());
                results.push((stem, config));
            }
            continue;
        }

        seen_names.insert(stem.clone(), rel.to_path_buf());
        results.push((stem, config));
    }

    results.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(results)
}

pub fn render_symlink_target(template: &str, overlays: &HashMap<String, String>) -> Result<String> {
    let env_vars: HashMap<String, String> = std::env::vars().collect();
    let mut state = HashMap::new();
    state.insert("env", env_vars);
    state.insert("overlays", overlays.clone());

    crate::exec::templates::render_string(template, &state)
        .with_context(|| format!("failed to render symlink target template '{}'", template))
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    use rstest::rstest;

    /// Install a tracing subscriber so `tracing::warn!` etc. bodies are executed during tests.
    fn init_test_tracing() {
        let _ = tracing_subscriber::fmt()
            .with_test_writer()
            .with_max_level(tracing::Level::TRACE)
            .try_init();
    }

    #[test]
    fn link_type_default_is_soft() {
        assert_eq!(LinkType::default(), LinkType::Soft);
    }

    #[test]
    fn link_type_deserialize_soft() {
        #[derive(serde::Deserialize)]
        struct Wrapper {
            lt: LinkType,
        }
        let w: Wrapper = toml::from_str("lt = \"soft\"").unwrap();
        assert_eq!(w.lt, LinkType::Soft);
    }

    #[test]
    fn link_type_deserialize_hard() {
        #[derive(serde::Deserialize)]
        struct Wrapper {
            lt: LinkType,
        }
        let w: Wrapper = toml::from_str("lt = \"hard\"").unwrap();
        assert_eq!(w.lt, LinkType::Hard);
    }

    #[test]
    fn symlink_config_deserialize_soft() {
        let cfg: SymlinkConfig = toml::from_str("target = \"/foo\"").unwrap();
        assert_eq!(cfg.target, "/foo");
        assert_eq!(cfg.r#type, LinkType::Soft);
    }

    #[test]
    fn symlink_config_deserialize_hard() {
        let cfg: SymlinkConfig = toml::from_str("target = \"/foo\"\ntype = \"hard\"").unwrap();
        assert_eq!(cfg.target, "/foo");
        assert_eq!(cfg.r#type, LinkType::Hard);
    }

    #[test]
    fn render_symlink_target_with_env() {
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("OVER_TEST_HOME", "/test/home");
        }
        let overlays = HashMap::new();
        let result = render_symlink_target("{{ env.OVER_TEST_HOME }}/config", &overlays).unwrap();
        assert_eq!(result, "/test/home/config");
    }

    #[test]
    fn render_symlink_target_with_overlays() {
        let mut overlays = HashMap::new();
        overlays.insert("apps/nvim".to_string(), "/opt/nvim".to_string());
        let result =
            render_symlink_target("{{ overlays['apps/nvim'] }}/init.lua", &overlays).unwrap();
        assert_eq!(result, "/opt/nvim/init.lua");
    }

    #[rstest]
    #[case("target = \"/foo\"", "/foo", LinkType::Soft)]
    #[case("target = \"/foo\"\ntype = \"hard\"", "/foo", LinkType::Hard)]
    fn symlink_config_from_toml(
        #[case] input: &str,
        #[case] expected_target: &str,
        #[case] expected_type: LinkType,
    ) {
        let cfg: SymlinkConfig = toml::from_str(input).unwrap();
        assert_eq!(cfg.target, expected_target);
        assert_eq!(cfg.r#type, expected_type);
    }

    #[tokio::test]
    async fn ensure_symlink_creates_soft_link_file() {
        let td = TempDir::new().unwrap();
        let source = td.path().join("source.txt");
        let target = td.path().join("target.txt");
        fs::write(&source, "hello").unwrap();

        let ctx = crate::exec::Context::builder().build();
        let action = EnsureSymlink::new(source.clone(), target.clone(), LinkType::Soft, false);
        action.execute(ctx).await.unwrap();

        assert!(target.is_symlink());
        assert_eq!(fs::read_link(&target).unwrap(), source);
    }

    #[tokio::test]
    async fn ensure_symlink_creates_soft_link_dir() {
        let td = TempDir::new().unwrap();
        let source = td.path().join("source_dir");
        let target = td.path().join("target_dir");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("file.txt"), "hello").unwrap();

        let ctx = crate::exec::Context::builder().build();
        let action = EnsureSymlink::new(source.clone(), target.clone(), LinkType::Soft, true);
        action.execute(ctx).await.unwrap();

        assert!(target.is_symlink());
        assert_eq!(fs::read_link(&target).unwrap(), source);
        assert!(target.join("file.txt").exists());
    }

    #[tokio::test]
    async fn ensure_symlink_hard_link_file() {
        let td = TempDir::new().unwrap();
        let source = td.path().join("source.txt");
        let target = td.path().join("target.txt");
        fs::write(&source, "hello").unwrap();

        let ctx = crate::exec::Context::builder().build();
        let action = EnsureSymlink::new(source.clone(), target.clone(), LinkType::Hard, false);
        action.execute(ctx).await.unwrap();

        assert!(!target.is_symlink());
        assert_eq!(fs::read_to_string(&target).unwrap(), "hello");
    }

    #[tokio::test]
    async fn ensure_symlink_hard_link_dir_falls_back() {
        init_test_tracing();
        let td = TempDir::new().unwrap();
        let source = td.path().join("source_dir");
        let target = td.path().join("target_dir");
        fs::create_dir_all(&source).unwrap();

        let ctx = crate::exec::Context::builder().build();
        let action = EnsureSymlink::new(source.clone(), target.clone(), LinkType::Hard, true);
        action.execute(ctx).await.unwrap();

        assert!(target.is_symlink());
        assert_eq!(fs::read_link(&target).unwrap(), source);
    }

    #[tokio::test]
    async fn ensure_symlink_dry_run() {
        let td = TempDir::new().unwrap();
        let source = td.path().join("source.txt");
        let target = td.path().join("target.txt");
        fs::write(&source, "hello").unwrap();

        let ctx = crate::exec::Context::builder().dry_run(true).build();
        let action = EnsureSymlink::new(source.clone(), target.clone(), LinkType::Soft, false);
        action.execute(ctx).await.unwrap();

        assert!(!target.exists());
    }

    #[test]
    fn discover_symlinks_finds_files() {
        let td = TempDir::new().unwrap();
        td.child("nvim.link.toml")
            .write_str("target = \"/opt/nvim\"")
            .unwrap();
        td.child("sub").create_dir_all().unwrap();
        td.child("sub/tool.link.toml")
            .write_str("target = \"/opt/tool\"")
            .unwrap();

        let results = discover_symlinks(td.path()).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, "nvim");
        assert_eq!(results[1].0, "sub/tool");
    }

    #[test]
    fn discover_symlinks_yaml_also_found() {
        let td = TempDir::new().unwrap();
        td.child("app.link.yaml")
            .write_str("target: /opt/app")
            .unwrap();

        let results = discover_symlinks(td.path()).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "app");
        assert_eq!(results[0].1.target, "/opt/app");
    }

    #[test]
    fn discover_symlinks_toml_takes_precedence_over_yaml() {
        init_test_tracing();
        let td = TempDir::new().unwrap();
        td.child("app.link.toml")
            .write_str("target = \"/from-toml\"")
            .unwrap();
        td.child("app.link.yaml")
            .write_str("target: /from-yaml")
            .unwrap();

        let results = discover_symlinks(td.path()).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "app");
        assert_eq!(results[0].1.target, "/from-toml");
    }

    #[test]
    fn discover_symlinks_skips_empty_target() {
        init_test_tracing();
        let td = TempDir::new().unwrap();
        td.child("bad.link.toml")
            .write_str("target = \"\"")
            .unwrap();

        let results = discover_symlinks(td.path()).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn discover_symlinks_yaml_first_toml_second_toml_wins() {
        init_test_tracing();
        let td = TempDir::new().unwrap();
        td.child("app.link.yaml")
            .write_str("target: /from-yaml")
            .unwrap();
        td.child("app.link.toml")
            .write_str("target = \"/from-toml\"")
            .unwrap();

        let results = discover_symlinks(td.path()).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "app");
        assert_eq!(results[0].1.target, "/from-toml");
    }

    #[test]
    fn discover_symlinks_empty_dir() {
        let td = TempDir::new().unwrap();
        let results = discover_symlinks(td.path()).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn ensure_symlink_display_soft() {
        let action = EnsureSymlink::new(
            PathBuf::from("/src/file.txt"),
            PathBuf::from("/dst/file.txt"),
            LinkType::Soft,
            false,
        );
        let display = format!("{}", action);
        assert!(display.contains("symlink"));
        assert!(display.contains("/src/file.txt"));
        assert!(display.contains("/dst/file.txt"));
    }

    #[test]
    fn ensure_symlink_display_hard() {
        let action = EnsureSymlink::new(
            PathBuf::from("/src/file.txt"),
            PathBuf::from("/dst/file.txt"),
            LinkType::Hard,
            false,
        );
        let display = format!("{}", action);
        assert!(display.contains("hardlink"));
    }

    #[tokio::test]
    async fn ensure_symlink_already_linked() {
        let td = TempDir::new().unwrap();
        let source = td.path().join("source.txt");
        let target = td.path().join("target.txt");
        fs::write(&source, "hello").unwrap();
        symlink_file(&source, &target).unwrap();

        let ctx = crate::exec::Context::builder().build();
        let action = EnsureSymlink::new(source.clone(), target.clone(), LinkType::Soft, false);
        action.execute(ctx).await.unwrap();

        assert!(target.is_symlink());
        assert_eq!(fs::read_link(&target).unwrap(), source);
    }

    #[tokio::test]
    async fn ensure_symlink_overwrites_wrong_symlink() {
        let td = TempDir::new().unwrap();
        let source = td.path().join("source.txt");
        let wrong_source = td.path().join("wrong.txt");
        let target = td.path().join("target.txt");
        fs::write(&source, "hello").unwrap();
        fs::write(&wrong_source, "wrong").unwrap();
        symlink_file(&wrong_source, &target).unwrap();

        let ctx = crate::exec::Context::builder().build();
        let action = EnsureSymlink::new(source.clone(), target.clone(), LinkType::Soft, false);
        action.execute(ctx).await.unwrap();

        assert!(target.is_symlink());
        assert_eq!(fs::read_link(&target).unwrap(), source);
    }

    #[tokio::test]
    async fn ensure_symlink_overwrites_regular_file() {
        let td = TempDir::new().unwrap();
        let source = td.path().join("source.txt");
        let target = td.path().join("target.txt");
        fs::write(&source, "hello").unwrap();
        fs::write(&target, "existing").unwrap();

        let ctx = crate::exec::Context::builder().build();
        let action = EnsureSymlink::new(source.clone(), target.clone(), LinkType::Soft, false);
        action.execute(ctx).await.unwrap();

        assert!(target.is_symlink());
        assert_eq!(fs::read_link(&target).unwrap(), source);
    }

    #[tokio::test]
    async fn ensure_symlink_overwrites_existing_dir() {
        let td = TempDir::new().unwrap();
        let source = td.path().join("source_dir");
        let target = td.path().join("target_dir");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("file.txt"), "hello").unwrap();
        fs::create_dir_all(&target).unwrap();

        let ctx = crate::exec::Context::builder().build();
        let action = EnsureSymlink::new(source.clone(), target.clone(), LinkType::Soft, true);
        action.execute(ctx).await.unwrap();

        assert!(target.is_symlink());
        assert_eq!(fs::read_link(&target).unwrap(), source);
    }

    #[tokio::test]
    async fn ensure_symlink_already_linked_dir() {
        let td = TempDir::new().unwrap();
        let source = td.path().join("source_dir");
        let target = td.path().join("target_dir");
        fs::create_dir_all(&source).unwrap();
        symlink_dir(&source, &target).unwrap();

        let ctx = crate::exec::Context::builder().build();
        let action = EnsureSymlink::new(source.clone(), target.clone(), LinkType::Soft, true);
        action.execute(ctx).await.unwrap();

        assert!(target.is_symlink());
        assert_eq!(fs::read_link(&target).unwrap(), source);
    }

    #[tokio::test]
    async fn ensure_symlink_overwrites_wrong_dir_symlink() {
        let td = TempDir::new().unwrap();
        let source = td.path().join("source_dir");
        let wrong_source = td.path().join("wrong_dir");
        let target = td.path().join("target_dir");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&wrong_source).unwrap();
        symlink_dir(&wrong_source, &target).unwrap();

        let ctx = crate::exec::Context::builder().build();
        let action = EnsureSymlink::new(source.clone(), target.clone(), LinkType::Soft, true);
        action.execute(ctx).await.unwrap();

        assert!(target.is_symlink());
        assert_eq!(fs::read_link(&target).unwrap(), source);
    }

    #[tokio::test]
    async fn ensure_symlink_hard_link_fallback_cross_device() {
        init_test_tracing();
        // Use a file from /proc (different filesystem) as source to force hard_link to fail.
        // hard_link across filesystems returns EXDEV, triggering the fallback to soft link.
        let source = PathBuf::from("/proc/self/exe");
        if !source.exists() {
            // Skip on systems without /proc
            return;
        }
        let td = TempDir::new().unwrap();
        let target = td.path().join("target_link");

        let ctx = crate::exec::Context::builder().build();
        let action = EnsureSymlink::new(source.clone(), target.clone(), LinkType::Hard, false);
        action.execute(ctx).await.unwrap();

        // Should have fallen back to a soft link
        assert!(target.is_symlink(), "should fall back to soft link");
        assert_eq!(fs::read_link(&target).unwrap(), source);
    }

    #[test]
    fn render_symlink_target_invalid_template() {
        let overlays = HashMap::new();
        let result = render_symlink_target("{{ invalid.template", &overlays);
        assert!(result.is_err());
    }

    #[test]
    fn render_symlink_target_plain() {
        let overlays = HashMap::new();
        let result = render_symlink_target("/plain/path", &overlays).unwrap();
        assert_eq!(result, "/plain/path");
    }

    #[test]
    fn render_symlink_target_combined_env_and_overlay() {
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("OVER_TEST_USER", "testuser");
        }
        let mut overlays = HashMap::new();
        overlays.insert("apps/tool".to_string(), "/opt/tool".to_string());
        let result = render_symlink_target(
            "{{ env.OVER_TEST_USER }}/{{ overlays['apps/tool'] }}",
            &overlays,
        )
        .unwrap();
        assert_eq!(result, "testuser//opt/tool");
    }
}
