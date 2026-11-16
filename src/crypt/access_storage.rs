//! Versioned public access files, independent of checkout and private key state.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, ensure};
use serde::{Deserialize, Serialize};

use super::access::{self, AccessKey, Registry};

pub(crate) const LEGACY: &str = ".over/crypt/access.json";
pub(crate) const HEADER: &str = ".over/crypt/repository.json";
pub(crate) const USERS: &str = ".over/crypt/users";
pub(crate) const ATTRIBUTES: &str = ".over/.gitattributes";
pub(crate) type Files = BTreeMap<PathBuf, Vec<u8>>;
pub(crate) type Delta = BTreeMap<PathBuf, Option<Vec<u8>>>;

#[derive(Deserialize, Serialize)]
struct Header {
    version: u32,
    repository: [u8; 16],
    generation: [u8; 16],
}

#[derive(Deserialize, Serialize)]
struct KeyRecord {
    #[serde(flatten)]
    header: Header,
    #[serde(flatten)]
    key: AccessKey,
}

pub(crate) fn is_path(path: &Path) -> bool {
    path == Path::new(LEGACY) || path == Path::new(HEADER) || path.starts_with(USERS)
}

pub(crate) fn portable_name(name: &str) -> Result<()> {
    access::validate_name(name)?;
    let upper = name.to_ascii_uppercase();
    ensure!(
        !matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            && !(upper.len() == 4
                && (upper.starts_with("COM") || upper.starts_with("LPT"))
                && matches!(upper.as_bytes()[3], b'1'..=b'9')),
        "user/key names must be portable filesystem names"
    );
    Ok(())
}

fn bounded(files: &Files) -> Result<()> {
    ensure!(
        files.values().map(Vec::len).sum::<usize>() <= access::METADATA_LIMIT
            && files.len() <= access::METADATA_LIMIT / 8,
        "repository access metadata exceeds the supported size limit"
    );
    Ok(())
}

fn check_version(bytes: &[u8], expected: u64) -> Result<()> {
    #[derive(Deserialize)]
    struct Version {
        version: u64,
    }
    let version: Version =
        serde_json::from_slice(bytes).context("invalid access metadata version")?;
    if version.version != expected {
        return Err(access::UnsupportedRegistryVersion(version.version).into());
    }
    Ok(())
}

pub(crate) fn parse(files: &Files) -> Result<Option<Registry>> {
    bounded(files)?;
    if let Some(bytes) = files.get(Path::new(LEGACY)) {
        ensure!(
            files.len() == 1,
            "mixed legacy and split access metadata; reconcile before continuing"
        );
        return access::parse_registry(bytes).map(Some);
    }
    if files.is_empty() {
        return Ok(None);
    }
    let bytes = files
        .get(Path::new(HEADER))
        .context("split access metadata is missing repository.json")?;
    check_version(bytes, 2)?;
    let header: Header =
        serde_json::from_slice(bytes).context("invalid repository encryption header")?;
    let mut registry = Registry {
        version: 1,
        repository: header.repository,
        generation: header.generation,
        users: BTreeMap::new(),
    };
    let mut markers = BTreeSet::new();
    let mut spellings = BTreeMap::new();
    for (path, bytes) in files
        .iter()
        .filter(|(path, _)| path.as_path() != Path::new(HEADER))
    {
        let relative = path
            .strip_prefix(USERS)
            .context("unexpected access metadata path")?;
        let parts = relative
            .iter()
            .map(|part| part.to_str().context("non-UTF-8 access name"))
            .collect::<Result<Vec<_>>>()?;
        ensure!(
            parts.len() == 2,
            "access methods must use users/<user>/<key>"
        );
        let (user, name) = (parts[0], parts[1]);
        portable_name(user)?;
        if let Some(existing) = spellings.insert(user.to_ascii_lowercase(), user) {
            ensure!(existing == user, "case-colliding user names");
        }
        let keys = registry.users.entry(user.into()).or_default();
        if name == ".user" {
            check_version(bytes, 2)?;
            markers.insert(user);
        } else {
            portable_name(name)?;
            ensure!(
                !keys.keys().any(|other| other.eq_ignore_ascii_case(name)),
                "case-colliding key names"
            );
            check_version(bytes, 2)?;
            let record: KeyRecord =
                serde_json::from_slice(bytes).context("invalid access key record")?;
            ensure!(
                record.header.repository == header.repository
                    && record.header.generation == header.generation,
                "access key record has a foreign repository or stale generation"
            );
            ensure!(
                record.key.envelope.len() <= super::age::ENVELOPE_LIMIT,
                "repository key envelope exceeds the supported size limit"
            );
            keys.insert(name.into(), record.key);
        }
    }
    ensure!(
        registry
            .users
            .keys()
            .all(|user| markers.contains(user.as_str())),
        "user metadata is missing its .user marker"
    );
    let mut recipients = BTreeSet::new();
    for method in registry.users.values().flat_map(|keys| keys.values()) {
        if let Some(recipient) = &method.recipient {
            ensure!(
                recipients.insert(recipient),
                "duplicate Age recipient in merged access metadata"
            );
        }
    }
    Ok(Some(registry))
}

