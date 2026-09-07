use std::path::PathBuf;

use config::{Config, File};
use globset::GlobBuilder;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use anyhow::{Context as _, Result};

use super::discovery::{OverlayDeclaration, resolve_declared_dirs};
use super::overlay::Overlay;
use super::{BASENAME, Format, GLOB_PATTERN};
use crate::exec;

/// Manage all overlays
#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct Repository {
    /// Repository root directory
    pub root: PathBuf,
}

impl std::fmt::Display for Repository {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.root.display(), f)
    }
}

impl Repository {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Returns a list of all overlays in the repository.
    /// Badly formatted overlay files are skipped with a warning.
    pub fn overlays(&self) -> Result<Vec<Overlay>> {
        let glob = GlobBuilder::new(&GLOB_PATTERN)
            .literal_separator(true)
            .build()?
            .compile_matcher();

        let mut dirs: Vec<PathBuf> = WalkDir::new(&self.root)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| {
                e.path()
                    .strip_prefix(&self.root)
                    .ok()
                    .is_some_and(|rel| glob.is_match(rel))
            })
            .filter_map(|e| e.path().parent().map(|p| p.to_path_buf()))
            .collect();

        // Union with root-declared `overlays:` directories (#113/#127) —
        // dedup by resolved path since a directory can be found by both
        // mechanisms (e.g. a root-declared dir that also has its own
        // descriptor).
        dirs.extend(self.declared_overlay_dirs());
        dirs.sort();
        dirs.dedup();

        let mut overlays = Vec::new();
        for (idx, dir) in dirs.iter().enumerate() {
            // Skip if this dir is a parent of another dir (more specific overlay wins)
            if matches!(dirs.get(idx + 1), Some(next) if next.starts_with(dir)) {
                continue;
            }
            match Overlay::new(self, dir)
                .with_context(|| format!("failed to load overlay at {}", dir.display()))
            {
                Ok(overlay) => overlays.push(overlay),
                Err(e) => {
                    tracing::warn!("skipping badly formatted overlay {}: {}", dir.display(), e,);
                }
            }
        }

        Ok(overlays)
    }

    /// Get an overlay by its name/relative path
    pub fn get(&self, name: &str) -> Result<Overlay> {
        let root = self.root.join(name);
        if !root.exists() {
            anyhow::bail!(
                "overlay '{}' not found (no such directory: {})",
                name,
                root.display()
            );
        }
        let overlay = Overlay::new(self, &root)
            .with_context(|| format!("failed to load overlay '{}'", name))?;
        Ok(overlay)
    }

    /// Load the preferred overlay descriptor format from the root config.
    ///
    /// Reads `format` from the repository root `over.{toml,yaml,yml}`.
    /// Returns `None` when no root config exists or the field is absent.
    pub fn preferred_format(&self) -> Option<Format> {
        self.root_config().format
    }

    /// Directories declared via the repository root's `overlays:` key
    /// (#113/#127), resolved to concrete filesystem paths. Public so
    /// `over lint`'s own discovery (`lint::discover_overlay_dirs`) can
    /// union the same set instead of re-deriving it.
    pub fn declared_overlay_dirs(&self) -> Vec<PathBuf> {
        let declarations = self.root_config().overlays.unwrap_or_default();
        resolve_declared_dirs(&self.root, &declarations)
    }

    /// Resolve the repository root's configured `default_overlay`, if any
    /// (#113/#128), rendering it as a template against `ctx` — the same
    /// `exec::Context` used for `target` (see `Overlay::resolve_target`),
    /// so `{{ machine.hostname }}` etc. can select a machine-specific
    /// overlay — then validating the rendered name actually exists.
    ///
    /// `Ok(None)` means no `default_overlay` is configured: silent,
    /// matching `preferred_format()`'s absent-means-opt-out contract.
    /// `Err` means it *is* configured but broken (bad template, or names
    /// an overlay that doesn't exist): a misconfigured default must never
    /// silently fall through to another selection method (never silence
    /// errors, AGENTS.md).
    pub fn default_overlay(&self, ctx: &exec::Context) -> Result<Option<Overlay>> {
        let Some(raw) = self.root_config().default_overlay else {
            return Ok(None);
        };
        let name = exec::templates::render_string(&raw, ctx)
            .with_context(|| format!("failed to render default_overlay template '{raw}'"))?;
        let overlay = self
            .get(&name)
            .with_context(|| format!("configured default_overlay '{name}' not found"))?;
        Ok(Some(overlay))
    }

    /// One-shot, non-cascading read of the repository root's own
    /// descriptor. Unlike `Overlay::new`'s ancestor-chain cascade
    /// (ADR-002), this reads *only* `self.root`'s file: `format` and
    /// `overlays` are root-scoped fields with no meaning cascaded
    /// per-overlay (ADR-018).
    fn root_config(&self) -> RootConfig {
        let basename = self.root.join(BASENAME);
        let Some(path) = basename.to_str() else {
            return RootConfig::default();
        };
        Config::builder()
            .add_source(File::with_name(path).required(false))
            .build()
            .ok()
            .and_then(|cfg| cfg.try_deserialize().ok())
            .unwrap_or_default()
    }
}

