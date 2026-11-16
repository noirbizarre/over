//! Versioned public access files, independent of checkout and private key state.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, ensure};
use serde::{Deserialize, Serialize};

use super::access::{self, AccessKey, Registry};

pub(crate) const LEGACY: &str = ".over/crypt/access.json";
pub(crate) const HEADER: &str = ".over/crypt.json";
pub(crate) const USERS: &str = ".over/users";
pub(crate) const MARKER: &str = "user.json";
pub(crate) const PREVIOUS_HEADER: &str = ".over/crypt/repository.json";
pub(crate) const PREVIOUS_USERS: &str = ".over/crypt/users";
pub(crate) const ATTRIBUTES: &str = ".over/.gitattributes";
pub(crate) const BOOTSTRAP_RULES: &str = "crypt/** -filter !diff -text\ncrypt.json -filter !diff -text\nusers/** -filter !diff -text\n.gitattributes -filter !diff\n";
pub(crate) const ROOT_BOOTSTRAP_RULES: &str = ".gitattributes -filter !diff\n.over/crypt/** -filter !diff -text\n.over/crypt.json -filter !diff -text\n.over/users/** -filter !diff -text\n";
pub(crate) type Files = BTreeMap<PathBuf, Vec<u8>>;
pub(crate) type Delta = BTreeMap<PathBuf, Option<Vec<u8>>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SplitLayout {
    pub header: &'static str,
    pub users: &'static str,
    pub marker: &'static str,
}

pub(crate) const FLAT: SplitLayout = SplitLayout {
    header: HEADER,
    users: USERS,
    marker: MARKER,
};
pub(crate) const NESTED: SplitLayout = SplitLayout {
    header: PREVIOUS_HEADER,
    users: PREVIOUS_USERS,
    marker: ".user",
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Layout {
    Aggregate,
    Split(SplitLayout),
}

pub(crate) fn layout(files: &Files) -> Result<Option<Layout>> {
    if files.contains_key(Path::new(LEGACY)) {
        ensure!(
            files.len() == 1,
            "mixed legacy and split access metadata; reconcile before continuing"
        );
        return Ok(Some(Layout::Aggregate));
    }
    let mut selected = None;
    for layout in [FLAT, NESTED] {
        if files
            .keys()
            .any(|path| path == Path::new(layout.header) || path.starts_with(layout.users))
        {
            ensure!(
                selected.is_none(),
                "mixed flat and nested access metadata; reconcile before continuing"
            );
            selected = Some(Layout::Split(layout));
        }
    }
    ensure!(
        selected.is_some() || files.is_empty(),
        "unexpected access metadata paths"
    );
    Ok(selected)
}

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
    path == Path::new(LEGACY)
        || [FLAT, NESTED]
            .iter()
            .any(|layout| path == Path::new(layout.header) || path.starts_with(layout.users))
}

fn is_raw_path(path: &[u8]) -> bool {
    path == LEGACY.as_bytes()
        || [FLAT, NESTED].iter().any(|layout| {
            path == layout.header.as_bytes()
                || path == layout.users.as_bytes()
                || path
                    .strip_prefix(layout.users.as_bytes())
                    .is_some_and(|suffix| suffix.starts_with(b"/"))
        })
}

/// Public bootstrap must remain readable before any repository key is available.
pub(crate) fn is_bootstrap_path(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name == ".gitattributes")
        || path.starts_with(".over/crypt")
        || path == Path::new(HEADER)
        || path.starts_with(USERS)
}