pub(crate) fn encode(registry: &Registry) -> Result<Files> {
    let header = || Header {
        version: 2,
        repository: registry.repository,
        generation: registry.generation,
    };
    let mut files = Files::from([(PathBuf::from(HEADER), serde_json::to_vec_pretty(&header())?)]);
    for (user, keys) in &registry.users {
        portable_name(user)?;
        let directory = PathBuf::from(USERS).join(user);
        files.insert(directory.join(".user"), b"{\"version\":2}\n".to_vec());
        for (name, key) in keys {
            portable_name(name)?;
            files.insert(
                directory.join(name),
                serde_json::to_vec_pretty(&KeyRecord {
                    header: header(),
                    key: key.clone(),
                })?,
            );
        }
    }
    // Use the same reader for writers so new files cannot publish unreadable grants.
    parse(&files)?;
    Ok(files)
}

pub(crate) fn delta(before: &Files, after: &Files) -> Delta {
    before
        .keys()
        .chain(after.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|path| before.get(*path) != after.get(*path))
        .map(|path| (path.clone(), after.get(path).cloned()))
        .collect()
}

pub(crate) fn worktree(repo: &git2::Repository) -> Result<Files> {
    let root = access::root(repo)?;
    let mut files = Files::new();
    for directory in [".over", ".over/crypt"] {
        if let Ok(metadata) = fs::symlink_metadata(root.join(directory)) {
            ensure!(
                metadata.is_dir() && !metadata.is_symlink(),
                "access metadata ancestors must be real directories"
            );
        }
    }
    for path in [LEGACY, HEADER] {
        read_file(root, Path::new(path), &mut files)?;
    }
    let users = root.join(USERS);
    match fs::symlink_metadata(&users) {
        Ok(metadata) => {
            ensure!(
                metadata.is_dir() && !metadata.is_symlink(),
                "users must be a real directory"
            );
            for user in fs::read_dir(users)? {
                let user = user?;
                let metadata = fs::symlink_metadata(user.path())?;
                ensure!(
                    metadata.is_dir() && !metadata.is_symlink(),
                    "user metadata must be a real directory"
                );
                let name = user.file_name();
                portable_name(name.to_str().context("non-UTF-8 user name")?)?;
                for key in fs::read_dir(user.path())? {
                    let key = key?;
                    read_file(root, key.path().strip_prefix(root)?, &mut files)?;
                }
            }
            ensure!(
                !files.keys().any(|path| path.starts_with(USERS))
                    || files.contains_key(Path::new(HEADER)),
                "users directory is missing repository.json"
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(files)
}

fn read_file(root: &Path, relative: &Path, files: &mut Files) -> Result<()> {
    let path = root.join(relative);
    match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            ensure!(
                metadata.is_file() && !metadata.is_symlink(),
                "access metadata must be regular files"
            );
            let remaining =
                access::METADATA_LIMIT.saturating_sub(files.values().map(Vec::len).sum());
            ensure!(
                metadata.len() <= remaining as u64,
                "repository access metadata exceeds the supported size limit"
            );
            // Bound the actual read too: a concurrent append must not bypass metadata checks.
            use std::io::Read as _;
            let mut bytes = Vec::new();
            fs::File::open(path)?
                .take(remaining as u64 + 1)
                .read_to_end(&mut bytes)?;
            files.insert(relative.to_path_buf(), bytes);
            bounded(files)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

pub(crate) fn tree(repo: &git2::Repository, tree: &git2::Tree<'_>) -> Result<Files> {
    fn walk(
        repo: &git2::Repository,
        tree: &git2::Tree<'_>,
        prefix: &Path,
        files: &mut Files,
    ) -> Result<()> {
        for entry in tree.iter() {
            let path = prefix.join(entry.name().context("non-UTF-8 access metadata path")?);
            if entry.kind() == Some(git2::ObjectType::Tree) {
                ensure!(
                    path != Path::new(LEGACY) && path != Path::new(HEADER),
                    "access header must be a regular Git blob"
                );
                if path.starts_with(USERS) || Path::new(USERS).starts_with(&path) {
                    ensure!(
                        path.components().count() <= 4,
                        "unexpected access metadata nesting"
                    );
                    walk(repo, &repo.find_tree(entry.id())?, &path, files)?;
                }
            } else if is_path(&path) {
                ensure!(
                    matches!(entry.filemode(), 0o100644 | 0o100755),
                    "access metadata must be regular Git blobs"
                );
                let remaining =
                    access::METADATA_LIMIT.saturating_sub(files.values().map(Vec::len).sum());
                ensure!(
                    repo.odb()?.read_header(entry.id())?.0 <= remaining,
                    "repository access metadata exceeds the supported size limit"
                );
                files.insert(path, repo.find_blob(entry.id())?.content().to_vec());
                bounded(files)?;
            }
        }
        Ok(())
    }
    let mut files = Files::new();
    walk(repo, tree, Path::new(""), &mut files)?;
    Ok(files)
}

pub(crate) fn index(repo: &git2::Repository, index: &git2::Index) -> Result<Files> {
    let mut files = Files::new();
    for entry in index.iter() {
        let path = PathBuf::from(std::str::from_utf8(&entry.path).context("non-UTF-8 index path")?);
        if is_path(&path) {
            ensure!(
                entry.flags & 0x3000 == 0 && matches!(entry.mode, 0o100644 | 0o100755),
                "conflicted or nonregular access metadata in index"
            );
            let remaining =
                access::METADATA_LIMIT.saturating_sub(files.values().map(Vec::len).sum());
            ensure!(
                repo.odb()?.read_header(entry.id)?.0 <= remaining,
                "repository access metadata exceeds the supported size limit"
            );
            files.insert(path, repo.find_blob(entry.id)?.content().to_vec());
            bounded(&files)?;
        }
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> Registry {
        let key = super::super::format::RepositoryKey::generate().unwrap();
        let recipient = age::x25519::Identity::generate().to_public().to_string();
        Registry {
            version: 1,
            repository: key.repository,
            generation: key.generation,
            users: [
                (
                    "alice".into(),
                    [(
                        "laptop".into(),
                        access::envelope(&key, Some(&recipient), None).unwrap(),
                    )]
                    .into(),
                ),
                ("empty".into(), BTreeMap::new()),
            ]
            .into(),
        }
    }

    #[test]
    fn split_records_round_trip_without_changing_envelopes_or_empty_users() {
        let registry = registry();
        let files = encode(&registry).unwrap();
        assert_eq!(parse(&files).unwrap(), Some(registry));
        assert!(files.contains_key(Path::new(".over/crypt/users/empty/.user")));
    }

    #[test]
    fn adding_a_key_changes_no_header_or_existing_files() {
        let mut registry = registry();
        let before = encode(&registry).unwrap();
        let mut key = registry.users["alice"]["laptop"].clone();
        key.recipient = None;
        registry
            .users
            .get_mut("alice")
            .unwrap()
            .insert("recovery".into(), key);
        let changes = delta(&before, &encode(&registry).unwrap());
        assert_eq!(
            changes.keys().collect::<Vec<_>>(),
            vec![Path::new(".over/crypt/users/alice/recovery")]
        );
    }

    #[test]
    fn stale_and_mixed_access_records_are_rejected() {
        let registry = registry();
        let mut files = encode(&registry).unwrap();
        files.insert(
            PathBuf::from(LEGACY),
            serde_json::to_vec(&registry).unwrap(),
        );
        assert!(parse(&files).is_err());
        files.remove(Path::new(LEGACY));
        let path = Path::new(".over/crypt/users/alice/laptop");
        let mut record: KeyRecord = serde_json::from_slice(&files[path]).unwrap();
        record.header.generation[0] ^= 1;
        files.insert(path.to_path_buf(), serde_json::to_vec(&record).unwrap());
        assert!(parse(&files).is_err());
    }

    #[test]
    fn bootstrap_loads_all_split_files_from_one_committed_tree() {
        let directory = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(directory.path()).unwrap();
        let registry = registry();
        let files = encode(&registry).unwrap();
        let mut staged = repo.index().unwrap();
        for (path, bytes) in &files {
            super::super::policy::stage_blob(&mut staged, &repo, path, bytes).unwrap();
        }
        let tree = staged.write_tree().unwrap();
        assert_eq!(
            super::super::bootstrap::registry(&repo, tree).unwrap(),
            registry
        );
        assert_eq!(index(&repo, &staged).unwrap(), files);
        assert!(worktree(&repo).unwrap().is_empty());
        assert_eq!(
            super::super::detect::inspect_tree(&repo, &repo.find_tree(tree).unwrap())
                .unwrap()
                .metadata,
            super::super::detect::Metadata::Supported
        );
    }
}
