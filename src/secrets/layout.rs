//! Distinguish historical directories from the extensionless encrypted aggregate.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, ensure};

pub(crate) const PATH: &str = ".over/secrets";
pub(crate) const OLD_PATH: &str = ".over/secrets.yaml";

pub(super) enum Layout {
    Missing,
    Native(PathBuf),
    Entries(Vec<String>),
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
            let mut names = Vec::new();
            let mut legacy = false;
            let mut native = false;
            for entry in fs::read_dir(root.join(PATH))? {
                let entry = entry?;
                let metadata = fs::symlink_metadata(entry.path())?;
                ensure!(
                    metadata.is_file() && !metadata.is_symlink(),
                    "secret entries must be real regular files"
                );
                let filename = entry.file_name();
                let filename = filename.to_str().context("non-UTF-8 secret filename")?;
                let name = if let Some(name) = filename.strip_suffix(".age") {
                    legacy = true;
                    name
                } else {
                    native = true;
                    filename
                };
                names.push(name.to_owned());
            }
            ensure!(
                !(legacy && native),
                "mixed Age and native secret files; reconcile them before continuing"
            );
            validate_names(names.iter().map(String::as_str))?;
            names.sort();
            return if legacy {
                Ok(Layout::Legacy)
            } else {
                Ok(Layout::Entries(names))
            };
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

pub(super) fn validate_names<'a>(names: impl IntoIterator<Item = &'a str>) -> Result<()> {
    let mut spellings = BTreeSet::new();
    for name in names {
        super::SecretName::parse(name)?;
        ensure!(
            spellings.insert(name.to_ascii_lowercase()),
            "case-colliding secret names"
        );
    }
    Ok(())
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

pub(crate) fn native_paths(root: &Path) -> Result<Vec<PathBuf>> {
    Ok(match inspect(root)? {
        Layout::Native(path) => vec![path],
        Layout::Entries(names) => names
            .into_iter()
            .map(|name| PathBuf::from(PATH).join(name))
            .collect(),
        _ => Vec::new(),
    })
}
