use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, bail, ensure};

use super::{CryptoService, SecretBytes};

#[derive(Debug, Clone)]
pub struct SecretName(String);

impl SecretName {
    pub fn parse(name: &str) -> Result<Self> {
        let upper = name.to_ascii_uppercase();
        // Restrict the namespace before joining paths, including Windows device aliases.
        ensure!(
            !name.is_empty()
                && name.len() <= 128
                && name.as_bytes()[0].is_ascii_alphanumeric()
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                && !matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                && !(upper.len() == 4
                    && (upper.starts_with("COM") || upper.starts_with("LPT"))
                    && matches!(upper.as_bytes()[3], b'1'..=b'9')),
            "invalid secret name; use 1–128 ASCII letters, digits, underscores or hyphens, starting with a letter or digit"
        );
        Ok(Self(name.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug)]
pub struct SecretStore {
    root: PathBuf,
    crypto: Arc<CryptoService>,
}

impl SecretStore {
    pub fn new(repository: &Path, crypto: Arc<CryptoService>) -> Result<Self> {
        ensure!(
            repository.is_dir(),
            "Over repository directory does not exist"
        );
        Ok(Self {
            root: repository.join(".over/secrets"),
            crypto,
        })
    }

    fn validate(&self) -> Result<()> {
        for path in [self.root.parent().expect("store has a parent"), &self.root] {
            match fs::symlink_metadata(path) {
                Ok(metadata) => ensure!(
                    metadata.is_dir() && !metadata.file_type().is_symlink(),
                    "secret store must contain only real directories"
                ),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error).context("could not inspect secret store"),
            }
        }
        Ok(())
    }

    fn path(&self, name: &SecretName) -> Result<PathBuf> {
        self.validate()?;
        let path = self.root.join(format!("{}.age", name.0));
        match fs::symlink_metadata(&path) {
            Ok(metadata) => ensure!(
                metadata.is_file() && !metadata.file_type().is_symlink(),
                "secret entry must be a regular file"
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("could not inspect encrypted secret"),
        }
        if path.exists() {
            // Case-folding filesystems must not let a differently spelled name replace an entry.
            let exact = fs::read_dir(&self.root)
                .context("could not inspect secret name spelling")?
                .map(|entry| {
                    entry.map(|entry| {
                        entry.file_name() == path.file_name().expect("secret has a filename")
                    })
                })
                .collect::<std::io::Result<Vec<_>>>()?
                .into_iter()
                .any(|exact| exact);
            ensure!(
                exact,
                "secret name spelling differs from its stored name; use secret list"
            );
        }
        Ok(path)
    }

    pub fn check_add(&self, name: &SecretName, replace: bool) -> Result<()> {
        let path = self.path(name)?;
        ensure!(
            replace || !path.exists(),
            "secret already exists; use --replace to replace it"
        );
        Ok(())
    }

    pub fn add(
        &self,
        name: &SecretName,
        value: &SecretBytes,
        replace: bool,
        prompt: bool,
    ) -> Result<()> {
        self.check_add(name, replace)?;
        let ciphertext = self.crypto.encrypt(value.expose(), prompt)?;
        fs::create_dir_all(&self.root).context("could not create secret store")?;
        let path = self.path(name)?;
        let mut prepared = tempfile::NamedTempFile::new_in(&self.root)
            .context("could not prepare encrypted secret")?;
        prepared
            .write_all(&ciphertext)
            .context("could not write encrypted secret")?;
        prepared
            .as_file()
            .sync_all()
            .context("could not flush encrypted secret")?;
        if replace {
            prepared
                .persist(path)
                .map_err(|error| error.error)
                .context("could not replace encrypted secret")?;
        } else {
            prepared
                .persist_noclobber(path)
                .map_err(|error| error.error)
                .context("could not add encrypted secret; use --replace if it exists")?;
        }
        Ok(())
    }

    pub fn get(&self, name: &SecretName, prompt: bool) -> Result<SecretBytes> {
        self.crypto.decrypt(
            &fs::read(self.path(name)?).context("could not read encrypted secret")?,
            prompt,
        )
    }

    pub(crate) fn read_for_resolution(
        &self,
        name: &SecretName,
        prompt: bool,
    ) -> Result<SecretBytes, super::ProviderError> {
        use super::ProviderError;
        let path = self.path(name).map_err(|_| ProviderError::StoreFailure)?;
        // Only absence at this read is recoverable; invalid layout must stop fallback.
        let ciphertext = fs::read(path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                ProviderError::Missing
            } else {
                ProviderError::StoreFailure
            }
        })?;
        self.crypto
            .decrypt(&ciphertext, prompt)
            .map_err(|_| ProviderError::DecryptionFailed)
    }

    pub fn list(&self) -> Result<Vec<String>> {
        self.validate()?;
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error).context("could not list secrets"),
        };
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry.context("could not inspect secret entry")?;
            let filename = entry.file_name();
            let Some(name) = filename.to_str().and_then(|s| s.strip_suffix(".age")) else {
                continue;
            };
            let name = SecretName::parse(name)?;
            self.path(&name)?;
            names.push(name.0);
        }
        names.sort();
        Ok(names)
    }

    pub fn remove(&self, name: &SecretName) -> Result<()> {
        let path = self.path(name)?;
        if !path.exists() {
            bail!("secret does not exist");
        }
        fs::remove_file(path).context("could not remove encrypted secret")
    }
}
