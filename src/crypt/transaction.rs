//! Filesystem rollback stores sensitive bytes only in an authenticated encrypted journal.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, ensure};
use zeroize::Zeroizing;

use super::{access, policy};

/// Filesystem identity authorizes reversing an ingestion rename, not cryptographic access.
pub(crate) fn file_identity(path: &Path) -> Result<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::metadata(path)?;
        Ok(format!("{}:{}", metadata.dev(), metadata.ino()))
    }
    #[cfg(windows)]
    {
        let information = winapi_util::file::information(std::fs::File::open(path)?)?;
        Ok(format!(
            "{}:{}",
            information.volume_serial_number(),
            information.file_index()
        ))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        anyhow::bail!("filesystem identities are unsupported on this platform")
    }
}

#[cfg(unix)]
pub(crate) fn desired_mode(existing: Option<&std::fs::Metadata>, git_mode: Option<u32>) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    let mut mode = existing
        .map(|metadata| metadata.permissions().mode() & 0o7777)
        .unwrap_or(0o600);
    if let Some(git_mode) = git_mode
        && (mode & 0o111 != 0) != (git_mode == 0o100755)
    {
        mode &= !0o111;
        if git_mode == 0o100755 {
            mode |= (mode & 0o444) >> 2;
        }
    }
    mode
}

