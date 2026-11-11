//! Filesystem rollback stores sensitive bytes only in an authenticated encrypted journal.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::PathBuf;

use anyhow::{Result, ensure};
use zeroize::Zeroizing;

use super::{access, atomic_write, policy};

type FileUpdate = Option<(Zeroizing<Vec<u8>>, u32)>;

pub(crate) struct FileTransaction {
    repository: PathBuf,
    updates: BTreeMap<PathBuf, FileUpdate>,
    originals: BTreeMap<PathBuf, Option<Zeroizing<Vec<u8>>>>,
    guard: Option<File>,
}

impl FileTransaction {
    pub fn new(repo: &git2::Repository) -> Self {
        Self {
            repository: repo.path().to_path_buf(),
            updates: BTreeMap::new(),
            originals: BTreeMap::new(),
            guard: None,
        }
    }

    pub fn prepare(&mut self, file: PathBuf, update: Option<(Vec<u8>, u32)>) -> Result<()> {
        ensure!(
            self.guard.is_none(),
            "cannot prepare after applying a crypt transaction"
        );
        let original = match std::fs::read(&file) {
            Ok(bytes) => Some(Zeroizing::new(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        self.originals.insert(file.clone(), original);
        self.updates.insert(
            file,
            update.map(|(bytes, mode)| (Zeroizing::new(bytes), mode)),
        );
        Ok(())
    }

    pub fn apply(&mut self) -> Result<()> {
        let repo = git2::Repository::open(&self.repository)?;
        let guard = access::guard(&repo, true)?;
        let key = access::local_key(&repo)?;
        for (path, before) in &self.originals {
            let current = match std::fs::read(path) {
                Ok(bytes) => Some(Zeroizing::new(bytes)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(error.into()),
            };
            ensure!(
                current == *before,
                "{} changed while preparing encrypted publication; retry",
                path.display()
            );
        }
        let expected = self
            .updates
            .iter()
            .map(|(path, update)| {
                (
                    path.clone(),
                    update.as_ref().map(|(bytes, _)| bytes.to_vec()),
                )
            })
            .collect();
        policy::begin_commit(
            &repo,
            &key,
            &self.updates.keys().cloned().collect::<Vec<_>>(),
            None,
            expected,
        )?;
        self.guard = Some(guard);
        for (path, update) in &self.updates {
            if let Some((bytes, mode)) = update {
                atomic_write(path, bytes)?;
                #[cfg(unix)]
                if *mode == 0o100755 {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
                }
                #[cfg(not(unix))]
                let _ = mode;
            } else if path.try_exists()? {
                std::fs::remove_file(path)?;
            }
        }
        Ok(())
    }

    pub fn rollback(&mut self) -> Result<()> {
        if self.guard.take().is_some() {
            policy::recover(&git2::Repository::open(&self.repository)?)?;
        }
        self.updates.clear();
        self.originals.clear();
        Ok(())
    }

    pub fn cleanup(&self) -> Result<()> {
        if self.guard.is_some() {
            policy::finish(&git2::Repository::open(&self.repository)?)?;
        }
        Ok(())
    }
}
