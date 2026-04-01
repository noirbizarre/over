use std::path::PathBuf;

use config::{Config, File};
use globset::GlobBuilder;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use anyhow::{Context as AnyhowContext, Result};

use super::overlay::Overlay;
use super::{Format, BASENAME, GLOB_PATTERN};
use crate::ui::style;

/// Manage all overlays
#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct Repository {
    /// Repository root directory
    pub root: PathBuf,
}

// impl std::fmt::Display for Repository {
//     fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
//         std::fmt::Display::fmt(&self.root.display(), f)
//     }
// }

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

        dirs.sort();

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
                Err(_e) => eprintln!(
                    "{} {} {}",
                    style::yellow("Warning:"),
                    style::white_b("skipping badly formatted overlay"),
                    style::cyan(&dir.display().to_string()),
                ),
            }
        }

        Ok(overlays)
    }

    /// Get a repository by its name/relative path
    pub fn get(&self, name: &str) -> Result<Overlay> {
        let root = self.root.join(name);
        let overlay = Overlay::new(self, &root)?;
        Ok(overlay)
    }

    /// Load the preferred overlay descriptor format from the root config.
    ///
    /// Reads `format` from the repository root `over.{toml,yaml,yml}`.
    /// Returns `None` when no root config exists or the field is absent.
    pub fn preferred_format(&self) -> Option<Format> {
        #[derive(Deserialize)]
        struct RootPrefs {
            format: Option<Format>,
        }
        let basename = self.root.join(BASENAME);
        let cfg = Config::builder()
            .add_source(File::with_name(basename.to_str()?).required(false))
            .build()
            .ok()?;
        let prefs: RootPrefs = cfg.try_deserialize().ok()?;
        prefs.format
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

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
}
