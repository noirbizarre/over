//! Repository-scoped encryption shared by Git filters and native secrets.

pub mod access;
pub mod filter;
pub mod format;
pub mod policy;
pub(crate) mod transaction;

use std::fs;
use std::io::Write;
use std::path::Path;

use anyhow::{Context as _, Result, ensure};

/// Publish complete bytes; no caller needs to create plaintext temporary files.
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("file has no parent")?;
    fs::create_dir_all(parent)?;
    ensure!(
        !fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()),
        "refusing a symlink destination"
    );
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(windows)]
    access::windows_acl(file.path(), true)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|error| error.error)?;
    #[cfg(unix)]
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}