/// Representation changes preserve the destination's access policy, unlike private key writes.
pub(crate) fn worktree_write(path: &Path, bytes: &[u8], git_mode: Option<u32>) -> Result<()> {
    let existing = match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            ensure!(
                metadata.is_file() && !metadata.is_symlink(),
                "worktree publication requires a regular file"
            );
            ensure!(
                !metadata.permissions().readonly(),
                "worktree destination is read-only: {}",
                path.display()
            );
            Some(metadata)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let parent = path.parent().context("worktree path has no parent")?;
    std::fs::create_dir_all(parent)?;
    let directory = tempfile::tempdir_in(parent)?;
    #[cfg(windows)]
    access::windows_acl(directory.path(), true)?;
    let prepared = directory.path().join("new");
    #[cfg(target_os = "macos")]
    if let Some(metadata) = &existing {
        crate::materialize::virtual_checkout::git::clone_macos_source(
            path,
            directory.path(),
            metadata,
        )?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut output = options.open(&prepared)?;
    output.write_all(bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = desired_mode(existing.as_ref(), git_mode);
        let desired = std::fs::Permissions::from_mode(mode);
        #[cfg(target_os = "linux")]
        if let Some(metadata) = &existing {
            crate::actions::fs::clone_linux_metadata(path, &prepared, metadata, desired)?;
        } else {
            output.set_permissions(desired)?;
        }
        #[cfg(not(target_os = "linux"))]
        output.set_permissions(desired)?;
    }
    #[cfg(windows)]
    if existing.is_some() {
        // Copy the established file's ACL, rather than broadening or narrowing it on refresh.
        let result = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", "$ErrorActionPreference='Stop'; Set-Acl -LiteralPath $env:OVER_CRYPT_DESTINATION -AclObject (Get-Acl -LiteralPath $env:OVER_CRYPT_SOURCE)"])
            .env("OVER_CRYPT_SOURCE", path).env("OVER_CRYPT_DESTINATION", &prepared).output()?;
        ensure!(
            result.status.success(),
            "could not preserve worktree access control"
        );
    }
    #[cfg(not(unix))]
    let _ = git_mode;
    output.sync_all()?;
    drop(output);
    std::fs::rename(&prepared, path)?;
    #[cfg(unix)]
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

type FileUpdate = Option<(Zeroizing<Vec<u8>>, u32)>;

pub(crate) struct FileTransaction {
    repository: PathBuf,
    updates: BTreeMap<PathBuf, FileUpdate>,
    originals: BTreeMap<PathBuf, Option<Zeroizing<Vec<u8>>>>,
    ancestry: BTreeMap<PathBuf, crate::materialize::disposal::PathGuard>,
    guard: Option<File>,
}

impl FileTransaction {
    pub fn new(repo: &git2::Repository) -> Self {
        Self {
            repository: repo.path().to_path_buf(),
            updates: BTreeMap::new(),
            originals: BTreeMap::new(),
            ancestry: BTreeMap::new(),
            guard: None,
        }
    }

    pub fn prepare(&mut self, file: PathBuf, update: Option<(Vec<u8>, u32)>) -> Result<()> {
        let update = update.map(|(bytes, mode)| (Zeroizing::new(bytes), mode));
        ensure!(
            self.guard.is_none(),
            "cannot prepare after applying a crypt transaction"
        );
        if let Ok(metadata) = std::fs::symlink_metadata(&file) {
            ensure!(
                metadata.is_file() && !metadata.is_symlink() && !metadata.permissions().readonly(),
                "transaction destination must be a writable regular file"
            );
        }
        let original = super::buffer::read_optional(&file)?;
        self.originals.insert(file.clone(), original);
        self.ancestry.insert(
            file.clone(),
            crate::materialize::disposal::PathGuard::capture(&file)?,
        );
        self.updates.insert(file, update);
        Ok(())
    }

    pub fn apply(&mut self) -> Result<()> {
        let repo = git2::Repository::open(&self.repository)?;
        let guard = access::guard(&repo, true)?;
        let key = access::local_key(&repo)?;
        for (path, before) in &self.originals {
            self.ancestry[path].verify()?;
            let current = super::buffer::read_optional(path)?;
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
        #[cfg(unix)]
        let mut modes: BTreeMap<PathBuf, u32> = BTreeMap::new();
        #[cfg(not(unix))]
        let modes: BTreeMap<PathBuf, u32> = BTreeMap::new();
        #[cfg(unix)]
        for (path, update) in &self.updates {
            self.ancestry[path].verify()?;
            if let Some((_, mode)) = update {
                modes.insert(
                    path.clone(),
                    desired_mode(std::fs::metadata(path).ok().as_ref(), Some(*mode)),
                );
            }
        }
        policy::begin_commit_with_modes(
            &repo,
            &key,
            &self.updates.keys().cloned().collect::<Vec<_>>(),
            None,
            expected,
            modes,
        )?;
        self.guard = Some(guard);
        for (path, update) in &self.updates {
            self.ancestry[path].verify()?;
            let current = super::buffer::read_optional(path)?;
            ensure!(
                self.originals.get(path) == Some(&current),
                "destination changed before encrypted publication"
            );
            if let Some((bytes, mode)) = update {
                worktree_write(path, bytes, Some(*mode))?;
            } else if path.try_exists()? {
                std::fs::remove_file(path)?;
            }
        }
        Ok(())
    }

    pub fn rollback(&mut self) -> Result<()> {
        if self.guard.take().is_some() {
            policy::recover_source(&git2::Repository::open(&self.repository)?)?;
        }
        self.updates.clear();
        self.originals.clear();
        self.ancestry.clear();
        Ok(())
    }

    pub fn cleanup(&self) -> Result<()> {
        if self.guard.is_some() {
            policy::finish(&git2::Repository::open(&self.repository)?)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[rstest::rstest]
    #[case(false)]
    #[case(true)]
    fn interrupted_creation_cleans_only_owned_empty_scaffolds(#[case] intervening_file: bool) {
        let directory = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(directory.path()).unwrap();
        let recipient = age::x25519::Identity::generate().to_public().to_string();
        access::add_key(&repo, "owner", "software", Some(&recipient), None, true).unwrap();
        let target = directory.path().join("new-parent/file");
        let mut transaction = FileTransaction::new(&repo);
        transaction
            .prepare(
                target.clone(),
                Some((b"prepared plaintext".to_vec(), 0o100644)),
            )
            .unwrap();
        transaction.apply().unwrap();
        drop(transaction);
        if intervening_file {
            std::fs::write(
                directory.path().join("new-parent/user-file"),
                b"preserve me",
            )
            .unwrap();
        }
        policy::recover(&repo).unwrap();
        assert!(!target.exists());
        if intervening_file {
            assert_eq!(
                std::fs::read(directory.path().join("new-parent/user-file")).unwrap(),
                b"preserve me"
            );
        } else {
            assert!(!directory.path().join("new-parent").exists());
        }
    }
}
