//! Distinguish historical directories from the extensionless encrypted aggregate.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, ensure};

pub(crate) const PATH: &str = ".over/secrets";
pub(crate) const OLD_PATH: &str = ".over/secrets.yaml";

pub(super) enum Layout {
    Missing,
    Native(PathBuf),
    Legacy,
}

pub(super) fn inspect(root: &Path) -> Result<Layout> {
    if let Ok(metadata) = fs::symlink_metadata(root.join(".over")) {
        ensure!(
            metadata.is_dir() && !metadata.is_symlink(),
            "secret administration must be a real directory"
        );
    }
    let current = metadata(&root.join(PATH))?;
    let old = metadata(&root.join(OLD_PATH))?;
    ensure!(
        current.is_none() || old.is_none(),
        "conflicting secret stores; preserve and reconcile them before continuing"
    );
    if let Some(metadata) = current {
        ensure!(!metadata.is_symlink(), "secret store must not be a symlink");
        if metadata.is_dir() {
            legacy_files(&root.join(PATH))?;
            return Ok(Layout::Legacy);
        }
        ensure!(metadata.is_file(), "secret store must be a regular file");
        return Ok(Layout::Native(PathBuf::from(PATH)));
    }
    if let Some(metadata) = old {
        ensure!(
            metadata.is_file() && !metadata.is_symlink(),
            "secret store must be a regular file"
        );
        return Ok(Layout::Native(PathBuf::from(OLD_PATH)));
    }
    Ok(Layout::Missing)
}

fn metadata(path: &Path) -> Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn legacy_files(directory: &Path) -> Result<BTreeMap<PathBuf, Vec<u8>>> {
    let mut files = BTreeMap::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        ensure!(
            metadata.is_file() && !metadata.is_symlink(),
            "legacy secrets must be regular files"
        );
        let filename = entry.file_name();
        let name = filename
            .to_str()
            .and_then(|name| name.strip_suffix(".age"))
            .context("legacy secret store contains unrecognized files")?;
        super::SecretName::parse(name)?;
        files.insert(PathBuf::from(filename), fs::read(entry.path())?);
    }
    Ok(files)
}
