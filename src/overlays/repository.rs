use std::path::PathBuf;

use config::{Config, File};
use globset::GlobBuilder;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use anyhow::{Context as AnyhowContext, Result};

use super::overlay::Overlay;
use super::{BASENAME, Format, GLOB_PATTERN};

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

    /// Returns a list of all overlays in the repository
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

        dirs.iter()
            .enumerate()
            .filter(|(idx, dir)| !matches!(dirs.get(idx + 1), Some(next) if next.starts_with(dir)))
            .map(|(_, dir)| {
                Overlay::new(self, dir)
                    .with_context(|| format!("failed to load overlay at {}", dir.display()))
            })
            .collect::<Result<Vec<_>>>()
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
