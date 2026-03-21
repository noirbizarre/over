use std::path::PathBuf;

use globset::GlobBuilder;
use serde::Serialize;
use walkdir::WalkDir;

use anyhow::{Context as AnyhowContext, Result};

use super::GLOB_PATTERN;
use super::overlay::Overlay;

/// Manage all overlays
#[derive(Debug, Default, Serialize, Clone)]
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
}