pub(crate) fn bootstrap_attributes(before: &[u8]) -> Vec<u8> {
    let mut bytes = before.to_vec();
    let newline = if before.windows(2).any(|bytes| bytes == b"\r\n") {
        b"\r\n".as_slice()
    } else {
        b"\n".as_slice()
    };
    for line in BOOTSTRAP_RULES.lines() {
        if !before
            .split(|byte| *byte == b'\n')
            .any(|existing| existing.strip_suffix(b"\r").unwrap_or(existing) == line.as_bytes())
        {
            if !bytes.is_empty() && !bytes.ends_with(b"\n") {
                bytes.extend_from_slice(newline);
            }
            bytes.extend_from_slice(line.as_bytes());
            bytes.extend_from_slice(newline);
        }
    }
    if before.is_empty() {
        // Do not filter legacy Age children before secret-store migration removes them.
        bytes.extend_from_slice(
            b"secrets filter=over diff=over -text\nsecrets.yaml filter=over diff=over -text\n",
        );
    }
    bytes
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
    let layout = match layout(files)? {
        None => return Ok(None),
        Some(Layout::Aggregate) => {
            return access::parse_registry(&files[Path::new(LEGACY)]).map(Some);
        }
        Some(Layout::Split(layout)) => layout,
    };
    let bytes = files
        .get(Path::new(layout.header))
        .with_context(|| format!("split access metadata is missing {}", layout.header))?;
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
        .filter(|(path, _)| path.as_path() != Path::new(layout.header))
    {
        let relative = path
            .strip_prefix(layout.users)
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
        if name == layout.marker {
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
        "user metadata is missing its {} record",
        layout.marker
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
    encode_for(registry, FLAT)
}

fn encode_for(registry: &Registry, layout: SplitLayout) -> Result<Files> {
    let header = || Header {
        version: 2,
        repository: registry.repository,
        generation: registry.generation,
    };
    let mut files = Files::from([(
        PathBuf::from(layout.header),
        serde_json::to_vec_pretty(&header())?,
    )]);
    for (user, keys) in &registry.users {
        portable_name(user)?;
        let directory = PathBuf::from(layout.users).join(user);
        files.insert(directory.join(layout.marker), b"{\"version\":2}\n".to_vec());
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

/// Preserve formatting and additive public fields on semantically unchanged
/// records; an unrelated enrollment must not normalize every file in the repo.
pub(crate) fn encode_preserving(registry: &Registry, before: &Files) -> Result<Files> {
    encode_preserving_as(registry, before, FLAT)
}

pub(crate) fn encode_preserving_as(
    registry: &Registry,
    before: &Files,
    target: SplitLayout,
) -> Result<Files> {
    let mut files = if target == FLAT {
        encode(registry)?
    } else {
        encode_for(registry, target)?
    };
    let Some(Layout::Split(source)) = layout(before)? else {
        return Ok(files);
    };
    let old = parse(before)?.context("missing original access metadata")?;
    let same_generation =
        old.repository == registry.repository && old.generation == registry.generation;
    if same_generation {
        files.insert(
            PathBuf::from(target.header),
            before[Path::new(source.header)].clone(),
        );
    }
    for (user, keys) in &registry.users {
        let directory = PathBuf::from(target.users).join(user);
        let original = PathBuf::from(source.users).join(user);
        if let Some(bytes) = before.get(&original.join(source.marker)) {
            files.insert(directory.join(target.marker), bytes.clone());
        }
        for (name, key) in keys {
            if same_generation && old.users.get(user).and_then(|keys| keys.get(name)) == Some(key) {
                let path = directory.join(name);
                if let Some(bytes) = before.get(&original.join(name)) {
                    files.insert(path, bytes.clone());
                }
            }
        }
    }
    Ok(files)
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
    for path in [LEGACY, HEADER, PREVIOUS_HEADER] {
        read_file(root, Path::new(path), &mut files)?;
    }
    for layout in [FLAT, NESTED] {
        let users = root.join(layout.users);
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
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
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
            let name = match std::str::from_utf8(entry.name_bytes()) {
                Ok(name) => name,
                Err(_) => {
                    // Only ASCII bootstrap prefixes are relevant outside users/;
                    // unrelated raw Git names must not prevent an access mutation.
                    ensure!(
                        ![FLAT, NESTED]
                            .iter()
                            .any(|layout| prefix.starts_with(layout.users)),
                        "non-UTF-8 access metadata path"
                    );
                    continue;
                }
            };
            let path = prefix.join(name);
            if entry.kind() == Some(git2::ObjectType::Tree) {
                ensure!(
                    ![LEGACY, HEADER, PREVIOUS_HEADER]
                        .iter()
                        .any(|header| path == Path::new(header)),
                    "access header must be a regular Git blob"
                );
                for layout in [FLAT, NESTED] {
                    if path.starts_with(layout.users) || Path::new(layout.users).starts_with(&path)
                    {
                        ensure!(
                            path.components().count()
                                <= Path::new(layout.users).components().count() + 1,
                            "unexpected access metadata nesting"
                        );
                        walk(repo, &repo.find_tree(entry.id())?, &path, files)?;
                        break;
                    }
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
        if is_raw_path(&entry.path) {
            let path = PathBuf::from(
                std::str::from_utf8(&entry.path).context("non-UTF-8 access metadata path")?,
            );
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
        assert!(files.contains_key(Path::new(".over/users/empty/user.json")));
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
            vec![Path::new(".over/users/alice/recovery")]
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
        let path = Path::new(".over/users/alice/laptop");
        let mut record: KeyRecord = serde_json::from_slice(&files[path]).unwrap();
        record.header.generation[0] ^= 1;
        files.insert(path.to_path_buf(), serde_json::to_vec(&record).unwrap());
        assert!(parse(&files).is_err());
    }

    #[rstest::rstest]
    #[case(FLAT)]
    #[case(NESTED)]
    fn bootstrap_loads_all_split_files_from_one_committed_tree(#[case] layout: SplitLayout) {
        let directory = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(directory.path()).unwrap();
        let registry = registry();
        let files = encode_for(&registry, layout).unwrap();
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

    #[test]
    fn flattening_preserves_header_key_and_user_record_bytes() {
        let registry = registry();
        let mut before = encode_for(&registry, NESTED).unwrap();
        for bytes in before.values_mut() {
            bytes.insert(0, b'\n');
            bytes.push(b'\n');
        }
        let after = encode_preserving(&registry, &before).unwrap();
        assert_eq!(parse(&after).unwrap(), Some(registry));
        assert_eq!(after[Path::new(HEADER)], before[Path::new(PREVIOUS_HEADER)]);
        assert_eq!(
            after[Path::new(".over/users/alice/user.json")],
            before[Path::new(".over/crypt/users/alice/.user")]
        );
        assert_eq!(
            after[Path::new(".over/users/alice/laptop")],
            before[Path::new(".over/crypt/users/alice/laptop")]
        );
        assert_eq!(
            after[Path::new(".over/users/empty/user.json")],
            before[Path::new(".over/crypt/users/empty/.user")]
        );
        assert!(portable_name(MARKER).is_err());
    }

    #[test]
    fn mixed_namespaces_are_rejected_even_with_the_same_repository_and_generation() {
        let registry = registry();
        let mut files = encode(&registry).unwrap();
        files.extend(encode_for(&registry, NESTED).unwrap());
        assert!(parse(&files).is_err());
    }

    #[rstest::rstest]
    #[case(FLAT)]
    #[case(NESTED)]
    fn orphan_records_and_incompatible_user_record_names_are_rejected(#[case] layout: SplitLayout) {
        let mut files = encode_for(&registry(), layout).unwrap();
        let header = files.remove(Path::new(layout.header)).unwrap();
        assert!(parse(&files).is_err());
        files.insert(PathBuf::from(layout.header), header);
        let marker = files
            .remove(
                &PathBuf::from(layout.users)
                    .join("alice")
                    .join(layout.marker),
            )
            .unwrap();
        let wrong = if layout == FLAT { ".user" } else { MARKER };
        files.insert(
            PathBuf::from(layout.users).join("alice").join(wrong),
            marker,
        );
        assert!(parse(&files).is_err());
    }

    #[rstest::rstest]
    #[case(FLAT)]
    #[case(NESTED)]
    fn compatibility_writers_preserve_the_existing_split_layout(#[case] layout: SplitLayout) {
        let directory = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(directory.path()).unwrap();
        let mut registry = registry();
        for (path, bytes) in encode_for(&registry, layout).unwrap() {
            let path = directory.path().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
        registry.users.remove("empty");
        access::save(&repo, &registry).unwrap();
        let files = worktree(&repo).unwrap();
        assert_eq!(self::layout(&files).unwrap(), Some(Layout::Split(layout)));
        assert_eq!(access::load(&repo).unwrap(), registry);
        assert_eq!(
            access::metadata_path(&repo).unwrap(),
            directory.path().join(layout.header)
        );
    }

    #[test]
    fn non_utf8_names_inside_access_metadata_are_still_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(directory.path()).unwrap();
        let files = encode(&registry()).unwrap();
        let mut staged = repo.index().unwrap();
        for (path, bytes) in &files {
            super::super::policy::stage_blob(&mut staged, &repo, path, bytes).unwrap();
        }
        let mut entry = staged
            .get_path(Path::new(".over/users/alice/laptop"), 0)
            .unwrap();
        entry.path = b".over/users/invalid-\xff/key".to_vec();
        staged.add(&entry).unwrap();
        assert!(index(&repo, &staged).is_err());
        let tree = repo.find_tree(staged.write_tree().unwrap()).unwrap();
        assert!(self::tree(&repo, &tree).is_err());
    }
}
