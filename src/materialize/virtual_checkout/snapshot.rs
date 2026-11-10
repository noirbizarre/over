//! Read canonical checkout inputs without following links outside ownership.

use std::path::Path;

use anyhow::{Context as _, Result, ensure};

use super::git;

/// Source drift is a Git-content comparison too: clean filters and ignored
/// executable bits must not make an unchanged source look independently edited.
pub(crate) fn canonical_source_entry(
    repo: &git2::Repository,
    path: &Path,
) -> Result<Option<(git2::Oid, i32)>> {
    let raw = git::source_entry(repo, path)?;
    let head = git::base_tree(repo, None)?
        .get_path(path)
        .ok()
        .map(|entry| (entry.id(), entry.filemode()));
    let indexed = repo
        .index()?
        .get_path(path, 0)
        .map(|entry| (entry.id, entry.mode as i32));
    if git::entries_match(raw, head) || git::entries_match(raw, indexed) {
        return Ok(raw);
    }
    let root = repo.workdir().context("source has no working directory")?;
    let Some((content, mut mode)) = target_content(root, path)? else {
        return Ok(None);
    };
    let scratch = crate::actions::git::diff::Scratch::new(repo)?;
    let oid = scratch.target_blob(repo, path, &content)?;
    if !honors_filemode(repo)? {
        mode = head.or(indexed).map(|entry| entry.1).unwrap_or(0o100644);
    }
    Ok(Some((oid, mode)))
}

pub(crate) fn honors_filemode(repo: &git2::Repository) -> Result<bool> {
    match repo.config()?.get_bool("core.filemode") {
        Ok(value) => Ok(cfg!(unix) && value),
        Err(error) if error.code() == git2::ErrorCode::NotFound => Ok(cfg!(unix)),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn target_content(root: &Path, relative: &Path) -> Result<Option<(Vec<u8>, i32)>> {
    let file = root.join(relative);
    let mut path = root.to_path_buf();
    // Inspect the root too: a replaced checkout root must not redirect reads.
    for component in std::iter::once(None).chain(relative.components().map(Some)) {
        if let Some(component) = component {
            path.push(component);
        }
        let metadata = match path.symlink_metadata() {
            Ok(metadata) => metadata,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(None);
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to inspect '{}'", path.display()));
            }
        };
        ensure!(
            !metadata.is_symlink(),
            "virtual checkout path '{}' is a symlink",
            path.display()
        );
        ensure!(
            metadata.is_dir() || metadata.is_file(),
            "unsupported virtual checkout target entry '{}'",
            path.display()
        );
        if path == file {
            return if metadata.is_file() {
                Ok(Some((
                    std::fs::read(&path)?,
                    i32::from(git::on_disk_filemode(&path)?),
                )))
            } else {
                Ok(None)
            };
        }
        if metadata.is_file() {
            return Ok(None);
        }
    }
    Ok(None)
}
