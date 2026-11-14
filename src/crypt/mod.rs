//! Repository-scoped encryption shared by Git filters and native secrets.

pub mod access;
pub(crate) mod buffer;
pub mod filter;
pub mod format;
pub(crate) mod ingest;
pub mod policy;
pub mod protection;
pub(crate) mod transaction;
#[cfg(windows)]
pub(crate) mod windows;

use std::fs;
use std::io::Write;
use std::path::Path;

use anyhow::{Context as _, Result, ensure};

/// Publish private administrative bytes with synchronized, ownership-preserving replacement.
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    atomic_write_named(path, bytes, ".tmp")
}

pub(crate) fn atomic_write_named(path: &Path, bytes: &[u8], prefix: &str) -> Result<()> {
    let parent = path.parent().context("file has no parent")?;
    fs::create_dir_all(parent)?;
    ensure!(
        !fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()),
        "refusing a symlink destination"
    );
    let mut file = tempfile::Builder::new()
        .prefix(prefix)
        .tempfile_in(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, chown};
        let owner = fs::metadata(parent)?;
        if file.as_file().metadata()?.uid() != owner.uid() {
            // Privileged maintenance must not publish a key the administrative owner cannot use.
            chown(file.path(), Some(owner.uid()), Some(owner.gid()))?;
        }
    }
    #[cfg(windows)]
    access::windows_acl(file.path(), true)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|error| error.error)?;
    #[cfg(unix)]
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, chown};

    #[test]
    fn private_publication_keeps_the_administrative_directory_owner() {
        let directory = tempfile::tempdir().unwrap();
        let parent = directory.path().join("private");
        fs::create_dir(&parent).unwrap();
        if fs::metadata(&parent).unwrap().uid() == 0 {
            // Root-only runners also exercise maintenance of another user's administrative state.
            chown(&parent, Some(65534), Some(65534)).unwrap();
        }
        let file = parent.join("key");
        atomic_write(&file, b"private fixture").unwrap();
        let metadata = fs::metadata(file).unwrap();
        assert_eq!(metadata.uid(), fs::metadata(&parent).unwrap().uid());
        assert_eq!(metadata.mode() & 0o077, 0);
        assert_eq!(metadata.nlink(), 1);
    }
}
