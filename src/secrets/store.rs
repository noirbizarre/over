use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, bail, ensure};

use super::{CryptoService, SecretBytes, SecretOperation};

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
            if let super::layout::Layout::Entries(names) = super::layout::inspect(&self.repository)?
            {
                crate::crypt::access::local_key(&repo)?;
                self.entry_path(name, &names)?;
                ensure!(
                    replace || !names.iter().any(|other| other == name.as_str()),
                    "secret already exists; use --replace to replace it"
                );
                return Ok(());
            }
            if matches!(
                super::layout::inspect(&self.repository)?,
                super::layout::Layout::Legacy
            ) {
                crate::crypt::access::local_key(&repo)?;
                ensure!(
                    replace
                        || !self
                            .root
                            .join(format!("{}.age", name.as_str()))
                            .try_exists()?,
                    "secret already exists; use --replace to replace it"
                );
                return Ok(());
            }
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
            let _mutation = super::commit::mutation_guard(&repo)?;
            let guard = crate::crypt::access::guard(&repo, true)?;
            let names = match super::layout::inspect(&self.repository)? {
                super::layout::Layout::Missing => Some(Vec::new()),
                super::layout::Layout::Entries(names) => Some(names),
                _ => None,
            };
            if let Some(names) = names {
                let key = crate::crypt::access::local_key(&repo)?;
                let path = self.entry_path(name, &names)?;
                let existing = self.read_entry(&repo, name, &names)?;
                ensure!(
                    replace || existing.is_none(),
                    "secret already exists; use --replace to replace it"
                );
                let operation = if existing.is_some() {
                    SecretOperation::Replace
                } else {
                    SecretOperation::Add
                };
                let relative = path.strip_prefix(&self.repository)?.to_path_buf();
                if existing
                    .as_ref()
                    .is_some_and(|old| old.expose() == value.expose())
                {
                    let ciphertext = fs::read(&path)?;
                    let oid = git2::Oid::hash_object(git2::ObjectType::Blob, &ciphertext)?;
                    if repo
                        .head()
                        .ok()
                        .and_then(|head| head.peel_to_tree().ok())
                        .is_some_and(|tree| {
                            tree.get_path(&relative)
                                .is_ok_and(|entry| entry.id() == oid)
                        })
                        && repo
                            .index()?
                            .get_path(&relative, 0)
                            .is_some_and(|entry| entry.id == oid)
                    {
                        return Ok(());
                    }
                }
                let message = self.message(name, operation)?;
                let mutation = super::commit::Mutation::prepare(
                    &repo,
                    [(
                        relative,
                        Some(super::entry::encode(&key, name, value.expose())?),
                    )]
                    .into(),
                    message,
                    &[],
                )?;
                drop(guard);
                return mutation.commit(&repo);
            }
            let mut map = self.read_map_with_prompt(&repo, prompt)?;
            ensure!(
                replace || !map.0.contains_key(name.as_str()),
                "secret already exists; use --replace to replace it"
            );
            let operation = if map.0.contains_key(name.as_str()) {
                SecretOperation::Replace
            } else {
                SecretOperation::Add
            };
            if let Some(mut previous) = map
                .0
                .insert(name.as_str().to_owned(), value.expose().to_vec())
            {
                use zeroize::Zeroize;
                previous.zeroize();
            }
            let mutation = self.prepare_map(&repo, &map, name, operation)?;
            drop(guard);
            return mutation.commit(&repo);
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
            if let super::layout::Layout::Entries(names) = super::layout::inspect(&self.repository)?
            {
                return self
                    .read_entry(&repo, name, &names)?
                    .context("secret does not exist");
            }
            let mut map = self.read_map_with_prompt(&repo, prompt)?;
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

    pub(crate) fn get_one_shot(
        &self,
        name: &SecretName,
        acquire: impl FnOnce(
            &git2::Repository,
            &crate::crypt::access::Registry,
        ) -> Result<crate::crypt::format::RepositoryKey>,
    ) -> Result<SecretBytes> {
        if let Some(value) = super::snapshot::read(&self.repository, name, &self.crypto, acquire)? {
            return Ok(value);
        }
        // Only historical Age stores retain filesystem reads; native data must never fall back.
        self.validate_legacy_fallback()?;
        self.crypto.decrypt(
            &fs::read(self.path(name)?).context("could not read encrypted secret")?,
            true,
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
            if let super::layout::Layout::Entries(names) =
                super::layout::inspect(&self.repository).map_err(|_| ProviderError::StoreFailure)?
            {
                return self
                    .read_entry(&repo, name, &names)
                    .map_err(|_| ProviderError::DecryptionFailed)?
                    .ok_or(ProviderError::Missing);
            }
            let mut map = self
                .read_map_with_prompt(&repo, prompt)
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

    pub(crate) fn read_for_resolution_read_only(
        &self,
        name: &SecretName,
        prompt: bool,
    ) -> Result<SecretBytes, super::ProviderError> {
        if let Some(value) = super::snapshot::read_existing(&self.repository, name, &self.crypto)? {
            return Ok(value);
        }
        // Only historical Age stores may use filesystem/credential reads without a native key.
        self.validate_legacy_fallback()
            .map_err(|_| super::ProviderError::StoreFailure)?;
        self.read_for_resolution(name, prompt)
    }

    pub fn list(&self) -> Result<Vec<String>> {
        if let Some(repo) = self.unified()? {
            let _guard = crate::crypt::access::guard(&repo, false)?;
            crate::crypt::access::local_key(&repo)?;
            if let super::layout::Layout::Entries(names) = super::layout::inspect(&self.repository)?
            {
                return Ok(names);
            }
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
            let _mutation = super::commit::mutation_guard(&repo)?;
            let guard = crate::crypt::access::guard(&repo, true)?;
            if let super::layout::Layout::Entries(names) = super::layout::inspect(&self.repository)?
            {
                self.read_entry(&repo, name, &names)?
                    .context("secret does not exist")?;
                let path = PathBuf::from(super::layout::PATH).join(name.as_str());
                let message = self.message(name, SecretOperation::Remove)?;
                let mutation =
                    super::commit::Mutation::prepare(&repo, [(path, None)].into(), message, &[])?;
                drop(guard);
                return mutation.commit(&repo);
            }
            let mut map = self.read_map_with_prompt(&repo, true)?;
            use zeroize::Zeroize;
            map.0
                .remove(name.as_str())
                .context("secret does not exist")?
                .zeroize();
            let mutation = self.prepare_map(&repo, &map, name, SecretOperation::Remove)?;
            drop(guard);
            return mutation.commit(&repo);
        }
        let path = self.path(name)?;
        if !path.exists() {
            bail!("secret does not exist");
        }
        fs::remove_file(path).context("could not remove encrypted secret")
    }
}

impl SecretStore {
    fn entry_path(&self, name: &SecretName, names: &[String]) -> Result<PathBuf> {
        ensure!(
            !names
                .iter()
                .any(|other| other.eq_ignore_ascii_case(name.as_str()) && other != name.as_str()),
            "secret name spelling differs from its stored name"
        );
        Ok(self.root.join(name.as_str()))
    }

    fn read_entry(
        &self,
        repo: &git2::Repository,
        name: &SecretName,
        names: &[String],
    ) -> Result<Option<SecretBytes>> {
        let key = crate::crypt::access::local_key(repo)?;
        let path = self.entry_path(name, names)?;
        if !names.iter().any(|other| other == name.as_str()) {
            return Ok(None);
        }
        let metadata = fs::symlink_metadata(&path)?;
        ensure!(
            metadata.is_file()
                && !metadata.is_symlink()
                && metadata.len()
                    <= (crate::crypt::format::MAX_BYTES + crate::crypt::format::HEADER_LEN + 16)
                        as u64,
            "secret entry must be a bounded regular file"
        );
        Ok(Some(super::entry::decode(&key, name, &fs::read(path)?)?))
    }

    fn message(&self, name: &SecretName, operation: SecretOperation) -> Result<String> {
        crate::overlays::Repository::new(self.repository.clone())
            .secrets_config()?
            .commit_messages
            .render(operation, name)
    }
    fn unified(&self) -> Result<Option<git2::Repository>> {
        let repo = match git2::Repository::discover(&self.repository) {
            Ok(repo) => repo,
            Err(error) if error.code() == git2::ErrorCode::NotFound => {
                self.validate_legacy_fallback()?;
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        if !crate::crypt::access::configured(&repo)? {
            self.validate_legacy_fallback()?;
            return Ok(None);
        }
        ensure!(
            self.repository.canonicalize()? == crate::crypt::access::root(&repo)?.canonicalize()?,
            "select the source Git repository root with --home for repository-key secrets"
        );
        super::layout::inspect(&self.repository)?;
        Ok(Some(repo))
    }

    fn validate_legacy_fallback(&self) -> Result<()> {
        match super::layout::inspect(&self.repository)? {
            super::layout::Layout::Native(_) => {
                bail!("native secrets require repository encryption metadata")
            }
            super::layout::Layout::Entries(names) if !names.is_empty() => {
                bail!("native secrets require repository encryption metadata")
            }
            _ => Ok(()),
        }
    }

    fn read_map(&self, repo: &git2::Repository) -> Result<SecretMap> {
        self.read_map_with_prompt(repo, false)
    }

    fn read_map_with_prompt(&self, repo: &git2::Repository, prompt: bool) -> Result<SecretMap> {
        let key = crate::crypt::access::local_key(repo)?;
        let path = match super::layout::inspect(&self.repository)? {
            super::layout::Layout::Missing => return Ok(SecretMap::default()),
            super::layout::Layout::Native(path) => self.repository.join(path),
            super::layout::Layout::Entries(names) => {
                let mut map = SecretMap::default();
                for name in &names {
                    let name = SecretName::parse(name)?;
                    let entry = self
                        .read_entry(repo, &name, &names)?
                        .context("secret entry disappeared")?;
                    map.0
                        .insert(name.as_str().to_owned(), entry.expose().to_vec());
                }
                return Ok(map);
            }
            super::layout::Layout::Legacy => {
                let mut map = SecretMap::default();
                for (filename, ciphertext) in super::layout::legacy_files(&self.root)? {
                    let name = filename
                        .file_stem()
                        .and_then(|name| name.to_str())
                        .context("invalid legacy secret name")?;
                    let plaintext = self.crypto.decrypt(&ciphertext, prompt)?;
                    map.0.insert(name.to_owned(), plaintext.expose().to_vec());
                }
                return Ok(map);
            }
        };
        let bytes = fs::read(&path)?;
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

    fn prepare_map(
        &self,
        repo: &git2::Repository,
        map: &SecretMap,
        name: &SecretName,
        operation: SecretOperation,
    ) -> Result<super::commit::Mutation> {
        let config = crate::overlays::Repository::new(self.repository.clone()).secrets_config()?;
        let message = config.commit_messages.render(operation, name)?;
        self.prepare_map_with_message(repo, map, message)
    }

    fn prepare_map_with_message(
        &self,
        repo: &git2::Repository,
        map: &SecretMap,
        message: String,
    ) -> Result<super::commit::Mutation> {
        let key = crate::crypt::access::local_key(repo)?;
        // Aggregates allowed case variants that cannot coexist in the portable
        // per-file namespace. Refuse the entire destination before encrypting entries.
        super::layout::validate_names(map.0.keys().map(String::as_str))?;
        let mut updates = std::collections::BTreeMap::new();
        for (name, value) in &map.0 {
            let name = SecretName::parse(name)?;
            updates.insert(
                PathBuf::from(super::layout::PATH).join(name.as_str()),
                Some(super::entry::encode(&key, &name, value)?),
            );
        }
        let mut transitions = Vec::new();
        match super::layout::inspect(&self.repository)? {
            super::layout::Layout::Native(path) => {
                transitions.push(path.clone());
                updates.insert(path, None);
            }
            super::layout::Layout::Legacy => {
                transitions.push(PathBuf::from(super::layout::PATH));
                for filename in super::layout::legacy_files(&self.root)?.keys() {
                    updates.insert(PathBuf::from(super::layout::PATH).join(filename), None);
                }
            }
            _ => {}
        }
        super::commit::Mutation::prepare(repo, updates, message, &transitions)
    }
}

pub(crate) fn migrate_native(repo: &git2::Repository) -> Result<bool> {
    let root = crate::crypt::access::root(repo)?;
    let _mutation = super::commit::mutation_guard(repo)?;
    let guard = crate::crypt::access::guard(repo, true)?;
    match super::layout::inspect(root)? {
        super::layout::Layout::Missing => return Ok(false),
        super::layout::Layout::Entries(_) => {
            return Ok(false);
        }
        _ => {}
    }
    let store = SecretStore::new(root, Arc::new(CryptoService::default()))?;
    let map = store.read_map_with_prompt(repo, true)?;
    let mutation =
        store.prepare_map_with_message(repo, &map, "chore(secrets): migrated store".into())?;
    drop(guard);
    mutation.commit(repo)?;
    Ok(true)
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct SecretMap(
    #[serde(deserialize_with = "unique_values")]
    pub(super)  std::collections::BTreeMap<String, Vec<u8>>,
    pub(super) u32,
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