/// The repository root's own descriptor fields, read once and not
/// cascaded to overlays (ADR-018) — see [`Repository::root_config`].
#[derive(Debug, Deserialize, Default)]
struct RootConfig {
    format: Option<Format>,
    overlays: Option<Vec<OverlayDeclaration>>,
    default_overlay: Option<String>,
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn display_shows_the_root_path() {
        #[cfg(unix)]
        let root = PathBuf::from("/some/dotfiles");
        #[cfg(windows)]
        let root = PathBuf::from("C:\\some\\dotfiles");

        let repo = Repository::new(root.clone());
        assert_eq!(repo.to_string(), root.display().to_string());
    }

    #[test]
    fn preferred_format_returns_toml_from_toml_config() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("over.toml"), b"format = \"toml\"").unwrap();
        let repo = Repository::new(tmp.path().to_path_buf());
        assert_eq!(repo.preferred_format(), Some(Format::Toml));
    }

    #[test]
    fn preferred_format_returns_yaml_from_toml_config() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("over.toml"), b"format = \"yaml\"").unwrap();
        let repo = Repository::new(tmp.path().to_path_buf());
        assert_eq!(repo.preferred_format(), Some(Format::Yaml));
    }

    #[test]
    fn preferred_format_returns_yaml_from_yaml_config() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("over.yaml"), b"format: yaml").unwrap();
        let repo = Repository::new(tmp.path().to_path_buf());
        assert_eq!(repo.preferred_format(), Some(Format::Yaml));
    }

    #[test]
    fn preferred_format_returns_none_when_no_config() {
        let tmp = TempDir::new().unwrap();
        let repo = Repository::new(tmp.path().to_path_buf());
        assert_eq!(repo.preferred_format(), None);
    }

    #[test]
    fn preferred_format_returns_none_when_field_absent() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("over.toml"), b"target = \"~\"").unwrap();
        let repo = Repository::new(tmp.path().to_path_buf());
        assert_eq!(repo.preferred_format(), None);
    }

    #[test]
    fn preferred_format_returns_none_for_invalid_value() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("over.toml"), b"format = \"invalid\"").unwrap();
        let repo = Repository::new(tmp.path().to_path_buf());
        assert_eq!(repo.preferred_format(), None);
    }

    #[test]
    fn preferred_format_ignores_extra_fields() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("over.toml"),
            b"format = \"yaml\"\ntarget = \"~\"\ndescription = \"root\"",
        )
        .unwrap();
        let repo = Repository::new(tmp.path().to_path_buf());
        assert_eq!(repo.preferred_format(), Some(Format::Yaml));
    }

    #[test]
    fn overlays_skips_badly_formatted_file() {
        let tmp = TempDir::new().unwrap();
        // Valid overlay
        let valid = tmp.path().join("valid");
        fs::create_dir_all(&valid).unwrap();
        fs::write(valid.join("over.toml"), "target = \"~\"").unwrap();
        // Badly formatted overlay (invalid TOML)
        let bad = tmp.path().join("bad");
        fs::create_dir_all(&bad).unwrap();
        fs::write(bad.join("over.toml"), "{{{{invalid toml}}}}").unwrap();

        let repo = Repository::new(tmp.path().to_path_buf());
        let overlays = repo.overlays().unwrap();
        assert_eq!(overlays.len(), 1);
        assert_eq!(overlays[0].name, "valid");
    }

    #[test]
    fn overlays_skips_parent_when_child_exists() {
        let tmp = TempDir::new().unwrap();
        // Parent overlay
        let parent = tmp.path().join("parent");
        fs::create_dir_all(&parent).unwrap();
        fs::write(parent.join("over.toml"), "target = \"~\"").unwrap();
        // Child overlay (more specific, should win)
        let child = parent.join("child");
        fs::create_dir_all(&child).unwrap();
        fs::write(child.join("over.toml"), "target = \"~\"").unwrap();

        let repo = Repository::new(tmp.path().to_path_buf());
        let overlays = repo.overlays().unwrap();
        assert_eq!(overlays.len(), 1);
        assert_eq!(overlays[0].name, "parent/child");
    }

    #[test]
    fn overlays_discovers_root_declared_directory_without_local_descriptor() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("over.toml"),
            "target = \"~\"\n\n[[overlays]]\npath = \"hosts/*\"",
        )
        .unwrap();
        // No `over.*` at all in `hosts/laptop` — must still be discovered
        // and resolve `target` from the repository root config.
        fs::create_dir_all(tmp.path().join("hosts/laptop")).unwrap();

        let repo = Repository::new(tmp.path().to_path_buf());
        let overlays = repo.overlays().unwrap();
        assert_eq!(overlays.len(), 1);
        assert_eq!(overlays[0].name, "hosts/laptop");
        assert_eq!(overlays[0].target, "~");
    }

    #[test]
    fn overlays_merges_declared_and_descriptor_discovery_without_duplicates() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("over.toml"),
            "target = \"~\"\n\n[[overlays]]\npath = \"shared\"",
        )
        .unwrap();
        // Also has its own local descriptor — matched by both mechanisms.
        let shared = tmp.path().join("shared");
        fs::create_dir_all(&shared).unwrap();
        fs::write(shared.join("over.toml"), "target = \"~/shared\"").unwrap();

        let repo = Repository::new(tmp.path().to_path_buf());
        let overlays = repo.overlays().unwrap();
        assert_eq!(overlays.len(), 1, "should not produce a duplicate entry");
        assert_eq!(overlays[0].name, "shared");
        assert_eq!(overlays[0].target, "~/shared");
    }

    #[test]
    fn default_overlay_returns_none_when_absent() {
        let tmp = TempDir::new().unwrap();
        let repo = Repository::new(tmp.path().to_path_buf());
        let ctx = exec::Context::builder().repository(repo.clone()).build();
        assert!(repo.default_overlay(&ctx).unwrap().is_none());
    }

    #[test]
    fn default_overlay_resolves_static_name() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("over.toml"),
            "default_overlay = \"myoverlay\"",
        )
        .unwrap();
        let ov = tmp.path().join("myoverlay");
        fs::create_dir_all(&ov).unwrap();
        fs::write(ov.join("over.toml"), "target = \"~\"").unwrap();

        let repo = Repository::new(tmp.path().to_path_buf());
        let ctx = exec::Context::builder().repository(repo.clone()).build();
        let overlay = repo.default_overlay(&ctx).unwrap().unwrap();
        assert_eq!(overlay.name, "myoverlay");
    }

    #[test]
    fn default_overlay_renders_machine_template() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("over.toml"),
            "default_overlay = \"hosts/{{ machine.hostname }}\"",
        )
        .unwrap();
        let ov = tmp.path().join("hosts/laptop");
        fs::create_dir_all(&ov).unwrap();
        fs::write(ov.join("over.toml"), "target = \"~\"").unwrap();

        let repo = Repository::new(tmp.path().to_path_buf());
        let machine = exec::MachineInfo {
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            hostname: "laptop".to_string(),
            username: "tester".to_string(),
            distro: None,
            distro_id: None,
        };
        let ctx = exec::Context::builder()
            .repository(repo.clone())
            .machine(machine)
            .build();
        let overlay = repo.default_overlay(&ctx).unwrap().unwrap();
        assert_eq!(overlay.name, "hosts/laptop");
    }

    #[test]
    fn default_overlay_errors_when_named_overlay_missing() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("over.toml"),
            "default_overlay = \"does-not-exist\"",
        )
        .unwrap();
        let repo = Repository::new(tmp.path().to_path_buf());
        let ctx = exec::Context::builder().repository(repo.clone()).build();
        assert!(repo.default_overlay(&ctx).is_err());
    }

    #[test]
    fn default_overlay_errors_on_invalid_template_syntax() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("over.toml"),
            "default_overlay = \"{{ invalid\"",
        )
        .unwrap();
        let repo = Repository::new(tmp.path().to_path_buf());
        let ctx = exec::Context::builder().repository(repo.clone()).build();
        assert!(repo.default_overlay(&ctx).is_err());
    }

    #[test]
    fn overlays_root_declaration_glob_matching_nothing_is_silently_empty() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("over.toml"),
            "target = \"~\"\n\n[[overlays]]\npath = \"nonexistent/*\"",
        )
        .unwrap();
        // A legitimate sibling overlay so this test isn't tripped up by
        // the pre-existing (unrelated to #127) edge case where the
        // repository root itself, when it has its own `over.toml` and no
        // descendant overlay at all, is discovered as an overlay with an
        // empty name — the root is always a path-prefix of every other
        // discovered dir and gets skipped by the "more specific overlay
        // wins" rule below as soon as one exists.
        let other = tmp.path().join("other");
        fs::create_dir_all(&other).unwrap();
        fs::write(other.join("over.toml"), "target = \"~\"").unwrap();

        let repo = Repository::new(tmp.path().to_path_buf());
        let overlays = repo.overlays().unwrap();
        assert_eq!(overlays.len(), 1);
        assert_eq!(overlays[0].name, "other");
    }
}
