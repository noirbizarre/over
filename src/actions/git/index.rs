//! Publish isolated index edits without losing concurrent staged changes.

use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

pub(crate) fn read_index(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).context("failed to snapshot source index"),
    }
}

pub(crate) struct IndexLock {
    path: PathBuf,
    file: File,
}

impl IndexLock {
    pub(crate) fn acquire(index: &Path) -> Result<Self> {
        let mut path = index.as_os_str().to_os_string();
        path.push(".lock");
        let path = PathBuf::from(path);
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .with_context(|| format!("cannot lock index at '{}'", path.display()))?;
        Ok(Self { path, file })
    }

    pub(crate) fn publish(&mut self, index: &Path, bytes: &[u8]) -> Result<()> {
        // A newly created lock must not broaden a private/shared index's access.
        match fs::metadata(index) {
            Ok(metadata) => self.file.set_permissions(metadata.permissions())?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        self.file.write_all(bytes)?;
        self.file.sync_all()?;
        fs::rename(&self.path, index).context("failed to publish selected index changes")?;
        self.path.clear();
        Ok(())
    }

    /// Keep cooperating Git writers excluded while a larger journaled operation finishes.
    pub(crate) fn replace_while_locked(&mut self, index: &Path, bytes: &[u8]) -> Result<()> {
        let parent = index.parent().context("index has no parent")?;
        let mut prepared = tempfile::NamedTempFile::new_in(parent)?;
        match fs::metadata(index) {
            Ok(metadata) => prepared.as_file().set_permissions(metadata.permissions())?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
        prepared.write_all(bytes)?;
        prepared.as_file().sync_all()?;
        prepared.persist(index).map_err(|error| error.error)?;
        #[cfg(unix)]
        File::open(parent)?.sync_all()?;
        Ok(())
    }
}

impl Drop for IndexLock {
    fn drop(&mut self) {
        // Publication clears ownership so cleanup cannot remove another writer's lock.
        if !self.path.as_os_str().is_empty() {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_publication_cleans_up_only_its_owned_lock() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("directory");
        fs::create_dir(&destination).unwrap();
        let lock_path;
        {
            let mut lock = IndexLock::acquire(&destination).unwrap();
            lock_path = lock.path.clone();
            assert!(
                lock.publish(&destination, b"cannot replace a directory")
                    .is_err()
            );
        }
        assert!(!lock_path.exists());
        assert!(destination.is_dir());
    }

    #[test]
    fn retained_publication_keeps_other_index_writers_out_until_completion() {
        let directory = tempfile::tempdir().unwrap();
        let index = directory.path().join("index");
        fs::write(&index, b"old").unwrap();
        let mut lock = IndexLock::acquire(&index).unwrap();
        lock.replace_while_locked(&index, b"prepared").unwrap();
        assert_eq!(fs::read(&index).unwrap(), b"prepared");
        assert!(IndexLock::acquire(&index).is_err());
        drop(lock);
        assert!(IndexLock::acquire(&index).is_ok());
    }
}
