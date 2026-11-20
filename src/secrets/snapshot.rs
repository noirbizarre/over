//! Capture committed secret inputs before asking for credentials, without filesystem locks.

use std::path::Path;

use anyhow::{Context as _, Result, ensure};

use crate::crypt::access::{self, Registry};
use crate::crypt::format::RepositoryKey;

use super::{CryptoService, SecretBytes, SecretName, entry, layout, store::SecretMap};

enum Encoding {
    Entry,
    Aggregate,
    Age,
}

struct Snapshot {
    registry: Registry,
    ciphertext: Vec<u8>,
    encoding: Encoding,
}

impl Snapshot {
    fn capture(repo: &git2::Repository, name: &SecretName) -> Result<Option<Self>> {
        let commit = match repo.head() {
            Ok(head) => head.peel_to_commit()?,
            Err(error)
                if matches!(
                    error.code(),
                    git2::ErrorCode::UnbornBranch | git2::ErrorCode::NotFound
                ) =>
            {
                ensure!(
                    !access::configured(repo)?,
                    "native secret reads require a committed HEAD snapshot; commit the repository first"
                );
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        // Never resolve HEAD again: a prompt may outlive a checkout or key rotation.
        let tree = commit.tree()?;
        let Some(registry) = access::registry_from_tree(repo, &tree)? else {
            ensure!(
                !access::configured(repo)?,
                "native secret reads require access metadata in the HEAD commit"
            );
            return Ok(None);
        };
        let administration = subtree(repo, &tree, ".over")?;
        let current = administration.get_name("secrets");
        let old = administration.get_name("secrets.yaml");
        ensure!(
            current.is_none() || old.is_none(),
            "conflicting secret stores in HEAD"
        );
        let selected = current.or(old).context("secret does not exist in HEAD")?;
        let aggregate = selected.kind() != Some(git2::ObjectType::Tree);
        let mut encoding = Encoding::Aggregate;
        let selected = if aggregate {
            selected
        } else {
            ensure!(
                selected.name()? == "secrets",
                "historical aggregate must be a regular blob"
            );
            let entries = repo.find_tree(selected.id())?;
            let mut names = Vec::new();
            for item in &entries {
                ensure!(
                    matches!(item.filemode(), 0o100644 | 0o100755),
                    "secret entries must be regular blobs"
                );
                names.push(item.name().context("non-UTF-8 secret filename")?.to_owned());
            }
            let legacy = names.iter().any(|name| name.ends_with(".age"));
            ensure!(
                !legacy || names.iter().all(|name| name.ends_with(".age")),
                "mixed Age and native secret files in HEAD"
            );
            layout::validate_names(
                names
                    .iter()
                    .map(|name| name.strip_suffix(".age").unwrap_or(name)),
            )?;
            encoding = if legacy {
                Encoding::Age
            } else {
                Encoding::Entry
            };
            let filename = if legacy {
                format!("{}.age", name.as_str())
            } else {
                name.as_str().to_owned()
            };
            entries
                .get_name(&filename)
                .context("secret does not exist in HEAD")?
                .to_owned()
        };
        let ciphertext = blob(repo, &selected)?;
        Ok(Some(Self {
            registry,
            ciphertext,
            encoding,
        }))
    }

    fn decrypt(
        &self,
        key: &RepositoryKey,
        name: &SecretName,
        crypto: &CryptoService,
    ) -> Result<SecretBytes> {
        ensure!(
            key.repository == self.registry.repository
                && key.generation == self.registry.generation,
            "key does not match the committed secret snapshot"
        );
        match self.encoding {
            Encoding::Entry => return entry::decode(key, name, &self.ciphertext),
            Encoding::Age => return crypto.decrypt(&self.ciphertext, true),
            Encoding::Aggregate => (),
        }
        let plaintext = key.decrypt(&self.ciphertext, 1)?;
        let mut map: SecretMap = serde_json::from_slice(&plaintext)
            .map_err(|_| anyhow::anyhow!("invalid native secret store"))?;
        ensure!(map.1 == 1, "unsupported native secret store version");
        // Historical aggregates can contain case variants; only migration needs a portable namespace.
        for name in map.0.keys() {
            SecretName::parse(name)?;
        }
        Ok(SecretBytes::new(
            map.0
                .remove(name.as_str())
                .context("secret does not exist in HEAD")?,
        ))
    }
}

fn subtree<'repo>(
    repo: &'repo git2::Repository,
    tree: &git2::Tree<'_>,
    name: &str,
) -> Result<git2::Tree<'repo>> {
    let entry = tree
        .get_name(name)
        .context("missing committed administration")?;
    ensure!(
        entry.filemode() == 0o040000,
        "secret administration must be a Git tree"
    );
    Ok(repo.find_tree(entry.id())?)
}

fn blob(repo: &git2::Repository, entry: &git2::TreeEntry<'_>) -> Result<Vec<u8>> {
    ensure!(
        matches!(entry.filemode(), 0o100644 | 0o100755),
        "secret must be a regular blob"
    );
    let (size, kind) = repo.odb()?.read_header(entry.id())?;
    ensure!(
        kind == git2::ObjectType::Blob
            && size <= crate::crypt::format::MAX_BYTES + crate::crypt::format::HEADER_LEN + 16,
        "secret entry must be a bounded regular blob"
    );
    Ok(repo.find_blob(entry.id())?.content().to_vec())
}

