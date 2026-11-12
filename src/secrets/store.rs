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
    repository: PathBuf,
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
            repository: repository.to_path_buf(),
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
        if let Some(repo) = self.unified()? {
            let _guard = crate::crypt::access::guard(&repo, false)?;
            ensure!(
                replace || !self.read_map(&repo)?.0.contains_key(name.as_str()),
                "secret already exists; use --replace to replace it"
            );
            return Ok(());
        }
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
        if let Some(repo) = self.unified()? {
            let _guard = crate::crypt::access::guard(&repo, true)?;
            let mut map = self.read_map(&repo)?;
            ensure!(
                replace || !map.0.contains_key(name.as_str()),
                "secret already exists; use --replace to replace it"
            );
            map.0
                .insert(name.as_str().to_owned(), value.expose().to_vec());
            return self.write_map(&repo, &map);
        }
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
        if let Some(repo) = self.unified()? {
            let _guard = crate::crypt::access::guard(&repo, false)?;
            let mut map = self.read_map(&repo)?;
            return Ok(SecretBytes::new(
                map.0
                    .remove(name.as_str())
                    .context("secret does not exist")?,
            ));
        }
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
        if let Some(repo) = self.unified().map_err(|_| ProviderError::StoreFailure)? {
            let _guard = crate::crypt::access::guard(&repo, false)
                .map_err(|_| ProviderError::StoreFailure)?;
            if crate::crypt::access::local_key_missing(&repo)
                .map_err(|_| ProviderError::StoreFailure)?
            {
                return Err(ProviderError::SessionUnavailable);
            }
            let mut map = self
                .read_map(&repo)
                .map_err(|_| ProviderError::DecryptionFailed)?;
            return map
                .0
                .remove(name.as_str())
                .map(SecretBytes::new)
                .ok_or(ProviderError::Missing);
        }
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
        if let Some(repo) = self.unified()? {
            let _guard = crate::crypt::access::guard(&repo, false)?;
            return Ok(self.read_map(&repo)?.0.keys().cloned().collect());
        }
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
        if let Some(repo) = self.unified()? {
            let _guard = crate::crypt::access::guard(&repo, true)?;
            let mut map = self.read_map(&repo)?;
            use zeroize::Zeroize;
            map.0
                .remove(name.as_str())
                .context("secret does not exist")?
                .zeroize();
            return self.write_map(&repo, &map);
        }
        let path = self.path(name)?;
        if !path.exists() {
            bail!("secret does not exist");
        }
        fs::remove_file(path).context("could not remove encrypted secret")
    }
}

impl SecretStore {
    fn unified(&self) -> Result<Option<git2::Repository>> {
        let repo = match git2::Repository::discover(&self.repository) {
            Ok(repo) => repo,
            Err(error) if error.code() == git2::ErrorCode::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if !crate::crypt::access::configured(&repo)? {
            return Ok(None);
        }
        ensure!(
            self.repository.canonicalize()? == crate::crypt::access::root(&repo)?.canonicalize()?,
            "select the source Git repository root with --home for repository-key secrets"
        );
        ensure!(
            !self.root.try_exists()?,
            "legacy secret store exists; run over crypt migrate before using repository secrets"
        );
        Ok(Some(repo))
    }

    fn read_map(&self, repo: &git2::Repository) -> Result<SecretMap> {
        let key = crate::crypt::access::local_key(repo)?;
        let path = self.repository.join(".over/secrets.yaml");
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(SecretMap::default());
            }
            Err(error) => return Err(error.into()),
        };
        ensure!(
            !fs::symlink_metadata(&path)?.file_type().is_symlink(),
            "secret store must not be a symlink"
        );
        let plaintext = key.decrypt(&bytes, 1)?;
        let map: SecretMap = serde_json::from_slice(&plaintext)
            .map_err(|_| anyhow::anyhow!("invalid native secret store"))?;
        ensure!(map.1 == 1, "unsupported native secret store version");
        for name in map.0.keys() {
            SecretName::parse(name)?;
        }
        Ok(map)
    }

    fn write_map(&self, repo: &git2::Repository, map: &SecretMap) -> Result<()> {
        let key = crate::crypt::access::local_key(repo)?;
        let plaintext = crate::crypt::buffer::to_json(map)?;
        crate::crypt::atomic_write(
            &self.repository.join(".over/secrets.yaml"),
            &key.encrypt(&plaintext, 1)?,
        )
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SecretMap(
    #[serde(deserialize_with = "unique_values")] std::collections::BTreeMap<String, Vec<u8>>,
    u32,
);

fn unique_values<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<std::collections::BTreeMap<String, Vec<u8>>, D::Error> {
    struct UniqueValues;
    impl<'de> serde::de::Visitor<'de> for UniqueValues {
        type Value = std::collections::BTreeMap<String, Vec<u8>>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a map of unique native secret names")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut access: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let mut map = SecretMap::default();
            while let Some((name, mut value)) = access.next_entry::<String, Vec<u8>>()? {
                if map.0.contains_key(&name) {
                    use zeroize::Zeroize;
                    value.zeroize();
                    return Err(serde::de::Error::custom("duplicate native secret name"));
                }
                map.0.insert(name, value);
            }
            Ok(std::mem::take(&mut map.0))
        }
    }
    deserializer.deserialize_map(UniqueValues)
}

impl Default for SecretMap {
    fn default() -> Self {
        Self(Default::default(), 1)
    }
}

impl Drop for SecretMap {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        for value in self.0.values_mut() {
            value.zeroize();
        }
    }
}
