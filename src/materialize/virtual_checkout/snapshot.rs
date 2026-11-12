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
    // Sources must reject directory obstructions, not treat them as deletions.
    if git::source_entry(repo, path)?.is_none() {
        return Ok(None);
    }
    let head = git::base_tree(repo, None)?
        .get_path(path)
        .ok()
        .map(|entry| (entry.id(), entry.filemode()));
    let indexed = repo
        .index()?
        .get_path(path, 0)
        .map(|entry| (entry.id, entry.mode as i32));
    let root = repo.workdir().context("source has no working directory")?;
    let Some((content, mut mode)) = target_content(root, path)? else {
        return Ok(None);
    };
    // Only targets contain materializer-written raw blobs. A real worktree must
    // always be cleaned, or a non-idempotent filter can hide native Git edits.
    let scratch = crate::actions::git::diff::Scratch::new(repo)?;
    // A newly protected historical plaintext blob is still an unchanged source
    // baseline. This comparison-only exception must never be used to stage targets.
    let legacy_baseline = if crate::crypt::policy::protected(repo, path)? {
        let raw = git2::Oid::hash_object(git2::ObjectType::Blob, &content)?;
        head.filter(|(oid, _)| raw == *oid)
            .map(|(oid, _)| repo.find_blob(oid))
            .transpose()?
            .filter(|blob| !crate::crypt::format::recognized(blob.content()))
    } else {
        None
    };
    let oid = if let Some(blob) = legacy_baseline {
        blob.id()
    } else {
        scratch.target_blob(repo, path, &content)?
    };
    if !honors_filemode(repo)? {
        mode = head.or(indexed).map(|entry| entry.1).unwrap_or(0o100644);
    }
    Ok(Some((oid, mode)))
}

/// Compare in Git's clean-filter domain, while accepting unchanged raw blobs
/// written by our materializer. Re-cleaning those can corrupt non-idempotent
/// filters; mode normalization must still happen even on that fast path.
pub(crate) fn canonical_target_entry(
    repo: &git2::Repository,
    root: &Path,
    relative: &Path,
    logical: &Path,
    expected: &[Option<(git2::Oid, i32)>],
) -> Result<Option<(git2::Oid, i32)>> {
    let Some((content, mode)) = target_content(root, relative)? else {
        return Ok(None);
    };
    let scratch = crate::actions::git::diff::Scratch::new(repo)?;
    canonical_entry(repo, &scratch, logical, &content, mode, expected).map(Some)
}

pub(crate) fn canonical_entry(
    repo: &git2::Repository,
    scratch: &crate::actions::git::diff::Scratch,
    logical: &Path,
    content: &[u8],
    mut mode: i32,
    expected: &[Option<(git2::Oid, i32)>],
) -> Result<(git2::Oid, i32)> {
    let raw = git2::Oid::hash_object(git2::ObjectType::Blob, content)?;
    let protected = crate::crypt::policy::protected(repo, logical)?;
    let oid = if (!protected || crate::crypt::format::inspect(content).is_ok())
        && expected.iter().flatten().any(|entry| entry.0 == raw)
    {
        raw
    } else {
        scratch.target_blob(repo, logical, content)?
    };
    if protected {
        let _guard = crate::crypt::access::guard(repo, false)?;
        // A successful configured filter is not proof that its output is ciphertext.
        let blob = scratch.repo.find_blob(oid)?;
        let domain = crate::crypt::format::file_domain(blob.content())?;
        crate::crypt::access::local_key(repo)?.decrypt(blob.content(), domain)?;
    }
    if !honors_filemode(repo)? {
        mode = expected
            .iter()
            .flatten()
            .next()
            .map(|entry| entry.1)
            .unwrap_or(0o100644);
    }
    Ok((oid, mode))
}

pub(crate) fn honors_filemode(repo: &git2::Repository) -> Result<bool> {
    match repo.config()?.get_bool("core.filemode") {
        Ok(value) => Ok(cfg!(unix) && value),
        Err(error) if error.code() == git2::ErrorCode::NotFound => Ok(cfg!(unix)),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn target_content(
    root: &Path,
    relative: &Path,
) -> Result<Option<(zeroize::Zeroizing<Vec<u8>>, i32)>> {
    ensure!(
        !crate::secrets::is_encrypted(relative),
        "encrypted virtual-checkout paths are unsupported"
    );
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
                    crate::crypt::buffer::read_file(&path)?,
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn raw_canonical_blobs_skip_filters_but_still_ignore_executable_bits() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(directory.path()).unwrap();
        repo.config()
            .unwrap()
            .set_bool("core.filemode", false)
            .unwrap();
        repo.config()
            .unwrap()
            .set_str("filter.broken.clean", "exit 1")
            .unwrap();
        repo.config()
            .unwrap()
            .set_bool("filter.broken.required", true)
            .unwrap();
        std::fs::write(
            directory.path().join(".gitattributes"),
            "file filter=broken\n",
        )
        .unwrap();
        let target = directory.path().join("file");
        std::fs::write(&target, b"canonical bytes").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
        let expected = (repo.blob(b"canonical bytes").unwrap(), 0o100644);
        assert_eq!(
            canonical_target_entry(
                &repo,
                directory.path(),
                Path::new("file"),
                Path::new("file"),
                &[Some(expected)]
            )
            .unwrap(),
            Some(expected)
        );
        // A real edit must run the required filter and fail, not silently hash raw bytes.
        std::fs::write(&target, b"edited bytes").unwrap();
        assert!(
            canonical_target_entry(
                &repo,
                directory.path(),
                Path::new("file"),
                Path::new("file"),
                &[Some(expected)]
            )
            .is_err()
        );
    }
}