pub(super) fn read(
    root: &Path,
    name: &SecretName,
    crypto: &CryptoService,
    acquire: impl FnOnce(&git2::Repository, &Registry) -> Result<RepositoryKey>,
) -> Result<Option<SecretBytes>> {
    let repo = match git2::Repository::discover(root) {
        Ok(repo) => repo,
        Err(error) if error.code() == git2::ErrorCode::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let Some(snapshot) = Snapshot::capture(&repo, name)? else {
        return Ok(None);
    };
    ensure!(
        root.canonicalize()? == access::root(&repo)?.canonicalize()?,
        "select the source Git repository root with --home for repository-key secrets"
    );
    let key = match access::snapshot_key(&repo, &snapshot.registry)? {
        Some(key) => key,
        None => acquire(&repo, &snapshot.registry)?,
    };
    Ok(Some(snapshot.decrypt(&key, name, crypto)?))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{CryptoService, Registry, RepositoryKey, SecretName, access, entry, read};

    fn publish(repo: &git2::Repository, key: &RepositoryKey, value: &[u8]) {
        let registry = Registry {
            version: 1,
            repository: key.repository,
            generation: key.generation,
            users: BTreeMap::from([(
                "alice".into(),
                BTreeMap::from([(
                    "recovery".into(),
                    access::AccessKey {
                        recipient: None,
                        envelope: vec![1],
                    },
                )]),
            )]),
        };
        let metadata = repo.blob(&serde_json::to_vec(&registry).unwrap()).unwrap();
        let ciphertext = repo
            .blob(&entry::encode(key, &SecretName::parse("token").unwrap(), value).unwrap())
            .unwrap();
        let build = |entries: &[(&str, git2::Oid, i32)]| {
            let mut builder = repo.treebuilder(None).unwrap();
            for (name, oid, mode) in entries {
                builder.insert(*name, *oid, *mode).unwrap();
            }
            builder.write().unwrap()
        };
        let crypt = build(&[("access.json", metadata, 0o100644)]);
        let secrets = build(&[("token", ciphertext, 0o100644)]);
        let administration = build(&[("crypt", crypt, 0o040000), ("secrets", secrets, 0o040000)]);
        let tree = repo
            .find_tree(build(&[(".over", administration, 0o040000)]))
            .unwrap();
        let signature = git2::Signature::now("Snapshot test", "snapshot@example.invalid").unwrap();
        let parent = repo.head().ok().map(|head| head.peel_to_commit().unwrap());
        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            "snapshot",
            &tree,
            &parent.iter().collect::<Vec<_>>(),
        )
        .unwrap();
    }

    #[test]
    fn moving_head_during_acquisition_does_not_mix_key_generations_or_values() {
        let temp = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(temp.path()).unwrap();
        let original = RepositoryKey::generate().unwrap();
        publish(&repo, &original, b"original\0\xff");
        let name = SecretName::parse("token").unwrap();
        let value = read(
            temp.path(),
            &name,
            &CryptoService::default(),
            |repo, registry| {
                assert_eq!(registry.generation, original.generation);
                publish(repo, &RepositoryKey::generate().unwrap(), b"replacement");
                Ok(original)
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(value.expose(), b"original\0\xff");
        assert!(!repo.commondir().join("over-crypt").exists());
    }

    #[test]
    fn an_acquired_key_must_match_the_captured_registry() {
        let temp = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(temp.path()).unwrap();
        publish(&repo, &RepositoryKey::generate().unwrap(), b"private");
        let error = read(
            temp.path(),
            &SecretName::parse("token").unwrap(),
            &CryptoService::default(),
            |_, _| RepositoryKey::generate(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("key does not match"));
        assert!(!repo.commondir().join("over-crypt").exists());
    }

    #[rstest::rstest]
    #[case(true)]
    #[case(false)]
    fn a_matching_local_key_skips_acquisition_and_another_generation_is_preserved(
        #[case] matching: bool,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(temp.path()).unwrap();
        let original = RepositoryKey::generate().unwrap();
        publish(&repo, &original, b"");
        if matching {
            access::install(&repo, &original).unwrap();
        } else {
            access::install(&repo, &RepositoryKey::generate().unwrap()).unwrap();
        }
        let path = repo.commondir().join("over-crypt/key");
        let before = std::fs::read(&path).unwrap();
        let mut acquired = false;
        let value = read(
            temp.path(),
            &SecretName::parse("token").unwrap(),
            &CryptoService::default(),
            |_, _| {
                acquired = true;
                Ok(original)
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(acquired, !matching);
        assert!(value.expose().is_empty());
        assert_eq!(std::fs::read(path).unwrap(), before);
    }

    #[test]
    fn a_missing_committed_secret_never_attempts_credential_acquisition() {
        let temp = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(temp.path()).unwrap();
        publish(&repo, &RepositoryKey::generate().unwrap(), b"private");
        let result = read(
            temp.path(),
            &SecretName::parse("missing").unwrap(),
            &CryptoService::default(),
            |_, _| panic!("must not prompt for a missing secret"),
        );
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("does not exist in HEAD")
        );
        assert!(!repo.commondir().join("over-crypt").exists());
    }
}
